//! Shared checks at the native/web trust boundary.
use base64::Engine;
/// Create a private profile before reading secrets or starting a web engine.
#[cfg(unix)]
pub fn private_directory(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other("profile must be a real directory"));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

/// A random, exclusively created temporary file avoids following predictable
/// .tmp symlinks and preserves the previous settings on an interrupted write.
pub fn atomic_write(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or_else(||std::io::Error::other("missing parent"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(data)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error|error.error)?;
    Ok(())
}
pub const MAX_IPC: usize = 6_000_000;
pub fn internal_document(uri: &str, html: &str) -> bool {
    if uri == "about:blank" {
        return true;
    }
    let Some((kind, body)) = uri.split_once(',') else {
        return false;
    };
    if !kind.eq_ignore_ascii_case("data:text/html;charset=utf-8;base64") || body.len() > MAX_IPC {
        return false;
    }
    base64::engine::general_purpose::STANDARD
        .decode(body)
        .ok()
        .is_some_and(|b| {
            b == html.as_bytes()
                || String::from_utf8_lossy(&b).replace("\r\n", "\n") == html.replace("\r\n", "\n")
        })
}
pub fn image_data(data: &str) -> bool {
    if data.len() > 4_000_000 {
        return false;
    }
    let Some((kind, body)) = data.split_once(',') else {
        return false;
    };
    if !matches!(
        kind,
        "data:image/png;base64"
            | "data:image/jpeg;base64"
            | "data:image/webp;base64"
            | "data:image/gif;base64"
    ) {
        return false;
    }
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(body) else {
        return false;
    };
    match kind {
        "data:image/png;base64" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "data:image/jpeg;base64" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "data:image/webp;base64" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        "data:image/gif;base64" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        _ => false,
    }
}
pub fn extension_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}
pub fn managed_extension_name(id: &str, name: &str) -> bool {
    extension_id(id)
        && name.starts_with(&format!("{id}-"))
        && name.len() > 33
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
pub fn extension_origin(uri: &str, id: &str) -> bool {
    extension_id(id)
        && url::Url::parse(uri).is_ok_and(|u| {
            u.scheme() == "chrome-extension"
                && u.host_str() == Some(id)
                && u.username().is_empty()
                && u.password().is_none()
                && u.port().is_none()
        })
}
pub fn dev_command(body: &str) -> bool {
    body.len() < 4096
        && serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .is_some_and(|v| {
                matches!(
                    v["cmd"].as_str(),
                    Some("ready" | "dev_close" | "dev_connect" | "dev_width" | "native_dev_tools")
                )
            })
}
#[cfg(test)]
mod tests {
    #[test]
    fn settings_replace_atomically_without_following_destination_link() {
        let root=tempfile::tempdir().unwrap();
        let file=root.path().join("settings.json");
        super::atomic_write(&file,b"old").unwrap();
        super::atomic_write(&file,b"new").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(),b"new");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(),1);
    }
    #[cfg(unix)]
    #[test]
    fn linux_profile_is_private_and_rejects_symlinks() {
        use std::os::unix::fs::{PermissionsExt,symlink};
        let root=tempfile::tempdir().unwrap();
        let profile=root.path().join("profile");
        super::private_directory(&profile).unwrap();
        assert_eq!(std::fs::metadata(&profile).unwrap().permissions().mode()&0o777,0o700);
        let link=root.path().join("link");symlink(&profile,&link).unwrap();
        assert!(super::private_directory(&link).is_err());
        let victim=root.path().join("victim");std::fs::write(&victim,b"keep").unwrap();
        let file=profile.join("settings.json");symlink(&victim,&file).unwrap();
        super::atomic_write(&file,b"new").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(),b"keep");
    }
    use super::*;
    #[test]
    fn internal_bridge_rejects_foreign_documents() {
        let html = "<h1>Lumen</h1>";
        assert!(internal_document("about:blank", html));
        assert!(internal_document(
            &format!(
                "data:text/html;charset=utf-8;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(html)
            ),
            html
        ));
        for uri in [
            "https://example.com/",
            "about:srcdoc",
            "about:blank#evil",
            "data:text/html,<script>evil()</script>",
            "data:text/html;charset=utf-8;base64,ZXZpbA==",
        ] {
            assert!(!internal_document(uri, html), "{uri}");
        }
    }
    #[test]
    fn backgrounds_cannot_inject_html_or_css() {
        for data in [
            "data:image/svg+xml,<svg onload=evil()>",
            "data:image/png;base64,\");}</style><script>evil()</script>",
            "data:image/png;base64,YWJj",
        ] {
            assert!(!image_data(data));
        }
        assert!(image_data("data:image/png;base64,iVBORw0KGgo="));
    }
    #[test]
    fn restricted_dev_and_extension_bridges() {
        let id = "mmioliijnhnoblpgimnlajmefafdfilb";
        assert!(extension_origin(
            &format!("chrome-extension://{id}/popup.html"),
            id
        ));
        for uri in [
            format!("https://{id}/"),
            format!("chrome-extension://{id}.evil/"),
            format!("chrome-extension://{id}@evil/"),
            "about:blank".into(),
        ] {
            assert!(!extension_origin(&uri, id));
        }
        assert!(dev_command(r#"{"cmd":"dev_close"}"#));
        assert!(!dev_command(r#"{"cmd":"ext_install","input":"C:\\evil"}"#));
    }
}
