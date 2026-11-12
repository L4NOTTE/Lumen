
mod blocker;
mod ext;
mod crx;
mod filters;
mod hook;
mod capture;
mod sys;
mod transfer;
mod default_browser;
mod favicon;

use std::{
    cell::RefCell,
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
const HARVEST_JS: &str = "(()=>{const c=new Set(),i=new Set();\
    document.querySelectorAll('[class]').forEach(e=>e.classList.forEach(x=>c.add(x)));\
    document.querySelectorAll('[id]').forEach(e=>i.add(e.id));\
    return JSON.stringify({c:[...c],i:[...i]});})()";

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
fn accent_pair(k: &str) -> (&str, &str) {
    if crate::security::hex_color(k) { return (k, k); }
    match k {
        "blue" => ("#2563eb", "#60a5fa"),
        "green" => ("#16a34a", "#4ade80"),
        "orange" => ("#ea580c", "#fb923c"),
        "pink" => ("#db2777", "#f472b6"),
        "red" => ("#dc2626", "#f87171"),
        _ => ("#4f46e5", "#8b8dff"),
    }
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
    Pulse,
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
    Tick,
    FiltersBuilt(Arc<Filters>),
    FiltersStatus(String),
    Cosmetic(u32),
    Harvest(u32, String),
    Extensions(String),
    ExtInstalled(Result<PathBuf, String>),
    ExtChanged(String),
    Shortcut(Shortcut),
    Link(LinkCmd),
    ExtAdded(PathBuf, Result<String, String>),
    Flush,
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
    SetLook { theme: String, accent: String, font: String, font_size: String },
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
    title: String,
    url: String,
    loading: bool,
    last_active: Instant,
    suspended: bool,
    /// true — для вкладки включён режим низкого потребления памяти
    low: bool,
}

struct App {
    newtab_token: String,
    popup_tabs: HashMap<u32,(u32,bool)>,
    download_hosts: Vec<WebView>,
    favicons: favicon::SharedCache,
    sent_icons: RefCell<std::collections::HashSet<String>>,
    downloads: crate::downloads::Shared,
    frame_dark: bool,
    fullscreen_keys: Arc<AtomicBool>,
    transferring: HashMap<u32, Instant>,
    fullscreen: bool,
    zoom: HashMap<u32, f64>,
    window: Window,
    ui: WebView,
    ctx: WebContext,
    tabs: Vec<Tab>,
    active: u32,
    next_id: u32,
    /// "" | "history" | "settings" — открытая панель поверх страницы
    panel: String,
    settings: Settings,
    settings_path: PathBuf,
    history: Vec<HistEntry>,
    history_path: PathBuf,
    bg_data: Option<String>,
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
    last_ui_state: RefCell<String>,
    ext_map: HashMap<String, String>,
    ext_map_path: PathBuf,
    secondary: bool,
    args: String,
    flush_tx: mpsc::Sender<()>,
    flush_pending: Arc<AtomicBool>,
}

fn toolbar_px(w: &Window) -> u32 {
    (TOOLBAR_H * w.scale_factor()).round() as u32
}

fn animate_window(w: &Window, show: bool) {
    use windows::Win32::{Foundation::{HWND, BOOL}, UI::WindowsAndMessaging::*};
    unsafe {
        // Keep tao's VISIBLE flag in sync: AnimateWindow alone bypasses its state.
        if show { w.set_visible(true); }
        let mut enabled = BOOL::default();
        // Respect Windows' accessibility preference for client animations.
        if SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut enabled as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).is_ok() && enabled.as_bool() {
            if show { let _ = ShowWindow(HWND(w.hwnd() as *mut _), SW_HIDE); }
            if AnimateWindow(HWND(w.hwnd() as *mut _), 120, if show { AW_BLEND | AW_ACTIVATE } else { AW_BLEND | AW_HIDE }).is_err() && show { w.set_visible(true); }
        } else if show { w.set_visible(true); }
    }
}

