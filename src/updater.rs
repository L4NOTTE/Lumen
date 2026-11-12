//! The release server supplies bytes. Only the embedded signing key authorizes an update.
use base64::{engine::general_purpose::STANDARD, Engine};
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

const RELEASES: &str = "https://github.com/L4NOTTE/Lumen/releases";
const KEY: &[u8; 32] = include_bytes!("../assets/update-public.bin");
const LIMIT: u64 = 64 * 1024 * 1024;
#[derive(Deserialize)]
struct Envelope {
    payload: String,
    signature: String,
}
#[derive(Deserialize)]
struct Manifest {
    version: String,
    expires: u64,
    windows_x64: Option<Asset>,
    linux_x64: Option<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    size: u64,
    sha256: String,
}

fn version(value: &str) -> Option<[u32; 3]> {
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut parsed = [0; 3];
    for (i, part) in parts.into_iter().enumerate() {
        parsed[i] = part.parse().ok()?;
        if parsed[i].to_string() != part {
            return None;
        }
    }
    Some(parsed)
}
fn authenticate(bytes: &[u8], key: &[u8], current: &str, now: u64) -> Option<Manifest> {
    if bytes.len() > 16384 {
        return None;
    }
    let envelope: Envelope = serde_json::from_slice(bytes).ok()?;
    let payload = STANDARD.decode(envelope.payload).ok()?;
    let signature = STANDARD.decode(envelope.signature).ok()?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&payload, &signature)
        .ok()?;
    let manifest: Manifest = serde_json::from_slice(&payload).ok()?;
    if version(&manifest.version)? <= version(current)?
        || manifest.expires <= now
        || manifest.expires > now.saturating_add(31 * 86400)
    {
        return None;
    }
    Some(manifest)
}
fn verify_asset(path: &Path, asset: &Asset) -> io::Result<()> {
    let file = fs::File::open(path)?;
    if asset.size == 0 || asset.size > LIMIT || file.metadata()?.len() != asset.size {
        return Err(io::Error::other("update size mismatch"));
    }
    let mut hasher = Sha256::new();
    let size = io::copy(&mut file.take(asset.size + 1), &mut hasher)?;
    if size != asset.size || format!("{:x}", hasher.finalize()) != asset.sha256 {
        return Err(io::Error::other("update SHA-256 mismatch"));
    }
    Ok(())
}
fn fetch(url: &str, output: &Path, limit: u64) -> bool {
    #[cfg(windows)]
    let mut command = crate::sys::hidden("curl.exe");
    #[cfg(target_os = "linux")]
    let mut command = std::process::Command::new("/usr/bin/curl");
    command
        .args([
            "-q",
            "--fail",
            "--silent",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "4",
            "--connect-timeout",
            "5",
            "--max-time",
            "60",
            "--max-filesize",
        ])
        .arg(limit.to_string())
        .arg("--output")
        .arg(output)
        .arg("--")
        .arg(url)
        .status()
        .is_ok_and(|status| status.success())
}
pub struct Pending {
    stage: tempfile::TempDir,
    target: PathBuf,
    asset: Asset,
    expires: u64,
    previous_modified: SystemTime,
}
const NAME: &str = if cfg!(windows) {
    "LumenSetup.exe"
} else {
    "lumen-linux-x64"
};

