
mod blocker;
mod ext;
mod crx;
mod filters;
mod hook;
mod capture;
mod sys;
mod default_browser;
mod favicon;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use blocker::Blocker;
use filters::{selectors_to_css, Filters};
use hook::SharedFilters;
use serde::{Deserialize, Serialize};
use tao::{
    event::{Event, WindowEvent},
    platform::windows::WindowExtWindows,
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{Window, WindowBuilder},
};
use wry::{
    dpi::{PhysicalPosition, PhysicalSize},
    MemoryUsageLevel, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder,
    WebViewBuilderExtWindows, WebViewExtWindows,
};

const TOOLBAR_H: f64 = 80.0;
const FAVORITES_WIDTH: f64 = 390.0;
const FAVORITES_RIGHT_GAP: f64 = 24.0;
const FAVORITES_LEFT_GAP: f64 = 12.0;
const FAVORITES_RADIUS: f64 = 8.0;
// A site can leave parts of its canvas transparent while assuming a white backdrop.
const WEB_PAGE_BACKGROUND: (u8, u8, u8, u8) = (255, 255, 255, 255);
const UI_HTML: &str = include_str!(concat!(env!("OUT_DIR"),"/ui.html"));
const NEWTAB_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/newtab.html"));

/// Аргументы движка. SmartScreen остаётся включённым, перевод страниц выключен,
/// число процессов ограничено, вкладки одного сайта делят процесс (экономия памяти).
/// «Эконом-режим» дополнительно включает облегчённый режим Chromium и отключает кэш переходов назад.
/// У всех окон одного профиля аргументы обязаны совпадать, поэтому строка считается один раз при запуске.
fn browser_args(eco: bool) -> String {
    let mut disabled = String::from("Translate,msWebOOUI,msPdfOOUI");
    let mut extra = String::new();
    if eco {
        disabled.push_str(",BackForwardCache");
        extra.push_str(" --enable-low-end-device-mode");
    }
    let limit = if eco { 3 } else { 4 };
    format!(
        "--enable-features=ParallelDownloading --disable-features={disabled} --renderer-process-limit={limit} --force-webrtc-ip-handling-policy=disable_non_proxied_udp{extra}"
    )
}

const TICK_EVERY: Duration = Duration::from_secs(60);
/// Через сколько скрытая вкладка приостанавливается (освобождает процессор и память).
const SUSPEND_AFTER: Duration = Duration::from_secs(60);
const BACKGROUND_VIEW_BUDGET: usize = 8;
const EARLY_EVICTIONS_PER_TICK: usize = 2;

/// Применяет CSS через конструируемую таблицу стилей — работает даже на сайтах со строгим CSP.
fn inject_css_js(key: &str, css: &str) -> String {
    format!(
        "(()=>{{const css={};const k={};try{{window.__lc=window.__lc||{{}};let s=window.__lc[k];\
         if(!s){{s=new CSSStyleSheet();window.__lc[k]=s;document.adoptedStyleSheets=[...document.adoptedStyleSheets,s];}}\
         s.replaceSync(css);}}catch(e){{}}}})()",
        serde_json::to_string(css).unwrap_or_else(|_| "\"\"".into()),
        serde_json::to_string(key).unwrap_or_else(|_| "\"k\"".into()),
    )
}

/// Собирает классы и id элементов страницы — для общих косметических правил.
const HARVEST_JS: &str = r#"(() => {
    const c = new Set(), i = new Set();
    const walk = document.createTreeWalker(document, NodeFilter.SHOW_ELEMENT);
    for (let n = 0, e; n < 20000 && (e = walk.nextNode()); n++) {
        if (e.id && e.id.length <= 128 && i.size < 2048) i.add(e.id);
        if (c.size < 2048) for (const name of e.classList) {
            if (name.length <= 128) c.add(name);
            if (c.size >= 2048) break;
        }
        if (c.size >= 2048 && i.size >= 2048) break;
    }
    return JSON.stringify({c: [...c], i: [...i]});
})()"#;

#[derive(Deserialize)]
struct Harvest {
    c: Vec<String>,
    i: Vec<String>,
}

/// Скрипт косметического движка: процедурные правила и скрытие карточек «Реклама».
const RUNTIME_JS: &str = include_str!("runtime.js");
/// Значок окна (64×64, RGBA).
const ICON_RGBA: &[u8] = include_bytes!("../assets/icon64.rgba");

#[derive(Serialize, Deserialize, Clone)]
struct Link {
    t: String,
    u: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct SavedTab {
    u: String,
    t: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Session {
    active: usize,
    tabs: Vec<SavedTab>,
}

#[derive(Clone, Copy, Debug)]
enum Shortcut {
    ZoomIn, ZoomOut, ZoomReset, DevTools, Fullscreen, Escape,
    NewTab,
    Reopen,
    CloseTab,
    FocusUrl,
    Incognito,
    History,
    Downloads,
    Settings,
    AddLink,
    NextTab,
    PrevTab,
    Back,
    Forward,
    Tab(usize),
}

enum LinkCmd {
    Add(String, String),
    Del(usize),
}

/// Сопоставление клавиш (виртуальные коды Windows) с действиями браузера.
fn map_key(vk: u32, ctrl: bool, shift: bool, alt: bool) -> Option<Shortcut> {
    use Shortcut::*;
    match (vk, ctrl, shift, alt) {
        (0x26, true, false, false) => Some(ZoomIn),           // Ctrl+Up
        (0x28, true, false, false) => Some(ZoomOut),          // Ctrl+Down
        (0xBB | 0x6B, true, _, false) => Some(ZoomIn),
        (0xBD | 0x6D, true, _, false) => Some(ZoomOut),
        (0x30 | 0x60, true, false, false) => Some(ZoomReset),
        (0x7B, false, false, false) | (0x49, true, true, false) => Some(DevTools),
        (0x7A, false, false, false) => Some(Fullscreen),
        (0x1B, false, false, false) => Some(Escape),
        (0x54, true, false, false) => Some(NewTab),            // Ctrl+T
        (0x54, true, true, false) => Some(Reopen),             // Ctrl+Shift+T
        (0x57, true, false, false) => Some(CloseTab),          // Ctrl+W
        (0x4C, true, false, false) | (0x75, false, false, false) => Some(FocusUrl), // Ctrl+L, F6
        (0x4E, true, true, false) => Some(Incognito),          // Ctrl+Shift+N
        (0x4A, true, false, false) => Some(Downloads),
        (0x48, true, false, false) => Some(History),           // Ctrl+H
        (0xBC, true, false, false) => Some(Settings),          // Ctrl+,
                     // Ctrl+M
        (0x44, true, false, false) => Some(AddLink),           // Ctrl+D
        (0x09, true, false, false) => Some(NextTab),           // Ctrl+Tab
        (0x09, true, true, false) => Some(PrevTab),            // Ctrl+Shift+Tab
        (0x25, false, false, true) => Some(Back),              // Alt+←
        (0x27, false, false, true) => Some(Forward),           // Alt+→
        (0x31..=0x39, true, false, false) => Some(Tab((vk - 0x31) as usize)), // Ctrl+1…9
        _ => None,
    }
}

fn zoom_key(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.starts_with("about:") || raw.starts_with("data:") { return Some("lumen:newtab".into()); }
    let parsed=url::Url::parse(raw).ok()?;
    if matches!(parsed.scheme(),"http"|"https") { Some(parsed.origin().ascii_serialization()) } else { None }
}

fn normalize_url(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let full = if t.contains("://") { t.to_string() } else { format!("https://{t}") };
    let p = url::Url::parse(&full).ok()?;
    if !matches!(p.scheme(), "http" | "https") {
        return None;
    }
    p.host_str()?;
    Some(p.to_string())
}

/// Сколько записей истории хранить.
const HISTORY_MAX: usize = 5000;

/// Сайты, вкладки которых не усыпляем (там может играть музыка/видео).
const KEEP_AWAKE: &[&str] = &[
    "youtube.com", "music.yandex.ru", "music.youtube.com", "vk.com", "vkvideo.ru", "rutube.ru",
    "twitch.tv", "spotify.com", "soundcloud.com", "netflix.com", "kinopoisk.ru", "ivi.ru",
    "okko.tv", "web.telegram.org", "discord.com", "meet.google.com", "zoom.us",
];

const THEMES: &[&str] = &["system", "light", "dark"];
const ACCENTS: &[&str] = &["indigo", "blue", "green", "orange", "pink", "red"];
const FONTS: &[&str] = &["system", "arial", "verdana", "georgia", "trebuchet", "consolas"];
const SIZES: &[&str] = &["small", "normal", "large"];
const BGS: &[&str] = &["none", "g1", "g2", "g3", "g4", "custom"];

fn keeps_awake(u: &str) -> bool {
    let Ok(parsed) = url::Url::parse(u) else { return false };
    let Some(host) = parsed.host_str() else { return false };
    KEEP_AWAKE.iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

fn search_url(engine: &str) -> &'static str {
    match engine {
        "yandex" => "https://yandex.ru/search/?text=",
        "duckduckgo" => "https://duckduckgo.com/?q=",
        _ => "https://www.google.com/search?q=",
    }
}

/// (цвет для светлой темы, цвет для тёмной)
fn accent_pair(k: &str) -> (String, String) {
    if crate::security::hex_color(k) { return (crate::security::accessible_accent(k, false), crate::security::accessible_accent(k, true)); }
    let (light, dark) = match k {
        "blue" => ("#2563eb", "#60a5fa"),
        "green" => ("#16a34a", "#4ade80"),
        "orange" => ("#ea580c", "#fb923c"),
        "pink" => ("#db2777", "#f472b6"),
        "red" => ("#dc2626", "#f87171"),
        _ => ("#4f46e5", "#8b8dff"),
    };
    (light.into(), dark.into())
}

fn font_stack(k: &str) -> &'static str {
    match k {
        "arial" => "Arial,Helvetica,sans-serif",
        "verdana" => "Verdana,Geneva,sans-serif",
        "georgia" => "Georgia,'Times New Roman',serif",
        "trebuchet" => "'Trebuchet MS',Tahoma,sans-serif",
        "consolas" => "Consolas,'Cascadia Mono','Courier New',monospace",
        _ => "'Segoe UI Variable','Segoe UI',system-ui,sans-serif",
    }
}

fn font_px(k: &str) -> u32 {
    match k {
        "small" => 12,
        "large" => 15,
        _ => 13,
    }
}

fn gradient(k: &str) -> Option<&'static str> {
    match k {
        "g1" => Some("linear-gradient(135deg,#4f46e5,#9333ea 50%,#ec4899)"),
        "g2" => Some("linear-gradient(135deg,#0ea5e9,#22d3ee 50%,#34d399)"),
        "g3" => Some("linear-gradient(160deg,#0f172a,#1e293b 50%,#334155)"),
        "g4" => Some("linear-gradient(135deg,#f97316,#ef4444 50%,#7c3aed)"),
        _ => None,
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------- журнал (только если задана переменная окружения LUMEN_LOG) ----------

fn logging() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LUMEN_LOG").is_some())
}

fn log(f: impl FnOnce() -> String) {
    use std::io::Write;
    if !logging() {
        return;
    }
    let path = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Lumen")
        .join("log.txt");
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} {}", now_secs(), f());
    }
}

// ---------- настройки и история ----------

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Settings {
    site_zoom: HashMap<String, f64>,
    engine: String,
    adblock: bool,
    theme: String,
    accent: String,
    font: String,
    font_size: String,
    bg_mode: String,
    adblock_level: String,
    startup: String,
    hide_labels: bool,
    sleep_minutes: u32,
    eco: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            site_zoom: HashMap::new(),
            engine: "google".into(),
            adblock: true,
            theme: "system".into(),
            accent: "indigo".into(),
            font: "system".into(),
            font_size: "normal".into(),
            bg_mode: "none".into(),
            adblock_level: "strong".into(),
            startup: "restore".into(),
            hide_labels: true,
            sleep_minutes: 5,
            eco: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct HistEntry {
    t: String,
    u: String,
    ts: u64,
}

// ---------- события ----------

enum UserEvent {
    DownloadPoll,
    ZoomChanged(u32, f64),
    Detach(u32, i32, i32),
    DetachedUi(u32, String),
    DetachedShortcut(u32, Shortcut),
    TransferTab(u32, Option<u32>, Option<u32>, usize),
    DetachedReady(u32),
    CheckUpdate,
    ApplyUpdate,
    UpdateStatus(&'static str),
    FullscreenChanged(u32, bool),
    Ui(String),
    Title(u32, String),
    FaviconsChanged,
    Url(u32, String),
    Loading(u32, bool),
    Navigate(u32, String),
    Search(u32, String),
    PopupTab(u32,String,bool),
    PopupCommitted(u32),
    PopupDownload(u32),
    FiltersBuilt(Arc<Filters>),
    FiltersStatus(String),
    Harvest(u32, u64, String),
    Extensions(String),
    ExtInstalled(Result<PathBuf, String>),
    ExtChanged(String),
    Shortcut(Shortcut),
    PageShortcut(u32, Shortcut),
    Link(LinkCmd),
    ExtAdded(PathBuf, Result<String, String>),
    Quit,
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Cmd {
    FrameTheme { dark: bool },
    ZoomIn, ZoomOut, ZoomReset, DevTools, Fullscreen, Escape,
    DownloadAction { id: String, action: String },
    ClearDownloads,
    DownloadsFolder,
    Ready,
    NewTab,
    CloseTab { id: u32 },
    Switch { id: u32 },
    Go { text: String },
    Back,
    Forward,
    Reload,
    SetEngine { engine: String },
    ToggleAdblock,
    Incognito,
    OpenPanel { name: String },
    ClosePanel,
    OpenUrl { url: String },
    ClearHistory,
    DeleteHistory { ts: u64, u: String },
    SetLook { theme: Option<String>, accent: Option<String>, font: Option<String>, font_size: Option<String> },
    CheckUpdate,
    ApplyUpdate,
    SetBgMode { mode: String },
    SetBackground { data: String },
    ClearBackground,
    SetAdblockLevel { level: String },
    UpdateFilters,
    ExtOpenDir,
    ExtInstall { input: String },
    ExtToggle { id: String, on: bool },
    ExtRemove { id: String },
    ExtRefresh,
    ExtOpen { id: String },
    AddCurrentLink,
    OpenFavorite { url: String },
    RemoveFavorite { url: String },
    SetStartup { mode: String },
    DefaultBrowser,
    SetLabels { on: bool },
    WinMin,
    WinMax,
    WinClose,
    WinDrag,
    DetachTab { id: u32, x: i32, y: i32 },
    DropTab { id: u32, index: usize, detach: bool },
    SetSleep { minutes: u32 },
    SetEco { on: bool },
}

// ---------- приложение ----------

struct Tab {
    favicon: favicon::Shared,
    visible: Option<bool>,
    bounds: Option<Rect>,
    id: u32,
    /// None — вкладка «спит» (выгружена из памяти), адрес и заголовок сохранены.
    webview: Option<WebView>,
    waiting_for_newtab: Rc<Cell<bool>>,
    detached_window: Option<u32>,
    newtab_document: Rc<RefCell<Rc<str>>>,
    protect_draft: Rc<Cell<bool>>,
    title: String,
    url: String,
    loading: bool,
    document_epoch: u64,
    last_active: Instant,
    /// true — для вкладки включён режим низкого потребления памяти
    low: bool,
}

struct App {
    main_closed: bool,
    newtab_token: String,
    popup_tabs: HashMap<u32, u32>,
    cosmetic_deadlines: HashMap<u32, (Instant, Option<Instant>)>,
    download_hosts: Vec<WebView>,
    favicons: favicon::SharedCache,
    sent_icons: RefCell<std::collections::HashSet<String>>,
    downloads: crate::downloads::Shared,
    frame_dark: bool,
    fullscreen_keys: Arc<AtomicBool>,
    fullscreen: bool,
    zoom: HashMap<u32, f64>,
    tabs: Vec<Tab>,
    detached: HashMap<u32, DetachedTab>,
    detached_spare: Option<(Instant, DetachedTab)>,
    ui: WebView,
    ctx: WebContext,
    window: Window,
    ui_bounds: Option<Rect>,
    ui_favorites_clip: Cell<Option<PhysicalSize<u32>>>,
    ui_visible: Option<bool>,
    active: u32,
    next_id: u32,
    /// "" | "history" | "settings" — открытая панель поверх страницы
    panel: String,
    settings: Settings,
    settings_path: PathBuf,
    history: Vec<HistEntry>,
    history_path: PathBuf,
    bg_data: Option<String>,
    newtab_cache: RefCell<Option<Rc<str>>>,
    bg_path: PathBuf,
    incognito: bool,
    blocker: Rc<Blocker>,
    adblock: Arc<AtomicBool>,
    proxy: EventLoopProxy<UserEvent>,
    filters: SharedFilters,
    filters_status: String,
    lists_dir: PathBuf,
    user_filters_dir: PathBuf,
    ext_dir: PathBuf,
    stats: Arc<hook::Stats>,
    links: Vec<Link>,
    links_path: PathBuf,
    closed: Vec<(String, String)>,
    session_path: PathBuf,
    last_session: RefCell<String>,
    session_deadline: std::cell::Cell<Option<Instant>>,
    last_ui_state: RefCell<String>,
    ext_map: HashMap<String, String>,
    ext_map_path: PathBuf,
    secondary: bool,
    args: String,
    state_deadline: Cell<Option<Instant>>,
}

struct DetachedTab {
    binding: Rc<Cell<Option<u32>>>,
    tabs: Vec<u32>,
    active: u32,
    panel: String,
    favorites_clip: Cell<Option<PhysicalSize<u32>>>,
    strip: WebView,
    window: Window,
    last_state: RefCell<String>,
    sent_icons: RefCell<std::collections::HashSet<String>>,
}

impl DetachedTab {
    fn layout(&self, page: &WebView, newtab: bool) {
        let full = self.window.fullscreen().is_some();
        let _ = self.strip.set_visible(!full);
        let open = !self.panel.is_empty() && !full;
        let favorites = open && self.panel == "favorites";
        let _ = self.strip.set_bounds(if open { panel_rect(&self.window) } else { ui_rect(&self.window) });
        layout_favorites_clip(&self.window, &self.strip, &self.favorites_clip, favorites);
        let _ = page.set_bounds(if full { full_rect(&self.window) } else { content_rect_with_favorites(&self.window, favorites && newtab) });
    }

    fn set_title(&self, title: &str) {
        self.window.set_title(&format!("Lumen {} — {title}", env!("CARGO_PKG_VERSION")));
    }
}

fn tab_strip_x(window: &Window, point: windows::Win32::Foundation::POINT) -> Option<f64> {
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{GetAncestor, WindowFromPoint, GA_ROOT},
    };
    if unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT) } != HWND(window.hwnd() as _) { return None; }
    let Ok(origin) = window.inner_position() else { return None; };
    if window.fullscreen().is_some() { return None; }
    let inset = f64::from(inset_px(window));
    let scale = window.scale_factor();
    let x = f64::from(point.x) - f64::from(origin.x) - inset;
    let y = f64::from(point.y) - f64::from(origin.y) - inset;
    let width = f64::from(window.inner_size().width) - 2.0 * inset;
    (x >= 0.0 && x < width && y >= 0.0 && y < 36.0 * scale).then_some(x / scale)
}

fn toolbar_px(w: &Window) -> u32 {
    (TOOLBAR_H * w.scale_factor()).round() as u32
}

fn animate_window(w: &Window, show: bool) {
    use windows::Win32::{Foundation::{HWND, BOOL}, UI::WindowsAndMessaging::*};
    unsafe {
        let hwnd = HWND(w.hwnd() as *mut _);
        let mut enabled = BOOL::default();
        let animate = SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut enabled as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).is_ok()
            && enabled.as_bool() && !w.is_minimized() && (show || w.is_visible());
        if animate {
            let _ = AnimateWindow(hwnd, 80, if show { AW_BLEND | AW_ACTIVATE } else { AW_BLEND | AW_HIDE });
        }
        w.set_visible(show);
    }
}

/// Keep the native resize hit area uncovered by child WebViews.
fn style_browser_frame(window: &Window) {
    window.set_undecorated_shadow(false);
    let none = 0xfffffffeu32;
    let _ = unsafe {
        windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            windows::Win32::Foundation::HWND(window.hwnd() as _),
            windows::Win32::Graphics::Dwm::DWMWA_BORDER_COLOR,
            &none as *const _ as *const _, std::mem::size_of::<u32>() as u32,
        )
    };
}

fn inset_px(w: &Window) -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXPADDEDBORDER, SM_CXFRAME, SM_CYFRAME,
    };
    if w.is_maximized() || w.fullscreen().is_some() || !w.is_resizable() { return 0; }
    let hit_area = unsafe {
        GetSystemMetrics(SM_CXFRAME).min(GetSystemMetrics(SM_CYFRAME))
            + GetSystemMetrics(SM_CXPADDEDBORDER)
    };
    ((5.0 * w.scale_factor()).round() as u32).min(hit_area.max(1) as u32)
}

