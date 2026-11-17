use std::{
    collections::HashSet,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, ToSocketAddrs},
    path::Path,
    process::Stdio,
};

use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use serde::{Deserialize, Serialize};
use url::{Host, Url};
use zeroize::Zeroizing;

const BODY_LIMIT: usize = 2 * 1024 * 1024;
const HEADER_LIMIT: usize = 32 * 1024;
const MAX_SERVERS: usize = 64;

#[derive(Deserialize, Serialize)]
pub struct Server {
    pub label: String,
    pub link: Zeroizing<String>,
}

const SAVED_LIMIT: u64 = 4 * 1024 * 1024;

pub fn load_servers(path: &Path) -> io::Result<Vec<Server>> {
    let encrypted = crate::security::read_profile(path, SAVED_LIMIT)?;
    let plain = Zeroizing::new(super::vpn::dpapi(&encrypted, true)?);
    let mut servers: Vec<Server> = serde_json::from_slice(&plain)?;
    servers.truncate(MAX_SERVERS);
    servers.retain(|server| super::vpn::configuration(&server.link, 0).is_ok());
    servers.sort_by_key(|server| super::vpn::is_unencrypted(&server.link));
    Ok(servers)
}

pub fn save_servers(path: &Path, servers: &[Server]) -> io::Result<()> {
    let plain = Zeroizing::new(serde_json::to_vec(servers)?);
    let encrypted = super::vpn::dpapi(&plain, false)?;
    if encrypted.len() as u64 > SAVED_LIMIT {
        return Err(io::Error::other(
            "VPN server list exceeds the profile limit",
        ));
    }
    crate::security::atomic_write(path, &encrypted)
}

fn parse_url(raw: &str) -> Result<Url, &'static str> {
    const BAD: &str = "Укажите HTTPS-ссылку подписки без логина, пароля и нестандартного порта";
    if raw.len() > super::vpn::MAX_LINK {
        return Err(super::vpn::LINK_TOO_LONG);
    }
    if raw.chars().any(char::is_control) {
        return Err(BAD);
    }
    let mut url = Url::parse(raw).map_err(|_| BAD)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.host().is_none()
        || url.as_str().contains(['"', '\\'])
    {
        return Err(BAD);
    }
    url.set_fragment(None);
    Ok(url)
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !matches!(
        (a, b, c),
        (0 | 10 | 127 | 224..=255, _, _)
            | (100, 64..=127, _)
            | (169, 254, _)
            | (172, 16..=31, _)
            | (192, 168, _)
            | (192, 0, 0 | 2)
            | (192, 88, 99)
            | (198, 18 | 19, _)
            | (198, 51, 100)
            | (203, 0, 113)
    )
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            let [a, b, ..] = ip.segments();
            (0x2000..=0x3fff).contains(&a)
                && !(a == 0x2001 && (b == 0x0db8 || b < 0x0200))
                && a != 0x2002
                && a != 0x3fff
        }
    }
}

fn resolve(url: &Url) -> Result<Option<String>, &'static str> {
    const LOCAL: &str = "Ссылка подписки должна вести на публичный HTTPS-сервер";
    match url.host().ok_or(LOCAL)? {
        Host::Ipv4(ip) if public_ip(ip.into()) => Ok(None),
        Host::Ipv6(ip) if public_ip(ip.into()) => Ok(None),
        Host::Domain(host)
            if host.contains('.')
                && !host.trim_end_matches('.').ends_with(".localhost")
                && !host.trim_end_matches('.').ends_with(".local")
                && host.trim_end_matches('.') != "localhost" =>
        {
            let addrs = (host, 443)
                .to_socket_addrs()
                .map_err(|_| "Не удалось найти сервер подписки")?;
            let mut pinned = Vec::new();
            for addr in addrs {
                let ip = addr.ip();
                if !public_ip(ip) {
                    return Err(LOCAL);
                }
                if pinned.len() < 8 && !pinned.contains(&ip) {
                    pinned.push(ip);
                }
            }
            if pinned.is_empty() {
                return Err("Не удалось найти сервер подписки");
            }
            let csv = pinned
                .iter()
                .map(|ip| match ip {
                    IpAddr::V4(ip) => ip.to_string(),
                    IpAddr::V6(ip) => format!("[{ip}]"),
                })
                .collect::<Vec<_>>()
                .join(",");
            Ok(Some(format!("{host}:443:{csv}")))
        }
        _ => Err(LOCAL),
    }
}