pub fn start(enabled: bool) -> mpsc::Receiver<Pending> {
    let (send, receive) = mpsc::channel();
    if !enabled || !cfg!(target_arch = "x86_64") || std::env::var_os("LUMEN_TEST_HIDDEN").is_some()
    {
        return receive;
    }
    let Ok(target) = std::env::current_exe() else {
        return receive;
    };
    #[cfg(windows)]
    let installed = target.file_name().is_some_and(|n| n == "lumen.exe")
        && target.with_file_name("unins000.exe").is_file();
    #[cfg(target_os = "linux")]
    let installed = dirs::home_dir().is_some_and(|p| p.join(".local/bin/lumen") == target);
    if !installed {
        return receive;
    }
    std::thread::spawn(move || {
        if let Some(update) = check(target) {
            let _ = send.send(update);
        }
    });
    receive
}
fn check(target: PathBuf) -> Option<Pending> {
    let previous_modified = fs::metadata(&target).ok()?.modified().ok()?;
    let stage = tempfile::Builder::new()
        .prefix("lumen-update-")
        .tempdir()
        .ok()?;
    let feed = stage.path().join("manifest.json");
    if !fetch(
        &format!("{RELEASES}/latest/download/lumen-update.json"),
        &feed,
        16384,
    ) {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(&feed)
        .ok()?
        .take(16385)
        .read_to_end(&mut bytes)
        .ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let manifest = authenticate(&bytes, KEY, env!("CARGO_PKG_VERSION"), now)?;
    let asset = if cfg!(windows) {
        manifest.windows_x64?
    } else {
        manifest.linux_x64?
    };
    if asset.size == 0 || asset.size > LIMIT {
        return None;
    }
    let file = stage.path().join(NAME);
    if !fetch(
        &format!("{RELEASES}/download/v{}/{NAME}", manifest.version),
        &file,
        asset.size,
    ) {
        return None;
    }
    verify_asset(&file, &asset).ok()?;
    Some(Pending {
        stage,
        target,
        asset,
        expires: manifest.expires,
        previous_modified,
    })
}
impl Pending {
    pub fn apply(self) -> io::Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs();
        if now >= self.expires || fs::metadata(&self.target)?.modified()? != self.previous_modified
        {
            return Err(io::Error::other(
                "staged update expired or installed binary changed",
            ));
        }
        let file = self.stage.path().join(NAME);
        verify_asset(&file, &self.asset)?;
        let directory = self
            .target
            .parent()
            .ok_or_else(|| io::Error::other("update target has no parent"))?;
        #[cfg(windows)]
        {
            let _ = directory;
            let helper = self.stage.path().join("lumen-update-helper.exe");
            fs::copy(std::env::current_exe()?, &helper)?;
            let modified = self
                .previous_modified
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos();
            std::process::Command::new(helper)
                .arg("--apply-staged-update")
                .arg(std::process::id().to_string())
                .arg(&self.target)
                .arg(modified.to_string())
                .spawn()?;
            let _ = self.stage.keep();
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut temp = tempfile::NamedTempFile::new_in(directory)?;
            io::copy(&mut fs::File::open(&file)?, &mut temp)?;
            verify_asset(temp.path(), &self.asset)?;
            temp.as_file()
                .set_permissions(fs::Permissions::from_mode(0o755))?;
            temp.as_file().sync_all()?;
            temp.persist(&self.target).map_err(|e| e.error)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
pub fn helper_mode() -> bool {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|arg| arg != "--apply-staged-update") {
        return false;
    }
    if args.len() == 5 {
        let _ = run_helper(&args[2], Path::new(&args[3]), &args[4]);
    }
    true
}

#[cfg(windows)]
fn run_helper(pid: &std::ffi::OsStr, target: &Path, modified: &std::ffi::OsStr) -> io::Result<()> {
    use windows::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_ACCESS_RIGHTS},
    };
    let pid = pid
        .to_string_lossy()
        .parse::<u32>()
        .map_err(io::Error::other)?;
    if pid == 0
        || !target.is_absolute()
        || target.file_name().is_none_or(|name| name != "lumen.exe")
        || !target.with_file_name("unins000.exe").is_file()
    {
        return Err(io::Error::other("invalid update target"));
    }
    unsafe {
        match OpenProcess(PROCESS_ACCESS_RIGHTS(0x100000), false, pid) {
            Ok(handle) => {
                let result = WaitForSingleObject(handle, 30000);
                let _ = CloseHandle(handle);
                if result != WAIT_OBJECT_0 {
                    return Err(io::Error::other("browser still running"));
                }
            }
            Err(error) if error.code().0 as u32 == 0x80070057 => {} // exited before OpenProcess
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    let current_modified = fs::metadata(target)?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    if modified.to_string_lossy() != current_modified.to_string() {
        return Err(io::Error::other("installation changed"));
    }
    let helper = std::env::current_exe()?;
    let stage = helper
        .parent()
        .ok_or_else(|| io::Error::other("missing update directory"))?;
    let mut bytes = Vec::new();
    fs::File::open(stage.join("manifest.json"))?
        .take(16385)
        .read_to_end(&mut bytes)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs();
    let manifest = authenticate(&bytes, KEY, env!("CARGO_PKG_VERSION"), now)
        .ok_or_else(|| io::Error::other("invalid signed update"))?;
    let installer = stage.join(NAME);
    // Deny replacement and writes while the verified installer is launched.
    use std::os::windows::fs::OpenOptionsExt;
    let _installer_lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&installer)?;
    verify_asset(
        &installer,
        &manifest
            .windows_x64
            .ok_or_else(|| io::Error::other("Windows asset missing"))?,
    )?;
    let mut dir_arg = std::ffi::OsString::from("/DIR=");
    dir_arg.push(
        target
            .parent()
            .ok_or_else(|| io::Error::other("missing installation folder"))?,
    );
    std::process::Command::new(installer)
        .args(["/SILENT", "/NORESTART", "/NOCLOSEAPPLICATIONS"])
        .arg(dir_arg)
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair},
    };
    #[test]
    fn payload_hash_is_verified_after_download() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"example").unwrap();
        let asset = Asset {
            size: 7,
            sha256: format!("{:x}", Sha256::digest(b"example")),
        };
        assert!(verify_asset(file.path(), &asset).is_ok());
        fs::write(file.path(), b"replace").unwrap();
        assert!(verify_asset(file.path(), &asset).is_err());
    }
    #[test]
    fn rejects_tampered_expired_and_older_updates() {
        let pk = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pk.as_ref()).unwrap();
        let data = br#"{"version":"0.4.9","expires":1000}"#;
        let mut envelope = serde_json::json!({"payload":STANDARD.encode(data),"signature":STANDARD.encode(pair.sign(data).as_ref())});
        let key = pair.public_key().as_ref();
        assert!(authenticate(envelope.to_string().as_bytes(), key, "0.4.8", 900).is_some());
        assert!(authenticate(envelope.to_string().as_bytes(), key, "0.4.9", 900).is_none());
        assert!(authenticate(envelope.to_string().as_bytes(), key, "0.4.8", 1000).is_none());
        envelope["payload"] = STANDARD
            .encode(br#"{"version":"9.0.0","expires":1000}"#)
            .into();
        assert!(authenticate(envelope.to_string().as_bytes(), key, "0.4.8", 900).is_none());
    }
}
