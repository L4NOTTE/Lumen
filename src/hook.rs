//! Низкоуровневые возможности WebView2: перехват сетевых запросов для блокировщика,
//! приостановка скрытых вкладок.

use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use webview2_com::{
    take_pwstr,
    Microsoft::Web::WebView2::Win32::*,
    AcceleratorKeyPressedEventHandler, TrySuspendCompletedHandler, WebResourceRequestedEventHandler,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
use windows_core::{Interface, HSTRING, PWSTR};
use wry::{WebView, WebViewExtWindows};

use crate::filters::Filters;

pub fn install_fullscreen(wv: &WebView, changed: impl Fn(bool) + 'static) -> windows_core::Result<()> {
    unsafe {
        let core = wv.controller().CoreWebView2()?;
        let mut token = Default::default();
        core.add_ContainsFullScreenElementChanged(
            &webview2_com::ContainsFullScreenElementChangedEventHandler::create(Box::new(move |sender, _| {
                if let Some(sender) = sender {
                    let mut full = windows::Win32::Foundation::BOOL::default();
                    sender.ContainsFullScreenElement(&mut full)?;
                    changed(full.as_bool());
                }
                Ok(())
            })), &mut token,
        )?;
    }
    Ok(())
}

/// Движок фильтров, общий для всех вкладок (появляется после загрузки списков).
pub type SharedFilters = Rc<RefCell<Option<Arc<Filters>>>>;

/// Счётчик заблокированных запросов и последние заблокированные адреса (для диагностики).
pub struct Stats {
    pub count: AtomicU64,
    pub recent: Mutex<VecDeque<String>>,
}

impl Stats {
    pub fn new() -> Self {
        Self { count: AtomicU64::new(0), recent: Mutex::new(VecDeque::new()) }
    }

    pub fn add(&self, url: &str) {
        self.count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut r) = self.recent.lock() {
            r.push_back(url.chars().take(160).collect());
            while r.len() > 20 {
                r.pop_front();
            }
        }
    }
}

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
    stats: Arc<Stats>,
) -> windows_core::Result<()> {
    unsafe {
        let core = wv.controller().CoreWebView2()?;
        let env = core.cast::<ICoreWebView2_2>()?.Environment()?;
        // перехватываем только то, что может быть рекламой (стили и шрифты не трогаем — меньше нагрузка)
        for ctx in [
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_PING,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_OTHER,
        ] {
            core.AddWebResourceRequestedFilter(&HSTRING::from("*"), ctx)?;
        }

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
                    stats.add(&url);
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

/// Горячие клавиши, которые работают, даже когда фокус внутри страницы.
/// `on_key(виртуальный_код, ctrl, shift, alt)` возвращает true, если клавиша обработана.
pub fn install_shortcuts(
    wv: &WebView,
    on_key: impl Fn(u32, bool, bool, bool) -> bool + 'static,
) -> windows_core::Result<()> {
    unsafe {
        let controller = wv.controller();
        let mut token = Default::default();
        controller.add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                args.KeyEventKind(&mut kind)?;
                if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                    && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
                {
                    return Ok(());
                }
                let mut vk = 0u32;
                args.VirtualKey(&mut vk)?;
                let down = |k: i32| GetKeyState(k) < 0;
                if on_key(vk, down(VK_CONTROL.0 as i32), down(VK_SHIFT.0 as i32), down(VK_MENU.0 as i32)) {
                    args.SetHandled(true)?;
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}
