//! Small local mailbox for transfers between Lumen processes. Only URLs are transferred.
use std::{fs, path::PathBuf, time::{SystemTime, UNIX_EPOCH}};
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
pub struct Peer { pub pid: u32, pub private: bool, pub x: i32, pub y: i32, pub w: u32, pub h: u32, pub stamp: u64 }
pub fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }
pub fn root() -> PathBuf { std::env::temp_dir().join("Lumen-window-mailboxes") }
pub fn publish(private: bool, x: i32, y: i32, w: u32, h: u32) {
    let dir = root(); let _ = fs::create_dir_all(&dir);
    let p = Peer { pid: std::process::id(), private, x, y, w, h, stamp: now() };
    if let Ok(s) = serde_json::to_vec(&p) { let _ = fs::write(dir.join(format!("{}.peer", p.pid)), s); }
}
pub fn target(private: bool, x: i32, y: i32) -> Option<u32> {
    fs::read_dir(root()).ok()?.flatten().filter_map(|e| {
        if e.path().extension()?.to_str()? != "peer" { return None }
        let p: Peer = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
        (p.pid != std::process::id() && p.private == private && now().saturating_sub(p.stamp) < 2000
            && x >= p.x && x < p.x + p.w as i32 && y >= p.y && y < p.y + p.h as i32).then_some(p.pid)
    }).next()
}
pub fn send(pid: u32, id: u32, url: &str) -> bool {
    let path = root().join(format!("{pid}-{}-{id}.tab", std::process::id()));
    let temp = path.with_extension("tmp");
    let _ = fs::create_dir_all(root());
    let data = serde_json::json!({"url":url,"created":now()}).to_string();
    fs::write(&temp, data).and_then(|_| fs::rename(temp, path)).is_ok()
}
pub fn receive(mut accept: impl FnMut(String)) {
    let prefix = format!("{}-", std::process::id());
    if let Ok(entries) = fs::read_dir(root()) { for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".tab") {
            if let Ok(raw) = fs::read_to_string(e.path()) {
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) {
                    let created = data["created"].as_u64().unwrap_or(0);
                    let url = data["url"].as_str().unwrap_or("");
                    if now().saturating_sub(created) < 5000 && (url.is_empty() || super::safe_url(url)) {
                        accept(url.to_string());
                        let _ = fs::write(e.path().with_extension("ack"), b"");
                    }
                }
                let _ = fs::remove_file(e.path());
            }
        }
    }}
}
pub fn acknowledged(id: u32) -> bool {
    let suffix = format!("-{}-{id}.ack", std::process::id());
    if let Ok(entries) = fs::read_dir(root()) { for e in entries.flatten() {
        if e.file_name().to_string_lossy().ends_with(&suffix) { let _ = fs::remove_file(e.path()); return true; }
    }} false
}
pub fn cleanup() { let _ = fs::remove_file(root().join(format!("{}.peer", std::process::id()))); }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_is_acknowledged_after_acceptance() {
        let id = u32::MAX;
        assert!(send(std::process::id(), id, "https://example.com/"));
        assert!(!acknowledged(id));
        let mut received = Vec::new();
        receive(|url| { assert!(!acknowledged(id)); received.push(url); });
        assert_eq!(received, ["https://example.com/"]);
        assert!(acknowledged(id));
        assert!(!acknowledged(id));
    }
}
