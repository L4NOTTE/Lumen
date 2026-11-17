//! Расширения браузера (распакованные расширения Chrome/Edge) через WebView2.

use std::path::Path;

use webview2_com::{
    take_pwstr, BrowserExtensionEnableCompletedHandler, BrowserExtensionRemoveCompletedHandler,
    Microsoft::Web::WebView2::Win32::*, ProfileAddBrowserExtensionCompletedHandler,
    ProfileGetBrowserExtensionsCompletedHandler,
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
    let p = match profile(wv) {
        Ok(p) => p,
        Err(e) => {
            return done(Err(format!(
                "Обновите Microsoft Edge WebView2 Runtime: {e}"
            )))
        }
    };
    let path = HSTRING::from(dir);
    let done = std::rc::Rc::new(std::cell::RefCell::new(Some(done)));
    let completed = done.clone();
    let handler = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |res, ext| {
        let id = if res.is_ok() {
            ext.and_then(|e| unsafe {
                let mut raw = PWSTR::null();
                e.Id(&mut raw).ok()?;
                Some(take_pwstr(raw))
            })
            .ok_or_else(|| "WebView2 не вернул ID расширения".to_string())
        } else {
            Err(format!("WebView2 отклонил расширение: {res:?}. Проверьте manifest.json и поддержку API расширения."))
        };
        if let Some(done) = completed.borrow_mut().take() {
            done(id);
        }
        Ok(())
    }));
    unsafe {
        if let Err(e) = p.AddBrowserExtension(&path, &handler) {
            if let Some(done) = done.borrow_mut().take() {
                done(Err(format!("Не удалось установить расширение: {e}")));
            }
        }
    }
}

