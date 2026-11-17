use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    os::windows::{fs::OpenOptionsExt, io::{AsRawHandle, FromRawSocket}, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    thread,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use url::{Host, Url};
use windows::Win32::{
    Foundation::{CloseHandle, LocalFree, ERROR_INSUFFICIENT_BUFFER, HANDLE, HLOCAL, NO_ERROR},
    NetworkManagement::IpHelper::{GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCP_STATE_ESTAB, TCP_TABLE_CLASS,
        TCP_TABLE_OWNER_PID_CONNECTIONS, TCP_TABLE_OWNER_PID_LISTENER},
    Networking::WinSock::AF_INET,
    Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    },
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};
use zeroize::Zeroizing;

const CORE_SHA256: &str = "02920479df2e64d67c0bbf5a22cbbf153cd987bb8bffa18ebcdf8a942000b01b";
pub const MAX_LINK: usize = 32 * 1024;
pub const SAVED_KEY_LIMIT: u64 = 64 * 1024;
pub const LINK_TOO_LONG: &str = "Ключ или ссылка VPN превышает 32 КБ. Запросите сокращённую ссылку у провайдера.";
pub const PROXY_ENV: &str = "LUMEN_VPN_LOOPBACK_PORT";

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub fn is_unencrypted(link: &str) -> bool {
    Url::parse(link).is_ok_and(|uri| is_unencrypted_uri(&uri))
}

pub(crate) const SUBSCRIPTION_PLACEHOLDER: &str =
    "Подписка вернула служебные записи вместо серверов VPN. Проверьте подписку или обратитесь в поддержку провайдера.";

pub(crate) fn is_subscription_placeholder(uri: &Url) -> bool {
    uri.scheme() == "vless"
        && uri.username() == "00000000-0000-0000-0000-000000000000"
        && uri.port() == Some(1)
        && is_unencrypted_uri(uri)
}

fn is_unencrypted_uri(uri: &Url) -> bool {
    uri.scheme() == "vless"
        && uri.query_pairs().any(|(key, value)| key == "security" && value == "none")
}

