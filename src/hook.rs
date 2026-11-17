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
    take_pwstr, AcceleratorKeyPressedEventHandler, Microsoft::Web::WebView2::Win32::*,
    TrySuspendCompletedHandler, WebResourceRequestedEventHandler,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
use windows_core::{Interface, HSTRING, PWSTR};
use wry::{WebView, WebViewExtWindows};

use crate::filters::Filters;

pub fn lock_down(wv: &WebView) -> windows_core::Result<()> {
    unsafe {
        let core = wv.controller().CoreWebView2()?;
        let settings = core.Settings()?;
        settings.SetAreDevToolsEnabled(false)?;
        settings.SetIsStatusBarEnabled(false)?;
        if let Ok(autofill)=settings.cast::<ICoreWebView2Settings4>() {
            autofill.SetIsPasswordAutosaveEnabled(false)?;
            autofill.SetIsGeneralAutofillEnabled(false)?;
        }
        let profile = core.cast::<ICoreWebView2_13>()?.Profile()?;
        profile.cast::<ICoreWebView2Profile3>()?
            .SetPreferredTrackingPreventionLevel(COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_STRICT)?;
        let mut token = Default::default();
        core.add_PermissionRequested(
            &webview2_com::PermissionRequestedEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    if let Ok(args) = args.cast::<ICoreWebView2PermissionRequestedEventArgs3>() {
                        args.SetSavesInProfile(false)?;
                    }
                    args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY)?;
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

/// Keep WebView2's image save command so it uses the existing download policy.
pub fn install_page_context_menu(wv: &WebView) -> windows_core::Result<()> {
    use webview2_com::ContextMenuRequestedEventHandler;
    use windows::Win32::Foundation::BOOL;

    unsafe {
        let core = wv.controller().CoreWebView2()?;
        let environment = core.cast::<ICoreWebView2_10>()?.Environment()?.cast::<ICoreWebView2Environment9>()?;
        core.cast::<ICoreWebView2_11>()?.add_ContextMenuRequested(
            &ContextMenuRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()); };
                let target = args.ContextMenuTarget()?;
                let items = args.MenuItems()?;
                let mut has_link = BOOL::default();
                target.HasLinkUri(&mut has_link)?;
                if has_link.as_bool() {
                    let mut address = PWSTR::null();
                    target.LinkUri(&mut address)?;
                    add_copy_address(&environment, &items, &["copyLink", "copyLinkAddress", "copyLinkLocation"], "Копировать адрес ссылки", take_pwstr(address))?;
                }
                let mut kind = COREWEBVIEW2_CONTEXT_MENU_TARGET_KIND::default();
                target.Kind(&mut kind)?;
                if kind == COREWEBVIEW2_CONTEXT_MENU_TARGET_KIND_IMAGE {
                    let mut address = PWSTR::null();
                    target.SourceUri(&mut address)?;
                    add_copy_address(&environment, &items, &["copyImageLink", "copyImageAddress", "copyImageLocation"], "Копировать URL картинки", take_pwstr(address))?;
                }
                crate::trace_render(|| {
                    let mut count = 0;
                    let _ = items.Count(&mut count);
                    let names = (0..count).filter_map(|index| {
                        let item = items.GetValueAtIndex(index).ok()?;
                        let mut name = PWSTR::null();
                        item.Name(&mut name).ok()?;
                        Some(take_pwstr(name))
                    }).collect::<Vec<_>>();
                    format!("page context menu: {}", names.join(", "))
                });
                Ok(())
            })),
            &mut Default::default(),
        )
    }
}

unsafe fn add_copy_address(
    environment: &ICoreWebView2Environment9,
    items: &ICoreWebView2ContextMenuItemCollection,
    native_names: &[&str],
    label: &str,
    address: String,
) -> windows_core::Result<()> {
    if address.is_empty() || address.len() > 2 * 1024 * 1024 || address.contains('\0') {
        return Ok(());
    }
    let mut count = 0;
    items.Count(&mut count)?;
    let mut native_index = None;
    for index in 0..count {
        let mut name = PWSTR::null();
        items.GetValueAtIndex(index)?.Name(&mut name)?;
        if native_names.contains(&take_pwstr(name).as_str()) {
            native_index = Some(index);
            break;
        }
    }
    let item = environment.CreateContextMenuItem(
        &HSTRING::from(label), None, COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND,
    )?;
    item.add_CustomItemSelected(
        &webview2_com::CustomItemSelectedEventHandler::create(Box::new(move |_, _| copy_address(&address))),
        &mut Default::default(),
    )?;
    let index = native_index.unwrap_or(count);
    items.InsertValueAtIndex(index, &item)?;
    if native_index.is_some() { items.RemoveValueAtIndex(index + 1)?; }
    Ok(())
}