/// Список установленных расширений в виде JSON: [{"id","name","on"}].
pub fn list(wv: &WebView, done: impl FnOnce(String) + 'static) {
    let Ok(p) = profile(wv) else {
        return done("[]".into());
    };
    let handler = ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |_, list| {
        let mut out = Vec::new();
        if let Some(list) = list {
            unsafe {
                let mut count = 0u32;
                let _ = list.Count(&mut count);
                for i in 0..count {
                    let Ok(ext) = list.GetValueAtIndex(i) else {
                        continue;
                    };
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
                    let Ok(ext) = list.GetValueAtIndex(i) else {
                        continue;
                    };
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
pub(crate) fn protobuf_fields(mut data: &[u8]) -> Option<Vec<(u64, &[u8])>> {
    fn varint(data: &mut &[u8]) -> Option<u64> {
        let mut n = 0u64;
        for shift in (0..=63).step_by(7) {
            let (&b, rest) = data.split_first()?;
            *data = rest;
            if shift == 63 && b > 1 {
                return None;
            }
            n |= ((b & 127) as u64) << shift;
            if b < 128 {
                return Some(n);
            }
        }
        None
    }
    let mut fields = Vec::new();
    while !data.is_empty() {
        let tag = varint(&mut data)?;
        match tag & 7 {
            0 => {
                varint(&mut data)?;
            }
            1 => {
                data = data.get(8..)?;
            }
            5 => {
                data = data.get(4..)?;
            }
            2 => {
                let len = usize::try_from(varint(&mut data)?).ok()?;
                let value = data.get(..len)?;
                fields.push((tag >> 3, value));
                data = data.get(len..)?;
            }
            _ => return None,
        }
    }
    Some(fields)
}

#[cfg(test)]
fn store_key(bytes: &[u8], id: &str) -> Option<String> {
    crate::crx::verify(bytes, id).ok().map(|v| v.key)
}

/// Скачивает расширение из Chrome Web Store и распаковывает в `dir/<id>`.
pub fn install_crx(dir: &Path, input: &str) -> Result<std::path::PathBuf, String> {
    use std::fs;
    let local = Path::new(input.trim().trim_matches('"'));
    if local.is_dir() {
        if !local.join("manifest.json").is_file() {
            return Err("В выбранной папке нет manifest.json".into());
        }
        return local.canonicalize().map_err(|e| e.to_string());
    }
    let id = extract_id(input)
        .ok_or("Не нашёл ID расширения. Вставьте ссылку из Chrome Web Store или ID из 32 букв")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let work = tempfile::Builder::new()
        .prefix(".lumen-install-")
        .tempdir_in(dir)
        .map_err(|e| e.to_string())?;
    let crx = work.path().join("extension.crx");
    let version = wry::webview_version().unwrap_or_else(|_| "130.0.0.0".into());
    let version = version.split_whitespace().next().unwrap_or("130.0.0.0");
    let url = format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&prodversion={version}&acceptformat=crx3&x=id%3D{id}%26uc"
    );
    if !crate::sys::curl_download(&url, &crx) {
        let _ = fs::remove_file(&crx);
        return Err("Не удалось скачать расширение (проверьте ID и интернет)".into());
    }
    if fs::metadata(&crx).map_err(|e| e.to_string())?.len() > crate::crx::MAX_CRX {
        return Err("Слишком большой CRX".into());
    }
    let bytes = fs::read(&crx).map_err(|e| e.to_string())?;
    let verified = crate::crx::verify(&bytes, &id)?;
    let staging = tempfile::Builder::new()
        .prefix(&format!("{id}-"))
        .tempdir_in(dir)
        .map_err(|e| e.to_string())?;
    let dest = staging.path();
    crate::crx::extract(verified.archive, dest)?;
    if !dest.join("manifest.json").is_file() {
        return Err("В расширении нет manifest.json".into());
    }
    let metadata = dest.join("_metadata");
    if metadata.is_dir() {
        fs::remove_dir_all(&metadata).map_err(|e| e.to_string())?;
    }
    let manifest_path = dest.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    manifest["key"] = verified.key.into();
    fs::write(
        manifest_path,
        serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let dest = staging.keep();
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
    #[ignore = "Requires WebView2 and official CRX fixtures"]
    fn official_extensions_register_and_open_popups_in_fresh_webview_profile() {
        const SHAZAM: &str = "mmioliijnhnoblpgimnlajmefafdfilb";
        use tao::{event::Event, event_loop::{ControlFlow, EventLoopBuilder},
            platform::{run_return::EventLoopExtRunReturn, windows::EventLoopBuilderExtWindows},
            window::WindowBuilder};
        use std::time::{Duration, Instant};
        use wry::WebViewBuilderExtWindows;

        let fixtures = std::path::PathBuf::from(std::env::var_os("LUMEN_CRX_FIXTURES").unwrap());
        let dir = tempfile::tempdir().unwrap();
        let mut events = EventLoopBuilder::<Result<String, String>>::with_user_event()
            .with_any_thread(true).build();
        let window = WindowBuilder::new().with_visible(false).build(&events).unwrap();
        let mut context = wry::WebContext::new(Some(dir.path().join("profile")));
        let view = wry::WebViewBuilder::with_web_context(&mut context)
            .with_browser_extensions_enabled(true)
            .with_html("<!doctype html><title>Extension test</title>")
            .build_as_child(&window).unwrap();
        let mut expected = Vec::new();
        for (file, id) in [
            ("shazam.crx", SHAZAM),
            ("vknext.crx", "jephanpkonkmnkekmlkcijdjgniikppl"),
        ] {
            let bytes = std::fs::read(fixtures.join(file)).unwrap();
            let verified = crate::crx::verify(&bytes, id).unwrap();
            let path = dir.path().join(id);
            std::fs::create_dir(&path).unwrap();
            crate::crx::extract(verified.archive, &path).unwrap();
            let metadata = path.join("_metadata");
            if metadata.is_dir() { std::fs::remove_dir_all(metadata).unwrap(); }
            let manifest = path.join("manifest.json");
            let mut json: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
            json["key"] = verified.key.into();
            std::fs::write(manifest, serde_json::to_vec(&json).unwrap()).unwrap();
            let proxy = events.create_proxy();
            add(&view, &path, move |result| { let _ = proxy.send_event(result); });
            expected.push(id.to_string());
        }
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut installed = Vec::new();
        events.run_return(|event, _, flow| {
            *flow = ControlFlow::WaitUntil(deadline);
            if let Event::UserEvent(result) = event { installed.push(result.unwrap()); }
            if installed.len() == expected.len() || Instant::now() >= deadline { *flow = ControlFlow::Exit; }
        });
        installed.sort();
        expected.sort();
        assert_eq!(installed, expected);

        let mut popups = Vec::new();
        for id in &expected {
            let url = format!("chrome-extension://{id}/popup.html");
            let extension = id.clone();
            let sender = extension.clone();
            let proxy = events.create_proxy();
            let popup = wry::WebViewBuilder::with_web_context(&mut context)
                .with_browser_extensions_enabled(true)
                .with_navigation_handler(move |uri| crate::security::extension_origin(&uri, &extension))
                .with_new_window_req_handler(|_| false)
                .with_ipc_handler(move |request| {
                    if crate::security::extension_origin(&request.uri().to_string(), &sender) {
                        let _ = proxy.send_event(Ok(request.body().clone()));
                    }
                })
                .with_initialization_script(include_str!("shazam-capture.js"))
                .with_initialization_script(r#"
                    addEventListener('load', () => setTimeout(async () => {
                        const result = {id: location.hostname, title: document.title,
                            runtime: chrome.runtime?.id,
                            capture: typeof window.__lumenApproveAudio === 'function',
                            shazamButton: !!document.querySelector('.BJ6G5IZZlwRqJcpMnRZt')};
                        try {
                            const store = chrome.storage.local;
                            await store.set({lumenPopupProbe: result.id});
                            result.storage = (await store.get('lumenPopupProbe')).lumenPopupProbe === result.id;
                            await store.remove('lumenPopupProbe');
                        } catch (error) { result.error = String(error); }
                        if (!result.capture) {
                            try {
                                result.worker = await Promise.race([
                                    chrome.runtime.sendMessage({type: 'lumen_popup_probe'}),
                                    new Promise(resolve => setTimeout(() => resolve(false), 3000))
                                ]) === true;
                            } catch (error) { result.workerError = String(error); }
                        }
                        window.ipc.postMessage(JSON.stringify(result));
                    }, 1000));
                "#)
                .with_url(&url)
                .build_as_child(&window).unwrap();
            popups.push(popup);
        }
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut reports = Vec::new();
        let mut capture_requested = false;
        events.run_return(|event, _, flow| {
            *flow = ControlFlow::WaitUntil(deadline);
            if let Event::UserEvent(message) = event {
                let report: serde_json::Value = serde_json::from_str(&message.unwrap()).unwrap();
                assert_ne!(report["cmd"], "capture_start", "capture started without consent");
                capture_requested |= report["cmd"] == "capture_request";
                if report["id"].is_string() {
                    println!("{report}");
                    reports.push(report);
                }
            }
            if reports.len() == expected.len() || Instant::now() >= deadline { *flow = ControlFlow::Exit; }
        });
        assert!(capture_requested, "Shazam did not request consent");
        for id in expected {
            let report = reports.iter().find(|row| row["id"] == id).expect("popup did not load");
            assert_eq!(report["runtime"], id);
            assert_eq!(report["storage"], true);
            if id == SHAZAM {
                assert_eq!(report["capture"], true);
                assert_eq!(report["shazamButton"], true);
            } else {
                assert_eq!(report["worker"], true, "{report}");
            }
        }
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
        let root = std::path::PathBuf::from(
            std::env::var_os("LUMEN_CRX_FIXTURES").expect("fixture directory"),
        );
        for (file, id) in [
            ("shazam.crx", "mmioliijnhnoblpgimnlajmefafdfilb"),
            ("vknext.crx", "jephanpkonkmnkekmlkcijdjgniikppl"),
        ] {
            let bytes = std::fs::read(root.join(file)).unwrap();
            assert!(store_key(&bytes, id).is_some(), "{file}: store ID changed");
            assert!(store_key(&bytes, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_none());
        }
    }
}