pub fn configuration(link: &str, socks: u16) -> io::Result<Value> {
    const FORMAT: &str = "Нужен корректный ключ vless:// с UUID, сервером и портом";
    if link.len() > MAX_LINK {
        return Err(invalid(LINK_TOO_LONG));
    }
    if link.chars().any(char::is_control) {
        return Err(invalid(FORMAT));
    }
    let uri = Url::parse(link).map_err(|_| invalid(FORMAT))?;
    if is_subscription_placeholder(&uri) {
        return Err(invalid(SUBSCRIPTION_PLACEHOLDER));
    }
    let id = uri.username();
    if uri.scheme() != "vless"
        || uri.password().is_some()
        || !matches!(uri.path(), "" | "/")
        || id.len() != 36
        || !id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
    {
        return Err(invalid(FORMAT));
    }
    let host =
        Host::parse(uri.host_str().ok_or_else(|| invalid(FORMAT))?).map_err(|_| invalid(FORMAT))?;
    let address = match &host {
        Host::Ipv6(ip) => ip.to_string(),
        _ => host.to_string(),
    };
    let port = uri
        .port()
        .filter(|&p| p > 0)
        .ok_or_else(|| invalid(FORMAT))?;
    let mut params = HashMap::new();
    for (key, value) in uri.query_pairs() {
        if !matches!(
            key.as_ref(),
            "type"
                | "security"
                | "encryption"
                | "flow"
                | "sni"
                | "fp"
                | "pbk"
                | "sid"
                | "spx"
                | "alpn"
                | "path"
                | "host"
                | "mode"
                | "headerType"
                | "allowInsecure"
        ) || value.chars().any(char::is_control)
            || params.insert(key, value).is_some()
        {
            return Err(invalid(
                "Ключ содержит повторяющийся или неподдерживаемый параметр",
            ));
        }
    }
    let param = |key, default| params.get(key).map(|v| v.as_ref()).unwrap_or(default);
    let network = match param("type", "tcp") {
        "tcp" | "raw" => "raw",
        "ws" => "ws",
        "xhttp" => "xhttp",
        _ => return Err(invalid("Поддерживаются транспорты TCP, WebSocket и XHTTP")),
    };
    let security = param("security", "");
    let flow = param("flow", "");
    if param("encryption", "none") != "none"
        || !matches!(param("allowInsecure", "0"), "0" | "false")
        || param("headerType", "none") != "none"
        || !(flow.is_empty() || flow == "xtls-rprx-vision" && network == "raw")
        || network == "raw" && (params.contains_key("host") || params.contains_key("path"))
        || network != "xhttp" && params.contains_key("mode")
        || security != "reality"
            && ["pbk", "sid", "spx"]
                .iter()
                .any(|key| params.contains_key(*key))
    {
        return Err(invalid("Ключ использует неподдерживаемые настройки VLESS"));
    }
    let sni = param("sni", &address);
    if sni.is_empty() || sni.contains(['/', '\\', '@', ' ']) {
        return Err(invalid("Некорректное имя сервера TLS"));
    }
    let mut stream = json!({ "network": network, "security": security });
    match security {
        "none" if network == "raw" && flow.is_empty() => {}
        "tls" => {
            let mut tls = json!({"serverName": sni});
            if let Some(fp) = params.get("fp") {
                tls["fingerprint"] = json!(fp);
            }
            if let Some(alpn) = params.get("alpn") {
                tls["alpn"] = json!(alpn.split(',').collect::<Vec<_>>());
            }
            stream["tlsSettings"] = tls;
        }
        "reality" if matches!(network, "raw" | "xhttp") && !params.contains_key("alpn") => {
            let key = param("pbk", "");
            let sid = param("sid", "");
            if !URL_SAFE_NO_PAD.decode(key).is_ok_and(|b| b.len() == 32)
                || sid.len() > 16
                || sid.len() % 2 != 0
                || !sid.bytes().all(|c| c.is_ascii_hexdigit())
                || !params.contains_key("sni")
            {
                return Err(invalid("Некорректные параметры REALITY"));
            }
            stream["realitySettings"] = json!({
                "serverName": sni, "fingerprint": param("fp", "chrome"), "password": key,
                "shortId": sid, "spiderX": param("spx", "")
            });
        }
        _ => {
            return Err(invalid(
                "Поддерживаются TLS и REALITY поверх TCP и XHTTP, TLS поверх WebSocket и TCP без шифрования",
            ))
        }
    }
    if network == "ws" {
        stream["wsSettings"] = json!({ "host": param("host", ""), "path": param("path", "/") });
    }
    if network == "xhttp" {
        let mode = param("mode", "auto");
        if !matches!(mode, "auto" | "packet-up" | "stream-up" | "stream-one") {
            return Err(invalid("Неизвестный режим XHTTP"));
        }
        stream["xhttpSettings"] = json!({
            "mode": mode, "host": param("host", ""), "path": param("path", "/")
        });
    }
    Ok(json!({
        "log": { "loglevel": "none" },
        "inbounds": [{
            "listen": "127.0.0.1", "port": socks, "protocol": "socks",
            "settings": { "auth": "noauth", "udp": false }
        }],
        "outbounds": [{
            "tag": "vpn",
            "protocol": "vless",
            "settings": { "vnext": [{ "address": address, "port": port,
                "users": [{ "id": id, "encryption": "none", "flow": flow }]
            }] },
            "streamSettings": stream
        }, { "tag": "blocked", "protocol": "blackhole" }],
        "routing": { "domainStrategy": "AsIs", "rules": [
            {"type":"field", "ip":["127.0.0.0/8","169.254.0.0/16","::1/128","fe80::/10"], "outboundTag":"blocked"},
            {"type":"field", "domain":["domain:localhost"], "outboundTag":"blocked"}
        ]}
    }))
}