fn copy_address(address: &str) -> windows_core::Result<()> {
    use windows::Win32::{
        Foundation::{GlobalFree, HANDLE},
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
        },
        UI::WindowsAndMessaging::GetForegroundWindow,
    };

    const CF_UNICODETEXT: u32 = 13;
    let text: Vec<u16> = address.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, text.len() * size_of::<u16>())?;
        let result = (|| {
            let output = GlobalLock(memory).cast::<u16>();
            if output.is_null() { return Err(windows_core::Error::from_win32()); }
            std::ptr::copy_nonoverlapping(text.as_ptr(), output, text.len());
            let _ = GlobalUnlock(memory);
            OpenClipboard(GetForegroundWindow())?;
            let result = EmptyClipboard().and_then(|()| SetClipboardData(CF_UNICODETEXT, HANDLE(memory.0)));
            let _ = CloseClipboard();
            result.map(|_| ())
        })();
        if result.is_err() { let _ = GlobalFree(memory); }
        result
    }
}

#[cfg(test)]
pub fn source(wv: &WebView) -> Option<String> {
    let core = unsafe { wv.controller().CoreWebView2() }.ok()?;
    read_source(&core).ok()
}

fn read_source(core: &ICoreWebView2) -> windows_core::Result<String> {
    let mut url = PWSTR::null();
    unsafe { core.Source(&mut url)?; }
    Ok(take_pwstr(url))
}

pub fn navigation_state(view: &WebView) -> (bool, bool) {
    unsafe {
        let Ok(core) = view.controller().CoreWebView2() else { return (false, false); };
        let mut back = windows::Win32::Foundation::BOOL::default();
        let mut forward = windows::Win32::Foundation::BOOL::default();
        if core.CanGoBack(&mut back).is_err() || core.CanGoForward(&mut forward).is_err() {
            return (false, false);
        }
        (back.as_bool(), forward.as_bool())
    }
}

pub fn install_history_changed(view: &WebView, changed: impl Fn() + 'static) -> windows_core::Result<()> {
    let handler = webview2_com::HistoryChangedEventHandler::create(Box::new(move |_, _| {
        changed();
        Ok(())
    }));
    unsafe { view.controller().CoreWebView2()?.add_HistoryChanged(&handler, &mut Default::default()) }
}

pub fn install_source_changed(
    wv: &WebView,
    changed: impl Fn(String) + 'static,
) -> windows_core::Result<()> {
    let handler = webview2_com::SourceChangedEventHandler::create(Box::new(move |sender, _| {
        if let Some(core) = sender {
            changed(read_source(&core)?);
        }
        Ok(())
    }));
    let mut token = Default::default();
    unsafe { wv.controller().CoreWebView2()?.add_SourceChanged(&handler, &mut token) }
}

pub fn open_devtools(wv: &WebView) -> windows_core::Result<()> {
    unsafe { wv.controller().CoreWebView2()?.OpenDevToolsWindow() }
}

pub fn install_process_failure_trace(
    view: &WebView,
    trace: impl Fn(&'static str, Option<i32>, Option<i32>) + 'static,
) -> windows_core::Result<()> {
    let handler = webview2_com::ProcessFailedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()); };
        unsafe {
            let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
            args.ProcessFailedKind(&mut kind)?;
            let name = match kind {
                COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED => "browser-exited",
                COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED => "renderer-exited",
                COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE => "renderer-unresponsive",
                COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED => "frame-renderer-exited",
                COREWEBVIEW2_PROCESS_FAILED_KIND_GPU_PROCESS_EXITED => "gpu-exited",
                _ => "other-process-failure",
            };
            let details = args.cast::<ICoreWebView2ProcessFailedEventArgs2>().ok();
            let reason = details.as_ref().and_then(|details| {
                let mut reason = COREWEBVIEW2_PROCESS_FAILED_REASON::default();
                details.Reason(&mut reason).ok().map(|()| reason.0)
            });
            let exit_code = details.as_ref().and_then(|details| {
                let mut code = 0;
                details.ExitCode(&mut code).ok().map(|()| code)
            });
            trace(name, reason, exit_code);
        }
        Ok(())
    }));
    let mut token = Default::default();
    unsafe { view.controller().CoreWebView2()?.add_ProcessFailed(&handler, &mut token) }
}

pub fn install_dom_ready(wv: &WebView, ready: impl Fn() + 'static) -> windows_core::Result<()> {
    let handler = webview2_com::DOMContentLoadedEventHandler::create(Box::new(move |_, _| {
        ready();
        Ok(())
    }));
    unsafe {
        wv.controller().CoreWebView2()?.cast::<ICoreWebView2_2>()?
            .add_DOMContentLoaded(&handler, &mut Default::default())
    }
}

/// The engine distinguishes a real user activation from script-created popups.
pub fn install_popup_request(
    wv: &WebView,
    open: impl Fn(String) + 'static,
) -> windows_core::Result<()> {
    let handler = webview2_com::NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()); };
        unsafe {
            args.SetHandled(true)?;
            let mut user_initiated = windows::Win32::Foundation::BOOL::default();
            args.IsUserInitiated(&mut user_initiated)?;
            if user_initiated.as_bool() {
                let mut uri = PWSTR::null();
                args.Uri(&mut uri)?;
                open(take_pwstr(uri));
            }
        }
        Ok(())
    }));
    unsafe { wv.controller().CoreWebView2()?.add_NewWindowRequested(&handler, &mut Default::default()) }
}