fn request(url: &Url, cookie: Option<&str>) -> Result<Zeroizing<Vec<u8>>, &'static str> {
    const FAILED: &str = "Не удалось получить подписку по HTTPS. Проверьте ссылку и подключение";
    let pinned = resolve(url)?;
    let mut command = crate::sys::hidden("curl.exe").map_err(|_| FAILED)?;
    command.args([
        "-q",
        "--config",
        "-",
        "--silent",
        "--include",
        "--proto",
        "=https",
        "--globoff",
        "--noproxy",
        "*",
        "--connect-timeout",
        "4",
        "--max-time",
        "8",
        // Bound the decoded stream below, including compressed or chunked replies.
        "--compressed",
        "--header",
        "Accept: text/plain, application/octet-stream;q=0.9, */*;q=0.1",
        "--user-agent",
        concat!("Lumen/", env!("CARGO_PKG_VERSION")),
    ]);
    if let Some(pinned) = &pinned {
        command.args(["--resolve", pinned]);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| FAILED)?;
    let mut result = Zeroizing::new(Vec::new());
    let read = (|| {
        // Config via stdin keeps the subscription token out of the process arguments.
        let mut input = child.stdin.take().ok_or(FAILED)?;
        writeln!(input, "url = \"{url}\"").map_err(|_| FAILED)?;
        if let Some(cookie) = cookie {
            writeln!(input, "cookie = \"{cookie}\"").map_err(|_| FAILED)?;
        }
        drop(input);
        child
            .stdout
            .take()
            .ok_or(FAILED)?
            .take((BODY_LIMIT + HEADER_LIMIT + 1) as u64)
            .read_to_end(&mut result)
            .map_err(|_| FAILED)?;
        Ok::<_, &'static str>(())
    })();
    if read.is_err() || result.len() > BODY_LIMIT + HEADER_LIMIT {
        let _ = child.kill();
        let _ = child.wait();
        return Err(if read.is_err() {
            FAILED
        } else {
            "Ответ подписки слишком большой"
        });
    }
    let status = child.wait().map_err(|_| FAILED)?;
    if !status.success() {
        return Err(FAILED);
    }
    Ok(result)
}

fn from_response(bytes: &[u8]) -> Result<(u16, Option<&str>, &[u8]), &'static str> {
    const BAD: &str = "Сервер вернул некорректный ответ подписки";
    let split = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .ok_or(BAD)?;
    if split > HEADER_LIMIT {
        return Err(BAD);
    }
    let headers = std::str::from_utf8(&bytes[..split]).map_err(|_| BAD)?;
    let mut lines = headers.lines();
    let code = lines
        .next()
        .ok_or(BAD)?
        .split_ascii_whitespace()
        .nth(1)
        .ok_or(BAD)?
        .parse()
        .map_err(|_| BAD)?;
    let location = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("location"))
        .map(|(_, value)| value.trim());
    Ok((code, location, &bytes[split + 4..]))
}

fn retry_cookie(response: &[u8]) -> Option<Zeroizing<String>> {
    let end = response.windows(4).position(|part| part == b"\r\n\r\n")?;
    let headers = std::str::from_utf8(&response[..end]).ok()?;
    let mut cookies = Zeroizing::new(String::new());
    for (name, value) in headers.lines().filter_map(|line| line.split_once(':')) {
        if !name.eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        let pair = value.split(';').next()?.trim();
        let Some((key, value)) = pair.split_once('=') else { continue; };
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || value.is_empty()
            || !value.bytes().all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
        {
            continue;
        }
        if cookies.len() + pair.len() + 2 > 4096 {
            return None;
        }
        if !cookies.is_empty() {
            cookies.push_str("; ");
        }
        cookies.push_str(pair);
    }
    (!cookies.is_empty()).then_some(cookies)
}

pub fn import(raw: &str) -> Result<Vec<Server>, &'static str> {
    let mut url = parse_url(raw)?;
    let mut cookie: Option<Zeroizing<String>> = None;
    for _ in 0..=3 {
        let response = request(&url, cookie.as_ref().map(|cookie| cookie.as_str()))?;
        let (code, location, body) = from_response(&response)?;
        match code {
            200 => return parse_body(body),
            301 | 302 | 303 | 307 | 308 => {
                let next = url
                    .join(location.ok_or("Сервер подписки не указал адрес перенаправления")?)
                    .map_err(|_| "Некорректное перенаправление подписки")?;
                let next = parse_url(next.as_str())?;
                // This retry is scoped to the identical HTTPS URL and never persisted.
                cookie = if next == url && cookie.is_none() {
                    retry_cookie(&response)
                } else {
                    None
                };
                if next == url && cookie.is_none() {
                    return Err("Сервер подписки повторяет перенаправление и не отдаёт список серверов");
                }
                url = next;
            }
            401 | 403 => return Err("Сервер отклонил ссылку. Проверьте, активна ли подписка"),
            404 | 410 => return Err("Ссылка подписки не найдена или уже недействительна"),
            414 => return Err("Сервер подписки отклонил слишком длинный адрес. Запросите короткую ссылку у провайдера."),
            429 => return Err("Сервер подписки ограничил запросы. Попробуйте импортировать позже."),
            _ => return Err("Сервер вернул ошибку при загрузке подписки"),
        }
    }
    Err("Слишком много перенаправлений подписки")
}