pub(crate) fn dpapi(bytes: &[u8], decrypt: bool) -> io::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut out = CRYPT_INTEGER_BLOB::default();
    let result = unsafe {
        if decrypt {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        } else {
            CryptProtectData(
                &input,
                windows::core::PCWSTR::null(),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
    };
    result.map_err(|_| invalid("Windows не смог обработать сохранённый ключ VPN"))?;
    let data = unsafe {
        let data = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        if decrypt {
            for i in 0..out.cbData as usize {
                std::ptr::write_volatile(out.pbData.add(i), 0);
            }
        }
        let _ = LocalFree(HLOCAL(out.pbData.cast()));
        data
    };
    Ok(data)
}

pub fn protect(link: &str) -> io::Result<Vec<u8>> {
    configuration(link, 0)?;
    dpapi(link.as_bytes(), false)
}

fn verified_core() -> io::Result<(PathBuf, File)> {
    let path = std::env::current_exe()?.with_file_name("xray.exe");
    let file = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .map_err(|_| invalid("Рядом с Lumen не найден Xray-core для этой сборки"))?;
    let mut hash = Sha256::new();
    let size = io::copy(&mut (&file).take(64 * 1024 * 1024 + 1), &mut hash)?;
    if size > 64 * 1024 * 1024 || format!("{:x}", hash.finalize()) != CORE_SHA256 {
        return Err(invalid("Контрольная сумма Xray-core не совпадает"));
    }
    Ok((path, file))
}

pub fn check_core() -> io::Result<()> {
    verified_core().map(drop)
}

pub fn check_unencrypted_core_configuration(link: &str) -> io::Result<()> {
    let (executable, verified_file) = verified_core()?;
    let config = Zeroizing::new(serde_json::to_vec(&configuration(link, 1080)?)?);
    let mut command = Command::new(executable);
    command.env_clear();
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    let mut child = command
        .args(["run", "-test", "-format", "json", "-config", "stdin:"])
        .creation_flags(0x0800_0000)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    drop(verified_file);
    let sent = child.stdin.take().ok_or_else(|| invalid("Не удалось запустить Xray-core"))
        .and_then(|mut input| input.write_all(&config));
    let output = child.wait_with_output()?;
    if output.status.success() {
        return sent;
    }
    let prohibited = b"vless without TLS or other encryption is prohibited";
    if [&output.stdout, &output.stderr].iter().any(|bytes| {
        bytes.windows(prohibited.len()).any(|part| part == prohibited)
    }) {
        return Err(invalid("Xray отклоняет VLESS без шифрования для этого сервера. Запросите у провайдера ключ с TLS или REALITY."));
    }
    Err(invalid("Xray не принял параметры ключа VLESS без шифрования. Проверьте ключ у провайдера."))
}

struct Job(HANDLE);
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub struct Core {
    child: Child,
    _job: Job,
    pub port: u16,
    relay: Option<Relay>,
}

struct Relay {
    stop: Arc<AtomicBool>,
    address: SocketAddr,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if TcpStream::connect_timeout(&self.address, Duration::from_millis(100)).is_ok() {
            if let Some(worker) = self.worker.take() { let _ = worker.join(); }
        }
    }
}

impl Core {
    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn saved_key(path: &Path) -> io::Result<Zeroizing<String>> {
    let encrypted = crate::security::read_profile(path, SAVED_KEY_LIMIT)?;
    let config = Zeroizing::new(dpapi(&encrypted, true)?);
    let link =
        std::str::from_utf8(&config).map_err(|_| invalid("Некорректный сохранённый ключ VPN"))?;
    Ok(Zeroizing::new(link.to_owned()))
}

pub fn check_connection(port: u16) -> &'static str {
    if internet_available(port, "6") {
        "VPN подключён. Проверочный HTTPS-запрос прошёл через выбранный сервер."
    } else {
        "Xray запущен, но проверочный HTTPS-запрос через выбранный сервер не прошёл. Выберите другой сервер или проверьте подписку."
    }
}