/// Keep the native resize hit area uncovered by child WebViews.
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
    fn resolve(&self, text: &str) -> String {
        let t = text.trim();
        if safe_url(t) {
            return t.to_string();
        }
        if !t.contains(' ') && (t.contains('.') || t.starts_with("localhost")) {
            return format!("https://{t}");
        }
        format!("{}{}", search_url(&self.settings.engine), urlencoding::encode(t))
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
        let _ = self
            .ui
            .evaluate_script(&format!("window.onHistory({})", serde_json::Value::Array(list)));
    }

    // ----- внешний вид -----

    fn newtab_html(&self) -> String {
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
            serde_json::to_string(&self.links.iter().enumerate().filter(|(_,link)| crate::security::web_link(&link.u)).map(|(i,link)|serde_json::json!({"i":i,"t":link.t,"u":link.u})).collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into())
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

    /// Пересоздаёт вкладки со стартовой страницей, чтобы применились тема, шрифт и фон.
    fn refresh_newtabs(&mut self) {
        let ids: Vec<u32> = self
            .tabs
            .iter()
            .filter(|t| t.url.is_empty() && t.webview.is_some())
            .map(|t| t.id)
            .collect();
        for id in ids {
            let (wv,icon) = self.make_webview(id, None);
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                t.webview = Some(wv);
                t.favicon=icon;
                t.visible = None; t.bounds = None;
            }
        }
        self.layout();
    }

    // ----- вкладки -----

    fn make_webview(&mut self, id: u32, url: Option<String>) -> (WebView,favicon::Shared) {
        let icon=favicon::State::new(self.tabs.iter().find(|t|t.id==id).and_then(|t|t.favicon.get()));
        let key = zoom_key(url.as_deref().unwrap_or(""));
        let factor = key.and_then(|key| self.settings.site_zoom.get(&key).copied())
            .filter(|factor| factor.is_finite() && (0.25..=5.0).contains(factor)).unwrap_or(1.0);
        self.zoom.insert(id, factor);
        let rect = content_rect(&self.window);
        let incognito = self.incognito;
        let newtab = self.newtab_html();
        let newtab_token = self.newtab_token.clone();
        let navigation_html = newtab.clone();
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
            .with_visible(false)
            .with_focused(false)
            .with_initialization_script(include_str!("background-links.js"))
            .with_document_title_changed_handler(move |t: String| {
                let _ = p_title.send_event(UserEvent::Title(id, t));
            })
            .with_on_page_load_handler(move |ev: PageLoadEvent, _: String| {
                let started = matches!(ev, PageLoadEvent::Started);
                let _ = p_url.send_event(UserEvent::Loading(id, started));
            })
            .with_new_window_req_handler(move |u: String| {
                let middle=popup_gesture.take().is_some_and(|at|at.elapsed()<Duration::from_secs(5));
                let _ = p_win.send_event(UserEvent::PopupTab(id,u,!middle));
                false
            })
            .with_navigation_handler(move |u: String| {
                if !safe_url(&u) && !crate::security::internal_document(&u,&navigation_html) { return false; }
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
                !(adblock.load(Ordering::Relaxed) && blocker.is_blocked(&u))
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
        b = match url {
            Some(u) => b.with_url(u),
            None => b.with_html(newtab),
        };
        let wv = b.build_as_child(&self.window).expect("не удалось создать WebView2");
        hook::lock_down(&wv).expect("site privacy policy");
        let source_proxy = self.proxy.clone();
        let _ = hook::install_source_changed(&wv, move |url| {
            let _ = source_proxy.send_event(UserEvent::Url(id, url));
        });
        if let Some(url) = hook::source(&wv).filter(|url| safe_url(url)) {
            let _ = self.proxy.send_event(UserEvent::Url(id, url));
        }
        let _ = crate::downloads::install(&wv, &self.downloads);
        let committed=self.proxy.clone();let download=self.proxy.clone();
        let _=hook::install_popup_outcome(&wv,
            move||{let _=committed.send_event(UserEvent::PopupCommitted(id));},
            move||{let _=download.send_event(UserEvent::PopupDownload(id));});
        let icon_proxy=self.proxy.clone();
        let _=favicon::install(&wv,&icon,&self.favicons,move||{let _=icon_proxy.send_event(UserEvent::FaviconsChanged);});
        let _=hook::install_link_gestures(&wv,gesture);
        let _ = hook::install_blocker(&wv, self.filters.clone(), self.adblock.clone(), page, self.stats.clone(), self.blocker.clone());
        let zoom_proxy=self.proxy.clone();
        let _=hook::install_zoom(&wv,move |factor| {let _=zoom_proxy.send_event(UserEvent::ZoomChanged(id,factor));});
        let _ = hook::install_shortcuts(&wv, move |vk, ctrl, shift, alt| {
            if vk == 0x1B && !fullscreen_keys.load(Ordering::Relaxed) { return false; }
            match map_key(vk, ctrl, shift, alt) {
            Some(sc) => {
                let _ = p_key.send_event(UserEvent::Shortcut(sc));
                true
            }
            None => false,
        }});
        let p = self.proxy.clone();
        let _ = hook::install_fullscreen(&wv, move |full| {
            let _ = p.send_event(UserEvent::FullscreenChanged(id, full));
        });
        let _ = wv.zoom(*self.zoom.get(&id).unwrap_or(&1.0));
        (wv,icon)
    }

    fn new_tab(&mut self, url: Option<String>) {
        self.create_tab(url,true);
    }

    fn create_tab(&mut self, url: Option<String>, foreground:bool) {
        let url=url.filter(|u|safe_url(u));
        let id = self.next_id;
        self.next_id += 1;
        let (webview,favicon) = self.make_webview(id, url);
        self.tabs.push(Tab {
            favicon,
            visible: None, bounds: None,
            id,
            webview: Some(webview),
            title: "Новая вкладка".into(),
            url: String::new(),
            loading: false,
            last_active: Instant::now(),
            suspended: false,
            low: false,
        });
        if foreground || self.active==0 { self.activate(id); }
        else { self.layout(); self.push_state(); }
    }

    /// Делает вкладку активной (закрывая панели); если она «спала» — будит.
    fn activate(&mut self, id: u32) {
        if !self.tabs.iter().any(|t| t.id == id) { return; }
        if id != self.active && self.fullscreen { self.set_fullscreen(false); }
        self.active = id;
        self.panel.clear();
        let sleeping = self.tabs.iter().find(|t| t.id == id).map(|t| t.webview.is_none());
        if sleeping == Some(true) {
            let url = self.tabs.iter().find(|t| t.id == id).map(|t| t.url.clone()).unwrap_or_default();
            let (wv,icon) = self.make_webview(id, if url.is_empty() { None } else { Some(url) });
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                t.webview = Some(wv);
                t.favicon=icon;
                t.visible = None; t.bounds = None;
            }
        }
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            t.last_active = Instant::now();
        }
        self.layout();
        self.with_active(|wv| {
            let _ = wv.focus();
        });
        self.push_state();
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

    fn layout(&mut self) {
        let open = !self.panel.is_empty() && !self.fullscreen;
        let page_rect=if self.fullscreen {full_rect(&self.window)} else {content_rect(&self.window)};
        let _ = self.ui.set_visible(!self.fullscreen);
        // при открытой панели интерфейс занимает всё окно, а страница скрыта
        let _ = self.ui.set_bounds(if open { panel_rect(&self.window) } else { ui_rect(&self.window) });
        for t in self.tabs.iter_mut() {
            if let Some(wv) = &t.webview {
                let visible = t.id == self.active && !open;
                if visible {
                    if t.suspended {
                        hook::resume(wv);
                        t.suspended = false;
                    }
                    if !t.bounds.as_ref().is_some_and(|r| r.position == page_rect.position && r.size == page_rect.size) {
                        if wv.set_bounds(page_rect).is_ok() { t.bounds = Some(page_rect); }
                    }
                }
                if t.visible != Some(visible) && wv.set_visible(visible).is_ok() { t.visible = Some(visible); }
                // скрытые вкладки переводим в режим низкого потребления памяти
                if visible == t.low {
                    let level = if visible { MemoryUsageLevel::Normal } else { MemoryUsageLevel::Low };
                    let _ = wv.set_memory_usage_level(level);
                    t.low = !visible;
                }
            }
        }
    }

    fn push_state(&self) {
        let icons:HashMap<_,_>=self.tabs.iter().filter_map(|t|t.favicon.get().map(|i|(i.key.clone(),i))).collect();
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
        let state = serde_json::json!({
            "download_count": self.downloads.borrow().active_count(),
            "tabs": tabs,
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
        let serialized = state.to_string();
        let changed = *self.last_ui_state.borrow() != serialized;
        if changed {
            if self.ui.evaluate_script(&format!("window.onState({serialized})")).is_ok() {
                *self.last_ui_state.borrow_mut() = serialized;
            }
        }
        if changed { self.save_session(); }
    }

    fn push_downloads(&self) { let data=serde_json::to_string(self.downloads.borrow().records()).unwrap_or_else(|_|"[]".into());let _=self.ui.evaluate_script(&format!("window.onDownloads({data})")); }

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
        if !self.flush_pending.swap(true, Ordering::SeqCst) {
            let _ = self.flush_tx.send(());
        }
    }

    /// Отдельное маленькое окно для расширения (как всплывающее окно у кнопки расширения).
    fn open_popup(&self, url: &str) {
        let sf = self.window.scale_factor();
        let (w, h) = (if url.contains("mmioliijnhnoblpgimnlajmefafdfilb") { 470.0f64 } else { 420.0f64 }, 620.0f64);
        let pos = self.window.outer_position().ok().map(|p| {
            let ow = self.window.outer_size().width as i32;
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
    }

    fn save_session(&self) {
        if self.incognito || self.secondary {
            return;
        }
        let real: Vec<&Tab> = self.tabs.iter().filter(|t| t.url.starts_with("http")).collect();
        let session = Session {
            active: real.iter().position(|t| t.id == self.active).unwrap_or(0),
            tabs: real.iter().map(|t| SavedTab { u: crate::security::persisted_url(&t.url), t: t.title.clone() }).collect(),
        };
        if let Ok(j) = serde_json::to_string(&session) {
            if *self.last_session.borrow() != j {
                if crate::security::atomic_write(&self.session_path, j.as_bytes()).is_ok() {
                    *self.last_session.borrow_mut() = j;
                }
            }
        }
    }

    /// Восстанавливает вкладки прошлого сеанса: загружается только активная, остальные «спят».
    fn restore_session(&mut self, s: Session) {
        for t in s.tabs.iter().filter(|t|safe_url(&t.u)) {
            if t.u.starts_with("chrome-extension://mmioliijnhnoblpgimnlajmefafdfilb/") { continue; }
            let id = self.next_id;
            self.next_id += 1;
            self.tabs.push(Tab {
                favicon:favicon::State::new(None),
            visible: None, bounds: None,
                id,
                webview: None,
                title: t.t.clone(),
                url: t.u.clone(),
                loading: false,
                last_active: Instant::now(),
                suspended: false,
                low: false,
            });
        }
        if self.tabs.is_empty() { self.new_tab(None); return; }
        let idx = s.active.min(self.tabs.len().saturating_sub(1));
        let id = self.tabs[idx].id;
        self.activate(id);
    }

    fn toast(&self, msg: &str) {
        if let Ok(j) = serde_json::to_string(msg) {
            let _ = self.ui.evaluate_script(&format!("window.onToast&&window.onToast({j})"));
        }
    }

    fn save_links(&self) {
        if let Ok(j) = serde_json::to_string_pretty(&self.links) {
            let _ = crate::security::atomic_write(&self.links_path, j.as_bytes());
        }
    }

    fn add_link(&mut self, title: &str, url: &str) {
        if self.incognito {
            return;
        }
        let Some(u) = normalize_url(url) else {
            self.toast("Не удалось добавить: проверьте адрес ссылки");
            return;
        };
        let host = url::Url::parse(&u).ok().and_then(|p| p.host_str().map(|h| h.trim_start_matches("www.").to_string())).unwrap_or_default();
        let t: String = if title.trim().is_empty() { host } else { title.trim().chars().take(40).collect() };
        if let Some(l) = self.links.iter_mut().find(|l| l.u == u) {
            l.t = t;
        } else if self.links.len() >= 32 {
            self.toast("Достигнут предел: 32 быстрые ссылки");
            return;
        } else {
            self.links.push(Link { t, u });
        }
        self.save_links();
        self.refresh_newtabs();
        self.toast("Добавлено в быстрые ссылки");
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
            let _ = wv.evaluate_script_with_callback(HARVEST_JS, move |res| {
                let _ = p.send_event(UserEvent::Harvest(id, res));
            });
        }
    }

    fn apply_generic(&self, id: u32, res: &str) {
        let Some(f) = self.filters.borrow().clone() else { return };
        let Some(tab) = self.tabs.iter().find(|t| t.id == id) else { return };
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
        let old: Vec<(u32, String, bool)> = self
            .tabs
            .iter()
            .map(|t| (t.id, t.url.clone(), t.webview.is_some()))
            .collect();
        for t in self.tabs.iter_mut() {
            t.webview = None;
        }
        for (id, url, awake) in old {
            if !awake && id != self.active {
                continue;
            }
            let (wv,icon) = self.make_webview(id, if url.is_empty() { None } else { Some(url) });
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                t.webview = Some(wv);
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
                self.tabs.retain(|t| t.id != id);
                if self.tabs.is_empty() {
                    self.new_tab(None);
                } else if self.active == id {
                    let last = self.tabs.last().map(|t| t.id).unwrap_or(0);
                    self.activate(last);
                } else {
                    self.push_state();
                }
            }
            Cmd::Switch { id } => self.activate(id),
            Cmd::Go { text } => {
                let u = self.resolve(&text);
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
                    self.with_active(|wv| {
                        let _ = wv.load_html(&html);
                    });
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
                if matches!(name.as_str(), "history" | "settings" | "ext" | "downloads") {
                    self.panel = name.clone();
                    self.layout();
                    self.push_state();
                    match name.as_str() {
                        "downloads" => self.push_downloads(),
                        "history" => self.push_history(),
                        "ext" => self.refresh_ext(),
                        _ => {}
                    }
                }
            }
            Cmd::ClosePanel => self.close_panel(),
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
            Cmd::SetLook { theme, accent, font, font_size } => {
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
                Some((u, true)) => self.open_popup(&u),
                Some((u, false)) => self.new_tab(Some(u)),
                None => {
                    let _ = self.proxy.send_event(UserEvent::ExtChanged(
                        "У этого расширения нет окна. Переустановите его, чтобы кнопка «Открыть» заработала".into(),
                    ));
                }
            },
            Cmd::AddCurrentLink => {
                let cur = self
                    .tabs
                    .iter()
                    .find(|t| t.id == self.active)
                    .map(|t| (t.title.clone(), t.url.clone()));
                match cur {
                    Some((t, u)) if u.starts_with("http") => self.add_link(&t, &u),
                    _ => self.toast("Откройте сайт, который хотите добавить"),
                }
            }
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
                } else if let Some(from) = self.tabs.iter().position(|t| t.id == id) {
                    let tab = self.tabs.remove(from);
                    let to = index.min(self.tabs.len());
                    self.tabs.insert(to, tab);
                    self.push_state();
                    self.save_session();
                }
            }
            Cmd::DetachTab { id, x, y } => {
                if self.incognito { self.toast("Перенос вкладок недоступен в инкогнито"); return; }
                let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else { return; };
                let url = tab.webview.as_ref().and_then(hook::source)
                    .map(|url| if safe_url(&url) { url } else { String::new() })
                    .unwrap_or_else(|| tab.url.clone());
                if let Some(pid) = transfer::target(self.incognito, x, y) {
                    if !self.transferring.contains_key(&id) && transfer::send(pid, id, &url) {
                        self.transferring.insert(id, Instant::now());
                    }
                    return;
                }
                if self.tabs.len() > 1 {
                    if let Ok(exe) = std::env::current_exe() {
                        let mut c = std::process::Command::new(exe);
                        c.arg("--secondary");
                        if self.incognito {
                            c.arg("--incognito");
                        }
                        if !url.is_empty() {
                            c.args(["--url", &url]);
                        }
                        c.args(["--pos", &format!("{},{}", (x - 240).max(0), (y - 20).max(0))]);
                        if c.spawn().is_ok() {
                            self.tabs.retain(|t| t.id != id);
                            if self.active == id {
                                let last = self.tabs.last().map(|t| t.id).unwrap_or(0);
                                self.activate(last);
                            } else {
                                self.push_state();
                            }
                        }
                    }
                }
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
            UserEvent::FaviconsChanged => self.request_state(),
            UserEvent::DownloadPoll => {
                let changed = self.downloads.borrow_mut().poll();
                if self.downloads.borrow().active_count()==0 {self.download_hosts.clear();}
                if changed {
                    let notice=self.downloads.borrow_mut().take_notice();
                    if let Some(text)=notice { self.toast(&text); }
                    if self.panel=="downloads" { self.push_downloads(); }
                    self.request_state();
                }
            },
            UserEvent::ZoomChanged(id, factor) => {
                if factor.is_finite() && factor > 0.0 && self.tabs.iter().any(|t|t.id==id) {
                    if self.zoom.get(&id).is_some_and(|old|(old-factor).abs()<0.0001) { return; }
                    self.remember_zoom(id,factor);
                    if id==self.active {self.request_state();}
                }
            },
            UserEvent::Pulse => {
                if self.incognito { return; }
                if !self.window.is_minimized() && !self.fullscreen {
                    if let Ok(p) = self.window.inner_position() {
                        transfer::publish(self.incognito, p.x, p.y, self.window.inner_size().width, (40.0 * self.window.scale_factor()) as u32);
                    }
                } else { transfer::cleanup(); }
                transfer::receive(|u| { self.new_tab(if u.is_empty() { None } else { Some(u) }); self.window.set_focus(); });
                let ids: Vec<_> = self.transferring.keys().copied().collect();
                for id in ids {
                    if transfer::acknowledged(id) {
                        self.transferring.remove(&id);
                        if self.tabs.len() == 1 { let _ = self.proxy.send_event(UserEvent::Quit); }
                        else { self.cmd(Cmd::CloseTab { id }); }
                    } else if self.transferring[&id].elapsed() > Duration::from_secs(5) {
                        self.transferring.remove(&id);
                        transfer::cancel(id);
                        self.toast("Окно не ответило. Вкладка осталась здесь.");
                    }
                }
            }
            UserEvent::FullscreenChanged(id, full) => { if id == self.active { self.set_fullscreen(full); } },
            UserEvent::Ui(s) => match serde_json::from_str::<Cmd>(&s) {
                Ok(c) => self.cmd(c),
                Err(e) => log(|| format!("bad ui message: {e}")),
            },
            UserEvent::Title(id, t) => {
                if let Some(tab) = self.tabs.iter_mut().find(|x| x.id == id && x.webview.is_some()) {
                    tab.title = t.clone();
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
                if let Some(tab) = self.tabs.iter_mut().find(|x| x.id == id && x.webview.is_some()) {
                    if tab.url == u { return; }
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
                let mut finished = false;
                if let Some(tab) = self.tabs.iter_mut().find(|x| x.id == id && x.webview.is_some()) {
                    if tab.loading != on {
                        tab.loading = on;
                        finished = !on;
                    }
                }
                if finished {
                    self.record_history(id);
                    self.apply_cosmetic(id);
                    let p = self.proxy.clone();
                    std::thread::spawn(move || {
                        for ms in [2500u64, 5500] {
                            std::thread::sleep(Duration::from_millis(ms));
                            let _ = p.send_event(UserEvent::Cosmetic(id));
                        }
                    });
                }
                self.request_state();
            }
            UserEvent::Flush => {
                self.flush_pending.store(false, Ordering::SeqCst);
                self.push_state();
            }
            UserEvent::Quit => {}
            UserEvent::Navigate(id, u) => {
                if let Some(wv) = self.tabs.iter().find(|x| x.id == id).and_then(|t| t.webview.as_ref()) {
                    let _ = wv.load_url(&u);
                }
            }
            UserEvent::Search(id, q) => {
                let u = self.resolve(&q);
                if let Some(wv) = self.tabs.iter().find(|x| x.id == id).and_then(|t| t.webview.as_ref()) {
                    let _ = wv.load_url(&u);
                }
            }
            UserEvent::PopupTab(source,url,foreground) => {
                if !safe_url(&url){return;}
                let id=self.next_id;
                self.create_tab(Some(url),false);
                self.popup_tabs.insert(id,(source,foreground));
            },
            UserEvent::PopupCommitted(id)=>{
                if let Some((source,foreground))=self.popup_tabs.remove(&id){
                    if foreground&&self.active==source {self.activate(id);}
                }
            },
            UserEvent::PopupDownload(id)=>{
                if let Some((source,_))=self.popup_tabs.remove(&id){
                    if let Some(index)=self.tabs.iter().position(|tab|tab.id==id){
                        let mut tab=self.tabs.remove(index);
                        if let Some(wv)=tab.webview.take(){
                            let _=wv.set_visible(false);
                            self.download_hosts.push(wv);
                        }
                        self.zoom.remove(&id);
                        if self.active==id {
                            if let Some(next)=self.tabs.iter().find(|t|t.id==source).or_else(||self.tabs.first()).map(|t|t.id){self.activate(next);}
                            else {self.new_tab(None);}
                        }
                        self.layout();self.request_state();self.save_session();
                    }
                }
            },
            UserEvent::Tick => {
                let active = self.active;
                let mut changed = false;
                let sleep_after = Duration::from_secs(self.settings.sleep_minutes as u64 * 60);
                for t in self.tabs.iter_mut() {
                    if self.settings.sleep_minutes > 0
                        && t.id != active
                        && t.webview.is_some()
                        && !t.url.is_empty()
                        && t.last_active.elapsed() > sleep_after
                        && !keeps_awake(&t.url)
                    {
                        t.webview = None; // освобождаем память процессов вкладки
                        t.loading = false;
                        changed = true;
                    }
                }
                // скрытые вкладки приостанавливаем (кроме сайтов с музыкой и видео)
                for t in self.tabs.iter_mut() {
                    if t.id != active
                        && !t.suspended
                        && !t.loading
                        && t.last_active.elapsed() > SUSPEND_AFTER
                        && !keeps_awake(&t.url)
                    {
                        if let Some(wv) = &t.webview {
                            hook::suspend(wv);
                            t.suspended = true;
                        }
                    }
                }
                if changed {
                    self.push_state();
                }
            }
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
            UserEvent::Cosmetic(id) => self.apply_cosmetic(id),
            UserEvent::Harvest(id, res) => self.apply_generic(id, &res),
            UserEvent::Extensions(json) => {
                let mut list: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap_or_default();
                for e in list.iter_mut() {
                    let id = e["id"].as_str().unwrap_or("").to_string();
                    e["openable"] = self.ext_page(&id).is_some().into();
                }
                let j = serde_json::Value::Array(list);
                let _ = self.ui.evaluate_script(&format!("window.onExt({j})"));
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
                    let n = self.tabs.len();
                    if let Some(i) = self.tabs.iter().position(|t| t.id == self.active) {
                        let j = if matches!(sc, Shortcut::NextTab) { (i + 1) % n } else { (i + n - 1) % n };
                        let id = self.tabs[j].id;
                        self.activate(id);
                    }
                }
                Shortcut::Tab(i) => {
                    if let Some(id) = self.tabs.get(i).map(|t| t.id) {
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
                    }
                }
            },
            UserEvent::ExtChanged(msg) => {
                if !msg.is_empty() {
                    if let Ok(j) = serde_json::to_string(&msg) {
                        let _ = self.ui.evaluate_script(&format!("window.onExtMsg({j})"));
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
        events.run_return(|event, _, flow| {
            *flow = ControlFlow::WaitUntil(deadline);
            if let Event::UserEvent(source) = event {
                if source.is_empty() {
                    view.evaluate_script("history.pushState(null, '', '/watch?v=latest')").unwrap();
                } else if source.ends_with("/watch?v=latest") {
                    assert_eq!(hook::source(&view).as_deref(), Some(source.as_str()));
                    hook::open_devtools(&view).unwrap();
                    checked = true;
                    *flow = ControlFlow::Exit;
                }
            }
            if Instant::now() >= deadline { *flow = ControlFlow::Exit; }
        });
        assert!(checked, "WebView2 must report the new URL after history.pushState");
    }

    #[test]
    #[ignore = "Requires the native WebView2 runtime"]
    fn settings_and_downloads_render_in_webview() {
        use tao::platform::{run_return::EventLoopExtRunReturn, windows::EventLoopBuilderExtWindows};
        let mut events = EventLoopBuilder::<Option<bool>>::with_user_event().with_any_thread(true).build();
        let window = WindowBuilder::new().with_visible(false).build(&events).unwrap();
        let profile = tempfile::tempdir().unwrap();
        let mut context = WebContext::new(Some(profile.path().join("profile")));
        let loaded = events.create_proxy();
        let view = WebViewBuilder::with_web_context(&mut context)
            .with_on_page_load_handler(move |event, _| {
                if matches!(event, PageLoadEvent::Finished) { let _ = loaded.send_event(None); }
            })
            .with_ipc_handler(|_| {})
            .with_html(UI_HTML)
            .build_as_child(&window).unwrap();
        let result = events.create_proxy();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut passed = false;
        events.run_return(|event, _, flow| {
            *flow = ControlFlow::WaitUntil(deadline);
            match event {
                Event::UserEvent(None) => {
                    let result = result.clone();
                    view.evaluate_script_with_callback(r#"
                        (() => {
                            const fixture = {
                                platform: 'windows', tabs: [], panel: 'settings', engine: 'google',
                                set: { theme: 'dark', accent: '#456789' },
                                look: { al: '#456789', ad: '#456789', font: 'sans-serif', fs: 13 },
                                filters: { level: 'basic' }, block: { count: 0, recent: [] }
                            };
                            window.onState(fixture);
                            const color = document.getElementById('accenthex').value === '#456789';
                            const settings = !document.getElementById('p-settings').hidden;
                            window.onState({ ...fixture, panel: 'downloads' });
                            window.onDownloads([
                                { id: 'running', name: 'one', path: 'one', status: 'running' },
                                { id: 'done', name: 'two', path: 'two', status: 'completed' }
                            ]);
                            const actions = id => [...document.querySelectorAll('[data-download-id=' + id + '] button:not([hidden])')].map(x => x.textContent);
                            return color && settings && !document.getElementById('devbtn').hidden
                                && !document.getElementById('p-downloads').hidden
                                && actions('running').includes('Отменить')
                                && !actions('running').includes('Удалить из списка')
                                && actions('done').includes('Удалить из списка');
                        })()
                    "#, move |value| { let _ = result.send_event(Some(value == "true")); }).unwrap();
                }
                Event::UserEvent(Some(result)) => {
                    passed = result;
                    *flow = ControlFlow::Exit;
                }
                _ => {}
            }
            if Instant::now() >= deadline { *flow = ControlFlow::Exit; }
        });
        assert!(passed, "settings and download controls must render");
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
                            const arrow = document.querySelector('#chevron path');
                            const uses = rgb => getComputedStyle(label).color === rgb
                                && getComputedStyle(arrow).fill === rgb;
                            const dark = uses('rgb(230, 231, 234)');
                            root.dataset.lumenTheme = 'light';
                            return dark && uses('rgb(31, 35, 40)');
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
        "http"|"https"=>p.host_str().is_some(),
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
                    matches!(value["cmd"].as_str(), Some("capture_start" | "capture_stop")))
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
    layout_extension_popup(&window, &webview, consent.as_ref(), false);
    window.set_visible(true);
    let mut capture_stop=Arc::new(AtomicBool::new(false));
    let mut audio_rx=None;
    let mut pending_consent: Option<Instant> = None;
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
                    capture_stop = Arc::new(AtomicBool::new(false));
                    let (tx, rx) = mpsc::sync_channel(32);
                    audio_rx = Some(rx);
                    capture::start(tx, capture_stop.clone());
                } else {
                    let _ = webview.evaluate_script("window.__lumenAudioEnd()");
                }
            }
            if let Ok(value)=serde_json::from_str::<serde_json::Value>(message) {
                match value["cmd"].as_str() {
                    Some("capture_start") if audio_rx.is_none() && pending_consent.is_none() && consent.is_some() => {
                        pending_consent = Some(Instant::now() + Duration::from_secs(15));
                        layout_extension_popup(&window, &webview, consent.as_ref(), true);
                    }
                    Some("capture_stop") => {
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
        *flow = if audio_rx.is_some() {
            ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(20))
        } else {
            pending_consent.map_or(ControlFlow::Wait, ControlFlow::WaitUntil)
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
        if asking { let _ = consent.set_bounds(bounds(0, strip)); }
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
    let settings: Settings = crate::security::read_profile_text(&settings_path,1048576)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if !incognito {
        if let Ok(data) = serde_json::to_vec_pretty(&settings) {
            let _ = crate::security::atomic_write(&settings_path,&data);
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

    let links: Vec<Link> = crate::security::read_profile_text(&links_path,524288)
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
    let pulse = proxy.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(200));
        if pulse.send_event(UserEvent::Pulse).is_err() { break; }
    });

    // таймер усыпления фоновых вкладок
    {
        let p = proxy.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(TICK_EVERY);
            if p.send_event(UserEvent::Tick).is_err() {
                break;
            }
        });
    }

    // склейка частых обновлений интерфейса
    let (flush_tx, flush_rx) = mpsc::channel::<()>();
    {
        let p = proxy.clone();
        std::thread::spawn(move || {
            while flush_rx.recv().is_ok() {
                std::thread::sleep(Duration::from_millis(35));
                while flush_rx.try_recv().is_ok() {}
                if p.send_event(UserEvent::Flush).is_err() {
                    break;
                }
            }
        });
    }

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
    window.set_undecorated_shadow(false);
    unsafe {
        // Suppress Windows 11's contrasting DWM border; older Windows ignore it.
        let none = 0xfffffffeu32;
        let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            windows::Win32::Foundation::HWND(window.hwnd() as *mut _),
            windows::Win32::Graphics::Dwm::DWMWA_BORDER_COLOR,
            &none as *const _ as *const _, std::mem::size_of::<u32>() as u32,
        );
    }
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
        newtab_token: crate::security::newtab_token(),
        popup_tabs: HashMap::new(),
        download_hosts: Vec::new(),
        favicons:Rc::new(RefCell::new(favicon::Cache::default())),
        sent_icons:RefCell::new(std::collections::HashSet::new()),
        downloads,
        frame_dark: matches!(window.theme(), tao::window::Theme::Dark),
        fullscreen_keys: Arc::new(AtomicBool::new(false)),
        transferring: HashMap::new(),
        fullscreen: false,
        zoom: HashMap::new(),
        window,
        ui,
        ctx,
        tabs: Vec::new(),
        active: 0,
        next_id: 1,
        panel: String::new(),
        adblock: Arc::new(AtomicBool::new(settings.adblock)),
        settings,
        settings_path,
        history,
        history_path,
        bg_data,
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
        last_ui_state: RefCell::new(String::new()),
        ext_map,
        ext_map_path,
        secondary,
        args,
        flush_tx,
        flush_pending: Arc::new(AtomicBool::new(false)),
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
    if std::env::var_os("LUMEN_TEST_HIDDEN").is_none() { animate_window(&app.window, true); }

    let updates=crate::updater::start(!incognito && !secondary);
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } | Event::UserEvent(UserEvent::Quit) => {
                animate_window(&app.window, false);
                if !app.incognito {
                    for id in app.transferring.keys().copied() { transfer::cancel(id); }
                    transfer::cleanup();
                }
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
    });
}