#[cfg(test)]
fn dock_width(width: u32, scale: f64, preferred: f64) -> u32 {
    let logical = width as f64 / scale;
    (preferred.clamp(240.0, 960.0).min((logical - 240.0).max(logical * 0.45)) * scale).round() as u32
}

fn content_rect(w: &Window) -> Rect {
    let sz = w.inner_size();
    let i = inset_px(w);
    let top = i + toolbar_px(w);
    Rect {
        position: PhysicalPosition::new(i as i32, top as i32).into(),
        size: PhysicalSize::new(sz.width.saturating_sub(2 * i), sz.height.saturating_sub(top + i)).into(),
    }
}

fn content_rect_with_favorites(w: &Window, reserve_space: bool) -> Rect {
    let mut rect = content_rect(w);
    if reserve_space {
        let scale = w.scale_factor();
        let mut size = rect.size.to_physical::<u32>(scale);
        // On a narrow window the panel needs to overlay the page.
        if f64::from(size.width) / scale >= 720.0 {
            size.width -= ((FAVORITES_WIDTH + FAVORITES_RIGHT_GAP + FAVORITES_LEFT_GAP) * scale).round() as u32;
            rect.size = size.into();
        }
    }
    rect
}

fn ui_rect(w: &Window) -> Rect {
    let sz = w.inner_size();
    let i = inset_px(w);
    Rect {
        position: PhysicalPosition::new(i as i32, i as i32).into(),
        size: PhysicalSize::new(sz.width.saturating_sub(2 * i), toolbar_px(w).min(sz.height.saturating_sub(2 * i))).into(),
    }
}

fn panel_rect(w: &Window) -> Rect {
    let sz = w.inner_size();
    let i = inset_px(w);
    Rect {
        position: PhysicalPosition::new(i as i32, i as i32).into(),
        size: PhysicalSize::new(sz.width.saturating_sub(2 * i), sz.height.saturating_sub(2 * i)).into(),
    }
}

