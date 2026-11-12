//! Small local mailbox for transfers between Lumen processes. Only URLs are transferred.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize)]
pub struct Peer {
    pub pid: u32,
    pub private: bool,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub stamp: u64,
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn root() -> Option<PathBuf> {
    let root = std::env::var_os("LUMEN_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::data_local_dir().map(|p| p.join("Lumen")))?;
    let dir = root.join("window-mailboxes");
    fs::create_dir_all(&dir).ok()?;
    use std::os::windows::fs::MetadataExt;
    let metadata = fs::symlink_metadata(&dir).ok()?;
    (metadata.is_dir() && metadata.file_attributes() & 0x400 == 0).then_some(dir)
}
fn bounded_read(path: &std::path::Path, limit: u64) -> Option<Vec<u8>> {
    use std::{io::Read, os::windows::fs::MetadataExt};
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 || metadata.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}
pub fn publish(private: bool, x: i32, y: i32, w: u32, h: u32) {
    if private {
        return;
    }
    let Some(dir) = root() else {
        return;
    };
    let p = Peer {
        pid: std::process::id(),
        private,
        x,
        y,
        w,
        h,
        stamp: now(),
    };
    if let Ok(s) = serde_json::to_vec(&p) {
        let _ = crate::security::atomic_write(&dir.join(format!("{}.peer", p.pid)), &s);
    }
}
pub fn target(private: bool, x: i32, y: i32) -> Option<u32> {
    // Resolve the top window rather than a stale/covered mailbox rectangle.
    let mut window_pid = 0;
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetAncestor, GetWindowThreadProcessId, WindowFromPoint, GA_ROOT,
        };
        let window = GetAncestor(
            WindowFromPoint(windows::Win32::Foundation::POINT { x, y }),
            GA_ROOT,
        );
        GetWindowThreadProcessId(window, Some(&mut window_pid));
    }
    fs::read_dir(root()?)
        .ok()?
        .flatten()
        .filter_map(|e| {
            if e.path().extension()?.to_str()? != "peer" {
                return None;
            }
            let p: Peer = serde_json::from_slice(&bounded_read(&e.path(), 1024)?).ok()?;
            (p.pid == window_pid
                && p.pid != std::process::id()
                && p.private == private
                && now().checked_sub(p.stamp).is_some_and(|age| age < 2000)
                && x >= p.x
                && x < p.x.saturating_add(p.w.min(i32::MAX as u32) as i32)
                && y >= p.y
                && y < p.y.saturating_add(p.h.min(i32::MAX as u32) as i32))
            .then_some(p.pid)
        })
        .next()
}
pub fn send(pid: u32, id: u32, url: &str) -> bool {
    let Some(dir) = root() else {
        return false;
    };
    if url.len() > 8192 {
        return false;
    }
    let path = dir.join(format!("{pid}-{}-{id}.tab", std::process::id()));
    let data = serde_json::json!({"url":url,"created":now()}).to_string();
    crate::security::atomic_write(&path, data.as_bytes()).is_ok()
}
pub fn receive(mut accept: impl FnMut(String)) {
    let prefix = format!("{}-", std::process::id());
    let Some(dir) = root() else {
        return Default::default();
    };
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && name.ends_with(".tab") {
                if let Some(raw) = bounded_read(&e.path(), 65536) {
                    if let Ok(data) = serde_json::from_slice::<serde_json::Value>(&raw) {
                        let created = data["created"].as_u64().unwrap_or(0);
                        let url = data["url"].as_str().unwrap_or("");
                        if now().checked_sub(created).is_some_and(|age| age < 5000)
                            && (url.is_empty() || super::safe_url(url))
                        {
                            accept(url.to_string());
                            let _ =
                                crate::security::atomic_write(&e.path().with_extension("ack"), b"");
                        }
                    }
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }
}
pub fn acknowledged(id: u32) -> bool {
    let suffix = format!("-{}-{id}.ack", std::process::id());
    let Some(dir) = root() else {
        return Default::default();
    };
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().ends_with(&suffix) {
                let _ = fs::remove_file(e.path());
                return true;
            }
        }
    }
    false
}
pub fn cancel(id: u32) {
    let Some(dir) = root() else {
        return;
    };
    let suffix = format!("-{}-{id}.tab", std::process::id());
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().ends_with(&suffix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}
pub fn cleanup() {
    if let Some(dir) = root() {
        let _ = fs::remove_file(dir.join(format!("{}.peer", std::process::id())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_is_acknowledged_after_acceptance() {
        let id = u32::MAX;
        assert!(send(std::process::id(), id, "https://example.com/"));
        assert!(!acknowledged(id));
        let mut received = Vec::new();
        receive(|url| {
            assert!(!acknowledged(id));
            received.push(url);
        });
        assert_eq!(received, ["https://example.com/"]);
        assert!(acknowledged(id));
        assert!(!acknowledged(id));
    }
}
