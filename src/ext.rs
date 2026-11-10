//! Расширения браузера (распакованные расширения Chrome/Edge) через WebView2.

use std::path::Path;

use webview2_com::{
    take_pwstr,
    Microsoft::Web::WebView2::Win32::*,
    BrowserExtensionEnableCompletedHandler, BrowserExtensionRemoveCompletedHandler,
    ProfileAddBrowserExtensionCompletedHandler, ProfileGetBrowserExtensionsCompletedHandler,
};
use windows::Win32::Foundation::BOOL;
use windows_core::{Interface, HSTRING, PWSTR};
use wry::{WebView, WebViewExtWindows};

fn profile(wv: &WebView) -> windows_core::Result<ICoreWebView2Profile7> {
    unsafe {
        wv.controller()
            .CoreWebView2()?
            .cast::<ICoreWebView2_13>()?
            .Profile()?
            .cast::<ICoreWebView2Profile7>()
    }
}

/// Добавляет распакованное расширение из папки. `done` получает идентификатор расширения (None — ошибка).
pub fn add(wv: &WebView, dir: &Path, done: impl FnOnce(Result<String, String>) + 'static) {
    let p = match profile(wv) { Ok(p) => p, Err(e) => return done(Err(format!("Обновите Microsoft Edge WebView2 Runtime: {e}"))) };
    let path = HSTRING::from(dir);
    let done = std::rc::Rc::new(std::cell::RefCell::new(Some(done)));
    let completed = done.clone();
    let handler = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |res, ext| {
        let id = if res.is_ok() {
            ext.and_then(|e| unsafe {
                let mut raw = PWSTR::null();
                e.Id(&mut raw).ok()?;
                Some(take_pwstr(raw))
            }).ok_or_else(|| "WebView2 не вернул ID расширения".to_string())
        } else {
            Err(format!("WebView2 отклонил расширение: {res:?}. Проверьте manifest.json и поддержку API расширения."))
        };
        if let Some(done) = completed.borrow_mut().take() { done(id); }
        Ok(())
    }));
    unsafe {
        if let Err(e) = p.AddBrowserExtension(&path, &handler) {
            if let Some(done) = done.borrow_mut().take() { done(Err(format!("Не удалось установить расширение: {e}"))); }
        }
    }
}

/// Список установленных расширений в виде JSON: [{"id","name","on"}].
pub fn list(wv: &WebView, done: impl FnOnce(String) + 'static) {
    let Ok(p) = profile(wv) else { return done("[]".into()) };
    let handler = ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |_, list| {
        let mut out = Vec::new();
        if let Some(list) = list {
            unsafe {
                let mut count = 0u32;
                let _ = list.Count(&mut count);
                for i in 0..count {
                    let Ok(ext) = list.GetValueAtIndex(i) else { continue };
                    let mut id = PWSTR::null();
                    let mut name = PWSTR::null();
                    let mut on = BOOL::default();
                    let _ = ext.Id(&mut id);
                    let _ = ext.Name(&mut name);
                    let _ = ext.IsEnabled(&mut on);
                    out.push(serde_json::json!({
                        "id": take_pwstr(id),
                        "name": take_pwstr(name),
                        "on": on.as_bool(),
                    }));
                }
            }
        }
        done(serde_json::Value::Array(out).to_string());
        Ok(())
    }));
    unsafe {
        let _ = p.GetBrowserExtensions(&handler);
    }
}

fn with_ext(wv: &WebView, id: String, f: impl FnOnce(ICoreWebView2BrowserExtension) + 'static) {
    let Ok(p) = profile(wv) else { return };
    let handler = ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |_, list| {
        if let Some(list) = list {
            unsafe {
                let mut count = 0u32;
                let _ = list.Count(&mut count);
                for i in 0..count {
                    let Ok(ext) = list.GetValueAtIndex(i) else { continue };
                    let mut eid = PWSTR::null();
                    let _ = ext.Id(&mut eid);
                    if take_pwstr(eid) == id {
                        f(ext);
                        break;
                    }
                }
            }
        }
        Ok(())
    }));
    unsafe {
        let _ = p.GetBrowserExtensions(&handler);
    }
}