fn parse_body(body: &[u8]) -> Result<Vec<Server>, &'static str> {
    const FORMAT: &str =
        "Подписка должна возвращать список ключей VLESS обычным текстом или Base64";
    if body.len() > BODY_LIMIT {
        return Err("Ответ подписки слишком большой");
    }
    let text = std::str::from_utf8(body)
        .map_err(|_| FORMAT)?
        .trim_matches(|c: char| c == '\u{feff}' || c.is_whitespace());
    let decoded;
    let text = if text
        .lines()
        .any(|line| line.trim_start().get(..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("vless://")))
    {
        text
    } else {
        let compact = Zeroizing::new(
            text.bytes()
                .filter(|b| !b.is_ascii_whitespace())
                .collect::<Vec<_>>(),
        );
        decoded = Zeroizing::new(
            [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD]
                .into_iter()
                .find_map(|engine| engine.decode(&compact[..]).ok())
                .ok_or(FORMAT)?,
        );
        std::str::from_utf8(&decoded).map_err(|_| FORMAT)?
    };

    let mut servers = Vec::new();
    let mut seen = HashSet::new();
    let mut oversized = false;
    let mut has_placeholder = false;
    for line in text
        .lines()
        .map(|line| line.trim().trim_start_matches('\u{feff}'))
    {
        if !line.get(..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("vless://")) {
            continue;
        }
        if line.len() > super::vpn::MAX_LINK {
            oversized = true;
            continue;
        }
        let Ok(url) = Url::parse(line) else {
            continue;
        };
        if super::vpn::is_subscription_placeholder(&url) {
            has_placeholder = true;
            continue;
        }
        if super::vpn::configuration(line, 0).is_err() {
            continue;
        }
        let base = line.split('#').next().unwrap_or(line);
        if !seen.insert(base) {
            continue;
        }
        let label = url
            .fragment()
            .and_then(|label| urlencoding::decode(label).ok())
            .map(|label| {
                label
                    .chars()
                    .filter(|c| {
                        !c.is_control()
                            && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                    })
                    .take(80)
                    .collect::<String>()
            })
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| url.host_str().unwrap_or("Сервер VLESS").to_owned());
        servers.push(Server {
            label,
            link: Zeroizing::new(base.to_owned()),
        });
        if servers.len() == MAX_SERVERS {
            break;
        }
    }
    if servers.is_empty() {
        if has_placeholder { return Err(super::vpn::SUBSCRIPTION_PLACEHOLDER); }
        if oversized { return Err(super::vpn::LINK_TOO_LONG); }
        return Err("В подписке нет совместимых серверов VLESS. Проверьте протокол, транспорт и параметры ключа.");
    }
    servers.sort_by_key(|server| super::vpn::is_unencrypted(&server.link));
    Ok(servers)
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn service_messages_do_not_replace_usable_servers() {
        let placeholder = "vless://00000000-0000-0000-0000-000000000000@notice.test:1?encryption=none&type=tcp&security=none";
        let notices = format!("{placeholder}#Support\n{placeholder}#Open%20bot");
        let encoded = STANDARD.encode(&notices);
        assert_eq!(parse_body(encoded.as_bytes()).err().unwrap(), crate::vpn::SUBSCRIPTION_PLACEHOLDER);
        assert!(crate::vpn::configuration(placeholder, 0).is_err());

        let valid = "vless://6c80c966-2aad-48fc-8918-97caede4712d@vpn.test:443?security=tls";
        let mixed = format!("{notices}\n{valid}#Working");
        let servers = parse_body(mixed.as_bytes()).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].link.as_str(), valid);
    }

    #[test]
    fn server_list_survives_reload_and_is_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vpn-servers.dpapi");
        let link = "vless://6c80c966-2aad-48fc-8918-97caede4712d@vpn.test:443?security=tls";
        let entries = parse_body(format!("{link}#Fixture").as_bytes()).unwrap();
        save_servers(&path, &entries).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes
            .windows(link.len())
            .any(|bytes| bytes == link.as_bytes()));
        let restored = load_servers(&path).unwrap();
        assert_eq!(restored[0].label, "Fixture");
        assert_eq!(restored[0].link.as_str(), link);
    }

    #[test]
    fn text_and_base64_subscriptions_keep_supported_nodes_and_labels() {
        let tcp = "vless://6c80c966-2aad-48fc-8918-97caede4712d@vpn.test:443?security=tls";
        let other =
            "vless://6c80c966-2aad-48fc-8918-97caede4712d@second.test:443?security=tls&type=ws";
        let content = format!("\u{feff}{tcp}#Paris%20%231\r\n{tcp}#duplicate\n{other}\n{other}&allowInsecure=1\nvmess://skip\n");
        for input in [
            content.as_bytes().to_vec(),
            STANDARD.encode(&content).into_bytes(),
            URL_SAFE_NO_PAD.encode(&content).into_bytes(),
        ] {
            let list = parse_body(&input).unwrap();
            assert_eq!(list.len(), 2);
            assert_eq!(list[0].label, "Paris #1");
            assert_eq!(list[0].link.as_str(), tcp);
            assert_eq!(list[1].label, "second.test");
            assert_eq!(list[1].link.as_str(), other);
        }
        assert!(parse_body(b"<!doctype html><title>Login</title>").is_err());
        assert!(parse_body(&vec![0; BODY_LIMIT + 1]).is_err());
    }

    #[test]
    fn longer_links_import_and_survive_encrypted_storage() {
        let raw = format!("https://example.test/subscribe?token={}", "a".repeat(18_000));
        assert_eq!(parse_url(&raw).unwrap().as_str(), raw);
        let node = format!("vless://6c80c966-2aad-48fc-8918-97caede4712d@vpn.test:443?security=tls&type=xhttp&path=%2F{}", "b".repeat(18_000));
        let response = format!("{node}\n").repeat(12);
        let servers = parse_body(STANDARD.encode(response).as_bytes()).unwrap();
        assert_eq!(servers.len(), 1);
        let dir = tempfile::tempdir().unwrap();
        let selected = dir.path().join("vpn.dpapi");
        std::fs::write(&selected, crate::vpn::protect(&node).unwrap()).unwrap();
        assert_eq!(crate::vpn::saved_key(&selected).unwrap().as_str(), node);
        let path = dir.path().join("vpn-servers.dpapi");
        save_servers(&path, &servers).unwrap();
        assert_eq!(load_servers(&path).unwrap()[0].link.as_str(), node);
        let too_long = format!("{node}{}", "x".repeat(crate::vpn::MAX_LINK));
        assert!(matches!(parse_body(too_long.as_bytes()), Err(message) if message == crate::vpn::LINK_TOO_LONG));
        assert_eq!(parse_url(&format!("{raw}{}", "x".repeat(crate::vpn::MAX_LINK))).unwrap_err(), crate::vpn::LINK_TOO_LONG);
    }

    #[test]
    fn imports_explicit_unencrypted_tcp_and_tls_servers() {
        let node = "vless://6c80c966-2aad-48fc-8918-97caede4712d@vpn.test:443?encryption=none&type=tcp";
        let plain = format!("{node}&security=none");
        let tls = format!("{node}&security=tls");
        for payload in [plain.clone(), STANDARD.encode(&plain)] {
            let servers = parse_body(payload.as_bytes()).unwrap();
            assert_eq!(servers.len(), 1);
            assert!(crate::vpn::is_unencrypted(&servers[0].link));
        }
        let mixed = format!("{plain}\n{tls}");
        let servers = parse_body(mixed.as_bytes()).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].link.as_str(), tls);
        assert_eq!(servers[1].link.as_str(), plain);
    }

    #[test]
    fn parses_bounded_cookie_for_same_url_retry() {
        let response = b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /sub\r\nSet-Cookie: retry=fixture_abc; Secure; HttpOnly\r\nSet-Cookie: extra=one; Path=/\r\n\r\n";
        assert_eq!(retry_cookie(response).as_ref().map(|value| value.as_str()), Some("retry=fixture_abc; extra=one"));
        assert!(retry_cookie(b"HTTP/1.1 307 Temporary Redirect\r\nSet-Cookie: retry=bad\"value\r\n\r\n").is_none());
    }

    #[test]
    fn subscription_fetch_blocks_non_public_destinations_and_keeps_https() {
        for url in [
            "https://127.0.0.1/",
            "https://192.168.1.2/",
            "https://[::ffff:127.0.0.1]/",
            "https://s.local/",
        ] {
            assert!(resolve(&parse_url(url).unwrap()).is_err(), "{url}");
        }
        for raw in [
            "http://example.test/",
            "https://u:p@example.test/",
            "https://example.test:8443/",
        ] {
            assert!(parse_url(raw).is_err(), "{raw}");
        }
        assert!(resolve(&parse_url("https://1.1.1.1/").unwrap())
            .unwrap()
            .is_none());
        let (status, location, body) =
            from_response(b"HTTP/2 307\r\nLocation: /sub/abc\r\n\r\n").unwrap();
        assert_eq!(
            (status, location, body),
            (307, Some("/sub/abc"), b"".as_slice())
        );
    }
}