/// Distinguish a committed document from a download-only popup. Never guess
/// from a filename: redirects and Content-Disposition decide the outcome.
pub fn install_popup_outcome(wv:&WebView, committed:impl Fn()+'static, download:impl Fn()+'static)->windows_core::Result<()> {
    unsafe {
        let core=wv.controller().CoreWebView2()?;
        let mut token=Default::default();
        core.add_ContentLoading(
            &webview2_com::ContentLoadingEventHandler::create(Box::new(move |_, _| {
                committed();
                Ok(())
            })),
            &mut token,
        )?;
        core.cast::<ICoreWebView2_4>()?.add_DownloadStarting(&webview2_com::DownloadStartingEventHandler::create(Box::new(move|_,_|{
            download();Ok(())
        })),&mut token)?;
    }
    Ok(())
}

pub fn install_page_signals(
    wv: &WebView,
    gesture: Rc<std::cell::Cell<Option<std::time::Instant>>>,
    protect_draft: Rc<std::cell::Cell<bool>>,
) -> windows_core::Result<()> {
    unsafe {
        let mut token = Default::default();
        wv.controller().CoreWebView2()?.add_WebMessageReceived(
            &webview2_com::WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut text = PWSTR::null();
                args.TryGetWebMessageAsString(&mut text)?;
                let text = take_pwstr(text);
                if text == "lumen-protect-draft" {
                    protect_draft.set(true);
                    return Ok(());
                }
                if text.len() > 1024 {
                    return Ok(());
                }
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    if value["kind"] == "lumen-link-gesture" {
                        gesture.set(if value["middle"] == true {
                            Some(std::time::Instant::now())
                        } else {
                            None
                        });
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}
pub fn install_zoom(wv: &WebView, changed: impl Fn() + 'static) -> windows_core::Result<()> {
    unsafe {
        let controller = wv.controller();
        controller
            .CoreWebView2()?
            .Settings()?
            .SetIsZoomControlEnabled(true)?;
        let mut token = Default::default();
        controller.add_ZoomFactorChanged(
            &webview2_com::ZoomFactorChangedEventHandler::create(Box::new(move |_, _| {
                changed();
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

pub fn install_fullscreen(
    wv: &WebView,
    changed: impl Fn(bool) + 'static,
) -> windows_core::Result<()> {
    unsafe {
        let core = wv.controller().CoreWebView2()?;
        let mut token = Default::default();
        core.add_ContainsFullScreenElementChanged(
            &webview2_com::ContainsFullScreenElementChangedEventHandler::create(Box::new(
                move |sender, _| {
                    if let Some(sender) = sender {
                        let mut full = windows::Win32::Foundation::BOOL::default();
                        sender.ContainsFullScreenElement(&mut full)?;
                        changed(full.as_bool());
                    }
                    Ok(())
                },
            )),
            &mut token,
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
        Self {
            count: AtomicU64::new(0),
            recent: Mutex::new(VecDeque::new()),
        }
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
    basic: Rc<crate::blocker::Blocker>,
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

                let req = args.Request()?;
                let url = {
                    let mut uri = PWSTR::null();
                    req.Uri(&mut uri)?;
                    take_pwstr(uri)
                };
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Ok(());
                }
                if basic.is_blocked(&url) {
                    stats.add(&url);
                    let response = env.CreateWebResourceResponse(
                        None,
                        403,
                        &HSTRING::from("Blocked"),
                        &HSTRING::from("Content-Type: text/plain"),
                    )?;
                    args.SetResponse(&response)?;
                    return Ok(());
                }
                let guard = filters.borrow();
                let Some(f) = guard.as_ref() else {
                    return Ok(());
                };
                let mut ctx = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
                args.ResourceContext(&mut ctx)?;
                let blocked = {
                    let page_url = page.borrow();
                    if page_url.is_empty() {
                        return Ok(());
                    }
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
                    f.should_block(&url, &page_url, rtype)
                };
                drop(guard);
                if blocked {
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

pub fn is_playing_audio(wv: &WebView) -> windows_core::Result<bool> {
    unsafe {
        let core = wv.controller().CoreWebView2()?.cast::<ICoreWebView2_8>()?;
        let mut playing = Default::default();
        core.IsDocumentPlayingAudio(&mut playing)?;
        Ok(playing.as_bool())
    }
}

/// Приостанавливает скрытую вкладку (освобождает процессор и часть памяти).
pub fn suspend(wv: &WebView) {
    unsafe {
        if let Ok(core) = wv.controller().CoreWebView2() {
            if let Ok(w3) = core.cast::<ICoreWebView2_3>() {
                let mut suspended = Default::default();
                if w3.IsSuspended(&mut suspended).is_err() || suspended.as_bool() {
                    return;
                }
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
                if on_key(
                    vk,
                    down(VK_CONTROL.0 as i32),
                    down(VK_SHIFT.0 as i32),
                    down(VK_MENU.0 as i32),
                ) {
                    args.SetHandled(true)?;
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}