pub fn set_enabled(wv: &WebView, id: String, on: bool, done: impl FnOnce() + 'static) {
    with_ext(wv, id, move |ext| unsafe {
        let h = BrowserExtensionEnableCompletedHandler::create(Box::new(move |_| {
            done();
            Ok(())
        }));
        let _ = ext.Enable(on, &h);
    });
}

pub fn remove(wv: &WebView, id: String, done: impl FnOnce() + 'static) {
    with_ext(wv, id, move |ext| unsafe {
        let h = BrowserExtensionRemoveCompletedHandler::create(Box::new(move |_| {
            done();
            Ok(())
        }));
        let _ = ext.Remove(&h);
    });
}

/// Ищет в строке (ссылка Chrome Web Store или просто ID) идентификатор расширения: 32 буквы a–p.
fn extract_id(input: &str) -> Option<String> {
    input
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find(|t| t.len() == 32 && t.chars().all(|c| ('a'..='p').contains(&c)))
        .map(|t| t.to_string())
}

// CRX3 stores public keys in protobuf AsymmetricKeyProof messages. Select the
// key matching the store ID, rather than deriving a new extension ID from its path.
fn protobuf_fields(mut data: &[u8]) -> Option<Vec<(u64, &[u8])>> {
    fn varint(data: &mut &[u8]) -> Option<u64> {
        let mut n = 0u64;
        for shift in (0..=63).step_by(7) {
            let (&b, rest) = data.split_first()?; *data = rest;
            if shift == 63 && b > 1 { return None; }
            n |= ((b & 127) as u64) << shift;
            if b < 128 { return Some(n); }
        } None
    }
    let mut fields = Vec::new();
    while !data.is_empty() {
        let tag = varint(&mut data)?;
        match tag & 7 {
            0 => { varint(&mut data)?; },
            1 => { data = data.get(8..)?; },
            5 => { data = data.get(4..)?; },
            2 => { let len = usize::try_from(varint(&mut data)?).ok()?; let value = data.get(..len)?; fields.push((tag >> 3, value)); data = data.get(len..)?; },
            _ => return None,
        }
    } Some(fields)
}