pub fn internet_available(port: u16, timeout: &str) -> bool {
    let proxy = format!("socks5h://127.0.0.1:{port}");
    for url in [
        "https://connectivitycheck.gstatic.com/generate_204",
        "https://example.com/",
    ] {
        let Ok(mut curl) = crate::sys::hidden("curl.exe") else {
            break;
        };
        let response = curl
            .args([
                "-q",
                "--silent",
                "--output",
                "NUL",
                "--write-out",
                "%{http_code}",
                "--proxy",
                &proxy,
                "--proto",
                "=https",
                "--connect-timeout",
                "3",
                "--max-time",
                timeout,
                "--max-filesize",
                "8192",
                url,
            ])
            .env_remove("NO_PROXY")
            .stderr(Stdio::null())
            .output();
        if response.is_ok_and(|response| {
            response.status.success()
                && matches!(
                    response.stdout.as_slice(),
                    [b'2'..=b'5', b'0'..=b'9', b'0'..=b'9']
                )
        }) {
            return true;
        }
    }
    false
}

fn inspect_tcp_table(class: TCP_TABLE_CLASS, inspect: impl FnOnce(&[MIB_TCPROW_OWNER_PID]) -> bool) -> bool {
    let query = |buffer, size: &mut u32| unsafe {
        GetExtendedTcpTable(buffer, size, false, u32::from(AF_INET.0), class, 0)
    };
    let mut size = 0;
    if query(None, &mut size) != ERROR_INSUFFICIENT_BUFFER.0 || size > 8 * 1024 * 1024 {
        return false;
    }
    let mut table = vec![0u32; (size as usize).div_ceil(size_of::<u32>())];
    if query(Some(table.as_mut_ptr().cast()), &mut size) != NO_ERROR.0 {
        return false;
    }
    let Some(&count) = table.first() else { return false; };
    let count = count as usize;
    if count > (table.len() - 1) * size_of::<u32>() / size_of::<MIB_TCPROW_OWNER_PID>() {
        return false;
    }
    // Windows returns an aligned DWORD count followed by the IPv4 listener rows.
    let rows = unsafe { std::slice::from_raw_parts(table.as_ptr().add(1).cast::<MIB_TCPROW_OWNER_PID>(), count) };
    inspect(rows)
}

fn owns_listener(port: u16, pid: u32) -> bool {
    inspect_tcp_table(TCP_TABLE_OWNER_PID_LISTENER, |rows| {
        let mut matching = rows.iter().filter(|row| {
        matches!(row.dwLocalAddr.to_ne_bytes(), [127, 0, 0, 1] | [0, 0, 0, 0])
            && u16::from_be(row.dwLocalPort as u16) == port
    });
        matching.next().is_some_and(|row| row.dwOwningPid == pid && row.dwLocalAddr != 0)
            && matching.next().is_none()
    })
}

fn owns_connection(stream: &TcpStream, upstream_port: u16, pid: u32) -> bool {
    let Ok(client) = stream.local_addr() else { return false; };
    inspect_tcp_table(TCP_TABLE_OWNER_PID_CONNECTIONS, |rows| rows.iter().any(|row| {
        row.dwOwningPid == pid
            && row.dwState == MIB_TCP_STATE_ESTAB.0 as u32
            && row.dwLocalAddr.to_ne_bytes() == [127, 0, 0, 1]
            && row.dwRemoteAddr.to_ne_bytes() == [127, 0, 0, 1]
            && u16::from_be(row.dwLocalPort as u16) == upstream_port
            && u16::from_be(row.dwRemotePort as u16) == client.port()
    }))
}

pub fn exclusive_listener() -> io::Result<TcpListener> {
    use windows::Win32::Networking::WinSock::*;
    // Initialize Winsock through the standard library before using its raw API.
    std::net::UdpSocket::bind("127.0.0.1:0")?;
    unsafe {
        let socket = WSASocketW(i32::from(AF_INET.0), SOCK_STREAM.0, IPPROTO_TCP.0, None, 0,
            WSA_FLAG_NO_HANDLE_INHERIT | WSA_FLAG_OVERLAPPED)?;
        let listener = TcpListener::from_raw_socket(socket.0 as _);
        let address = SOCKADDR_IN {
            sin_family: AF_INET,
            sin_addr: IN_ADDR { S_un: IN_ADDR_0 { S_addr: u32::from_ne_bytes([127, 0, 0, 1]) } },
            ..Default::default()
        };
        if setsockopt(socket, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, Some(&1i32.to_ne_bytes())) != 0
            || bind(socket, (&address as *const SOCKADDR_IN).cast(), size_of::<SOCKADDR_IN>() as i32) != 0
            || listen(socket, 128) != 0
        {
            return Err(io::Error::from_raw_os_error(WSAGetLastError().0));
        }
        Ok(listener)
    }
}

