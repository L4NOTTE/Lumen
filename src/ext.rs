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
pub fn add(wv: &WebView, dir: &Path, done: impl FnOnce(Option<String>) + 'static) {
    let Ok(p) = profile(wv) else { return done(None) };
    let path = HSTRING::from(dir);
    let handler = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |res, ext| {
        let id = if res.is_ok() {
            ext.and_then(|e| unsafe {
                let mut raw = PWSTR::null();
                e.Id(&mut raw).ok()?;
                Some(take_pwstr(raw))
            })
        } else {
            None
        };
        done(id);
        Ok(())
    }));
    unsafe {
        let _ = p.AddBrowserExtension(&path, &handler);
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

/// Скачивает расширение из Chrome Web Store и распаковывает в `dir/<id>`.
pub fn install_crx(dir: &Path, input: &str) -> Result<std::path::PathBuf, String> {
    use std::fs;
    let id = extract_id(input).ok_or("Не нашёл ID расширения. Вставьте ссылку из Chrome Web Store или ID из 32 букв")?;
    let tmp = std::env::temp_dir();
    let crx = tmp.join(format!("lumen-ext-{id}.crx"));
    let zip = tmp.join(format!("lumen-ext-{id}.zip"));
    let url = format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&prodversion=130.0.0.0&acceptformat=crx2,crx3&x=id%3D{id}%26uc"
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

    let dest = dir.join(&id);
    let _ = fs::remove_dir_all(&dest);
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
    Ok(dest)
}