fn store_key(bytes: &[u8], id: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    use base64::Engine;
    let le = |i| bytes.get(i..i+4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let keys: Vec<&[u8]> = match le(4)? {
        2 => vec![bytes.get(16..16usize.checked_add(le(8)?)?)?],
        3 => protobuf_fields(bytes.get(12..12usize.checked_add(le(8)?)?)?)?.into_iter()
            .filter(|(n, _)| *n == 2 || *n == 3)
            .filter_map(|(_, proof)| protobuf_fields(proof)?.into_iter().find(|(n, _)| *n == 1).map(|(_, key)| key)).collect(),
        _ => return None,
    };
    keys.into_iter().find_map(|key| {
        let hash = Sha256::digest(key);
        let derived: String = hash[..16].iter().flat_map(|b| [(b'a' + (b >> 4)) as char, (b'a' + (b & 15)) as char]).collect();
        (derived == id).then(|| base64::engine::general_purpose::STANDARD.encode(key))
    })
}

/// Скачивает расширение из Chrome Web Store и распаковывает в `dir/<id>`.
pub fn install_crx(dir: &Path, input: &str) -> Result<std::path::PathBuf, String> {
    use std::fs;
    let local = Path::new(input.trim().trim_matches('"'));
    if local.is_dir() {
        if !local.join("manifest.json").is_file() { return Err("В выбранной папке нет manifest.json".into()); }
        return local.canonicalize().map_err(|e| e.to_string());
    }
    let id = extract_id(input).ok_or("Не нашёл ID расширения. Вставьте ссылку из Chrome Web Store или ID из 32 букв")?;
    // A separate working directory prevents concurrent installs overwriting each other.
    let tmp = std::env::temp_dir().join(format!("lumen-install-{}-{}", std::process::id(), crate::transfer::now()));
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let crx = tmp.join(format!("lumen-ext-{id}.crx"));
    let zip = tmp.join(format!("lumen-ext-{id}.zip"));
    let version = wry::webview_version().unwrap_or_else(|_| "130.0.0.0".into());
    let version = version.split_whitespace().next().unwrap_or("130.0.0.0");
    let url = format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&prodversion={version}&acceptformat=crx2,crx3&x=id%3D{id}%26uc"
    );
    if !crate::sys::curl_download(&url, &crx) {
        let _ = fs::remove_file(&crx);
        return Err("Не удалось скачать расширение (проверьте ID и интернет)".into());
    }
    let bytes = fs::read(&crx).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&crx);
    let le = |o: usize| -> Option<usize> {
        bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    if bytes.len() < 16 || &bytes[..4] != b"Cr24" {
        return Err("Файл расширения повреждён или не является CRX".into());
    }
    let start = match le(4) {
        Some(3) => 12 + le(8).ok_or("Повреждённый CRX")?,
        Some(2) => 16 + le(8).ok_or("Повреждённый CRX")? + le(12).ok_or("Повреждённый CRX")?,
        _ => return Err("Неизвестная версия формата CRX".into()),
    };
    if start >= bytes.len() {
        return Err("Повреждённый CRX".into());
    }
    fs::write(&zip, &bytes[start..]).map_err(|e| e.to_string())?;

    // Do not delete the directory of an extension still loaded by WebView2.
    let dest = dir.join(format!("{id}-{}", crate::transfer::now()));
    fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let ok = crate::sys::hidden("tar.exe")
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(&dest)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = fs::remove_file(&zip);
    if !ok || !dest.join("manifest.json").exists() {
        let _ = fs::remove_dir_all(&dest);
        return Err("Не удалось распаковать расширение".into());
    }
    // CRX store metadata is reserved by Chromium and rejects unpacked installation.
    let metadata = dest.join("_metadata");
    if metadata.exists() { fs::remove_dir_all(metadata).map_err(|e| format!("Не удалось удалить служебные данные CRX: {e}"))?; }
    let manifest_path = dest.join("manifest.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let key = store_key(&bytes, &id).ok_or("Не удалось проверить ID расширения по публичному ключу CRX")?;
    manifest["key"] = key.into();
    fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir(&tmp);
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn store_links_and_invalid_ids() {
        assert_eq!(extract_id("https://chromewebstore.google.com/detail/shazam/mmioliijnhnoblpgimnlajmefafdfilb?hl=ru").as_deref(), Some("mmioliijnhnoblpgimnlajmefafdfilb"));
        assert!(extract_id("../../manifest.json").is_none());
        assert!(extract_id("zmioliijnhnoblpgimnlajmefafdfilb").is_none());
    }
    #[test]
    fn rejects_truncated_protobuf_and_crx() {
        assert!(protobuf_fields(&[0x12, 0x7f, 0x01]).is_none());
        assert!(protobuf_fields(&[0x80; 12]).is_none());
        assert!(store_key(b"Cr24", "mmioliijnhnoblpgimnlajmefafdfilb").is_none());
        assert!(store_key(&[0; 24], "mmioliijnhnoblpgimnlajmefafdfilb").is_none());
    }
    #[test]
    #[ignore = "Requires downloaded official CRX files in LUMEN_CRX_FIXTURES"]
    fn official_crx_ids_are_preserved() {
        let root = std::path::PathBuf::from(std::env::var_os("LUMEN_CRX_FIXTURES").expect("fixture directory"));
        for (file, id) in [("shazam.crx", "mmioliijnhnoblpgimnlajmefafdfilb"), ("vknext.crx", "jephanpkonkmnkekmlkcijdjgniikppl")] {
            let bytes = std::fs::read(root.join(file)).unwrap();
            assert!(store_key(&bytes, id).is_some(), "{file}: store ID changed");
            assert!(store_key(&bytes, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_none());
        }
    }
}