fn relay_connections(listener: TcpListener, upstream_port: u16, pid: u32) -> io::Result<Relay> {
    let address = listener.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let active = stop.clone();
    let upstream = ([127, 0, 0, 1], upstream_port).into();
    let worker = thread::Builder::new().spawn(move || {
        for client in listener.incoming() {
            if active.load(Ordering::Acquire) { break; }
            let Ok(client) = client else { continue; };
            if Arc::strong_count(&active) >= 130 { continue; }
            let active = active.clone();
            let _ = thread::Builder::new().spawn(move || {
                let Ok(server) = TcpStream::connect_timeout(&upstream, Duration::from_millis(200)) else { return; };
                // Check the accepted connection before sending any browser bytes.
                if active.load(Ordering::Acquire) || !owns_connection(&server, upstream_port, pid) { return; }
                let (client, server) = (&client, &server);
                thread::scope(|scope| {
                    let outbound = |source: &TcpStream, destination: &TcpStream| {
                        let result = io::copy(&mut &*source, &mut &*destination);
                        let _ = destination.shutdown(if result.is_ok() { Shutdown::Write } else { Shutdown::Both });
                    };
                    let Ok(_) = thread::Builder::new().spawn_scoped(scope, move || outbound(client, server)) else { return; };
                    outbound(server, client);
                });
            });
        }
    })?;
    Ok(Relay { stop, address, worker: Some(worker) })
}

#[test]
fn vpn_checks_windows_listener_owner() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    assert!(owns_listener(port, std::process::id()));
    assert!(!owns_listener(port, std::process::id().wrapping_add(1)));
}

pub fn start(link: &str) -> io::Result<Core> {
    let (executable, verified_file) = verified_core()?;
    let frontend = exclusive_listener()?;
    let frontend_port = frontend.local_addr()?.port();
    let reservation = TcpListener::bind("127.0.0.1:0")?;
    let port = reservation.local_addr()?.port();
    let config = Zeroizing::new(serde_json::to_vec(&configuration(link, port)?)?);
    let job = Job(unsafe { CreateJobObjectW(None, windows::core::PCWSTR::null()) }?);
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )?;
    }
    let mut command = Command::new(executable);
    command.env_clear();
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    let child = command
        .args(["run", "-format", "json", "-config", "stdin:"])
        .creation_flags(0x0800_0000)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    drop(verified_file);
    let mut core = Core {
        child,
        _job: job,
        port: frontend_port,
        relay: None,
    };
    unsafe {
        AssignProcessToJobObject(core._job.0, HANDLE(core.child.as_raw_handle()))?;
    }
    // The executable waits for this pipe before it can open a listener.
    let mut input = core
        .child
        .stdin
        .take()
        .ok_or_else(|| invalid("Не удалось запустить Xray-core"))?;
    input.write_all(&config)?;
    drop(reservation);
    drop(input);

    let addr = ([127, 0, 0, 1], port).into();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && core.running() {
        if owns_listener(port, core.child.id()) {
            let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(100)) else { break; };
            let mut response = [0; 2];
            stream.set_read_timeout(Some(Duration::from_millis(100)))?;
            stream.set_write_timeout(Some(Duration::from_millis(100)))?;
            if stream.write_all(&[5, 1, 0]).is_ok()
                && stream.read_exact(&mut response).is_ok()
                && response == [5, 0]
                && core.running()
                && owns_listener(port, core.child.id())
            {
                core.relay = Some(relay_connections(frontend, port, core.child.id())?);
                return Ok(core);
            }
        }
        thread::sleep(Duration::from_millis(30));
    }
    Err(invalid(
        "Xray-core не запустил локальный прокси. Проверьте параметры ключа",
    ))
}

