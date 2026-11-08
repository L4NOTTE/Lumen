//! Низкоуровневые возможности WebView2: перехват сетевых запросов для блокировщика,
//! приостановка скрытых вкладок.

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use webview2_com::{
    take_pwstr,
    Microsoft::Web::WebView2::Win32::*,
    TrySuspendCompletedHandler, WebResourceRequestedEventHandler,
};
use windows_core::{Interface, HSTRING, PWSTR};
use wry::{WebView, WebViewExtWindows};

use crate::filters::Filters;

/// Движок фильтров, общий для всех вкладок (появляется после загрузки списков).
pub type SharedFilters = Rc<RefCell<Option<Arc<Filters>>>>;

fn strip_fragment(u: &str) -> &str {
    u.split('#').next().unwrap_or(u)
}

/// Подключает блокировку запросов: каждый запрос страницы проходит проверку по правилам
/// EasyList/RU AdList/AdGuard; рекламные и трекерные запросы получают пустой ответ 403.
pub fn install_blocker(
    wv: &WebView,
    filters: SharedFilters,
    enabled: Arc<AtomicBool>,
    page: Rc<RefCell<String>>,
) -> windows_core::Result<()> {
    unsafe {
        let core = wv.controller().CoreWebView2()?;
        let env = core.cast::<ICoreWebView2_2>()?.Environment()?;
        core.AddWebResourceRequestedFilter(&HSTRING::from("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;

        let mut token = Default::default();
        core.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                if !enabled.load(Ordering::Relaxed) {
                    return Ok(());
                }
                let guard = filters.borrow();
                let Some(f) = guard.as_ref() else { return Ok(()) };

                let req = args.Request()?;
                let url = {
                    let mut uri = PWSTR::null();
                    req.Uri(&mut uri)?;
                    take_pwstr(uri)
                };
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Ok(());
                }
                let page_url = page.borrow().clone();
                if page_url.is_empty() {
                    return Ok(());
                }

                let mut ctx = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
                args.ResourceContext(&mut ctx)?;
                let rtype = match ctx {
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT => {
                        if strip_fragment(&url) == strip_fragment(&page_url) {
                            return Ok(()); // сама страница
                        }
                        "sub_frame"
                    }
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET => "stylesheet",
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE => "image",
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA => "media",
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT => "font",
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT => "script",
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST
                    | COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH => "xmlhttprequest",
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET => "websocket",
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_PING => "ping",
                    _ => "other",
                };

                if f.should_block(&url, &page_url, rtype) {
                    let resp = env.CreateWebResourceResponse(
                        None,
                        403,
                        &HSTRING::from("Blocked"),
                        &HSTRING::from("Content-Type: text/plain"),
                    )?;
                    args.SetResponse(&resp)?;
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

/// Приостанавливает скрытую вкладку (освобождает процессор и часть памяти).
pub fn suspend(wv: &WebView) {
    unsafe {
        if let Ok(core) = wv.controller().CoreWebView2() {
            if let Ok(w3) = core.cast::<ICoreWebView2_3>() {
                let h = TrySuspendCompletedHandler::create(Box::new(|_, _| Ok(())));
                let _ = w3.TrySuspend(&h);
            }
        }
    }
}

/// Возобновляет приостановленную вкладку.
pub fn resume(wv: &WebView) {
    unsafe {
        if let Ok(core) = wv.controller().CoreWebView2() {
            if let Ok(w3) = core.cast::<ICoreWebView2_3>() {
                let _ = w3.Resume();
            }
        }
    }
}