// Clip the existing toolbar surface so the page stays visible and clickable
// outside the popup. Keep these logical bounds in sync with .favorites-popup.
fn layout_favorites_clip(w: &Window, view: &WebView, cached: &Cell<Option<PhysicalSize<u32>>>, open: bool) {
    use windows::Win32::{
        Foundation::HWND,
        Graphics::Gdi::{CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, SetWindowRgn, RGN_ERROR, RGN_OR},
        UI::WindowsAndMessaging::{SetWindowPos, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE},
    };
    let bounds = panel_rect(w).size.to_physical::<u32>(w.scale_factor());
    let next = open.then_some(bounds);
    if cached.get() == next { return; }
    unsafe {
        let mut hwnd = HWND::default();
        if view.controller().ParentWindow(&mut hwnd).is_err() || hwnd.is_invalid() { return; }
        if !open {
            if SetWindowRgn(hwnd, None, true) != 0 { cached.set(None); }
            return;
        }
        let px = |value: f64| (value * w.scale_factor()).round() as i32;
        let width = bounds.width as i32;
        let height = bounds.height as i32;
        let toolbar = CreateRectRgn(0, 0, width, toolbar_px(w) as i32);
        if toolbar.is_invalid() { return; }
        let right = width - px(FAVORITES_RIGHT_GAP);
        let top = px(90.0);
        let bottom = (height - px(12.0)).min(top + px(500.0));
        if right > px(FAVORITES_LEFT_GAP) && bottom > top {
            let left = (right - px(FAVORITES_WIDTH)).max(px(FAVORITES_LEFT_GAP));
            let diameter = px(2.0 * FAVORITES_RADIUS);
            let popup = CreateRoundRectRgn(left, top, right, bottom, diameter, diameter);
            if !popup.is_invalid() {
                let combined = CombineRgn(toolbar, toolbar, popup, RGN_OR);
                let _ = DeleteObject(popup);
                if combined == RGN_ERROR {
                    let _ = DeleteObject(toolbar);
                    return;
                }
            }
        }
        if SetWindowRgn(hwnd, toolbar, true) == 0 {
            let _ = DeleteObject(toolbar);
            return;
        }
        let _ = SetWindowPos(hwnd, HWND_TOP, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
        cached.set(next);
    }
}

fn full_rect(w: &Window) -> Rect {
    Rect {
        position: PhysicalPosition::new(0, 0).into(),
        size: w.inner_size().into(),
    }
}

/// Цвет подложки окна под цвет интерфейса (виден по краям рамки).
fn window_bg(w: &Window, theme: &str, incognito: bool) -> (u8, u8, u8, u8) {
    if incognito {
        return (22, 15, 36, 255);
    }
    let dark = match theme {
        "dark" => true,
        "light" => false,
        _ => matches!(w.theme(), tao::window::Theme::Dark),
    };
    if dark { (27, 28, 32, 255) } else { (243, 244, 246, 255) }
}

/// Удаляет временный профиль инкогнито уже после выхода процесса.
impl App {
    fn resolve(&self, text: &str) -> Option<String> {
        let t = text.trim();
        if safe_url(t) {
            return Some(t.to_string());
        }
        if t.contains("://") && url::Url::parse(t).is_ok() { return None; }
        if !t.contains(' ') && (t.contains('.') || t.starts_with("localhost")) {
            let url = format!("https://{t}");
            return safe_url(&url).then_some(url);
        }
        Some(format!("{}{}", search_url(&self.settings.engine), urlencoding::encode(t)))
    }

    fn save_settings(&self) {
        if self.incognito {
            return; // в инкогнито ничего не пишем на диск
        }
        if let Ok(s) = serde_json::to_string_pretty(&self.settings) {
            let _ = crate::security::atomic_write(&self.settings_path, s.as_bytes());
        }
    }

    // ----- история -----

    fn write_history_all(&self) {
        if self.incognito || self.secondary {
            return;
        }
        let mut s = String::new();
        for e in &self.history {
            if let Ok(line) = serde_json::to_string(e) {
                s.push_str(&line);
                s.push('\n');
            }
        }
        let _ = crate::security::atomic_write(&self.history_path, s.as_bytes());
    }

    fn record_history(&mut self, id: u32) {
        if self.incognito || self.secondary {
            return;
        }
        let Some(t) = self.tabs.iter().find(|t| t.id == id) else { return };
        if t.url.len()>8192 || !crate::security::web_link(&t.url) {
            return;
        }
        let (u, title) = (crate::security::persisted_url(&t.url), t.title.chars().take(512).collect());
        let ts = now_secs();
        if let Some(last) = self.history.last_mut() {
            if last.u == u && ts.saturating_sub(last.ts) < 30 {
                last.t = title;
                return;
            }
        }
        let entry = HistEntry { t: title, u, ts };
        if let Ok(line) = serde_json::to_string(&entry) {
            let _ = crate::security::append_history(&self.history_path, &line);
        }
        self.history.push(entry);
        if self.history.len() > HISTORY_MAX + 500 {
            let cut = self.history.len() - HISTORY_MAX;
            self.history.drain(..cut);
            self.write_history_all();
        }
    }

    fn push_history(&self) {
        let list: Vec<_> = self
            .history
            .iter()
            .rev()
            .take(1000)
            .map(|e| serde_json::json!({ "t": e.t, "u": e.u, "ts": e.ts }))
            .collect();
        self.send_to_panel("history", &format!("window.onHistory({})", serde_json::Value::Array(list)));
    }

    // ----- внешний вид -----

    fn newtab_html(&self) -> Rc<str> {
        if let Some(html) = self.newtab_cache.borrow().as_ref() {
            return html.clone();
        }
        let html: Rc<str> = self.render_newtab().into();
        self.newtab_cache.replace(Some(html.clone()));
        html
    }

    fn render_newtab(&self) -> String {
        let s = &self.settings;
        let (al, ad) = accent_pair(&s.accent);
        let vars = format!(
            ":root{{--acc-l:{al};--acc-d:{ad};--font:{};--fs:{}px}}",
            font_stack(&s.font),
            font_px(&s.font_size)
        );
        let theme = if self.incognito { "dark" } else { match s.theme.as_str() { "dark"=>"dark", "light"=>"light", _=>"system" } };
        let (bg_attr, bg_css) = if self.incognito {
            ("0", String::new())
        } else if s.bg_mode == "custom" {
            match &self.bg_data {
                Some(d) => ("1", format!("body{{background:url(\"{d}\") center/cover no-repeat fixed}}")),
                None => ("0", String::new()),
            }
        } else {
            match gradient(&s.bg_mode) {
                Some(g) => ("1", format!("body{{background:{g} fixed}}")),
                None => ("0", String::new()),
            }
        };
        let links_json = if self.incognito {
            "[]".to_string()
        } else {
            self.favorite_rows().to_string()
        }
        .replace("</", "<\\/");
        NEWTAB_HTML
            .replace("__NEWTAB_TOKEN__", &self.newtab_token)
            .replace("/*__LINKS__*/[]", &links_json)
            .replace("__INC__", if self.incognito { "1" } else { "0" })
            .replace("__THEME__", theme)
            .replace("__BGATTR__", bg_attr)
            .replace("/*__VARS__*/", &vars)
            .replace("/*__BGCSS__*/", &bg_css)
    }

    fn load_newtab(tab: &Tab, html: &Rc<str>) {
        if let Some(view) = &tab.webview {
            let previous = tab.newtab_document.replace(html.clone());
            if view.load_html(html).is_err() {
                tab.newtab_document.replace(previous);
            }
        }
    }

    fn refresh_newtabs(&self) {
        self.newtab_cache.replace(None);
        let html = self.newtab_html();
        for tab in self.tabs.iter().filter(|tab| tab.url.is_empty()) {
            Self::load_newtab(tab, &html);
        }
    }

    // ----- вкладки -----

    fn make_webview(&mut self, id: u32, url: Option<String>, protect_draft: &Rc<Cell<bool>>) -> (WebView, favicon::Shared, Rc<Cell<bool>>, Rc<RefCell<Rc<str>>>) {
        let icon=favicon::State::new(self.tabs.iter().find(|t|t.id==id).and_then(|t|t.favicon.get()));
        let key = zoom_key(url.as_deref().unwrap_or(""));
        let factor = key.and_then(|key| self.settings.site_zoom.get(&key).copied())
            .filter(|factor| factor.is_finite() && (0.25..=5.0).contains(factor)).unwrap_or(1.0);
        self.zoom.insert(id, factor);
        let rect = content_rect(&self.window);
        let incognito = self.incognito;
        let waiting_for_newtab = Rc::new(Cell::new(url.is_none()));
        let waiting_for_load = waiting_for_newtab.clone();
        let background = if url.is_none() {
            window_bg(&self.window, &self.settings.theme, incognito)
        } else {
            WEB_PAGE_BACKGROUND
        };
        let newtab = if url.is_none() { self.newtab_html() } else { Rc::from("") };
        let newtab_token = self.newtab_token.clone();
        let newtab_document = Rc::new(RefCell::new(newtab.clone()));
        let navigation_html = newtab_document.clone();
        let (p_title, p_url, p_win, p_nav) = (
            self.proxy.clone(),
            self.proxy.clone(),
            self.proxy.clone(),
            self.proxy.clone(),
        );
        let blocker = self.blocker.clone();
        let adblock = self.adblock.clone();
        let page = Rc::new(RefCell::new(String::new()));
        let page_nav = page.clone();

        let gesture=Rc::new(std::cell::Cell::new(None::<Instant>));
        let popup_gesture=gesture.clone();
        let mut b = WebViewBuilder::with_web_context(&mut self.ctx)
            .with_devtools(false)
            .with_incognito(incognito)
            .with_browser_extensions_enabled(!incognito)
            .with_additional_browser_args(&self.args)
            .with_bounds(rect)
            .with_background_color(background)
            .with_visible(false)
            .with_focused(false)
            .with_initialization_script(include_str!("background-links.js"))
            .with_document_title_changed_handler(move |t: String| {
                let _ = p_title.send_event(UserEvent::Title(id, t));
            })
            .with_on_page_load_handler(move |ev: PageLoadEvent, _: String| {
                let started = matches!(ev, PageLoadEvent::Started);
                if !started { waiting_for_load.set(false); }
                let _ = p_url.send_event(UserEvent::Loading(id, started));
            })
            .with_navigation_handler(move |u: String| {
                if !safe_url(&u) && !crate::security::internal_document(&u,&navigation_html.borrow()) { return false; }
                let Ok(mut parsed) = url::Url::parse(&u) else { return true };

                // поисковая строка стартовой страницы
                if parsed.host_str() == Some("lumen.invalid") {
                    // команды работают только со стартовой страницы (у неё адрес страницы ещё пуст)
                    if !page_nav.borrow().is_empty() || !crate::security::newtab_request(&parsed, &newtab_token) { return false; }
                    let q = |name: &str| {
                        parsed.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned())
                    };
                    match parsed.path() {
                        "/s" => {
                            if let Some(text) = q("q") {
                                let _ = p_nav.send_event(UserEvent::Search(id, text));
                            }
                        }
                        "/add" => {
                            if let Some(u) = q("u") {
                                let _ = p_nav.send_event(UserEvent::Link(LinkCmd::Add(q("t").unwrap_or_default(), u)));
                            }
                        }
                        "/del" => {
                            if let Some(i) = q("i").and_then(|v| v.parse::<usize>().ok()) {
                                let _ = p_nav.send_event(UserEvent::Link(LinkCmd::Del(i)));
                            }
                        }
                        _ => {}
                    }
                    return false;
                }

                // адрес страницы нужен блокировщику для проверки «сторонних» запросов
                if matches!(parsed.scheme(), "http" | "https") {
                    *page_nav.borrow_mut() = u.clone();
                }

                // HTTPS-only: http -> https (кроме локальных адресов)
                if parsed.scheme() == "http" {
                    let local = matches!(
                        parsed.host_str(),
                        Some("localhost") | Some("127.0.0.1") | Some("[::1]")
                    );
                    if !local && parsed.set_scheme("https").is_ok() {
                        let _ = p_nav.send_event(UserEvent::Navigate(id, parsed.to_string()));
                        return false;
                    }
                }

                // блокировка рекламных и трекерных доменов
                !(adblock.load(Ordering::Relaxed) && parsed.host_str().is_some_and(|host| blocker.is_host_blocked(host)))
            });

        // базовый скрипт-блокировщик нужен в «базовом» режиме и пока не загрузились большие фильтры
        let need_basic = self.settings.adblock
            && (self.settings.adblock_level == "basic" || self.filters.borrow().is_none());
        if need_basic {
            b = b.with_initialization_script(self.blocker.script());
        }
        if self.settings.adblock {
            let rt = format!("window.__lumenCfg={{labels:{}}};\n{}", self.settings.hide_labels, RUNTIME_JS);
            b = b.with_initialization_script(&rt);
        }
        let p_key = self.proxy.clone();
        let fullscreen_keys = self.fullscreen_keys.clone();
        if url.is_none() {
            b = b.with_html(newtab.as_ref());
        }
        let wv = b.build_as_child(&self.window).expect("не удалось создать WebView2");
        hook::lock_down(&wv).expect("site privacy policy");
        hook::install_popup_request(&wv, move |url| {
            let middle = popup_gesture.take().is_some_and(|at| at.elapsed() < Duration::from_secs(5));
            let _ = p_win.send_event(UserEvent::PopupTab(id, url, !middle));
        }).expect("site popup policy");
        let source_proxy = self.proxy.clone();
        let _ = hook::install_source_changed(&wv, move |url| {
            let _ = source_proxy.send_event(UserEvent::Url(id, url));
        });
        let _ = crate::downloads::install(&wv, &self.downloads);
        let committed=self.proxy.clone();let download=self.proxy.clone();
        let new_document = protect_draft.clone();
        let _=hook::install_popup_outcome(&wv,
            move||{
                new_document.set(false);
                let _=committed.send_event(UserEvent::PopupCommitted(id));
            },
            move||{let _=download.send_event(UserEvent::PopupDownload(id));});
        let icon_proxy=self.proxy.clone();
        let _=favicon::install(&wv,&icon,&self.favicons,move||{let _=icon_proxy.send_event(UserEvent::FaviconsChanged);});
        let _=hook::install_page_signals(&wv,gesture,protect_draft.clone());
        let _ = hook::install_blocker(&wv, self.filters.clone(), self.adblock.clone(), page, self.stats.clone(), self.blocker.clone());
        let zoom_proxy=self.proxy.clone();
        let _=hook::install_zoom(&wv,move |factor| {let _=zoom_proxy.send_event(UserEvent::ZoomChanged(id,factor));});
        let _ = hook::install_shortcuts(&wv, move |vk, ctrl, shift, alt| {
            if vk == 0x1B && !fullscreen_keys.load(Ordering::Relaxed) { return false; }
            match map_key(vk, ctrl, shift, alt) {
            Some(sc) => {
                let _ = p_key.send_event(UserEvent::PageShortcut(id, sc));
                true
            }
            None => false,
        }});
        let p = self.proxy.clone();
        let _ = hook::install_fullscreen(&wv, move |full| {
            let _ = p.send_event(UserEvent::FullscreenChanged(id, full));
        });
        let _ = wv.zoom(factor);
        if let Some(url) = url {
            let _ = wv.load_url(&url);
        }
        (wv, icon, waiting_for_newtab, newtab_document)
    }

    fn new_tab(&mut self, url: Option<String>) {
        self.create_tab(url,true);
    }

    fn create_tab(&mut self, url: Option<String>, foreground:bool) {
        self.create_tab_in(url, foreground, None);
    }

    fn create_tab_in(&mut self, url: Option<String>, foreground: bool, window_id: Option<u32>) {
        if window_id.is_none() && self.main_closed { return; }
        let url=url.filter(|u|safe_url(u));
        let id = self.next_id;
        self.next_id += 1;
        let protect_draft = Rc::new(Cell::new(false));
        let (webview, favicon, waiting_for_newtab, newtab_document) = self.make_webview(id, url, &protect_draft);
        if let Some(shell) = window_id.and_then(|key| self.detached.get(&key)) {
            if webview.reparent(shell.window.hwnd()).is_err() {
                return;
            }
        }
        self.tabs.push(Tab {
            favicon,
            visible: None, bounds: None,
            id,
            webview: Some(webview),
            waiting_for_newtab,
            detached_window: window_id,
            newtab_document,
            protect_draft,
            title: "Новая вкладка".into(),
            url: String::new(),
            loading: false,
            document_epoch: 0,
            last_active: Instant::now(),
            low: false,
        });
        if let Some(key) = window_id {
            if let Some(shell) = self.detached.get_mut(&key) {
                shell.tabs.push(id);
                if foreground { self.activate_detached(key, id); }
                else { self.request_state(); }
            }
        } else if foreground || self.active==0 { self.activate(id); }
        else { self.layout(); self.push_state(); }
    }

    /// Делает вкладку активной (закрывая панели); если она «спала» — будит.
    fn activate(&mut self, id: u32) {
        if !self.tabs.iter().any(|t| t.id == id) || !self.reattach(id) { return; }
        if id != self.active && self.fullscreen { self.set_fullscreen(false); }
        if id != self.active {
            if let Some(previous) = self.tabs.iter_mut().find(|tab| tab.id == self.active) {
                previous.last_active = Instant::now();
            }
        }
        self.active = id;
        self.panel.clear();
        if let Some(tab) = self.tabs.iter().find(|t| t.id == id && t.webview.is_none()) {
            let url = tab.url.clone();
            let draft = tab.protect_draft.clone();
            let (wv,icon, waiting_for_newtab, newtab_document) = self.make_webview(id, if url.is_empty() { None } else { Some(url) }, &draft);
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                t.webview = Some(wv);
                t.waiting_for_newtab = waiting_for_newtab;
                t.newtab_document = newtab_document;
                t.favicon=icon;
                t.visible = None; t.bounds = None;
                t.low = false;
            }
        }
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            t.last_active = Instant::now();
        }
        self.layout();
        self.focus_active_page();
        self.push_state();
    }

    fn focus_active_page(&self) {
        if let Some(view) = self.tabs.iter().find(|tab| tab.id == self.active && tab.visible == Some(true))
            .and_then(|tab| tab.webview.as_ref())
        {
            let _ = view.focus();
        }
    }

    fn close_panel(&mut self) {
        if self.panel.is_empty() {
            return;
        }
        self.panel.clear();
        self.layout();
        self.with_active(|wv| {
            let _ = wv.focus();
        });
        self.push_state();
    }

    fn detach(&mut self, id: u32, x: i32, y: i32, target: &tao::event_loop::EventLoopWindowTarget<UserEvent>) {
        if self.incognito { return; }
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else { return; };
        let source = tab.detached_window;
        if source.is_none() && self.main_closed { return; }
        let point = windows::Win32::Foundation::POINT { x, y };
        let destination = (!self.main_closed && source.is_some())
            .then(|| tab_strip_x(&self.window, point).map(|x| (None, &self.ui, x)))
            .flatten()
            .or_else(|| self.detached.iter().find_map(|(&key, shell)| {
                if Some(key) == source { return None; }
                tab_strip_x(&shell.window, point).map(|x| (Some(key), &shell.strip, x))
            }));
        if let Some((destination, strip, x)) = destination {
            let proxy = self.proxy.clone();
            let result = strip.evaluate_script_with_callback(&format!("window.tabDropIndex({x})"), move |value| {
                let index = serde_json::from_str::<usize>(&value).unwrap_or(usize::MAX);
                let _ = proxy.send_event(UserEvent::TransferTab(id, source, destination, index));
            });
            if result.is_err() { self.transfer_tab(id, source, destination, usize::MAX); }
            return;
        }
        let count = source.map_or_else(
            || self.tabs.iter().filter(|tab| tab.detached_window.is_none()).count(),
            |key| self.detached.get(&key).map_or(0, |shell| shell.tabs.len()),
        );
        if count < 2 || tab_strip_x(
            source.and_then(|key| self.detached.get(&key)).map_or(&self.window, |shell| &shell.window),
            point,
        ).is_some() { return; }
        if tab.webview.is_none() {
            let url = (!tab.url.is_empty()).then(|| tab.url.clone());
            let draft = tab.protect_draft.clone();
            let (view, icon, waiting_for_newtab, newtab_document) = self.make_webview(id, url, &draft);
            let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else { return; };
            tab.webview = Some(view);
            tab.waiting_for_newtab = waiting_for_newtab;
            tab.newtab_document = newtab_document;
            tab.favicon = icon;
            tab.bounds = None;
            tab.visible = None;
        }
        let mut detached = match self.detached_spare.take() {
            Some((_, spare)) => {
                spare.window.set_fullscreen(None);
                spare.window.set_maximized(false);
                let _ = spare.strip.set_memory_usage_level(MemoryUsageLevel::Normal);
                let _ = spare.strip.evaluate_script("window.resetTabDrag()");
                fit_extension_popup(&spare.window, Some((x.saturating_sub(240), y.saturating_sub(20))));
                spare
            }
            None => {
                let Some(shell) = self.create_detached_shell(x, y, target) else { return; };
                shell
            }
        };
        detached.binding.set(Some(id));
        detached.tabs = vec![id];
        detached.active = id;
        detached.panel.clear();
        detached.last_state.borrow_mut().clear();
        detached.sent_icons.borrow_mut().clear();
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else { return; };
        let Some(view) = tab.webview.as_ref() else { return; };
        if view.reparent(detached.window.hwnd()).is_err() {
            self.toast("Не удалось переместить вкладку");
            return;
        }
        hook::resume(view);
        tab.low = false;
        let _ = view.set_memory_usage_level(MemoryUsageLevel::Normal);
        detached.layout(view, tab.url.is_empty());
        detached.set_title(&tab.title);
        let _ = view.set_visible(true);
        tab.bounds = None;
        tab.visible = Some(true);
        tab.detached_window = Some(id);
        detached.window.set_visible(true);
        // A shell key must remain stable even after its original tab moves.
        let key = if self.detached.contains_key(&id) {
            let key = self.next_id;
            self.next_id += 1;
            key
        } else { id };
        detached.binding.set(Some(key));
        self.tabs.iter_mut().find(|tab| tab.id == id).unwrap().detached_window = Some(key);
        self.detached.insert(key, detached);
        if let Some(source) = source { self.remove_from_detached(source, id, true); }
        self.replace_main_active(id);
        if let Some(detached) = self.detached.get(&key) { detached.window.set_focus(); }
        self.request_state();
    }

    fn replace_main_active(&mut self, id: u32) {
        if self.active == id {
            if let Some(next) = self.tabs.iter()
                .filter(|tab| tab.detached_window.is_none())
                .max_by_key(|tab| (tab.webview.is_some(), tab.last_active))
                .map(|tab| tab.id)
            {
                self.activate(next);
            }
        }
    }

    fn create_detached_shell(&mut self, x: i32, y: i32, target: &tao::event_loop::EventLoopWindowTarget<UserEvent>) -> Option<DetachedTab> {
        let window = WindowBuilder::new()
            .with_title("Lumen")
            .with_window_icon(tao::window::Icon::from_rgba(ICON_RGBA.to_vec(), 64, 64).ok())
            .with_decorations(false)
            .with_min_inner_size(tao::dpi::LogicalSize::new(480.0, 320.0))
            .with_position(PhysicalPosition::new(x.saturating_sub(240), y.saturating_sub(20)))
            .with_inner_size(tao::dpi::LogicalSize::new(960.0, 640.0))
            .with_visible(false)
            .build(target).ok()?;
        fit_extension_popup(&window, Some((x.saturating_sub(240), y.saturating_sub(20))));
        style_browser_frame(&window);
        window.set_background_color(Some(window_bg(&window, &self.settings.theme, false)));
        let proxy = self.proxy.clone();
        let ready = proxy.clone();
        let binding = Rc::new(std::cell::Cell::new(None));
        let ipc_binding = binding.clone();
        let ready_binding = binding.clone();
        let strip = WebViewBuilder::with_web_context(&mut self.ctx)
            .with_devtools(false)
            .with_browser_extensions_enabled(true)
            .with_additional_browser_args(&self.args)
            .with_navigation_handler(|url| crate::security::internal_document(&url, UI_HTML))
            .with_new_window_req_handler(|_| false)
            .with_ipc_handler(move |req| {
                if req.body().len() <= crate::security::MAX_IPC
                    && crate::security::internal_document(&req.uri().to_string(), UI_HTML)
                {
                    if let Some(id) = ipc_binding.get() {
                        let _ = proxy.send_event(UserEvent::DetachedUi(id, req.body().clone()));
                    }
                }
            })
            .with_on_page_load_handler(move |event, _| {
                if matches!(event, PageLoadEvent::Finished) {
                    if let Some(id) = ready_binding.get() { let _ = ready.send_event(UserEvent::DetachedReady(id)); }
                }
            })
            .with_bounds(ui_rect(&window))
            .with_html(UI_HTML)
            .build_as_child(&window).ok()?;
        let shortcuts = binding.clone();
        let proxy = self.proxy.clone();
        let _ = hook::install_shortcuts(&strip, move |vk, ctrl, shift, alt| {
            let Some(id) = shortcuts.get() else { return false; };
            let Some(shortcut) = map_key(vk, ctrl, shift, alt) else { return false; };
            proxy.send_event(UserEvent::DetachedShortcut(id, shortcut)).is_ok()
        });
        Some(DetachedTab { binding, tabs: Vec::new(), active: 0, panel: String::new(), favorites_clip: Cell::new(None), strip, window, last_state: RefCell::new(String::new()), sent_icons: RefCell::default() })
    }

    fn move_tab_to(&mut self, id: u32, index: usize) {
        let Some(from) = self.tabs.iter().position(|tab| tab.id == id) else { return; };
        let tab = self.tabs.remove(from);
        let to = self.tabs.iter().enumerate()
            .filter(|(_, tab)| tab.detached_window.is_none())
            .nth(index).map_or(self.tabs.len(), |(index, _)| index);
        self.tabs.insert(to, tab);
    }

    fn transfer_tab(&mut self, id: u32, source: Option<u32>, destination: Option<u32>, index: usize) {
        if source == destination || !self.tabs.iter().any(|tab| tab.id == id && tab.detached_window == source) { return; }
        let Some(key) = destination else {
            if self.main_closed || !self.reattach(id) { return; }
            self.move_tab_to(id, index);
            self.activate(id);
            self.window.set_focus();
            return;
        };
        let Some(shell) = self.detached.get(&key) else { return; };
        let hwnd = shell.window.hwnd();
        if self.tabs.iter().any(|tab| tab.id == id && tab.webview.is_none()) {
            let tab = self.tabs.iter().find(|tab| tab.id == id).unwrap();
            let url = tab.url.clone();
            let draft = tab.protect_draft.clone();
            let (view, icon, waiting, document) = self.make_webview(id, (!url.is_empty()).then_some(url), &draft);
            let tab = self.tabs.iter_mut().find(|tab| tab.id == id).unwrap();
            tab.webview = Some(view);
            tab.favicon = icon;
            tab.waiting_for_newtab = waiting;
            tab.newtab_document = document;
        }
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else { return; };
        let Some(view) = &tab.webview else { return; };
        if view.reparent(hwnd).is_err() { return; }
        hook::resume(view);
        tab.detached_window = Some(key);
        tab.bounds = None;
        tab.visible = None;
        if let Some(source) = source { self.remove_from_detached(source, id, true); }
        self.replace_main_active(id);
        if !self.main_closed && !self.tabs.iter().any(|tab| tab.detached_window.is_none()) {
            self.close_main_window();
        }
        if let Some(shell) = self.detached.get_mut(&key) {
            shell.tabs.insert(index.min(shell.tabs.len()), id);
            shell.window.set_focus();
        }
        self.activate_detached(key, id);
    }

    fn detached_key(&self, id: u32) -> Option<u32> {
        self.tabs.iter().find(|tab| tab.id == id)?.detached_window
    }

    fn activate_detached(&mut self, key: u32, id: u32) {
        let Some(shell) = self.detached.get_mut(&key) else { return; };
        if !shell.tabs.contains(&id) { return; }
        shell.active = id;
        shell.panel.clear();
        self.layout_detached(key, true);
        self.request_state();
    }

    fn layout_detached(&mut self, key: u32, focus: bool) {
        let Some(shell) = self.detached.get(&key) else { return; };
        let active = shell.active;
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == active) else { return; };
        if tab.waiting_for_newtab.get() && tab.visible != Some(true)
            && self.tabs.iter().any(|other| other.id != active && other.detached_window == Some(key) && other.visible == Some(true))
        {
            return;
        }
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active) else { return; };
        let Some(view) = tab.webview.as_ref() else { return; };
        shell.layout(view, tab.url.is_empty());
        if tab.low && view.set_memory_usage_level(MemoryUsageLevel::Normal).is_ok() {
            tab.low = false;
        }
        shell.set_title(&tab.title);
        let show_page = shell.panel.is_empty() || shell.panel == "favorites" || shell.window.fullscreen().is_some();
        if tab.visible != Some(show_page) && view.set_visible(show_page).is_ok() {
            tab.visible = Some(show_page);
        }
        if focus {
            if shell.panel.is_empty() { let _ = view.focus(); } else { let _ = shell.strip.focus(); }
        }
        for tab in self.tabs.iter_mut().filter(|tab| tab.detached_window == Some(key) && tab.id != active) {
            if tab.visible != Some(false) {
                if let Some(view) = &tab.webview {
                    if view.set_visible(false).is_ok() { tab.visible = Some(false); }
                    if !tab.low && view.set_memory_usage_level(MemoryUsageLevel::Low).is_ok() {
                        tab.low = true;
                    }
                }
            }
        }
    }

    fn remove_from_detached(&mut self, key: u32, id: u32, reuse: bool) {
        let Some(shell) = self.detached.get_mut(&key) else { return; };
        let Some(index) = shell.tabs.iter().position(|&tab| tab == id) else { return; };
        shell.tabs.remove(index);
        if shell.tabs.is_empty() {
            let shell = self.detached.remove(&key).unwrap();
            shell.binding.set(None);
            shell.window.set_visible(false);
            if reuse && self.tabs.iter().filter(|tab| tab.webview.is_some()).count() <= BACKGROUND_VIEW_BUDGET + 1 {
                let _ = shell.strip.set_memory_usage_level(MemoryUsageLevel::Low);
                self.detached_spare = Some((Instant::now() + Duration::from_secs(8), shell));
            }
        } else if shell.active == id {
            let next = shell.tabs[index.min(shell.tabs.len() - 1)];
            self.activate_detached(key, next);
        }
    }

    fn reattach(&mut self, id: u32) -> bool {
        let Some(key) = self.detached_key(id) else { return true; };
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else { return false; };
        let Some(view) = tab.webview.as_ref() else { return false; };
        if view.reparent(self.window.hwnd()).is_err() {
            if let Some(shell) = self.detached.get(&key) {
                let _ = shell.strip.evaluate_script(&format!("window.cancelTabDrop({id})"));
            }
            return false;
        }
        tab.bounds = None;
        tab.visible = None;
        tab.detached_window = None;
        self.remove_from_detached(key, id, true);
        true
    }

    fn manage_background_views(&mut self) {
        let sleep_minutes = self.settings.sleep_minutes;
        let sleep_after = Duration::from_secs(u64::from(sleep_minutes) * 60);
        let downloads_active = self.downloads.borrow().active_count() > 0;
        let mut candidates = Vec::new();
        let mut background_views = 0;
        for (index, tab) in self.tabs.iter().enumerate() {
            let Some(view) = tab.webview.as_ref() else { continue; };
            if tab.id == self.active || tab.visible == Some(true) || tab.detached_window.is_some() { continue; }
            background_views += 1;
            let idle = tab.last_active.elapsed();
            if idle >= SUSPEND_AFTER && !tab.loading && !keeps_awake(&tab.url)
                && hook::is_playing_audio(view) == Ok(false)
            {
                candidates.push((index, idle));
            }
        }
        candidates.sort_unstable_by_key(|&(_, idle)| std::cmp::Reverse(idle));

        let mut early_evictions = 0;
        let mut changed = false;
        for (index, idle) in candidates {
            let tab = &mut self.tabs[index];
            let early = idle < sleep_after;
            let over_budget = background_views > BACKGROUND_VIEW_BUDGET
                && early_evictions < EARLY_EVICTIONS_PER_TICK;
            let http_page = tab.url.starts_with("https://") || tab.url.starts_with("http://");
            if sleep_minutes > 0 && !downloads_active && http_page && !tab.protect_draft.get()
                && (!early || over_budget)
            {
                tab.webview = None;
                self.cosmetic_deadlines.remove(&tab.id);
                background_views -= 1;
                early_evictions += usize::from(early);
                changed = true;
            } else if !downloads_active && (http_page || tab.url.is_empty()) {
                if let Some(view) = tab.webview.as_ref() {
                    hook::suspend(view);
                }
            }
        }
        if changed { self.push_state(); }
    }

    fn has_background_views(&self) -> bool {
        self.tabs.iter().any(|tab| tab.webview.is_some()
            && tab.id != self.active
            && tab.visible != Some(true)
            && tab.detached_window.is_none()
            && (tab.url.is_empty() || tab.url.starts_with("http://") || tab.url.starts_with("https://"))
            && !keeps_awake(&tab.url))
    }

    fn layout(&mut self) {
        let open = !self.panel.is_empty() && !self.fullscreen;
        let favorites = open && self.panel == "favorites";
        let visible = !self.fullscreen;
        if self.ui_visible != Some(visible) && self.ui.set_visible(visible).is_ok() {
            self.ui_visible = Some(visible);
        }
        let bounds = if open { panel_rect(&self.window) } else { ui_rect(&self.window) };
        if !self.ui_bounds.is_some_and(|old| old.position == bounds.position && old.size == bounds.size)
            && self.ui.set_bounds(bounds).is_ok()
        {
            self.ui_bounds = Some(bounds);
        }
        layout_favorites_clip(&self.window, &self.ui, &self.ui_favorites_clip, favorites);
        let active = (!open || favorites).then(|| self.tabs.iter().position(|tab| {
            tab.id == self.active && tab.detached_window.is_none()
        })).flatten();
        let previous = active.and_then(|active| {
            self.tabs[active].waiting_for_newtab.get().then(|| {
                self.tabs.iter().position(|tab| {
                    tab.id != self.active && tab.visible == Some(true)
                        && tab.webview.is_some() && tab.detached_window.is_none()
                })
            }).flatten()
        });
        let shown = previous.or(active);
        // Show the replacement before hiding the outgoing WebView.
        if let Some(tab) = shown.map(|index| &mut self.tabs[index]) {
            let page_rect = if self.fullscreen { full_rect(&self.window) } else {
                content_rect_with_favorites(&self.window, favorites && tab.url.is_empty())
            };
            if let Some(view) = &tab.webview {
                if tab.visible != Some(true) {
                    hook::resume(view);
                }
                if !tab.bounds.is_some_and(|r| r.position == page_rect.position && r.size == page_rect.size)
                    && view.set_bounds(page_rect).is_ok()
                {
                    tab.bounds = Some(page_rect);
                }
                if tab.visible != Some(true) && view.set_visible(true).is_ok() {
                    tab.visible = Some(true);
                }
            }
        }
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            if tab.detached_window.is_some() { continue; }
            let Some(view) = &tab.webview else { continue; };
            let visible = Some(index) == shown;
            if !visible && tab.visible != Some(false) && view.set_visible(false).is_ok() {
                tab.visible = Some(false);
            }
            if visible == tab.low {
                let level = if visible { MemoryUsageLevel::Normal } else { MemoryUsageLevel::Low };
                if view.set_memory_usage_level(level).is_ok() {
                    tab.low = !visible;
                }
            }
        }
    }

    fn push_state(&self) {
        let icons:HashMap<_,_>=self.tabs.iter().filter(|t| t.detached_window.is_none()).filter_map(|t|t.favicon.get().map(|i|(i.key.clone(),i))).collect();
        let mut sent=self.sent_icons.borrow_mut();
        sent.retain(|key|icons.contains_key(key));
        for (key,icon) in &icons {
            if !sent.contains(key) {
                let script=format!("window.onTabIcon({},{})",serde_json::to_string(key).unwrap(),serde_json::to_string(&icon.data).unwrap());
                if self.ui.evaluate_script(&script).is_ok(){sent.insert(key.clone());}
            }
        }
        drop(sent);
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .filter(|tab| tab.detached_window.is_none())
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "icon": t.favicon.get().map(|i|i.key.clone()),
                    "title": t.title,
                    "url": t.url,
                    "loading": t.loading,
                    "sleeping": t.webview.is_none(),
                })
            })
            .collect();
        let s = &self.settings;
        let (al, ad) = accent_pair(&s.accent);
        let mut state = serde_json::json!({
            "download_count": self.downloads.borrow().active_count(),
            "version": env!("CARGO_PKG_VERSION"),
            "active": self.active,
            "zoom": (self.zoom.get(&self.active).copied().unwrap_or(1.0) * 100.0).round(),
            "panel": &self.panel,
            "dev_open": false,
            "maximized": self.window.is_maximized(),
            "incognito": self.incognito,
            "engine": s.engine,
            "adblock": s.adblock,
            "set": {
                "theme": s.theme,
                "accent": s.accent,
                "font": s.font,
                "font_size": s.font_size,
                "bg_mode": s.bg_mode,
                "has_bg": self.bg_data.is_some(),
                "startup": s.startup,
                "labels": s.hide_labels,
                "sleep": s.sleep_minutes,
                "eco": s.eco,
            },
            "look": { "al": al, "ad": ad, "font": font_stack(&s.font), "fs": font_px(&s.font_size) },
            "filters": { "level": s.adblock_level, "status": self.filters_status },
            "block": {
                "count": self.stats.count.load(Ordering::Relaxed),
                "recent": if self.panel == "settings" {
                    self.stats.recent.lock().map(|r| r.iter().rev().cloned().collect::<Vec<_>>()).unwrap_or_default()
                } else {
                    Vec::new()
                },
            },
        });
        if !self.detached.is_empty() {
            self.push_detached_state(&state);
        }
        state["tabs"] = serde_json::Value::Array(tabs);
        let serialized = state.to_string();
        let changed = *self.last_ui_state.borrow() != serialized;
        if changed {
            if self.ui.evaluate_script(&format!("window.onState({serialized})")).is_ok() {
                *self.last_ui_state.borrow_mut() = serialized;
            }
        }
        if changed { self.schedule_session_save(); }
    }

    fn push_detached_state(&self, shared: &serde_json::Value) {
        let tabs: HashMap<_, _> = self.tabs.iter().filter(|tab| tab.detached_window.is_some())
            .map(|tab| (tab.id, tab)).collect();
        for detached in self.detached.values() {
            let id = detached.active;
            let mut state = shared.clone();
            let mut icons = Vec::new();
            state["tabs"] = serde_json::json!(detached.tabs.iter().filter_map(|id| {
                let tab = tabs.get(id)?;
                let icon = tab.favicon.get();
                let key = icon.as_ref().map(|icon| icon.key.clone());
                icons.extend(icon);
                Some(serde_json::json!({
                    "id": id, "title": tab.title, "url": tab.url, "loading": tab.loading, "icon": key
                }))
            }).collect::<Vec<_>>());
            state["active"] = id.into();
            state["detached"] = true.into();
            state["panel"] = detached.panel.as_str().into();
            state["maximized"] = detached.window.is_maximized().into();
            state["zoom"] = (self.zoom.get(&id).copied().unwrap_or(1.0) * 100.0).round().into();
            let serialized = state.to_string();
            if *detached.last_state.borrow() == serialized { continue; }
            let mut sent = detached.sent_icons.borrow_mut();
            sent.retain(|key| icons.iter().any(|icon| &icon.key == key));
            for icon in icons {
                if !sent.contains(&icon.key)
                    && detached.strip.evaluate_script(&format!("window.onTabIcon({},{})",
                        serde_json::to_string(&icon.key).unwrap(),
                        serde_json::to_string(&icon.data).unwrap())).is_ok()
                {
                    sent.insert(icon.key.clone());
                }
            }
            if detached.strip.evaluate_script(&format!("window.onState({serialized})")).is_ok() {
                *detached.last_state.borrow_mut() = serialized;
            }
        }
    }

    fn detached_command(&mut self, key: u32, message: &str) {
        let Ok(cmd) = serde_json::from_str::<Cmd>(message) else { return; };
        let Some(detached) = self.detached.get(&key) else { return; };
        let id = detached.active;
        let Some(view) = self.tabs.iter().find(|tab| tab.id == id).and_then(|tab| tab.webview.as_ref()) else { return; };
        match cmd {
            Cmd::Ready => {
                detached.last_state.borrow_mut().clear();
                detached.sent_icons.borrow_mut().clear();
                self.push_state();
                self.refresh_panel(&detached.panel);
            }
            Cmd::DropTab { id, detach: true, .. } if detached.tabs.contains(&id) => {
                let mut point = windows::Win32::Foundation::POINT::default();
                if unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point) }.is_ok() {
                    let _ = self.proxy.send_event(UserEvent::Detach(id, point.x, point.y));
                }
            }
            Cmd::WinMin => detached.window.set_minimized(true),
            Cmd::WinMax => detached.window.set_maximized(!detached.window.is_maximized()),
            Cmd::WinDrag => { let _ = detached.window.drag_window(); }
            Cmd::CloseTab { id } if detached.tabs.contains(&id) => self.cmd(Cmd::CloseTab { id }),
            Cmd::WinClose => self.close_detached_window(key),
            Cmd::Back => { let _ = view.evaluate_script("history.back()"); }
            Cmd::Forward => { let _ = view.evaluate_script("history.forward()"); }
            Cmd::Reload => {
                if let Some(tab) = self.tabs.iter().find(|tab| tab.id == id && tab.url.is_empty()) {
                    Self::load_newtab(tab, &self.newtab_html());
                } else {
                    let _ = view.evaluate_script("location.reload()");
                }
            }
            Cmd::Go { text } => {
                if let Some(url) = self.resolve(&text) {
                    let _ = view.load_url(&url);
                    self.set_detached_panel(key, "");
                }
            }
            Cmd::ZoomIn => self.handle(UserEvent::PageShortcut(id, Shortcut::ZoomIn)),
            Cmd::ZoomOut => self.handle(UserEvent::PageShortcut(id, Shortcut::ZoomOut)),
            Cmd::ZoomReset => self.handle(UserEvent::PageShortcut(id, Shortcut::ZoomReset)),
            Cmd::OpenPanel { name } if name == "dev" => { let _ = hook::open_devtools(view); }
            Cmd::DropTab { id, index, detach: false } if detached.tabs.contains(&id) => {
                let shell = self.detached.get_mut(&key).unwrap();
                shell.tabs.retain(|tab| *tab != id);
                shell.tabs.insert(index.min(shell.tabs.len()), id);
                self.request_state();
            }
            Cmd::Switch { id } => self.activate_detached(key, id),
            Cmd::ExtOpen { id } => match self.ext_page(&id) {
                Some((url, true)) => self.open_popup(&detached.window, &url),
                Some((url, false)) => {
                    self.set_detached_panel(key, "");
                    self.create_tab_in(Some(url), true, Some(key));
                }
                None => self.toast("У этого расширения нет окна"),
            },
            Cmd::SetEngine { .. } | Cmd::ToggleAdblock | Cmd::Incognito
            | Cmd::DownloadAction { .. } | Cmd::ClearDownloads | Cmd::DownloadsFolder
            | Cmd::ClearHistory | Cmd::DeleteHistory { .. }
            | Cmd::SetLook { .. } | Cmd::SetBgMode { .. } | Cmd::SetBackground { .. }
            | Cmd::ClearBackground | Cmd::SetAdblockLevel { .. } | Cmd::UpdateFilters
            | Cmd::SetStartup { .. } | Cmd::SetLabels { .. } | Cmd::SetSleep { .. }
            | Cmd::SetEco { .. } | Cmd::DefaultBrowser | Cmd::CheckUpdate | Cmd::ApplyUpdate
            | Cmd::ExtOpenDir | Cmd::ExtInstall { .. } | Cmd::ExtToggle { .. }
            | Cmd::ExtRemove { .. } | Cmd::ExtRefresh => self.cmd(cmd),
            Cmd::AddCurrentLink => {
                self.add_current_link(id);
            }
            Cmd::OpenPanel { name } => self.set_detached_panel(key, &name),
            Cmd::ClosePanel => self.set_detached_panel(key, ""),
            Cmd::RemoveFavorite { url } => self.remove_favorite(&url),
            Cmd::OpenFavorite { url } if self.is_favorite(&url) => {
                let _ = view.load_url(&url);
                self.set_detached_panel(key, "");
            }
            Cmd::OpenUrl { url } if crate::security::web_link(&url) => {
                let _ = view.load_url(&url);
                self.set_detached_panel(key, "");
            }
            Cmd::NewTab => {
                self.set_detached_panel(key, "");
                self.create_tab_in(None, true, Some(key));
            }
            _ => {}
        }
    }

    fn set_detached_panel(&mut self, key: u32, name: &str) {
        if !matches!(name, "" | "history" | "downloads" | "settings" | "ext" | "favorites")
            || (self.incognito && matches!(name, "favorites" | "ext")) { return; }
        let Some(shell) = self.detached.get_mut(&key) else { return };
        if shell.panel == name { return; }
        shell.panel = name.into();
        self.layout_detached(key, true);
        self.push_state();
        self.refresh_panel(name);
    }

    fn close_main_window(&mut self) {
        if self.main_closed { return; }
        self.main_closed = true;
        self.window.set_visible(false);
        self.active = 0;
        self.panel.clear();
        let removed: Vec<_> = self.tabs.iter().filter(|tab| tab.detached_window.is_none()).map(|tab| tab.id).collect();
        self.tabs.retain(|tab| tab.detached_window.is_some());
        for id in removed {
            self.zoom.remove(&id);
            self.popup_tabs.remove(&id);
            self.cosmetic_deadlines.remove(&id);
        }
        self.save_session();
        self.push_state();
        if let Some(shell) = self.detached.values().find(|shell| !shell.window.is_minimized()) {
            shell.window.set_focus();
        }
    }

    fn close_detached_window(&mut self, key: u32) {
        if self.main_closed && self.detached.len() == 1 && self.detached.contains_key(&key) {
            let _ = self.proxy.send_event(UserEvent::Quit);
            return;
        }
        if let Some(shell) = self.detached.get(&key) {
            for id in shell.tabs.clone() {
                self.cmd(Cmd::CloseTab { id });
            }
        }
    }

    fn send_to_panel(&self, name: &str, script: &str) {
        if self.panel == name { let _ = self.ui.evaluate_script(script); }
        for shell in self.detached.values().filter(|shell| shell.panel == name) {
            let _ = shell.strip.evaluate_script(script);
        }
    }

    fn refresh_panel(&self, name: &str) {
        match name {
            "downloads" => self.push_downloads(),
            "history" => self.push_history(),
            "favorites" => self.push_favorites(),
            "ext" => self.refresh_ext(),
            _ => {}
        }
    }

    fn push_downloads(&self) {
        let data = serde_json::to_string(self.downloads.borrow().records()).unwrap_or_else(|_| "[]".into());
        self.send_to_panel("downloads", &format!("window.onDownloads({data})"));
    }

    fn push_update_status(&self, status: &str) {
        let script = format!("window.onUpdateStatus({},{})", serde_json::to_string(status).unwrap(),
            status == crate::updater::READY_STATUS);
        let _ = self.ui.evaluate_script(&script);
        for shell in self.detached.values() {
            let _ = shell.strip.evaluate_script(&script);
        }
    }

    fn with_active(&self, f: impl FnOnce(&WebView)) {
        if let Some(wv) = self
            .tabs
            .iter()
            .find(|t| t.id == self.active)
            .and_then(|t| t.webview.as_ref())
        {
            f(wv);
        }
    }

    /// Отложенная отправка состояния интерфейсу: серия событий склеивается в одну отправку.
    fn request_state(&self) {
        if self.state_deadline.get().is_none() {
            self.state_deadline.set(Some(Instant::now() + Duration::from_millis(35)));
        }
    }

    /// Отдельное маленькое окно для расширения (как всплывающее окно у кнопки расширения).
    fn open_popup(&self, window: &Window, url: &str) {
        let sf = window.scale_factor();
        let (w, h) = if url.contains("mmioliijnhnoblpgimnlajmefafdfilb") { (470.0, 420.0) } else { (420.0, 620.0) };
        let pos = window.outer_position().ok().map(|p| {
            let ow = window.outer_size().width as i32;
            (p.x + ow - (w * sf) as i32 - (24.0 * sf) as i32, p.y + ((TOOLBAR_H + 6.0) * sf) as i32)
        });
        if let Ok(exe) = std::env::current_exe() {
            let mut c = std::process::Command::new(exe);
            c.args(["--popup", url, "--size", &format!("{w},{h}")]);
            if let Some((x, y)) = pos {
                c.args(["--pos", &format!("{x},{y}")]);
            }
            let _ = c.spawn();
        }
    }

    fn update_window_bg(&self) {
        let theme = if self.settings.theme == "system" { if self.frame_dark { "dark" } else { "light" } } else { &self.settings.theme };
        self.window.set_background_color(Some(window_bg(&self.window, theme, self.incognito)));
        for detached in self.detached.values() {
            detached.window.set_background_color(Some(window_bg(&detached.window, theme, false)));
        }
        let newtab_background = window_bg(&self.window, theme, self.incognito);
        for tab in self.tabs.iter().filter(|tab| tab.url.is_empty()) {
            if let Some(view) = &tab.webview {
                let _ = view.set_background_color(newtab_background);
            }
        }
    }

    fn schedule_session_save(&self) {
        if !self.incognito && !self.secondary && self.session_deadline.get().is_none() {
            self.session_deadline.set(Some(Instant::now() + Duration::from_millis(250)));
        }
    }

    fn save_session(&self) {
        let _ = self.try_save_session();
    }

    fn try_save_session(&self) -> std::io::Result<()> {
        self.session_deadline.set(None);
        if self.incognito || self.secondary {
            return Ok(());
        }
        let real: Vec<&Tab> = self.tabs.iter().filter(|t| t.url.starts_with("http")).collect();
        let session = Session {
            active: real.iter().position(|t| t.id == self.active).unwrap_or(0),
            tabs: real.iter().map(|t| SavedTab { u: crate::security::persisted_url(&t.url), t: t.title.clone() }).collect(),
        };
        let json = serde_json::to_string(&session)?;
        if *self.last_session.borrow() != json {
            crate::security::atomic_write(&self.session_path, json.as_bytes())?;
            *self.last_session.borrow_mut() = json;
        }
        Ok(())
    }

    /// Восстанавливает вкладки прошлого сеанса: загружается только активная, остальные «спят».
    fn restore_session(&mut self, s: Session) {
        let mut active = None;
        for (index, t) in s.tabs.iter().enumerate() {
            if !safe_url(&t.u) { continue; }
            if t.u.starts_with("chrome-extension://mmioliijnhnoblpgimnlajmefafdfilb/") { continue; }
            let id = self.next_id;
            self.next_id += 1;
            if index <= s.active || active.is_none() { active = Some(id); }
            self.tabs.push(Tab {
                favicon:favicon::State::new(None),
            visible: None, bounds: None,
                id,
                webview: None,
                waiting_for_newtab: Rc::new(Cell::new(false)),
                detached_window: None,
                newtab_document: Rc::new(RefCell::new(Rc::from(""))),
                protect_draft: Rc::new(Cell::new(false)),
                title: t.t.clone(),
                url: t.u.clone(),
                loading: false,
                document_epoch: 0,
                last_active: Instant::now(),
                low: false,
            });
        }
        if let Some(id) = active { self.activate(id); }
        else { self.new_tab(None); }
    }

    fn toast(&self, msg: &str) {
        if let Ok(j) = serde_json::to_string(msg) {
            let script = format!("window.onToast&&window.onToast({j})");
            let foreground = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            let detached = self.detached.values().find(|shell| shell.window.hwnd() == foreground.0 as isize);
            let strip = detached.map(|shell| &shell.strip).unwrap_or(&self.ui);
            let _ = strip.evaluate_script(&script);
        }
    }

    fn save_links(&self) {
        if let Ok(j) = serde_json::to_string_pretty(&self.links) {
            let _ = crate::security::atomic_write(&self.links_path, j.as_bytes());
        }
    }

    fn update_link_icons(&mut self) -> bool {
        if self.incognito || self.links.is_empty() { return false; }
        let mut icons = HashMap::new();
        for tab in &self.tabs {
            if let Some(icon) = tab.favicon.get() {
                if let Ok(url) = url::Url::parse(&tab.url) {
                    if matches!(url.scheme(), "http" | "https") {
                        icons.entry(url.origin()).or_insert(icon);
                    }
                }
            }
        }
        let mut changed = false;
        for link in &mut self.links {
            let Ok(url) = url::Url::parse(&link.u) else { continue; };
            if let Some(icon) = icons.get(&url.origin()) {
                if link.icon.as_deref() != Some(&icon.data) {
                    link.icon = Some(icon.data.clone());
                    changed = true;
                }
            }
        }
        changed
    }

    fn is_favorite(&self, url: &str) -> bool {
        !self.incognito && crate::security::web_link(url) && self.links.iter().any(|link| link.u == url)
    }

    fn push_favorites(&self) {
        let main_open = self.panel == "favorites";
        let detached = self.detached.values().filter(|shell| shell.panel == "favorites");
        if self.incognito || (!main_open && detached.clone().next().is_none()) { return; }
        let script = format!("window.onFavorites({})", self.favorite_rows());
        if main_open { let _ = self.ui.evaluate_script(&script); }
        for shell in detached { let _ = shell.strip.evaluate_script(&script); }
    }

    fn favorite_rows(&self) -> serde_json::Value {
        serde_json::Value::Array(self.links.iter().enumerate()
            .filter(|(_, link)| crate::security::web_link(&link.u))
            .map(|(i, link)| serde_json::json!({
                "i": i, "t": link.t, "u": link.u,
                "icon": link.icon.as_deref().filter(|icon| favicon::valid_tile_icon(icon))
            })).collect())
    }

    fn remove_favorite(&mut self, url: &str) {
        if !self.is_favorite(url) { return; }
        self.links.retain(|link| link.u != url);
        self.save_links();
        self.refresh_newtabs();
        self.push_favorites();
    }

    fn add_current_link(&mut self, id: u32) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id && crate::security::web_link(&tab.url)) else {
            self.toast("Откройте сайт, который хотите добавить");
            return;
        };
        let (title, url) = (tab.title.clone(), tab.url.clone());
        self.add_link(&title, &url);
    }

    fn add_link(&mut self, title: &str, url: &str) {
        if self.incognito {
            return;
        }
        let Some(u) = normalize_url(url) else {
            self.toast("Не удалось добавить: проверьте адрес ссылки");
            return;
        };
        if !crate::security::web_link(&u) { return; }
        let host = url::Url::parse(&u).ok().and_then(|p| p.host_str().map(|h| h.trim_start_matches("www.").to_string())).unwrap_or_default();
        let t: String = if title.trim().is_empty() { host } else { title.trim().chars().take(40).collect() };
        if let Some(l) = self.links.iter_mut().find(|l| l.u == u) {
            if l.t == t {
                self.toast("Страница уже в избранном");
                return;
            }
            l.t = t;
        } else if self.links.len() >= 32 {
            self.toast("Достигнут предел избранного: 32 страницы");
            return;
        } else {
            self.links.push(Link { t, u, icon: None });
        }
        self.update_link_icons();
        self.save_links();
        self.refresh_newtabs();
        self.push_favorites();
        self.toast("Добавлено в избранное");
    }

    /// Адрес окна расширения (всплывающее окно или страница настроек), если оно есть.
    fn ext_page(&self, id: &str) -> Option<(String, bool)> {
        let dir = self.ext_dir.join(self.ext_map.get(id)?);
        let raw = crate::security::read_profile_text(&dir.join("manifest.json"),1048576).ok()?;
        let m: serde_json::Value = serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()?;
        let popup = m["action"]["default_popup"]
            .as_str()
            .or(m["browser_action"]["default_popup"].as_str())
            .or(m["page_action"]["default_popup"].as_str());
        let (path, is_popup) = match popup {
            Some(p) => (p, true),
            None => (m["options_ui"]["page"].as_str().or(m["options_page"].as_str())?, false),
        };
        Some((format!("chrome-extension://{id}/{}", path.trim_start_matches('/')), is_popup))
    }

    fn spawn_filters(&self, force: bool) {
        if self.settings.adblock_level != "strong" {
            return;
        }
        let (lists, user, p) = (self.lists_dir.clone(), self.user_filters_dir.clone(), self.proxy.clone());
        std::thread::spawn(move || {
            let status = |m: &str| {
                let _ = p.send_event(UserEvent::FiltersStatus(m.to_string()));
            };
            let mut built = false;
            if !force {
                if let Some(f) = Filters::build(&lists, &user) {
                    status(&format!("Правил загружено: {}", f.rules));
                    let _ = p.send_event(UserEvent::FiltersBuilt(Arc::new(f)));
                    built = true;
                } else {
                    status("Загрузка фильтров…");
                }
            } else {
                status("Обновление фильтров…");
            }
            let n = filters::update_lists(&lists, force);
            if n > 0 || force {
                match Filters::build(&lists, &user) {
                    Some(f) => {
                        status(&format!("Правил загружено: {}", f.rules));
                        let _ = p.send_event(UserEvent::FiltersBuilt(Arc::new(f)));
                    }
                    None => status("Не удалось загрузить фильтры (проверьте интернет)"),
                }
            } else if !built {
                status("Не удалось загрузить фильтры (проверьте интернет)");
            }
        });
    }

    fn run_cosmetic_deadlines(&mut self) -> Option<Instant> {
        let now = Instant::now();
        let mut due = Vec::new();
        self.cosmetic_deadlines.retain(|id, (deadline, later)| {
            if *deadline > now { return true; }
            due.push(*id);
            if let Some(next) = later.take().filter(|next| *next > now) {
                *deadline = next;
                true
            } else {
                false
            }
        });
        for id in due { self.apply_cosmetic(id); }
        self.cosmetic_deadlines.values().map(|(deadline, _)| *deadline).min()
    }

    fn apply_cosmetic(&self, id: u32) {
        if !self.adblock.load(Ordering::Relaxed) || self.settings.adblock_level != "strong" {
            return;
        }
        let Some(f) = self.filters.borrow().clone() else { return };
        let Some(tab) = self.tabs.iter().find(|t| t.id == id) else { return };
        let Some(wv) = tab.webview.as_ref() else { return };
        if !tab.url.starts_with("http") {
            return;
        }
        let cos = f.cosmetic(&tab.url);
        let css = selectors_to_css(cos.hide.iter());
        if !css.is_empty() {
            let _ = wv.evaluate_script(&inject_css_js("d", &css));
        }
        if !cos.procedural.is_empty() {
            let rules:Vec<serde_json::Value>=cos.procedural.iter().filter_map(|s|serde_json::from_str(s).ok()).collect();
            let arr=serde_json::to_string(&rules).unwrap_or_else(|_|"[]".into());
            let _ = wv.evaluate_script(&format!("window.__lumenSetRules&&window.__lumenSetRules({arr})"));
        }
        if !cos.generichide {
            let p = self.proxy.clone();
            let epoch = tab.document_epoch;
            let _ = wv.evaluate_script_with_callback(HARVEST_JS, move |res| {
                if res.len() <= 1024 * 1024 { let _ = p.send_event(UserEvent::Harvest(id, epoch, res)); }
            });
        }
    }

    fn apply_generic(&self, id: u32, epoch: u64, res: &str) {
        let Some(f) = self.filters.borrow().clone() else { return };
        let Some(tab) = self.tabs.iter().find(|t| t.id == id) else { return };
        if tab.document_epoch != epoch || tab.loading || !self.settings.adblock || self.settings.adblock_level != "strong" { return; }
        let Some(wv) = tab.webview.as_ref() else { return };
        let raw = serde_json::from_str::<String>(res).unwrap_or_else(|_| res.to_string());
        let Ok(h) = serde_json::from_str::<Harvest>(&raw) else { return };
        let cos = f.cosmetic(&tab.url);
        let sels = f.generic(&h.c, &h.i, &cos.exceptions);
        let css = selectors_to_css(sels.iter());
        if !css.is_empty() {
            let _ = wv.evaluate_script(&inject_css_js("g", &css));
        }
    }

    fn refresh_ext(&self) {
        let p = self.proxy.clone();
        ext::list(&self.ui, move |json| {
            let _ = p.send_event(UserEvent::Extensions(json));
        });
    }

    fn rebuild_tabs(&mut self) {
        let old: Vec<_> = self
            .tabs
            .iter()
            .filter(|t| t.detached_window.is_none())
            .map(|t| (t.id, t.url.clone(), t.webview.is_some(), t.protect_draft.clone()))
            .collect();
        for t in self.tabs.iter_mut() {
            if t.detached_window.is_none() { t.webview = None; }
        }
        for (id, url, awake, draft) in old {
            if !awake && id != self.active {
                continue;
            }
            let (wv,icon, waiting_for_newtab, newtab_document) = self.make_webview(id, if url.is_empty() { None } else { Some(url) }, &draft);
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                t.webview = Some(wv);
                t.waiting_for_newtab = waiting_for_newtab;
                t.newtab_document = newtab_document;
                t.favicon=icon;
                t.visible = None; t.bounds = None;
            }
        }
        self.layout();
        self.push_state();
    }

    fn set_fullscreen(&mut self, full: bool) {
        if self.fullscreen == full { return; }
        self.fullscreen = full;
        self.fullscreen_keys.store(full, Ordering::Relaxed);
        self.window.set_fullscreen(if full { Some(tao::window::Fullscreen::Borderless(self.window.current_monitor())) } else { None });
        self.layout();
        self.with_active(|wv| { let _ = wv.focus(); });
    }

    fn change_zoom(&mut self, step: i32) {
        let old = self.zoom.get(&self.active).copied().unwrap_or(1.0);
        let next = if step == 0 { 1.0 } else { ((old * 100.0 + step as f64 * 10.0).round() / 100.0).clamp(0.25, 5.0) };
        self.with_active(|wv| { let _ = wv.zoom(next); });
        self.remember_zoom(self.active, next);
        self.push_state();
    }

    fn remember_zoom(&mut self, id: u32, factor: f64) {
        if !factor.is_finite() || !(0.25..=5.0).contains(&factor) { return; }
        let changed = self.zoom.get(&id).is_none_or(|old| (old-factor).abs()>0.0001);
        self.zoom.insert(id, factor);
        let Some(key) = self.tabs.iter().find(|t|t.id==id).and_then(|t|zoom_key(&t.url)) else { return; };
        let stored = self.settings.site_zoom.get(&key).copied().unwrap_or(1.0);
        if changed || (stored-factor).abs()>0.0001 {
            if (factor-1.0).abs()<0.0001 { self.settings.site_zoom.remove(&key); }
            else { self.settings.site_zoom.insert(key, factor); }
            self.save_settings();
        }
    }

    fn cmd(&mut self, c: Cmd) {
        if self.incognito && matches!(&c,Cmd::ExtInstall{..}|Cmd::ExtRemove{..}|Cmd::ExtToggle{..}|Cmd::ExtOpen{..}|Cmd::ExtOpenDir) {
            self.toast("Расширения недоступны в режиме инкогнито");
            return;
        }
        match c {
            Cmd::FrameTheme { dark } => {self.frame_dark=dark;self.update_window_bg();},
            Cmd::DevTools => {
                if self.incognito { return; }
                self.with_active(|wv| {
                    if hook::open_devtools(wv).is_err() {
                        self.toast("Не удалось открыть инспектор WebView2");
                    }
                });
            }
            Cmd::ZoomIn => self.change_zoom(1),
            Cmd::ZoomOut => self.change_zoom(-1),
            Cmd::ZoomReset => self.change_zoom(0),
            Cmd::Fullscreen => self.set_fullscreen(!self.fullscreen),
            Cmd::Escape => { if self.fullscreen { self.with_active(|wv| { let _ = wv.evaluate_script("document.exitFullscreen().catch(()=>{})"); }); self.set_fullscreen(false); } else { self.close_panel(); } },
            Cmd::DownloadAction { id, action } => { self.downloads.borrow_mut().action(&id,&action); self.push_downloads(); self.request_state(); },
            Cmd::ClearDownloads => { self.downloads.borrow_mut().clear(); self.push_downloads(); },
            Cmd::DownloadsFolder => self.downloads.borrow().open_folder(),
            Cmd::Ready => { self.sent_icons.borrow_mut().clear(); self.last_ui_state.borrow_mut().clear(); self.push_state(); if self.panel=="downloads"{self.push_downloads();} },
            Cmd::NewTab => self.new_tab(None),
            Cmd::CloseTab { id } => {
                self.popup_tabs.remove(&id);
                self.cosmetic_deadlines.remove(&id);
                if id == self.active && self.fullscreen { self.set_fullscreen(false); }
                self.zoom.remove(&id);
                if let Some(t) = self.tabs.iter().find(|t| t.id == id) {
                    if t.url.starts_with("http") {
                        self.closed.push((t.url.clone(), t.title.clone()));
                        if self.closed.len() > 20 {
                            self.closed.remove(0);
                        }
                    }
                }
                let key = self.detached_key(id);
                if self.main_closed && self.tabs.len() == 1 && key.is_some() {
                    let _ = self.proxy.send_event(UserEvent::Quit);
                    return;
                }
                self.tabs.retain(|t| t.id != id);
                if let Some(key) = key { self.remove_from_detached(key, id, false); }
                if self.tabs.is_empty() {
                    self.new_tab(None);
                } else if self.active == id {
                    if let Some(last) = self.tabs.iter().rev().find(|t| t.detached_window.is_none()).map(|t| t.id) {
                        self.activate(last);
                    } else {
                        self.new_tab(None);
                    }
                } else {
                    self.push_state();
                }
            }
            Cmd::Switch { id } => self.activate(id),
            Cmd::Go { text } => {
                let Some(u) = self.resolve(&text) else { self.toast("Недопустимый адрес"); return; };
                self.close_panel();
                self.with_active(|wv| {
                    let _ = wv.load_url(&u);
                });
            }
            Cmd::Back => self.with_active(|wv| {
                let _ = wv.evaluate_script("history.back()");
            }),
            Cmd::Forward => self.with_active(|wv| {
                let _ = wv.evaluate_script("history.forward()");
            }),
            Cmd::Reload => {
                // стартовая страница загружена из строки: обычное обновление превращает её в about:blank
                let is_newtab = self
                    .tabs
                    .iter()
                    .find(|t| t.id == self.active)
                    .map(|t| t.url.is_empty())
                    .unwrap_or(false);
                if is_newtab {
                    let html = self.newtab_html();
                    if let Some(tab) = self.tabs.iter().find(|tab| tab.id == self.active) {
                        Self::load_newtab(tab, &html);
                    }
                } else {
                    self.with_active(|wv| {
                        let _ = wv.evaluate_script("location.reload()");
                    });
                }
            }
            Cmd::SetEngine { engine } => {
                self.settings.engine = engine;
                self.save_settings();
                self.push_state();
            }
            Cmd::ToggleAdblock => {
                self.settings.adblock = !self.settings.adblock;
                self.adblock.store(self.settings.adblock, Ordering::Relaxed);
                self.save_settings();
                self.rebuild_tabs(); // скрипт блокировки подключается при создании вкладки
            }
            Cmd::Incognito => {
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).arg("--incognito").spawn();
                }
            }
            Cmd::OpenPanel { name } => {
                if name == "dev" { self.cmd(Cmd::DevTools); return; }
                if matches!(name.as_str(), "history" | "settings" | "ext" | "downloads")
                    || (name == "favorites" && !self.incognito)
                {
                    self.panel = name.clone();
                    self.layout();
                    self.push_state();
                    self.refresh_panel(&name);
                }
            }
            Cmd::ClosePanel => self.close_panel(),
            Cmd::OpenFavorite { url } => {
                if self.is_favorite(&url) { self.cmd(Cmd::OpenUrl { url }); }
            }
            Cmd::RemoveFavorite { url } => self.remove_favorite(&url),
            Cmd::OpenUrl { url } => {
                self.close_panel();
                self.with_active(|wv| {
                    let _ = wv.load_url(&url);
                });
            }
            Cmd::ClearHistory => {
                self.history.clear();
                self.write_history_all();
                self.push_history();
            }
            Cmd::DeleteHistory { ts, u } => {
                self.history.retain(|e| !(e.ts == ts && e.u == u));
                self.write_history_all();
                self.push_history();
            }
            Cmd::ApplyUpdate => { let _ = self.proxy.send_event(UserEvent::ApplyUpdate); }
            Cmd::CheckUpdate => { let _ = self.proxy.send_event(UserEvent::CheckUpdate); }
            Cmd::SetLook { theme, accent, font, font_size } => {
                let theme = theme.unwrap_or_else(|| self.settings.theme.clone());
                let accent = accent.unwrap_or_else(|| self.settings.accent.clone());
                let font = font.unwrap_or_else(|| self.settings.font.clone());
                let font_size = font_size.unwrap_or_else(|| self.settings.font_size.clone());
                if THEMES.contains(&theme.as_str())
                    && (ACCENTS.contains(&accent.as_str()) || crate::security::hex_color(&accent))
                    && FONTS.contains(&font.as_str())
                    && SIZES.contains(&font_size.as_str())
                {
                    self.settings.theme = theme;
                    self.settings.accent = accent;
                    self.settings.font = font;
                    self.settings.font_size = font_size;
                    self.save_settings();
                    self.update_window_bg();
                    self.refresh_newtabs();
                    self.push_state();
                }
            }
            Cmd::SetBgMode { mode } => {
                if BGS.contains(&mode.as_str()) && (mode != "custom" || self.bg_data.is_some()) {
                    self.settings.bg_mode = mode;
                    self.save_settings();
                    self.refresh_newtabs();
                    self.push_state();
                }
            }
            Cmd::SetBackground { data } => {
                if crate::security::image_data(&data) {
                    if !self.incognito {
                        let _ = crate::security::atomic_write(&self.bg_path, data.as_bytes());
                    }
                    self.bg_data = Some(data);
                    self.settings.bg_mode = "custom".into();
                    self.save_settings();
                    self.refresh_newtabs();
                    self.push_state();
                }
            }
            Cmd::ClearBackground => {
                if !self.incognito { let _ = fs::remove_file(&self.bg_path); }
                self.bg_data = None;
                if self.settings.bg_mode == "custom" {
                    self.settings.bg_mode = "none".into();
                }
                self.save_settings();
                self.refresh_newtabs();
                self.push_state();
            }
            Cmd::SetAdblockLevel { level } => {
                if level == "basic" || level == "strong" {
                    self.settings.adblock_level = level.clone();
                    self.save_settings();
                    if level == "strong" {
                        self.filters_status = "Загрузка фильтров…".into();
                        self.spawn_filters(false);
                    } else {
                        *self.filters.borrow_mut() = None; // освобождаем память движка
                        self.filters_status = "Выключено (базовый режим)".into();
                    }
                    self.push_state();
                }
            }
            Cmd::UpdateFilters => {
                if self.settings.adblock_level == "strong" {
                    self.filters_status = "Обновление фильтров…".into();
                    self.spawn_filters(true);
                    self.push_state();
                }
            }
            Cmd::ExtOpenDir => {
                let _ = fs::create_dir_all(&self.ext_dir);
                let _ = std::process::Command::new("explorer.exe").arg(&self.ext_dir).spawn();
            }
            Cmd::ExtRefresh => self.refresh_ext(),
            Cmd::ExtInstall { input } => {
                if self.incognito {
                    let _ = self.proxy.send_event(UserEvent::ExtChanged("В режиме инкогнито расширения недоступны".into()));
                } else {
                    let _ = self.proxy.send_event(UserEvent::ExtChanged("Скачиваю расширение…".into()));
                    let (dir, p) = (self.ext_dir.clone(), self.proxy.clone());
                    std::thread::spawn(move || {
                        let r = ext::install_crx(&dir, &input);
                        let _ = p.send_event(UserEvent::ExtInstalled(r));
                    });
                }
            }
            Cmd::ExtToggle { id, on } => {
                let p = self.proxy.clone();
                ext::set_enabled(&self.ui, id, on, move || {
                    let _ = p.send_event(UserEvent::ExtChanged(String::new()));
                });
            }
            Cmd::ExtOpen { id } => match self.ext_page(&id) {
                Some((u, true)) => self.open_popup(&self.window, &u),
                Some((u, false)) => self.new_tab(Some(u)),
                None => {
                    let _ = self.proxy.send_event(UserEvent::ExtChanged(
                        "У этого расширения нет окна. Переустановите его, чтобы кнопка «Открыть» заработала".into(),
                    ));
                }
            },
            Cmd::AddCurrentLink => self.add_current_link(self.active),
            Cmd::DefaultBrowser => {
                match std::env::current_exe().and_then(|exe|default_browser::open_settings(&exe)) {
                    Ok(())=>self.toast("Выберите Lumen в приложениях по умолчанию Windows"),
                    Err(error)=>self.toast(&format!("Не удалось открыть настройки Windows: {error}")),
                }
            }
            Cmd::SetStartup { mode } => {
                if mode == "restore" || mode == "new" {
                    self.settings.startup = mode;
                    self.save_settings();
                    self.push_state();
                }
            }
            Cmd::WinMin => self.window.set_minimized(true),
            Cmd::WinMax => self.window.set_maximized(!self.window.is_maximized()),
            Cmd::WinClose => {
                let _ = self.proxy.send_event(UserEvent::Quit);
            }
            Cmd::WinDrag => {
                let _ = self.window.drag_window();
            }
            Cmd::DropTab { id, index, detach } => {
                let mut point = windows::Win32::Foundation::POINT::default();
                let cursor = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point) }.is_ok();
                // The drag controller owns the gesture boundary. A stale system
                // cursor must not turn a within-strip reorder into a transfer.
                if cursor && detach {
                    self.cmd(Cmd::DetachTab { id, x: point.x, y: point.y });
                } else {
                    self.move_tab_to(id, index);
                    self.push_state();
                    self.schedule_session_save();
                }
            }
            Cmd::DetachTab { id, x, y } => {
                if self.incognito { self.toast("Перенос вкладок недоступен в инкогнито"); return; }
                let _ = self.proxy.send_event(UserEvent::Detach(id, x, y));
            }
            Cmd::SetSleep { minutes } => {
                if matches!(minutes, 0 | 3 | 5 | 10) {
                    self.settings.sleep_minutes = minutes;
                    self.save_settings();
                    self.push_state();
                }
            }
            Cmd::SetEco { on } => {
                self.settings.eco = on;
                self.save_settings();
                self.push_state();
                self.toast("Эконом-режим применится после перезапуска браузера");
            }
            Cmd::SetLabels { on } => {
                self.settings.hide_labels = on;
                self.save_settings();
                for t in &self.tabs {
                    if let Some(wv) = &t.webview {
                        let _ = wv.evaluate_script(&format!("window.__lumenSetLabels?window.__lumenSetLabels({on}):(window.__lumenCfg&&(window.__lumenCfg.labels={on}))"));
                    }
                }
                self.push_state();
            }
            Cmd::ExtRemove { id } => {
                if let Some(name) = self.ext_map.remove(&id) {
                    if crate::security::managed_extension_name(&id,&name) {
                        let dir = self.ext_dir.join(name);
                        let root=self.ext_dir.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_secs(2));
                            if let (Ok(root),Ok(resolved))=(root.canonicalize(),dir.canonicalize()) {
                                use std::os::windows::fs::MetadataExt;
                                if resolved.parent()==Some(root.as_path()) && fs::symlink_metadata(&dir).is_ok_and(|m|m.file_attributes()&0x400==0) {
                                    let _ = fs::remove_dir_all(resolved);
                                }
                            }
                        });
                    }
                    if let Ok(j) = serde_json::to_string(&self.ext_map) {
                        let _ = crate::security::atomic_write(&self.ext_map_path, j.as_bytes());
                    }
                }
                let p = self.proxy.clone();
                ext::remove(&self.ui, id, move || {
                    let _ = p.send_event(UserEvent::ExtChanged("Расширение удалено".into()));
                });
            }
        }
    }

    fn handle(&mut self, ev: UserEvent) {
        match ev {
            UserEvent::FaviconsChanged => {
                if self.update_link_icons() {
                    self.newtab_cache.replace(None);
                    self.save_links();
                    let icons = self.links.iter().map(|link| &link.icon).collect::<Vec<_>>();
                    let data = serde_json::to_string(&icons).unwrap();
                    for view in self.tabs.iter().filter(|tab| tab.url.is_empty()).filter_map(|tab| tab.webview.as_ref()) {
                        let _ = view.evaluate_script(&format!("window.updateLinkIcons?.({data})"));
                    }
                    self.push_favorites();
                }
                self.request_state();
            }
            UserEvent::DownloadPoll => {
                let changed = self.downloads.borrow_mut().poll();
                if self.downloads.borrow().active_count()==0 {self.download_hosts.clear();}
                if changed {
                    let notice=self.downloads.borrow_mut().take_notice();
                    if let Some(text)=notice { self.toast(&text); }
                    if self.panel == "downloads" || self.detached.values().any(|shell| shell.panel == "downloads") {
                        self.push_downloads();
                    }
                    self.request_state();
                }
            },
            UserEvent::ZoomChanged(id, factor) => {
                if factor.is_finite() && factor > 0.0 && self.tabs.iter().any(|t|t.id==id) {
                    if self.zoom.get(&id).is_some_and(|old|(old-factor).abs()<0.0001) { return; }
                    self.remember_zoom(id,factor);
                    if id == self.active || self.detached_key(id).is_some() { self.request_state(); }
                }
            },
            UserEvent::FullscreenChanged(id, full) => {
                if let Some(window) = self.detached_key(id).and_then(|key| self.detached.get(&key)).filter(|shell| shell.active == id) {
                    let window = &window.window;
                    window.set_fullscreen(full.then(|| tao::window::Fullscreen::Borderless(window.current_monitor())));
                } else if id == self.active {
                    self.set_fullscreen(full);
                }
            },
            UserEvent::Ui(s) if !self.main_closed => match serde_json::from_str::<Cmd>(&s) {
                Ok(c) => self.cmd(c),
                Err(e) => log(|| format!("bad ui message: {e}")),
            },
            UserEvent::Title(id, t) => {
                if let Some(tab) = self.tabs.iter_mut().find(|x| x.id == id && x.webview.is_some()) {
                    tab.title = t.clone();
                    if let Some(window) = tab.detached_window.and_then(|key| self.detached.get(&key)).filter(|shell| shell.active == id) {
                        window.set_title(&t);
                    }
                    let url = tab.url.clone();
                    if let Some(last) = self.history.last_mut() {
                        if last.u == url {
                            last.t = t;
                        }
                    }
                    self.request_state();
                }
            }
            UserEvent::Url(id, u) => {
                let u = if u.starts_with("about:") || u.starts_with("data:") { String::new() } else { u };
                let background = if u.is_empty() {
                    window_bg(&self.window, &self.settings.theme, self.incognito)
                } else {
                    WEB_PAGE_BACKGROUND
                };
                if let Some(tab) = self.tabs.iter_mut().find(|x| x.id == id && x.webview.is_some()) {
                    if tab.url == u { return; }
                    if tab.url.is_empty() != u.is_empty() {
                        if let Some(view) = &tab.webview {
                            let _ = view.set_background_color(background);
                        }
                    }
                    let origin_changed = zoom_key(&tab.url) != zoom_key(&u);
                    tab.url = u;
                    if !origin_changed {
                        self.request_state();
                        return;
                    }
                    tab.favicon.clear();
                    let factor = zoom_key(&tab.url).and_then(|key|self.settings.site_zoom.get(&key).copied())
                        .filter(|factor|factor.is_finite() && (0.25..=5.0).contains(factor)).unwrap_or(1.0);
                    self.zoom.insert(id,factor);
                    if let Some(wv)=&tab.webview { let _=wv.zoom(factor); }
                    self.request_state();
                }
            }
            UserEvent::Loading(id, on) => {
                if on { self.cosmetic_deadlines.remove(&id); }
                let mut finished = false;
                if let Some(tab) = self.tabs.iter_mut().find(|x| x.id == id && x.webview.is_some()) {
                    if tab.loading != on {
                        if on { tab.document_epoch = tab.document_epoch.wrapping_add(1); }
                        tab.loading = on;
                        finished = !on;
                    }
                }
                if finished {
                    self.record_history(id);
                    self.apply_cosmetic(id);
                    if self.settings.adblock && self.settings.adblock_level == "strong" {
                        let now = Instant::now();
                        self.cosmetic_deadlines.insert(id, (
                            now + Duration::from_millis(2500),
                            Some(now + Duration::from_secs(8)),
                        ));
                    }
                }
                if !on {
                    if let Some(key) = self.detached_key(id) {
                        if self.detached.get(&key).is_some_and(|shell| shell.active == id) {
                            let focus = self.tabs.iter().any(|tab| tab.id == id && tab.visible != Some(true));
                            self.layout_detached(key, focus);
                        }
                    }
                }
                if !on && id == self.active && self.panel.is_empty() {
                    let was_visible = self.tabs.iter().any(|tab| tab.id == id && tab.visible == Some(true));
                    self.layout();
                    if !was_visible { self.focus_active_page(); }
                }
                self.request_state();
            }
            UserEvent::DetachedReady(key) => {
                if let Some(shell) = self.detached.get(&key) {
                    shell.last_state.borrow_mut().clear();
                    shell.sent_icons.borrow_mut().clear();
                    self.push_state();
                }
            }
            UserEvent::DetachedShortcut(key, shortcut) => {
                if let Some(shell) = self.detached.get(&key) {
                    self.handle(UserEvent::PageShortcut(shell.active, shortcut));
                }
            }
            UserEvent::TransferTab(id, source, destination, index) => self.transfer_tab(id, source, destination, index),
            UserEvent::Ui(_) | UserEvent::Quit | UserEvent::Detach(..) | UserEvent::DetachedUi(..) | UserEvent::CheckUpdate | UserEvent::ApplyUpdate | UserEvent::UpdateStatus(_) => {}
            UserEvent::Navigate(id, u) => {
                if let Some(wv) = self.tabs.iter().find(|x| x.id == id).and_then(|t| t.webview.as_ref()) {
                    let _ = wv.load_url(&u);
                }
            }
            UserEvent::Search(id, q) => {
                let Some(u) = self.resolve(&q) else { return; };
                if let Some(wv) = self.tabs.iter().find(|x| x.id == id).and_then(|t| t.webview.as_ref()) {
                    let _ = wv.load_url(&u);
                }
            }
            UserEvent::PopupTab(source,url,foreground) => {
                if !safe_url(&url) || self.popup_tabs.len() >= 8 || !self.tabs.iter().any(|tab| tab.id == source) { return; }
                let id=self.next_id;
                let window = self.detached_key(source);
                let source_active = match window {
                    Some(key) => self.detached.get(&key).is_some_and(|shell| shell.active == source),
                    None => self.active == source,
                };
                self.popup_tabs.insert(id, source);
                self.create_tab_in(Some(url), foreground && source_active, window);
                if !self.tabs.iter().any(|tab| tab.id == id) {
                    self.popup_tabs.remove(&id);
                }
            },
            UserEvent::PopupCommitted(id) => {
                self.popup_tabs.remove(&id);
            },
            UserEvent::PopupDownload(id)=>{
                if let Some(source) = self.popup_tabs.remove(&id){
                    if let Some(index)=self.tabs.iter().position(|tab|tab.id==id){
                        let mut tab=self.tabs.remove(index);
                        if let Some(wv)=tab.webview.take(){
                            let _=wv.set_visible(false);
                            self.download_hosts.push(wv);
                        }
                        self.zoom.remove(&id);
                        if let Some(key) = tab.detached_window { self.remove_from_detached(key, id, false); }
                        if self.active==id {
                            if let Some(next) = self.tabs.iter().filter(|tab| tab.detached_window.is_none())
                                .max_by_key(|tab| (tab.id == source, tab.last_active)).map(|tab| tab.id)
                            {
                                self.activate(next);
                            }
                            else {self.new_tab(None);}
                        }
                        self.layout();self.request_state();self.schedule_session_save();
                    }
                }
            },
            UserEvent::FiltersBuilt(f) => {
                if self.settings.adblock_level == "strong" {
                    *self.filters.borrow_mut() = Some(f);
                    self.push_state();
                }
            }
            UserEvent::FiltersStatus(m) => {
                if self.settings.adblock_level == "strong" {
                    self.filters_status = m;
                    self.push_state();
                }
            }
            UserEvent::Harvest(id, epoch, res) => self.apply_generic(id, epoch, &res),
            UserEvent::Extensions(json) => {
                let mut list: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap_or_default();
                for e in list.iter_mut() {
                    let id = e["id"].as_str().unwrap_or("").to_string();
                    e["openable"] = self.ext_page(&id).is_some().into();
                }
                let j = serde_json::Value::Array(list);
                self.send_to_panel("ext", &format!("window.onExt({j})"));
            }
            UserEvent::ExtInstalled(res) => match res {
                Ok(dir) => {
                    let p = self.proxy.clone();
                    let d = dir.clone();
                    ext::add(&self.ui, &dir, move |id| {
                        let _ = p.send_event(UserEvent::ExtAdded(d, id));
                    });
                }
                Err(e) => {
                    let _ = self.proxy.send_event(UserEvent::ExtChanged(e));
                }
            },
            UserEvent::ExtAdded(dir, id) => {
                let msg = match id {
                    Ok(id) => {
                        if let Some(name) = dir.file_name().map(|n| n.to_string_lossy().to_string()) {
                            let path = if dir.parent() == Some(self.ext_dir.as_path()) { name } else { dir.to_string_lossy().into_owned() };
                            self.ext_map.insert(id, path);
                            if let Ok(j) = serde_json::to_string(&self.ext_map) {
                                let _ = crate::security::atomic_write(&self.ext_map_path, j.as_bytes());
                            }
                        }
                        "Расширение установлено. Обновите открытые страницы.".to_string()
                    }
                    Err(e) => e,
                };
                let _ = self.proxy.send_event(UserEvent::ExtChanged(msg.to_string()));
            }
            UserEvent::PageShortcut(id, sc) => {
                if let Some(key) = self.detached_key(id) {
                    let Some(detached) = self.detached.get(&key).filter(|shell| shell.active == id) else { return; };
                    let window = &detached.window;
                    let view = self.tabs.iter().find(|tab| tab.id == id).and_then(|tab| tab.webview.as_ref());
                    match (sc, view) {
                        (Shortcut::CloseTab, _) => self.cmd(Cmd::CloseTab { id }),
                        (Shortcut::NewTab, _) => self.create_tab_in(None, true, Some(key)),
                        (Shortcut::AddLink, _) => self.add_current_link(id),
                        (Shortcut::Escape, _) => self.set_detached_panel(key, ""),
                        (Shortcut::History, _) => self.set_detached_panel(key, "history"),
                        (Shortcut::Downloads, _) => self.set_detached_panel(key, "downloads"),
                        (Shortcut::Settings, _) => self.set_detached_panel(key, "settings"),
                        (Shortcut::FocusUrl, _) => {
                            let _ = detached.strip.focus();
                            let _ = detached.strip.evaluate_script("document.getElementById('url').focus()");
                        }
                        (Shortcut::Tab(index), _) => {
                            if let Some(&tab) = detached.tabs.get(index) {
                                self.activate_detached(key, tab);
                            }
                        }
                        (Shortcut::NextTab | Shortcut::PrevTab, _) => {
                            let position = detached.tabs.iter().position(|tab| *tab == id).unwrap();
                            let count = detached.tabs.len();
                            let offset = if matches!(sc, Shortcut::NextTab) { 1 } else { count - 1 };
                            self.activate_detached(key, detached.tabs[(position + offset) % count]);
                        }
                        (Shortcut::Back, Some(view)) => { let _ = view.evaluate_script("history.back()"); }
                        (Shortcut::Forward, Some(view)) => { let _ = view.evaluate_script("history.forward()"); }
                        (Shortcut::DevTools, Some(view)) => { let _ = hook::open_devtools(view); }
                        (Shortcut::Fullscreen, _) => {
                            let mode = window.fullscreen().is_none()
                                .then(|| tao::window::Fullscreen::Borderless(window.current_monitor()));
                            window.set_fullscreen(mode);
                        }
                        (Shortcut::ZoomIn | Shortcut::ZoomOut | Shortcut::ZoomReset, Some(view)) => {
                            let zoom = self.zoom.get(&id).copied().unwrap_or(1.0);
                            let factor = match sc {
                                Shortcut::ZoomIn => (zoom + 0.1).min(5.0),
                                Shortcut::ZoomOut => (zoom - 0.1).max(0.25),
                                _ => 1.0,
                            };
                            let _ = view.zoom(factor);
                        }
                        _ => {}
                    }
                } else if id == self.active {
                    self.handle(UserEvent::Shortcut(sc));
                }
            }
            UserEvent::Shortcut(sc) => match sc {
                Shortcut::ZoomIn => self.cmd(Cmd::ZoomIn),
                Shortcut::ZoomOut => self.cmd(Cmd::ZoomOut),
                Shortcut::ZoomReset => self.cmd(Cmd::ZoomReset),
                Shortcut::DevTools => self.cmd(Cmd::DevTools),
                Shortcut::Fullscreen => self.cmd(Cmd::Fullscreen),
                Shortcut::Escape => self.cmd(Cmd::Escape),
                Shortcut::NewTab => self.new_tab(None),
                Shortcut::Reopen => {
                    if let Some((u, _)) = self.closed.pop() {
                        self.new_tab(Some(u));
                    }
                }
                Shortcut::CloseTab => self.cmd(Cmd::CloseTab { id: self.active }),
                Shortcut::FocusUrl => {
                    self.close_panel();
                    let _ = self.ui.focus();
                    let _ = self.ui.evaluate_script("(()=>{const u=document.getElementById('url');u.focus();u.select();})()");
                }
                Shortcut::Incognito => self.cmd(Cmd::Incognito),
                Shortcut::Downloads => self.cmd(Cmd::OpenPanel { name: "downloads".into() }),
                Shortcut::History => self.cmd(Cmd::OpenPanel { name: "history".into() }),
                Shortcut::Settings => self.cmd(Cmd::OpenPanel { name: "settings".into() }),
                Shortcut::AddLink => self.cmd(Cmd::AddCurrentLink),
                Shortcut::Back => self.cmd(Cmd::Back),
                Shortcut::Forward => self.cmd(Cmd::Forward),
                Shortcut::NextTab | Shortcut::PrevTab => {
                    let tabs: Vec<_> = self.tabs.iter().filter(|tab| tab.detached_window.is_none()).map(|tab| tab.id).collect();
                    let n = tabs.len();
                    if let Some(i) = tabs.iter().position(|&id| id == self.active) {
                        let j = if matches!(sc, Shortcut::NextTab) { (i + 1) % n } else { (i + n - 1) % n };
                        self.activate(tabs[j]);
                    }
                }
                Shortcut::Tab(i) => {
                    if let Some(id) = self.tabs.iter().filter(|tab| tab.detached_window.is_none()).nth(i).map(|t| t.id) {
                        self.activate(id);
                    }
                }
            },
            UserEvent::Link(c) => match c {
                LinkCmd::Add(t, u) => self.add_link(&t, &u),
                LinkCmd::Del(i) => {
                    if i < self.links.len() && !self.incognito {
                        self.links.remove(i);
                        self.save_links();
                        self.refresh_newtabs();
                        self.push_favorites();
                    }
                }
            },
            UserEvent::ExtChanged(msg) => {
                if !msg.is_empty() {
                    if let Ok(j) = serde_json::to_string(&msg) {
                        self.send_to_panel("ext", &format!("window.onExtMsg({j})"));
                    }
                }
                self.refresh_ext();
            }
        }
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    #[test]
    fn web_navigation_rejects_embedded_credentials() {
        assert!(!safe_url("https://user:secret@example.test/"));
        assert!(!safe_url("https://user@example.test/"));
        assert!(safe_url("https://example.test/watch?v=123"));
    }

    #[test]
    fn site_zoom_survives_settings_round_trip() {
        let mut settings=Settings::default();
        let key=zoom_key("https://Example.com/watch?v=123#part").unwrap();
        assert_eq!(key,"https://example.com");
        settings.site_zoom.insert(key.clone(),1.3);
        let restored:Settings=serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.site_zoom.get(&zoom_key("https://example.com/other").unwrap()),Some(&1.3));
        assert!(!restored.site_zoom.contains_key(&zoom_key("https://other.example/").unwrap()));
        assert_eq!(zoom_key(""),zoom_key("about:blank"));
        assert_ne!(zoom_key("http://localhost:8000/"),zoom_key("http://localhost:8001/"));
        assert!(zoom_key("chrome-extension://id/popup.html").is_none());
        let old:Settings=serde_json::from_str("{}").unwrap();assert!(old.site_zoom.is_empty());
    }
    #[test]
    #[ignore = "Requires an interactive Windows desktop"]
    fn animated_window_stays_visible_when_maximized_and_fullscreen() {
        use tao::platform::windows::EventLoopBuilderExtWindows;
        use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::IsWindowVisible};
        let event_loop = EventLoopBuilder::<()>::new().with_any_thread(true).build();
        let window = WindowBuilder::new().with_title("Lumen fullscreen regression")
            .with_decorations(false).with_visible(false)
            .with_inner_size(tao::dpi::LogicalSize::new(640.0, 400.0)).build(&event_loop).unwrap();
        animate_window(&window, true);
        assert!(window.is_visible(), "tao still thinks the animated window is hidden");
        window.set_maximized(true);
        assert!(unsafe { IsWindowVisible(HWND(window.hwnd() as *mut _)).as_bool() });
        for _ in 0..2 {
            window.set_fullscreen(Some(tao::window::Fullscreen::Borderless(window.current_monitor())));
            assert!(window.is_visible() && !window.is_minimized());
            assert!(unsafe { IsWindowVisible(HWND(window.hwnd() as *mut _)).as_bool() });
            window.set_fullscreen(None);
            assert!(unsafe { IsWindowVisible(HWND(window.hwnd() as *mut _)).as_bool() });
        }
    }
    #[test]
    #[ignore = "Requires the native WebView2 runtime"]
    fn webview_leaves_all_native_window_resize_handles_reachable() {
        use tao::platform::windows::EventLoopBuilderExtWindows;
        use windows::Win32::{
            Foundation::{HWND, LPARAM, POINT, WPARAM},
            Graphics::Gdi::ClientToScreen,
            UI::WindowsAndMessaging::*,
        };
        let event_loop = EventLoopBuilder::<()>::new().with_any_thread(true).build();
        let window = WindowBuilder::new().with_decorations(false).with_visible(false)
            .with_inner_size(tao::dpi::LogicalSize::new(640.0, 400.0)).build(&event_loop).unwrap();
        let profile = tempfile::tempdir().unwrap();
        let mut context = WebContext::new(Some(profile.path().join("profile")));
        let _view = WebViewBuilder::with_web_context(&mut context)
            .with_bounds(panel_rect(&window))
            .with_html("<!doctype html><title>Resize fixture</title>")
            .build_as_child(&window).unwrap();
        window.set_visible(true);
        let hwnd = HWND(window.hwnd() as _);
        let size = window.inner_size();
        let right = size.width as i32 - 1;
        let bottom = size.height as i32 - 1;
        let center = POINT { x: right / 2, y: bottom / 2 };
        assert!(inset_px(&window) > 0);
        assert_ne!(unsafe { ChildWindowFromPointEx(hwnd, center, CWP_SKIPINVISIBLE) }, hwnd);
        for (x, y, expected) in [
            (0, center.y, HTLEFT), (right, center.y, HTRIGHT),
            (center.x, 0, HTTOP), (center.x, bottom, HTBOTTOM),
            (0, 0, HTTOPLEFT), (right, 0, HTTOPRIGHT),
            (0, bottom, HTBOTTOMLEFT), (right, bottom, HTBOTTOMRIGHT),
        ] {
            let point = POINT { x, y };
            assert_eq!(unsafe { ChildWindowFromPointEx(hwnd, point, CWP_SKIPINVISIBLE) }, hwnd);
            let mut screen = point;
            assert!(unsafe { ClientToScreen(hwnd, &mut screen) }.as_bool());
            let packed = u32::from(screen.x as u16) | (u32::from(screen.y as u16) << 16);
            let hit = unsafe { SendMessageW(hwnd, WM_NCHITTEST, WPARAM(0), LPARAM(packed as isize)) };
            assert_eq!(hit.0, expected as isize, "resize hit at ({x}, {y})");
        }
        window.set_maximized(true);
        assert_eq!(inset_px(&window), 0);
        window.set_fullscreen(Some(tao::window::Fullscreen::Borderless(window.current_monitor())));
        assert_eq!(inset_px(&window), 0);
    }

    #[test]
    #[ignore = "Requires the native WebView2 runtime"]
    fn draft_signal_survives_same_document_navigation_and_resets_on_reload() {
        use tao::platform::{run_return::EventLoopExtRunReturn, windows::EventLoopBuilderExtWindows};

        let mut events = EventLoopBuilder::<bool>::with_user_event().with_any_thread(true).build();
        let window = WindowBuilder::new().with_visible(false).build(&events).unwrap();
        let profile = tempfile::tempdir().unwrap();
        let mut context = WebContext::new(Some(profile.path().join("profile")));
        let loaded = events.create_proxy();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/draft", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for mut stream in listener.incoming().flatten() {
                let mut request = [0; 2048];
                let _ = stream.read(&mut request);
                let html = "<!doctype html><title>Draft</title><textarea></textarea>";
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}", html.len());
                if request.starts_with(b"GET /fresh ") { break; }
            }
        });
        let view = WebViewBuilder::with_web_context(&mut context)
            .with_initialization_script(include_str!("background-links.js"))
            .with_on_page_load_handler(move |event, _| {
                if matches!(event, PageLoadEvent::Finished) {
                    let _ = loaded.send_event(true);
                }
            })
            .build_as_child(&window).unwrap();
        let draft = Rc::new(Cell::new(false));
        hook::install_page_signals(&view, Rc::default(), draft.clone()).unwrap();
        let source = events.create_proxy();
        hook::install_source_changed(&view, move |_| { let _ = source.send_event(false); }).unwrap();
        let new_document = draft.clone();
        hook::install_popup_outcome(&view, move || new_document.set(false), || {}).unwrap();
        view.load_url(&url).unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut phase = 0;
        events.run_return(|event, _, flow| {
            *flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(40));
            if matches!(event, Event::UserEvent(true)) {
                if phase == 0 && view.url().is_ok_and(|url| url.ends_with("/draft")) {
                    view.evaluate_script(r#"
                        const field = document.querySelector('textarea');
                        if (!field) throw Error('fixture not ready');
                        field.value = 'unsaved';
                        field.dispatchEvent(new Event('input', {bubbles: true}));
                        location.hash = 'same-document';
                    "#).unwrap();
                    phase = 1;
                } else if phase == 2 && !draft.get() {
                    assert!(!draft.get());
                    phase = 3;
                }
            }
            if phase == 1 && draft.get() && view.url().is_ok_and(|url| url.ends_with("#same-document")) {
                phase = 2;
                view.load_url(&url.replace("/draft", "/fresh")).unwrap();
            }
            if phase == 3 || Instant::now() >= deadline { *flow = ControlFlow::Exit; }
        });
        assert_eq!(phase, 3);
    }

    #[test]
    #[ignore = "Requires the native WebView2 runtime"]
    fn start_page_csp_allows_search_and_blocks_inline_injection() {
        use tao::platform::{run_return::EventLoopExtRunReturn, windows::EventLoopBuilderExtWindows};
        let token = crate::security::newtab_token();
        let html = NEWTAB_HTML.replace("__NEWTAB_TOKEN__", &token)
            .replace("__INC__", "0").replace("__THEME__", "dark").replace("__BGATTR__", "0")
            .replace("/*__LINKS__*/[]", r#"[{"t":"Fixture","u":"https://example.com/"}]"#);
        let mut events = EventLoopBuilder::<u8>::with_user_event().with_any_thread(true).build();
        let window = WindowBuilder::new().with_visible(false).build(&events).unwrap();
        let profile = tempfile::tempdir().unwrap();
        let mut context = WebContext::new(Some(profile.path().join("profile")));
        let messages = events.create_proxy();
        let navigation = events.create_proxy();
        let allowed = html.clone();
        let _view = WebViewBuilder::with_web_context(&mut context)
            .with_devtools(false)
            .with_bounds(Rect {
                position: PhysicalPosition::new(0, 0).into(),
                size: PhysicalSize::new(640, 400).into(),
            })
            .with_on_page_load_handler(move |event, _| {
                if matches!(event, PageLoadEvent::Finished) { let _ = messages.send_event(0); }
            })
            .with_navigation_handler(move |url| {
                if url::Url::parse(&url).is_ok_and(|u|crate::security::newtab_request(&u,&token)) {
                    let _ = navigation.send_event(2);
                    return false;
                }
                crate::security::internal_document(&url, &allowed)
            })
            .with_html(html)
            .build_as_child(&window).unwrap();
        let evaluation = events.create_proxy();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut result = 0;
        events.run_return(|event, _, flow| {
            *flow = ControlFlow::WaitUntil(deadline);
            if let Event::UserEvent(value) = event {
                if value == 0 {
                    let proxy = evaluation.clone();
                    _view.evaluate_script_with_callback(r#"
                        (() => {
                            const script = document.createElement('script');
                            script.textContent = 'window.injected = true';
                            document.body.append(script);
                            const ready = document.querySelector('#tiles a.tile')?.textContent.includes('Fixture');
                            document.querySelector('input[name=q]').value = 'probe';
                            document.querySelector('main form').requestSubmit();
                            return !!ready && !window.injected;
                        })()
                    "#, move |result| {
                        let _ = proxy.send_event(if result == "true" {1} else {3});
                    }).unwrap();
                }
                if value == 3 { *flow = ControlFlow::Exit; return; }
                result |= value;
            }
            if result == 3 || Instant::now() >= deadline { *flow = ControlFlow::Exit; }
        });
        assert_eq!(result, 3, "expected rendered links, blocked injection and search submission");
    }

    #[test]
    #[ignore = "Requires the native WebView2 runtime"]
    fn same_document_url_and_native_inspector() {
        use tao::platform::{run_return::EventLoopExtRunReturn, windows::EventLoopBuilderExtWindows};
        use std::{io::{Read, Write}, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/watch?v=old", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let html = "<!doctype html><title>Lumen inspector fixture</title>";
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}", html.len());
        });
        let mut events = EventLoopBuilder::<String>::with_user_event().with_any_thread(true).build();
        let window = WindowBuilder::new().with_visible(false).build(&events).unwrap();
        let detached = WindowBuilder::new().with_visible(false).build(&events).unwrap();
        let profile = tempfile::tempdir().unwrap();
        let mut context = WebContext::new(Some(profile.path().join("profile")));
        let loaded = events.create_proxy();
        let view = WebViewBuilder::with_web_context(&mut context)
            .with_devtools(false)
            .with_on_page_load_handler(move |event, _| {
                if matches!(event, PageLoadEvent::Finished) {
                    let _ = loaded.send_event(String::new());
                }
            })
            .with_url(&url)
            .build_as_child(&window).unwrap();
        hook::lock_down(&view).unwrap();
        let changed = events.create_proxy();
        hook::install_source_changed(&view, move |source| { let _ = changed.send_event(source); }).unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut checked = false;
        let events_proxy = events.create_proxy();
        events.run_return(|event, _, flow| {
            *flow = ControlFlow::WaitUntil(deadline);
            if let Event::UserEvent(source) = event {
                if source.is_empty() {
                    view.evaluate_script("window.transferMarker = 73; history.pushState(null, '', '/watch?v=latest')").unwrap();
                } else if source.ends_with("/watch?v=latest") {
                    assert_eq!(hook::source(&view).as_deref(), Some(source.as_str()));
                    view.reparent(detached.hwnd()).unwrap();
                    view.set_bounds(full_rect(&detached)).unwrap();
                    let result = events_proxy.clone();
                    view.evaluate_script_with_callback("window.transferMarker", move |value| {
                        let _ = result.send_event(format!("detached:{value}"));
                    }).unwrap();
                } else if source == "detached:73" {
                    view.reparent(window.hwnd()).unwrap();
                    view.set_bounds(full_rect(&window)).unwrap();
                    let result = events_proxy.clone();
                    view.evaluate_script_with_callback("window.transferMarker", move |value| {
                        let _ = result.send_event(format!("reattached:{value}"));
                    }).unwrap();
                } else if source == "reattached:73" {
                    hook::open_devtools(&view).unwrap();
                    checked = true;
                    *flow = ControlFlow::Exit;
                }
            }
            if Instant::now() >= deadline { *flow = ControlFlow::Exit; }
        });
        assert!(checked, "WebView2 must retain page state after detach and reattach");
    }

    #[test]
    #[ignore = "Requires the native WebView2 runtime"]
    fn settings_and_downloads_render_in_webview() {
        use tao::platform::{run_return::EventLoopExtRunReturn, windows::EventLoopBuilderExtWindows};
        let mut events = EventLoopBuilder::<Option<(bool, bool)>>::with_user_event().with_any_thread(true).build();
        let window = WindowBuilder::new().with_visible(false).build(&events).unwrap();
        let profile = tempfile::tempdir().unwrap();
        let mut context = WebContext::new(Some(profile.path().join("profile")));
        let loaded = events.create_proxy();
        let view = WebViewBuilder::with_web_context(&mut context)
            .with_on_page_load_handler(move |event, _| {
                if matches!(event, PageLoadEvent::Finished) { let _ = loaded.send_event(None); }
            })
            .with_ipc_handler({
                let click = events.create_proxy();
                move |req| {
                    if matches!(serde_json::from_str::<Cmd>(req.body()), Ok(Cmd::ApplyUpdate)) {
                        let _ = click.send_event(Some((false, true)));
                    }
                }
            })
            .with_bounds(Rect {
                position: PhysicalPosition::new(0, 0).into(),
                size: tao::dpi::LogicalSize::new(480.0, 400.0).into(),
            })
            .with_html(UI_HTML)
            .build_as_child(&window).unwrap();
        let result = events.create_proxy();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut passed = false;
        let mut clicked = false;
        events.run_return(|event, _, flow| {
            *flow = ControlFlow::WaitUntil(deadline);
            match event {
                Event::UserEvent(None) => {
                    let result = result.clone();
                    view.evaluate_script_with_callback(r#"
                        (() => {
                            const fixture = {
                                platform: 'windows', active: 40, panel: 'settings', engine: 'google',
                                tabs: Array.from({length: 40}, (_, i) => ({id: i + 1, title: 'Вкладка ' + (i + 1)})),
                                set: { theme: 'dark', accent: '#456789' },
                                look: { al: '#456789', ad: '#456789', font: 'sans-serif', fs: 13 },
                                filters: { level: 'basic' }, block: { count: 0, recent: [] }
                            };
                            window.onState(fixture);
                            const color = document.getElementById('accenthex').value === '#456789';
                            const settings = !document.getElementById('p-settings').hidden;
                            const apply = document.getElementById('applyupdate');
                            const initiallyHidden = apply.hidden;
                            window.onUpdateStatus('Ready', true);
                            const updateReady = !apply.hidden;
                            apply.click();
                            window.onUpdateStatus('Unavailable', false);
                            const updateHidden = apply.hidden;
                            window.onState({ ...fixture, panel: 'downloads' });
                            window.onDownloads([
                                { id: 'running', name: 'one', path: 'one', status: 'running' },
                                { id: 'done', name: 'two', path: 'two', status: 'completed' }
                            ]);
                            const actions = id => [...document.querySelectorAll('[data-download-id=' + id + '] button:not([hidden])')].map(x => x.textContent);
                            const strip = document.getElementById("tablist");
                            const buttonsFit = ["wmin", "wmax", "wclose", "plus"].every(id => {
                                const r = document.getElementById(id).getBoundingClientRect();
                                return r.width > 0 && r.left >= 0 && r.right <= innerWidth + 1;
                            });
                            const active = strip.querySelector(".tab.on").getBoundingClientRect();
                            const lane = strip.getBoundingClientRect();
                            const activeVisible = active.left >= lane.left - 1 && active.right <= lane.right + 1;
                            return initiallyHidden && updateReady && updateHidden && buttonsFit && activeVisible && strip.scrollWidth > strip.clientWidth
                                && color && settings && !document.getElementById('devbtn').hidden
                                && !document.getElementById('p-downloads').hidden
                                && actions('running').includes('Отменить')
                                && !actions('running').includes('Удалить из списка')
                                && actions('done').includes('Удалить из списка');
                        })()
                    "#, move |value| { let _ = result.send_event(Some((value == "true", false))); }).unwrap();
                }
                Event::UserEvent(Some((result, button))) => {
                    passed |= result;
                    clicked |= button;
                    if passed && clicked { *flow = ControlFlow::Exit; }
                }
                _ => {}
            }
            if Instant::now() >= deadline { *flow = ControlFlow::Exit; }
        });
        assert!(passed && clicked, "settings and download controls must render");
    }

    #[test]
    #[ignore = "Requires the native WebView2 runtime"]
    fn shazam_consent_is_a_separate_child_of_the_popup() {
        use tao::platform::{run_return::EventLoopExtRunReturn, windows::EventLoopBuilderExtWindows};
        let mut events = EventLoopBuilder::<String>::with_user_event().with_any_thread(true).build();
        let window = WindowBuilder::new().with_visible(false).build(&events).unwrap();
        let profile = tempfile::tempdir().unwrap();
        let mut context = WebContext::new(Some(profile.path().join("profile")));
        let args = browser_args(false);
        let page = WebViewBuilder::with_web_context(&mut context)
            .with_browser_extensions_enabled(true)
            .with_additional_browser_args(&args)
            .with_html("<!doctype html><title>Popup fixture</title>")
            .build_as_child(&window).unwrap();
        let consent = build_audio_consent(&mut context, &window, &args, events.create_proxy());
        layout_extension_popup(&window, &page, Some(&consent), true);

        let proxy = events.create_proxy();
        let start = Instant::now();
        let mut evaluated = false;
        let mut ready = false;
        let mut blocked = true;
        events.run_return(|event, _, flow| {
            *flow = ControlFlow::WaitUntil(start + Duration::from_millis(if evaluated { 1200 } else { 500 }));
            if !evaluated && start.elapsed() >= Duration::from_millis(500) {
                evaluated = true;
                let proxy = proxy.clone();
                consent.evaluate_script_with_callback(
                    "Boolean(document.getElementById('approve')?.click() === undefined && document.getElementById('deny') && window.ipc)",
                    move |result| { let _ = proxy.send_event(result); },
                ).unwrap();
            }
            if let Event::UserEvent(message) = event {
                ready |= message == "true";
                blocked &= message != "approve-audio";
            }
            if start.elapsed() >= Duration::from_millis(1200) { *flow = ControlFlow::Exit; }
        });
        assert!(ready && blocked, "consent controls must load and reject synthetic approval");
    }

    #[test]
    #[ignore = "Requires the native WebView2 runtime"]
    fn shazam_library_label_and_arrow_follow_popup_theme() {
        use tao::platform::{run_return::EventLoopExtRunReturn, windows::EventLoopBuilderExtWindows};
        let mut events = EventLoopBuilder::<u8>::with_user_event().with_any_thread(true).build();
        let window = WindowBuilder::new().with_visible(false).build(&events).unwrap();
        let profile = tempfile::tempdir().unwrap();
        let mut context = WebContext::new(Some(profile.path().join("profile")));
        let loaded = events.create_proxy();
        let theme = include_str!("shazam-theme.js").replace(
            "if(location.protocol!=='chrome-extension:'||location.hostname!=='mmioliijnhnoblpgimnlajmefafdfilb')return;",
            "",
        );
        let view = WebViewBuilder::with_web_context(&mut context)
            .with_initialization_script("window.__lumenPopupTheme = 'dark';")
            .with_initialization_script(&theme)
            .with_on_page_load_handler(move |event, _| {
                if matches!(event, PageLoadEvent::Finished) { let _ = loaded.send_event(0); }
            })
            .with_html(r#"<!doctype html><style>.mfXuTaLWnnw65XOyBQDy{color:#000}</style>
                <div class="S4Qz81BGxxpjuGEo02kM">
                    <svg id="chevron"><g fill="black"><path d="M0 0L10 10"/></g></svg>
                    <p class="mfXuTaLWnnw65XOyBQDy">Песни</p>
                </div>
                <div class="RLHtwz_RlwPoJIR5A33T" style="max-height:250px;overflow-y:scroll">
                    <div class="rhysztLkYlwtcLuG7H89" style="height:320px">
                        <a href="/" class="RBMeVUhHAc8pBWUx74I7">Трек</a>
                    </div>
                    <div class="ZtXQ5aYtGTcCScAnlJIt"><p class="FsY6eItFAw34Un8UStdk">Удалить все</p></div>
                </div>"#)
            .build_as_child(&window).unwrap();
        let proxy = events.create_proxy();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut passed = false;
        events.run_return(|event, _, flow| {
            *flow = ControlFlow::WaitUntil(deadline);
            match event {
                Event::UserEvent(0) => {
                    let proxy = proxy.clone();
                    view.evaluate_script_with_callback(r#"
                        (() => {
                            const root = document.documentElement;
                            const label = document.querySelector('p');
                            const list = document.querySelector('.RLHtwz_RlwPoJIR5A33T');
                            const track = document.querySelector('.RBMeVUhHAc8pBWUx74I7');
                            const arrow = document.querySelector('#chevron path');
                            const uses = rgb => getComputedStyle(label).color === rgb
                                && getComputedStyle(arrow).fill === rgb;
                            track.focus();
                            const row = track.parentElement;
                            const expanded = getComputedStyle(list).maxHeight === 'none'
                                && getComputedStyle(list).overflowY === 'visible' && list.clientHeight >= 320;
                            const footer = list.lastElementChild.getBoundingClientRect();
                            const footerFits = footer.height >= 36 && list.getBoundingClientRect().bottom - footer.bottom >= 10;
                            const dark = uses('rgb(230, 231, 234)')
                                && getComputedStyle(track).color === 'rgb(230, 231, 234)'
                                && getComputedStyle(row).backgroundColor === 'rgb(52, 54, 69)';
                            root.dataset.lumenTheme = 'light';
                            return footerFits && expanded && dark && uses('rgb(31, 35, 40)')
                                && getComputedStyle(row).backgroundColor === 'rgb(233, 237, 247)';
                        })()
                    "#, move |value| { let _ = proxy.send_event(if value == "true" {1} else {2}); }).unwrap();
                }
                Event::UserEvent(result) => { passed = result == 1; *flow = ControlFlow::Exit; }
                _ => {}
            }
            if Instant::now() >= deadline { *flow = ControlFlow::Exit; }
        });
        assert!(passed, "Shazam history label and chevron must follow the theme");
    }

    #[test]
    fn extension_popup_fits_work_area() {
        assert_eq!(fit_popup_axis(0, 1920, 1760, 470), (1450, 470));
        assert_eq!(fit_popup_axis(-1920, 0, -180, 705), (-705, 705));
        assert_eq!(fit_popup_axis(40, 440, 100, 620), (40, 400));
        assert_eq!(fit_popup_axis(0, 1040, 900, 650), (390, 650));
    }

    #[test]
    fn dock_fits_small_windows_and_scales_with_dpi() {
        assert_eq!(dock_width(1280, 1.0, 400.0), 400);
        assert_eq!(dock_width(2560, 2.0, 400.0), 800);
        assert_eq!(dock_width(720, 1.5, 400.0), 360);
        for width in [480, 640, 800, 1280, 3840] {
            assert!(width - dock_width(width, 1.0, 620.0) >= 240);
        }
    }
    #[test]
    fn page_shortcuts_have_correct_modifiers() {
        assert!(map_key(0x5A,true,false,false).is_none()); // Preserve editing Undo.
        assert!(map_key(0x5A,true,true,false).is_none());
        assert!(matches!(map_key(0x25,false,false,true),Some(Shortcut::Back)));
        assert!(matches!(map_key(0x26,true,false,false),Some(Shortcut::ZoomIn)));
        assert!(matches!(map_key(0x28,true,false,false),Some(Shortcut::ZoomOut)));
        assert!(map_key(0x5A,false,false,false).is_none());
        assert!(matches!(map_key(0xBB, true, true, false), Some(Shortcut::ZoomIn)));
        assert!(matches!(map_key(0x30, true, false, false), Some(Shortcut::ZoomReset)));
        assert!(matches!(map_key(0x49, true, true, false), Some(Shortcut::DevTools)));
        assert!(matches!(map_key(0x4A, true, false, false), Some(Shortcut::Downloads)));
        assert!(map_key(0x4A, false, false, false).is_none());
        assert!(matches!(map_key(0x7A, false, false, false), Some(Shortcut::Fullscreen)));
        assert!(map_key(0x49, true, false, false).is_none());
        assert!(map_key(0xBB, false, false, false).is_none());
    }
}

#[derive(Default)]
struct Cli {
    incognito: bool,
    secondary: bool,
    restart_after: Option<u32>,
    restore_session: bool,
    url: Option<String>,
    pos: Option<(i32, i32)>,
    popup: Option<String>,
    size: Option<(f64, f64)>,
}

fn parse_pair<T: std::str::FromStr>(s: &str) -> Option<(T, T)> {
    let (a, b) = s.split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn parse_cli() -> Cli {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut c = Cli::default();
    let mut i = 0;
    while i < a.len() {
        match a[i].as_str() {
            "--" => { c.url=a.get(i+1).cloned(); break; },
            "--incognito" => c.incognito = true,
            "--secondary" => c.secondary = true,
            "--restore-session" => c.restore_session = true,
            "--restart-after" => {i+=1;c.restart_after=a.get(i).and_then(|v|v.parse().ok());},
            "--url" => {
                i += 1;
                c.url = a.get(i).cloned();
            }
            "--popup" => {
                i += 1;
                c.popup = a.get(i).cloned();
            }
            "--pos" => {
                i += 1;
                c.pos = a.get(i).and_then(|v| parse_pair::<i32>(v));
            }
            "--size" => {
                i += 1;
                c.size = a.get(i).and_then(|v| parse_pair::<f64>(v));
            }
            value if !value.starts_with('-') && c.url.is_none() => { c.url=Some(value.to_string()); },
            _ => {}
        }
        i += 1;
    }
    c
}

fn safe_url(u: &str) -> bool {
    url::Url::parse(u).is_ok_and(|p|match p.scheme() {
        "http"|"https"=>p.host_str().is_some() && p.username().is_empty() && p.password().is_none(),
        "chrome-extension"=>p.host_str().is_some_and(crate::security::extension_id)&&p.username().is_empty()&&p.password().is_none()&&p.port().is_none(),
        _=>false,
    })
}

/// Режим «всплывающего окна» расширения: маленькое окно без вкладок, закрывается при потере фокуса.
fn run_popup(url: String, size: (f64, f64), pos: Option<(i32, i32)>, args: String, profile: PathBuf, theme: String) {
    let extension = url::Url::parse(&url).ok().and_then(|u|u.host_str().map(str::to_owned))
        .filter(|id|crate::security::extension_origin(&url,id));
    let Some(extension) = extension else { return; };
    let keep_open = extension == "mmioliijnhnoblpgimnlajmefafdfilb";
    let event_loop = EventLoopBuilder::<String>::with_user_event().build();
    let capture_proxy = event_loop.create_proxy();
    let icon = tao::window::Icon::from_rgba(ICON_RGBA.to_vec(), 64, 64).ok();
    let mut wb = WindowBuilder::new()
        .with_window_icon(icon)
        .with_title("Lumen — расширение")
        .with_inner_size(tao::dpi::LogicalSize::new(size.0, size.1))
        .with_always_on_top(true)
        .with_visible(false);
    if let Some((x, y)) = pos {
        wb = wb.with_position(tao::dpi::PhysicalPosition::new(x, y));
    }
    let window = wb.build(&event_loop).expect("не удалось создать окно");
    fit_extension_popup(&window, pos);
    let mut ctx = WebContext::new(Some(profile));
    let webview = WebViewBuilder::with_web_context(&mut ctx)
        .with_initialization_script(include_str!("extension-theme.js"))
        .with_initialization_script(include_str!("shazam-capture.js"))
        .with_initialization_script(&format!("window.__lumenPopupTheme={};",serde_json::to_string(&theme).unwrap()))
        .with_initialization_script(include_str!("shazam-theme.js"))
        .with_navigation_handler(move |url| crate::security::extension_origin(&url, &extension))
        .with_new_window_req_handler(|_|false)
        .with_ipc_handler(move |req:wry::http::Request<String>| {
            if keep_open && req.body().len()<4096 && crate::security::extension_origin(&req.uri().to_string(),"mmioliijnhnoblpgimnlajmefafdfilb") {
                if serde_json::from_str::<serde_json::Value>(req.body()).ok().is_some_and(|value|
                    matches!(value["cmd"].as_str(), Some("capture_start" | "capture_stop" | "capture_request")))
                {
                    let _ = capture_proxy.send_event(req.body().clone());
                }
            }
        })
        .with_devtools(false)
        .with_incognito(false)
        .with_browser_extensions_enabled(true)
        .with_additional_browser_args(&args)
        .with_url(&url)
        .build_as_child(&window)
        .expect("не удалось создать WebView2");
    let consent = keep_open.then(|| build_audio_consent(&mut ctx, &window, &args, event_loop.create_proxy()));
    layout_extension_popup(&window, &webview, consent.as_ref(), keep_open);
    window.set_visible(true);
    let mut capture_stop=Arc::new(AtomicBool::new(false));
    let mut audio_rx=None;
    let mut pending_consent = keep_open.then(|| Instant::now() + Duration::from_secs(30));
    let mut approved_until = None;
    event_loop.run(move |event, _, flow| {
        use base64::Engine;
        if let Event::UserEvent(message)=&event {
            if matches!(message.as_str(), "approve-audio" | "deny-audio")
                && pending_consent.is_some_and(|deadline| Instant::now() < deadline)
            {
                pending_consent = None;
                layout_extension_popup(&window, &webview, consent.as_ref(), false);
                let _ = webview.focus();
                if message == "approve-audio" {
                    approved_until = Some(Instant::now() + Duration::from_secs(5));
                    use windows_core::HSTRING;
                    let params = HSTRING::from(r#"{"expression":"window.__lumenApproveAudio()","userGesture":true}"#);
                    let handler = webview2_com::CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
                    let _ = unsafe {
                        webview.controller().CoreWebView2().and_then(|core| {
                            core.CallDevToolsProtocolMethod(windows_core::w!("Runtime.evaluate"), &params, &handler)
                        })
                    };
                } else {
                    let _ = webview.evaluate_script("window.__lumenAudioEnd()");
                }
            }
            if let Ok(value)=serde_json::from_str::<serde_json::Value>(message) {
                match value["cmd"].as_str() {
                    Some("capture_start") if audio_rx.is_none()
                        && approved_until.take().is_some_and(|deadline| Instant::now() < deadline) =>
                    {
                        capture_stop = Arc::new(AtomicBool::new(false));
                        let (tx, rx) = mpsc::sync_channel(32);
                        audio_rx = Some(rx);
                        capture::start(tx, capture_stop.clone());
                    }
                    Some("capture_request") if audio_rx.is_none() && pending_consent.is_none() && consent.is_some() => {
                        pending_consent = Some(Instant::now() + Duration::from_secs(30));
                        layout_extension_popup(&window, &webview, consent.as_ref(), true);
                    }
                    Some("capture_stop") => {
                        approved_until = None;
                        capture_stop.store(true, Ordering::Relaxed);
                        audio_rx = None;
                        pending_consent = None;
                        layout_extension_popup(&window, &webview, consent.as_ref(), false);
                    }
                    _=>{}
                }
            }
        }
        if pending_consent.is_some_and(|deadline| Instant::now() >= deadline) {
            pending_consent = None;
            layout_extension_popup(&window, &webview, consent.as_ref(), false);
            let _ = webview.evaluate_script("window.__lumenAudioEnd()");
        }
        if approved_until.is_some_and(|deadline| Instant::now() >= deadline) {
            approved_until = None;
            let _ = webview.evaluate_script(r#"window.__lumenAudioError("Shazam не начал распознавание. Нажмите на логотип, чтобы повторить.")"#);
        }
        *flow = if audio_rx.is_some() {
            ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(20))
        } else {
            pending_consent.or(approved_until).map_or(ControlFlow::Wait, ControlFlow::WaitUntil)
        };
        let mut ended=false;
        if let Some(rx)=&audio_rx { for packet in rx.try_iter().take(16) {
            match packet {
                capture::Packet::Audio(rate,samples)=>{
                    let bytes:Vec<u8>=samples.iter().flat_map(|x|x.to_le_bytes()).collect();
                    let b64=base64::engine::general_purpose::STANDARD.encode(bytes);
                    let _=webview.evaluate_script(&format!("window.__lumenAudio({rate},'{b64}')"));
                },
                capture::Packet::Error(error)=>{let _=webview.evaluate_script(&format!("window.__lumenAudioError({})",serde_json::to_string(&error).unwrap()));}
                capture::Packet::End=>{ended=true;let _=webview.evaluate_script("window.__lumenAudioEnd()");}
            }
        }}
        if ended {audio_rx=None;}
        let _ = &webview;
        if let Event::WindowEvent { event: we, .. } = event {
            match we {
                WindowEvent::CloseRequested => {capture_stop.store(true,Ordering::Relaxed);*flow = ControlFlow::Exit;},
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    layout_extension_popup(&window, &webview, consent.as_ref(), pending_consent.is_some());
                }
                _ => {}
            }
        }
    });
}

fn fit_popup_axis(start: i32, end: i32, position: i32, size: i32) -> (i32, i32) {
    let available = end.saturating_sub(start).max(1);
    let size = size.clamp(1, available);
    (position.clamp(start, start.saturating_add(available - size)), size)
}

fn fit_extension_popup(window: &Window, requested: Option<(i32, i32)>) {
    use windows::Win32::{
        Foundation::{HWND, POINT},
        Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST},
        UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER},
    };
    let Ok(outer) = window.outer_position() else { return; };
    let (x, y) = requested.unwrap_or((outer.x, outer.y));
    let hwnd = HWND(window.hwnd() as _);
    let monitor = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() { return; }
    let area = info.rcWork;
    let size = window.outer_size();
    let (x, width) = fit_popup_axis(area.left, area.right, x, size.width as i32);
    let (y, height) = fit_popup_axis(area.top, area.bottom, y, size.height as i32);
    let _ = unsafe { SetWindowPos(hwnd, None, x, y, width, height, SWP_NOACTIVATE | SWP_NOZORDER) };
}

fn layout_extension_popup(window: &Window, page: &WebView, consent: Option<&WebView>, asking: bool) {
    let size = window.inner_size();
    let strip = if asking { ((108.0 * window.scale_factor()) as u32).min(size.height / 2) } else { 0 };
    let bounds = |y: u32, height: u32| Rect {
        position: PhysicalPosition::new(0, y as i32).into(),
        size: PhysicalSize::new(size.width.max(1), height.max(1)).into(),
    };
    let _ = page.set_bounds(bounds(strip, size.height.saturating_sub(strip)));
    if let Some(consent) = consent {
        let _ = consent.set_bounds(bounds(0, strip));
        let _ = consent.set_visible(asking);
    }
}

fn build_audio_consent(
    context: &mut WebContext, window: &Window, args: &str, proxy: EventLoopProxy<String>,
) -> WebView {
    const HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/audio-consent.html"));
    WebViewBuilder::with_web_context(context)
        .with_devtools(false)
        .with_browser_extensions_enabled(true)
        .with_additional_browser_args(args)
        .with_visible(false)
        .with_new_window_req_handler(|_| false)
        .with_navigation_handler(|url| crate::security::internal_document(&url, HTML))
        .with_ipc_handler(move |request| {
            if crate::security::internal_document(&request.uri().to_string(), HTML)
                && matches!(request.body().as_str(), "approve-audio" | "deny-audio")
            {
                let _ = proxy.send_event(request.body().clone());
            }
        })
        .with_html(HTML)
        .build_as_child(window)
        .expect("не удалось создать панель доступа к звуку")
}

fn main() {
    if crate::updater::helper_mode() { return; }
    if sys::cleanup_mode() { return; }
    if matches!(std::env::args().nth(1).as_deref(),Some("--register-browser"|"--default-browser")) {
        let settings=std::env::args().nth(1).as_deref()==Some("--default-browser");
        let result=std::env::current_exe().and_then(|exe|if settings{default_browser::open_settings(&exe)}else{default_browser::register(&exe)});
        if let Err(error)=result {eprintln!("Lumen registration: {error}");std::process::exit(1);}
        return;
    }
    let cli = parse_cli();
    if let Some(pid)=cli.restart_after {
        use windows::Win32::{Foundation::{CloseHandle,WAIT_OBJECT_0},System::Threading::{OpenProcess,WaitForSingleObject,PROCESS_ACCESS_RIGHTS}};
        unsafe {
            if let Ok(handle)=OpenProcess(PROCESS_ACCESS_RIGHTS(0x100000),false,pid) {
                let result=WaitForSingleObject(handle,30000);
                let _=CloseHandle(handle);
                if result!=WAIT_OBJECT_0 {return;}
            }
        }
    }
    let incognito = cli.incognito;
    let secondary = cli.secondary;

    // профиль и настройки: %LOCALAPPDATA%\Lumen
    let dir = std::env::var_os("LUMEN_DATA_DIR").map(PathBuf::from)
        .unwrap_or_else(|| dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Lumen"));
    let filters_dir = dir.join("filters");
    let lists_dir = filters_dir.join("lists");
    let ext_dir = dir.join("extensions");
    let _ = fs::create_dir_all(&lists_dir);
    let _ = fs::create_dir_all(&ext_dir);

    let settings_path = dir.join("settings.json");
    let stored_settings = crate::security::read_profile_text(&settings_path,1048576).ok();
    let settings: Settings = stored_settings.as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    if !incognito {
        if let Ok(data) = serde_json::to_vec_pretty(&settings) {
            if stored_settings.as_deref().map(str::as_bytes) != Some(data.as_slice()) {
                let _ = crate::security::atomic_write(&settings_path,&data);
            }
        }
    }
    let args = browser_args(settings.eco);
    std::env::remove_var("WEBVIEW2_PIPE_FOR_SCRIPT_DEBUGGER");
    std::env::remove_var("WEBVIEW2_WAIT_FOR_SCRIPT_DEBUGGER");
    // WebView2 can ignore the debugging switch in per-controller options.
    // Set it before creating any environment; only this process and its children
    // inherit it, never the user's global Windows environment.
    std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", &args);

    if let Some(url) = cli.popup.clone() {
        if !incognito && url.starts_with("chrome-extension://") && safe_url(&url) {
            run_popup(url, cli.size.unwrap_or((470.0, 620.0)), cli.pos, args, dir.join("profile"), settings.theme.clone());
        }
        return;
    }
    let profile_dir=if incognito {std::env::temp_dir().join(format!("Lumen-incognito-{}",std::process::id()))}else{dir.join("profile")};
    if incognito {
        fs::create_dir(&profile_dir).expect("не удалось создать отдельный приватный профиль");
        if let Err(error) = sys::schedule_cleanup(&profile_dir) {
            let _ = fs::remove_dir(&profile_dir);
            panic!("не удалось запустить очистку приватного профиля: {error}");
        }
    }
    let links_path=dir.join("links.json");
    let session_path=dir.join("session.json");
    let ext_map_path=dir.join("ext_map.json");
    let history_path = dir.join("history.jsonl");
    let mut history: Vec<HistEntry> = if incognito {
        Vec::new()
    } else {
        crate::security::read_profile_text(&history_path,8388608)
            .map(|s| s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
            .unwrap_or_default()
    };
    if !incognito && !secondary {
        let mut changed = false;
        for entry in &mut history { changed |= crate::security::redact_persisted_url(&mut entry.u); }
        if changed {
            let cleaned: Vec<_> = history.iter().filter_map(|entry| serde_json::to_string(entry).ok()).collect();
            let _ = crate::security::atomic_write(&history_path, format!("{}\n", cleaned.join("\n")).as_bytes());
        }
    }
    if history.len() > HISTORY_MAX {
        let cut = history.len() - HISTORY_MAX;
        history.drain(..cut);
    }

    let links: Vec<Link> = crate::security::read_profile_text(&links_path,786432)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut session: Session = if incognito || secondary {
        Session::default()
    } else {
        crate::security::read_profile_text(&session_path,2097152)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    };
    if !incognito && !secondary {
        let mut changed = false;
        for entry in &mut session.tabs { changed |= crate::security::redact_persisted_url(&mut entry.u); }
        if changed {
            if let Ok(data) = serde_json::to_vec(&session) {
                let _ = crate::security::atomic_write(&session_path, &data);
            }
        }
    }
    let ext_map: HashMap<String, String> = crate::security::read_profile_text(&ext_map_path,524288)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let restore = !incognito && !secondary && (settings.startup == "restore" || cli.restore_session) && !session.tabs.is_empty();

    // своя фотография для главного экрана
    let bg_path = dir.join("background.txt");
    let bg_data = crate::security::read_profile_text(&bg_path,4000000)
        .ok()
        .filter(|d| crate::security::image_data(d));

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let icon = tao::window::Icon::from_rgba(ICON_RGBA.to_vec(), 64, 64).ok();
    let mut wb = WindowBuilder::new()
        .with_visible(false)
        .with_window_icon(icon)
        .with_decorations(false) // вкладки и кнопки окна рисует сам браузер
        .with_title(format!("Lumen {}{}",env!("CARGO_PKG_VERSION"),if incognito {" — Инкогнито"} else {""}))
        .with_inner_size(tao::dpi::LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(480.0, 320.0));
    if let Some((x, y)) = cli.pos {
        wb = wb.with_position(tao::dpi::PhysicalPosition::new(x, y));
    }
    let window = wb.build(&event_loop).expect("не удалось создать окно");
    if let Some(monitor) = window.current_monitor() {
        let scale = window.scale_factor();
        let screen = monitor.size();
        window.set_inner_size(tao::dpi::LogicalSize::new(
            (screen.width as f64 / scale * 0.92).min(1280.0).max(480.0),
            (screen.height as f64 / scale * 0.86).min(800.0).max(320.0),
        ));
    }
    style_browser_frame(&window);
    window.set_background_color(Some(window_bg(&window, &settings.theme, incognito)));

    let mut ctx = WebContext::new(Some(profile_dir.clone()));

    let p_ui = proxy.clone();
    let p_ready = proxy.clone();
    let ui = WebViewBuilder::with_web_context(&mut ctx)
        .with_devtools(false)
        .with_incognito(incognito)
        .with_browser_extensions_enabled(!incognito) // у всех WebView одного профиля параметры должны совпадать
        .with_additional_browser_args(&args) // у всех WebView одного профиля аргументы должны совпадать
        .with_html(UI_HTML)
        .with_navigation_handler(|uri| crate::security::internal_document(&uri,UI_HTML))
        .with_new_window_req_handler(|_| false)
        .with_bounds(ui_rect(&window))
        .with_on_page_load_handler(move |ev: PageLoadEvent, _: String| {
            if matches!(ev, PageLoadEvent::Finished) {
                let _ = p_ready.send_event(UserEvent::Ui(r#"{"cmd":"ready"}"#.to_string()));
            }
        })
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            if req.body().len()<=crate::security::MAX_IPC && crate::security::internal_document(&req.uri().to_string(),UI_HTML) {
                let _ = p_ui.send_event(UserEvent::Ui(req.body().clone()));
            }
        })
        .build_as_child(&window)
        .expect("не удалось создать панель интерфейса");

    // Use the same native shortcuts in the toolbar and in page WebViews.
    unsafe {
        if let Ok(core)=ui.controller().CoreWebView2() {
            if let Ok(settings)=core.Settings() {let _=settings.SetIsZoomControlEnabled(false);}
        }
    }
    // Handling here prevents Ctrl+Shift+T falling through to UI's Ctrl+T.
    let p_toolbar = proxy.clone();
    let _ = hook::install_shortcuts(&ui, move |vk, ctrl, shift, alt| {
        if vk == 0x1B { return false; } // panel Escape is handled by UI
        if let Some(sc) = map_key(vk, ctrl, shift, alt) {
            let _ = p_toolbar.send_event(UserEvent::Shortcut(sc));
            true
        } else { false }
    });

    let download_proxy=proxy.clone();
    let downloads=crate::downloads::Manager::new(&dir,incognito,move||download_proxy.send_event(UserEvent::DownloadPoll).is_ok());
    let mut app = App {
        main_closed: false,
        newtab_token: crate::security::newtab_token(),
        popup_tabs: HashMap::new(),
        cosmetic_deadlines: HashMap::new(),
        download_hosts: Vec::new(),
        favicons:Rc::new(RefCell::new(favicon::Cache::default())),
        sent_icons:RefCell::new(std::collections::HashSet::new()),
        downloads,
        frame_dark: matches!(window.theme(), tao::window::Theme::Dark),
        fullscreen_keys: Arc::new(AtomicBool::new(false)),
        fullscreen: false,
        zoom: HashMap::new(),
        window,
        ui,
        ctx,
        tabs: Vec::new(),
        detached: HashMap::new(),
        detached_spare: None,
        ui_bounds: None,
        ui_favorites_clip: Cell::new(None),
        ui_visible: None,
        active: 0,
        next_id: 1,
        panel: String::new(),
        adblock: Arc::new(AtomicBool::new(settings.adblock)),
        settings,
        settings_path,
        history,
        history_path,
        bg_data,
        newtab_cache: RefCell::default(),
        bg_path,
        incognito,
        blocker: Rc::new(Blocker::load(&filters_dir)),
        proxy,
        filters: Rc::new(RefCell::new(None)),
        filters_status: "Загрузка фильтров…".into(),
        lists_dir,
        user_filters_dir: filters_dir,
        ext_dir,
        stats: Arc::new(hook::Stats::new()),
        links,
        links_path,
        closed: Vec::new(),
        session_path,
        last_session: RefCell::new(String::new()),
        session_deadline: std::cell::Cell::new(None),
        last_ui_state: RefCell::new(String::new()),
        ext_map,
        ext_map_path,
        secondary,
        args,
        state_deadline: Cell::new(None),
    };
    let first_url = cli.url.filter(|u| safe_url(u));
    if let Some(u) = first_url {
        app.new_tab(Some(u));
    } else if restore {
        app.restore_session(session);
    } else {
        app.new_tab(None);
    }
    app.spawn_filters(false);
    if std::env::var_os("LUMEN_TEST_HIDDEN").is_none() {
        // AnimateWindow can show an empty frame while the child WebViews are starting.
        app.window.set_visible(true);
        app.ui_visible = None;
        app.layout();
        app.focus_active_page();
    }

    let update_proxy = app.proxy.clone();
    let updates = crate::updater::start_with_status(!incognito && !secondary, move |status| {
        let _ = update_proxy.send_event(UserEvent::UpdateStatus(status));
    });
    let mut update_status = "";
    let mut toolbar_ready = false;
    let mut background_deadline = None;
    event_loop.run(move |event, target, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::CheckUpdate) => {
                if update_status != crate::updater::READY_STATUS {
                    update_status = updates.request();
                }
                app.push_update_status(update_status);
            }
            Event::UserEvent(UserEvent::UpdateStatus(status)) => {
                update_status = status;
                app.push_update_status(status);
            }
            Event::UserEvent(UserEvent::ApplyUpdate) => {
                if app.downloads.borrow().active_count() > 0 {
                    app.toast("Дождитесь завершения загрузок перед перезапуском");
                } else if !app.detached.is_empty() {
                    app.toast("Верните вкладки в основное окно перед перезапуском");
                } else if app.try_save_session().is_err() {
                    app.toast("Не удалось сохранить вкладки. Обновление отложено.");
                } else if let Ok(update) = updates.try_recv() {
                    match update.apply_and_restart() {
                        Ok(()) => *flow = ControlFlow::Exit,
                        Err(_) => {
                            let _ = app.proxy.send_event(UserEvent::UpdateStatus(
                                "Не удалось подготовить установку. Перезапустите браузер и проверьте обновления.",
                            ));
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::Ui(message)) if matches!(serde_json::from_str::<Cmd>(&message), Ok(Cmd::Ready)) => {
                // The document is fixed for the lifetime of this toolbar.
                // Both the load callback and the toolbar script report readiness.
                if !toolbar_ready {
                    toolbar_ready = true;
                    app.handle(UserEvent::Ui(message));
                    app.push_update_status(update_status);
                    app.push_favorites();
                }
            }
            Event::UserEvent(UserEvent::Detach(id, x, y)) => app.detach(id, x, y, target),
            Event::UserEvent(UserEvent::DetachedUi(id, message)) => {
                app.detached_command(id, &message);
                if matches!(serde_json::from_str::<Cmd>(&message), Ok(Cmd::Ready | Cmd::OpenPanel { .. })) {
                    app.push_update_status(update_status);
                }
            }
            Event::WindowEvent { window_id, event, .. } if window_id != app.window.id() => {
                if let Some((&key, _)) = app.detached.iter().find(|(_, shell)| shell.window.id() == window_id) {
                    match event {
                        WindowEvent::CloseRequested => app.close_detached_window(key),
                        WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                            app.layout_detached(key, false);
                            app.request_state();
                        }
                        _ => {}
                    }
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. }
                | Event::UserEvent(UserEvent::Quit) if !app.main_closed && !app.detached.is_empty() => {
                app.close_main_window();
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } if app.main_closed => {}
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } | Event::UserEvent(UserEvent::Quit) => {
                app.save_session();
                if !app.main_closed { animate_window(&app.window, false); }
                if app.downloads.borrow().active_count()==0 {
                    if let Ok(update)=updates.try_recv(){let _=update.apply();}
                }
                if app.incognito {
                    app.tabs.clear();
                }
                *flow = ControlFlow::Exit
            }
            Event::WindowEvent { event: WindowEvent::Resized(_), .. }
            | Event::WindowEvent { event: WindowEvent::ScaleFactorChanged { .. }, .. } => {
                app.layout();
                app.request_state();
            }
            Event::UserEvent(ev) => app.handle(ev),
            _ => {}
        }
        if matches!(*flow, ControlFlow::Wait) {
            let now = Instant::now();
            if background_deadline.is_some_and(|deadline| now >= deadline) {
                app.manage_background_views();
                background_deadline = None;
            }
            if app.has_background_views() {
                background_deadline.get_or_insert(now + TICK_EVERY);
            } else {
                background_deadline = None;
            }
            if app.state_deadline.get().is_some_and(|deadline| now >= deadline) {
                app.state_deadline.set(None);
                app.push_state();
            }
            let cosmetic = app.run_cosmetic_deadlines();
            if app.session_deadline.get().is_some_and(|deadline| Instant::now() >= deadline) {
                app.save_session();
            }
            if app.detached_spare.as_ref().is_some_and(|(expires, _)| Instant::now() >= *expires) {
                app.detached_spare.take();
            }
            let spare_expiry = app.detached_spare.as_ref().map(|(expires, _)| *expires);
            if let Some(deadline) = cosmetic.into_iter()
                .chain(app.session_deadline.get()).chain(spare_expiry)
                .chain(app.state_deadline.get()).chain(background_deadline).min()
            {
                *flow = ControlFlow::WaitUntil(deadline);
            }
        }
    });
}