pub fn browser_args(base: &str, port: u16) -> String {
    format!(
        r#"{base} --proxy-server=socks5://127.0.0.1:{port} --proxy-bypass-list=<-loopback> --disable-quic --host-resolver-rules="MAP * ~NOTFOUND, EXCLUDE 127.0.0.1""#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: &str = "vless://6c80c966-2aad-48fc-8918-97caede4712d@proxy.example:443";

    #[test]
    fn xhttp_configuration_preserves_reality_path_and_mode() {
        let pk = URL_SAFE_NO_PAD.encode([9; 32]);
        let link = format!("{LINK}?type=xhttp&security=reality&sni=proxy.example&pbk={pk}&path=%2Fedge&mode=stream-one");
        let config = configuration(&link, 1234).unwrap();
        let stream = &config["outbounds"][0]["streamSettings"];
        assert_eq!(stream["network"], "xhttp");
        assert_eq!(stream["realitySettings"]["password"], pk);
        assert_eq!(stream["xhttpSettings"]["path"], "/edge");
        assert_eq!(stream["xhttpSettings"]["mode"], "stream-one");
    }

    #[test]
    fn vless_links_compile_to_loopback_only_and_reject_unsafe_options() {
        let config =
            configuration(&format!("{LINK}?security=tls&type=ws&path=%2Fproxy"), 1234).unwrap();
        assert_eq!(config["inbounds"][0]["listen"], "127.0.0.1");
        assert_eq!(config["inbounds"][0]["settings"]["udp"], false);
        assert_eq!(
            config["outbounds"][0]["streamSettings"]["wsSettings"]["path"],
            "/proxy"
        );
        assert_eq!(
            config["outbounds"][0]["streamSettings"]["tlsSettings"]["serverName"],
            "proxy.example"
        );
        assert_eq!(config["outbounds"].as_array().unwrap().len(), 2);
        for query in [
            "security=none&type=ws",
            "security=none&flow=xtls-rprx-vision",
            "security=tls&allowInsecure=1",
            "security=tls&security=none",
            "security=tls&type=grpc",
            "security=reality&sni=proxy.example&pbk=invalid",
        ] {
            assert!(
                configuration(&format!("{LINK}?{query}"), 1234).is_err(),
                "{query}"
            );
        }
    }

    #[test]
    fn explicit_unencrypted_tcp_config_has_no_tls_or_direct_fallback() {
        let link = format!("{LINK}?type=tcp&encryption=none&security=none");
        assert!(is_unencrypted(&link));
        assert!(!is_unencrypted(&format!("{LINK}?security=tls")));
        let config = configuration(&link, 1234).unwrap();
        let stream = &config["outbounds"][0]["streamSettings"];
        assert_eq!(stream, &json!({"network": "raw", "security": "none"}));
        assert_eq!(config["outbounds"][1]["protocol"], "blackhole");
    }

    #[test]
    #[ignore = "Requires the pinned Xray executable next to the test binary"]
    fn pinned_xray_reports_unencrypted_public_server_before_restart() {
        let public = "vless://6c80c966-2aad-48fc-8918-97caede4712d@8.8.8.8:443?security=none";
        let error = check_unencrypted_core_configuration(public).unwrap_err();
        assert!(error.to_string().contains("Xray отклоняет VLESS без шифрования"));
        let private = "vless://6c80c966-2aad-48fc-8918-97caede4712d@192.168.1.2:443?security=none";
        check_unencrypted_core_configuration(private).unwrap();
    }

    #[test]
    fn vless_key_round_trips_through_current_user_dpapi() {
        let link = format!("{LINK}?security=tls");
        let protected = protect(&link).unwrap();
        assert!(!protected
            .windows(link.len())
            .any(|bytes| bytes == link.as_bytes()));
        assert_eq!(dpapi(&protected, true).unwrap(), link.as_bytes());
        let mut corrupted = protected;
        corrupted[0] ^= 255;
        assert!(dpapi(&corrupted, true).is_err());
    }
}
