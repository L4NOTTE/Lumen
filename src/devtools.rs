//! Use WebView2's own complete frontend, served only on loopback.
use std::{fs, io::Write, net::TcpListener, path::Path};

pub fn port(profile: &Path) -> u16 {
    let _ = fs::create_dir_all(profile);
    let path = profile.join("lumen-debug-port");
    let read = || fs::read_to_string(&path).ok()?.trim().parse::<u16>().ok().filter(|p| *p >= 1024);
    if let Some(port) = read() { return port; }
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback debugger port");
    let port = listener.local_addr().unwrap().port();
    match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => { let _ = write!(file, "{port}"); port },
        Err(_) => {
            for _ in 0..20 { if let Some(port) = read() { return port; } std::thread::sleep(std::time::Duration::from_millis(5)); }
            port
        },
    }
}

pub fn frontend_url(port: u16, target: &str) -> Option<String> {
    if target.is_empty() || !target.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') { return None; }
    Some(format!("http://127.0.0.1:{port}/devtools/inspector.html?ws=127.0.0.1:{port}/devtools/page/{target}&panel=elements"))
}

pub fn navigation_allowed(port: u16, url: &str) -> bool {
    if url.is_empty() || url == "about:blank" || url.starts_with(&format!("http://127.0.0.1:{port}/devtools/")) { return true; }
    // NavigateToString is exposed as a synthetic data URL on some WebView2
    // versions. Permit only our exact static placeholder, never arbitrary HTML.
    if let Some((kind, body)) = url.split_once(',') {
        if kind.eq_ignore_ascii_case("data:text/html;charset=utf-8;base64") {
            use base64::Engine;
            return base64::engine::general_purpose::STANDARD.decode(body).ok()
                .and_then(|b| String::from_utf8(b).ok())
                .map(|html| html.replace("\r\n", "\n") == crate::DEV_HTML.replace("\r\n", "\n")).unwrap_or(false);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn debugger_uses_loopback_and_validated_target() {
        let url = super::frontend_url(19337, "A1B2-C3").unwrap();
        assert!(url.starts_with("http://127.0.0.1:19337/devtools/"));
        assert!(super::frontend_url(19337, "../other?ws=evil").is_none());
        assert!(super::navigation_allowed(19337, &url));
        assert!(!super::navigation_allowed(19337, "https://example.com/"));
        assert!(!super::navigation_allowed(19337, "http://127.0.0.1:19337.evil/devtools/"));
        assert!(!super::navigation_allowed(19337, "data:text/html;charset=utf-8;base64,PGgxPmV2aWw8L2gxPg=="));
    }
}
