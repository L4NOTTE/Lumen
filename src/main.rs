#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod blocker;
mod ext;
mod filters;
mod hook;
mod music;
mod sys;
mod devtools;
mod transfer;

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
use music::MusicState;
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
const UI_HTML: &str = include_str!("../ui/ui.html");
const DEV_HTML: &str = include_str!("../ui/devtools.html");
const NEWTAB_HTML: &str = include_str!("../ui/newtab.html");

/// Аргументы движка. SmartScreen остаётся включённым, перевод страниц выключен,
/// число процессов ограничено, вкладки одного сайта делят процесс (экономия памяти).
/// «Эконом-режим» дополнительно включает облегчённый режим Chromium и отключает кэш переходов назад.
/// У всех окон одного профиля аргументы обязаны совпадать, поэтому строка считается один раз при запуске.
fn browser_args(eco: bool, debug_port: u16) -> String {
    let mut disabled = String::from("Translate,msWebOOUI,msPdfOOUI");
    let mut extra = String::new();
    if eco {
        disabled.push_str(",BackForwardCache");
        extra.push_str(" --enable-low-end-device-mode");
    }
    let limit = if eco { 3 } else { 4 };
    format!(
        "--enable-features=ParallelDownloading --disable-features={disabled} --renderer-process-limit={limit} --process-per-site --remote-debugging-port={debug_port} --remote-debugging-address=127.0.0.1{extra}"
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
    Settings,
    Music,
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
        (0x48, true, false, false) => Some(History),           // Ctrl+H
        (0xBC, true, false, false) => Some(Settings),          // Ctrl+,
        (0x4D, true, false, false) => Some(Music),             // Ctrl+M
        (0x44, true, false, false) => Some(AddLink),           // Ctrl+D
        (0x09, true, false, false) => Some(NextTab),           // Ctrl+Tab
        (0x09, true, true, false) => Some(PrevTab),            // Ctrl+Shift+Tab
        (0x25, false, false, true) => Some(Back),              // Alt+←
        (0x27, false, false, true) => Some(Forward),           // Alt+→
        (0x31..=0x39, true, false, false) => Some(Tab((vk - 0x31) as usize)), // Ctrl+1…9
        _ => None,
    }
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
/// Максимальный размер своей фотографии фона (символов data-URL).
const BG_MAX_LEN: usize = 1_800_000;

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
fn accent_pair(k: &str) -> (&'static str, &'static str) {
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
    engine: String,
    adblock: bool,
    theme: String,
    accent: String,
    font: String,
    font_size: String,
    bg_mode: String,
    adblock_level: String,
    audd_token: String,
    music_source: String,
    startup: String,
    hide_labels: bool,
    sleep_minutes: u32,
    eco: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            engine: "google".into(),
            adblock: true,
            theme: "system".into(),
            accent: "indigo".into(),
            font: "system".into(),
            font_size: "normal".into(),
            bg_mode: "none".into(),
            adblock_level: "strong".into(),
            audd_token: String::new(),
            music_source: "system".into(),
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
    DevTarget(u32, Result<String, String>),
    DevResult(String),
    Pulse,
    FullscreenChanged(u32, bool),
    Ui(String),
    Title(u32, String),
    Url(u32, String),
    Loading(u32, bool),
    Navigate(u32, String),
    Search(u32, String),
    NewTab(String),
    Tick,
    FiltersBuilt(Arc<Filters>),
    FiltersStatus(String),
    Cosmetic(u32),
    Harvest(u32, String),
    Extensions(String),
    ExtInstalled(Result<PathBuf, String>),
    ExtChanged(String),
    Music(MusicState),
    Shortcut(Shortcut),
    Link(LinkCmd),
    ExtAdded(PathBuf, Result<String, String>),
    Flush,
    Quit,
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Cmd {
    WinResize { edge: String },
    DevConnect,
    NativeDevTools,
    FrameTheme { dark: bool },
    DevClose,
    DevWidth { width: f64 },
    DevQuery { kind: String, expression: Option<String> },
    ZoomIn, ZoomOut, ZoomReset, DevTools, Fullscreen, Escape,
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
    Recognize,
    SetMusic { token: String, source: String },
    ExtOpenDir,
    ExtInstall { input: String },
    ExtToggle { id: String, on: bool },
    ExtRemove { id: String },
    ExtRefresh,
    ExtOpen { id: String },
    AddCurrentLink,
    SetStartup { mode: String },
    SetLabels { on: bool },
    WinMin,
    WinMax,
    WinClose,
    WinDrag,
    DetachTab { id: u32, x: i32, y: i32 },
    SetSleep { minutes: u32 },
    SetEco { on: bool },
}

// ---------- приложение ----------

struct Tab {
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
    debug_port: u16,
    frame_dark: bool,
    dev: WebView,
    dev_open: bool,
    dev_width: f64,
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
    profile_dir: PathBuf,
    blocker: Rc<Blocker>,
    adblock: Arc<AtomicBool>,
    proxy: EventLoopProxy<UserEvent>,
    filters: SharedFilters,
    filters_status: String,
    lists_dir: PathBuf,
    user_filters_dir: PathBuf,
    ext_dir: PathBuf,
    music: MusicState,
    music_busy: Arc<AtomicBool>,
    stats: Arc<hook::Stats>,
    links: Vec<Link>,
    links_path: PathBuf,
    closed: Vec<(String, String)>,
    session_path: PathBuf,
    last_session: RefCell<String>,
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

/// Отступ от края окна: по нему окно без рамки можно растягивать мышью (в развёрнутом виде не нужен).
fn inset_px(w: &Window) -> u32 {
    let _ = w;
    0
}

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
        size: PhysicalSize::new(sz.width.saturating_sub(2 * i), toolbar_px(w).min(sz.height)).into(),
    }
}

fn full_rect(w: &Window) -> Rect {
    let sz = w.inner_size();
    let i = inset_px(w);
    Rect {
        position: PhysicalPosition::new(i as i32, i as i32).into(),
        size: PhysicalSize::new(sz.width.saturating_sub(2 * i), sz.height.saturating_sub(2 * i)).into(),
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
fn schedule_cleanup(dir: &PathBuf) {
    use std::os::windows::process::CommandExt;
    let cmd = format!("ping -n 4 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display());
    let _ = std::process::Command::new("cmd")
        .args(["/C", &cmd])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
}

impl App {
    fn resolve(&self, text: &str) -> String {
        let t = text.trim();
        if t.contains("://") {
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
            let _ = fs::write(&self.settings_path, s);
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
        let _ = fs::write(&self.history_path, s);
    }

    fn record_history(&mut self, id: u32) {
        if self.incognito || self.secondary {
            return;
        }
        let Some(t) = self.tabs.iter().find(|t| t.id == id) else { return };
        if !(t.url.starts_with("http://") || t.url.starts_with("https://")) {
            return;
        }
        let (u, title) = (t.url.clone(), t.title.clone());
        let ts = now_secs();
        if let Some(last) = self.history.last_mut() {
            if last.u == u && ts.saturating_sub(last.ts) < 30 {
                last.t = title;
                return;
            }
        }
        let entry = HistEntry { t: title, u, ts };
        if let Ok(line) = serde_json::to_string(&entry) {
            use std::io::Write;
            if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&self.history_path) {
                let _ = writeln!(f, "{line}");
            }
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
        let theme = if self.incognito { "dark" } else { s.theme.as_str() };
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
            serde_json::to_string(&self.links).unwrap_or_else(|_| "[]".into())
        }
        .replace("</", "<\\/");
        NEWTAB_HTML
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
            let wv = self.make_webview(id, None);
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                t.webview = Some(wv);
            }
        }
        self.layout();
    }

    // ----- вкладки -----

    fn make_webview(&mut self, id: u32, url: Option<String>) -> WebView {
        let rect = content_rect(&self.window);
        let incognito = self.incognito;
        let newtab = self.newtab_html();
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

        let mut b = WebViewBuilder::with_web_context(&mut self.ctx)
            .with_initialization_script(include_str!("shazam-capture.js"))
            .with_devtools(true)
            .with_incognito(incognito)
            .with_browser_extensions_enabled(!incognito)
            .with_additional_browser_args(&self.args)
            .with_bounds(rect)
            .with_document_title_changed_handler(move |t: String| {
                let _ = p_title.send_event(UserEvent::Title(id, t));
            })
            .with_on_page_load_handler(move |ev: PageLoadEvent, u: String| {
                let started = matches!(ev, PageLoadEvent::Started);
                let _ = p_url.send_event(UserEvent::Url(id, u));
                let _ = p_url.send_event(UserEvent::Loading(id, started));
            })
            .with_new_window_req_handler(move |u: String| {
                let _ = p_win.send_event(UserEvent::NewTab(u));
                false
            })
            .with_navigation_handler(move |u: String| {
                let Ok(mut parsed) = url::Url::parse(&u) else { return true };

                // поисковая строка стартовой страницы
                if parsed.host_str() == Some("lumen.invalid") {
                    // команды работают только со стартовой страницы (у неё адрес страницы ещё пуст)
                    let on_newtab = page_nav.borrow().is_empty();
                    let q = |name: &str| {
                        parsed.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned())
                    };
                    match parsed.path() {
                        "/s" => {
                            if let Some(text) = q("q") {
                                let _ = p_nav.send_event(UserEvent::Search(id, text));
                            }
                        }
                        "/add" if on_newtab => {
                            if let Some(u) = q("u") {
                                let _ = p_nav.send_event(UserEvent::Link(LinkCmd::Add(q("t").unwrap_or_default(), u)));
                            }
                        }
                        "/del" if on_newtab => {
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
        let _ = hook::install_blocker(&wv, self.filters.clone(), self.adblock.clone(), page, self.stats.clone());
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
        wv
    }

    fn new_tab(&mut self, url: Option<String>) {
        let id = self.next_id;
        self.next_id += 1;
        let webview = self.make_webview(id, url);
        self.tabs.push(Tab {
            id,
            webview: Some(webview),
            title: "Новая вкладка".into(),
            url: String::new(),
            loading: false,
            last_active: Instant::now(),
            suspended: false,
            low: false,
        });
        self.activate(id);
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
            let wv = self.make_webview(id, if url.is_empty() { None } else { Some(url) });
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                t.webview = Some(wv);
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
        if self.dev_open { self.cmd(Cmd::DevConnect); }
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
        let docked = self.dev_open && !self.fullscreen && !open;
        let mut page_rect = if self.fullscreen { full_rect(&self.window) } else { content_rect(&self.window) };
        let size = self.window.inner_size();
        let i = inset_px(&self.window);
        let available_width = size.width.saturating_sub(2 * i);
        let available_height = size.height.saturating_sub(toolbar_px(&self.window) + 2 * i);
        let dock = dock_width(available_width, self.window.scale_factor(), self.dev_width).min(available_width);
        if docked {
            page_rect.size = PhysicalSize::new(available_width.saturating_sub(dock), available_height).into();
            let _ = self.dev.set_bounds(Rect {
                position: PhysicalPosition::new((size.width - i - dock) as i32, (toolbar_px(&self.window) + i) as i32).into(),
                size: PhysicalSize::new(dock, available_height).into(),
            });
        }
        let _ = self.dev.set_visible(docked);
        let _ = self.ui.set_visible(!self.fullscreen);
        // при открытой панели интерфейс занимает всё окно, а страница скрыта
        let _ = self.ui.set_bounds(if open { full_rect(&self.window) } else { ui_rect(&self.window) });
        for t in self.tabs.iter_mut() {
            if let Some(wv) = &t.webview {
                let visible = t.id == self.active && !open;
                if visible {
                    if t.suspended {
                        hook::resume(wv);
                        t.suspended = false;
                    }
                    let _ = wv.set_bounds(page_rect);
                }
                let _ = wv.set_visible(visible);
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
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
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
            "tabs": tabs,
            "version": env!("CARGO_PKG_VERSION"),
            "active": self.active,
            "zoom": (self.zoom.get(&self.active).copied().unwrap_or(1.0) * 100.0).round(),
            "panel": if self.dev_open && self.panel.is_empty() { "dev" } else { &self.panel },
            "dev_open": self.dev_open,
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
            "music_cfg": { "token": s.audd_token, "source": s.music_source },
        });
        let _ = self.ui.evaluate_script(&format!("window.onState({state})"));
        let _ = self.dev.evaluate_script(&format!("window.onState({state})"));
        self.save_session();
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
        if !self.flush_pending.swap(true, Ordering::SeqCst) {
            let _ = self.flush_tx.send(());
        }
    }

    /// Отдельное маленькое окно для расширения (как всплывающее окно у кнопки расширения).
    fn open_popup(&self, url: &str) {
        let sf = self.window.scale_factor();
        let (w, h) = (420.0f64, 620.0f64);
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
            tabs: real.iter().map(|t| SavedTab { u: t.url.clone(), t: t.title.clone() }).collect(),
        };
        if let Ok(j) = serde_json::to_string(&session) {
            if *self.last_session.borrow() != j {
                let _ = fs::write(&self.session_path, &j);
                *self.last_session.borrow_mut() = j;
            }
        }
    }

    /// Восстанавливает вкладки прошлого сеанса: загружается только активная, остальные «спят».
    fn restore_session(&mut self, s: Session) {
        for t in &s.tabs {
            let id = self.next_id;
            self.next_id += 1;
            self.tabs.push(Tab {
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
            let _ = fs::write(&self.links_path, j);
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
        let raw = fs::read_to_string(dir.join("manifest.json")).ok()?;
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
            let arr = cos.procedural.iter().cloned().collect::<Vec<_>>().join(",");
            let _ = wv.evaluate_script(&format!("window.__lumenSetRules&&window.__lumenSetRules([{arr}])"));
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

    fn push_music(&self) {
        if let Ok(j) = serde_json::to_string(&self.music) {
            let _ = self.ui.evaluate_script(&format!("window.onMusic({j})"));
        }
    }

    fn start_recognize(&mut self) {
        self.panel = "music".into();
        self.layout();
        if self.music_busy.load(Ordering::SeqCst) {
            self.push_state();
            self.push_music();
            return;
        }
        let token = self.settings.audd_token.trim().to_string();
        if token.is_empty() {
            self.music = MusicState::error("Укажите ключ AudD ниже — без него распознавание не работает");
            self.push_state();
            self.push_music();
            return;
        }
        self.music_busy.store(true, Ordering::SeqCst);
        self.music = MusicState::new("listening");
        self.push_state();
        self.push_music();

        let mic = self.settings.music_source == "mic";
        let (p, busy) = (self.proxy.clone(), self.music_busy.clone());
        std::thread::spawn(move || {
            let send = |m: MusicState| {
                let _ = p.send_event(UserEvent::Music(m));
            };
            match music::record(mic, 10) {
                Err(e) => send(MusicState::error(e)),
                Ok(wav) => {
                    send(MusicState::new("searching"));
                    match music::recognize(&token, &wav) {
                        Ok(Some(song)) => send(MusicState { status: "found".into(), song: Some(song), msg: String::new() }),
                        Ok(None) => send(MusicState::new("notfound")),
                        Err(e) => send(MusicState::error(e)),
                    }
                }
            }
            busy.store(false, Ordering::SeqCst);
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
            let wv = self.make_webview(id, if url.is_empty() { None } else { Some(url) });
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                t.webview = Some(wv);
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
        self.zoom.insert(self.active, next);
        self.push_state();
    }

    fn cmd(&mut self, c: Cmd) {
        match c {
            Cmd::WinResize { edge } => {
                use tao::window::ResizeDirection::*;
                let direction = match edge.as_str() { "n" => North, "ne" => NorthEast, "nw" => NorthWest, "e" => East, "w" => West, _ => return };
                let _ = self.window.drag_resize_window(direction);
            },
            Cmd::DevConnect => {
                let proxy = self.proxy.clone();
                let id = self.active;
                self.with_active(|wv| unsafe {
                    if let Ok(core) = wv.controller().CoreWebView2() {
                        let handler = webview2_com::CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
                            let target = if result.is_ok() {
                                serde_json::from_str::<serde_json::Value>(&json).ok().and_then(|j| j["targetInfo"]["targetId"].as_str().map(str::to_string)).ok_or_else(|| "WebView2 не вернул targetId".to_string())
                            } else { Err(format!("Target.getTargetInfo: {result:?}")) };
                            let _ = proxy.send_event(UserEvent::DevTarget(id, target)); Ok(())
                        }));
                        let _ = core.CallDevToolsProtocolMethod(&windows_core::HSTRING::from("Target.getTargetInfo"), &windows_core::HSTRING::from("{}"), &handler);
                    }
                });
            },
            Cmd::FrameTheme { dark } => {
                self.frame_dark = dark;
                self.window.set_background_color(Some(if self.incognito { (22,15,36,255) } else if dark { (27,28,32,255) } else { (243,244,246,255) }));
            },
            Cmd::DevClose => { self.dev_open = false; let _ = self.dev.load_html(DEV_HTML); self.layout(); self.push_state(); },
            Cmd::DevWidth { width } => { if width.is_finite() { self.dev_width = width.clamp(240.0, 960.0); self.layout(); } },
            Cmd::DevQuery { kind, expression } => {
                let (method, params) = match kind.as_str() {
                    "console" => ("Runtime.evaluate", serde_json::json!({"expression": expression.unwrap_or_default(), "returnByValue": true, "awaitPromise": true, "generatePreview": true})),
                    "network" => ("Runtime.evaluate", serde_json::json!({"expression": "performance.getEntriesByType('resource').slice(-200).map(r=>({url:r.name,type:r.initiatorType,ms:Math.round(r.duration),bytes:r.transferSize}))", "returnByValue": true})),
                    "storage" => ("Runtime.evaluate", serde_json::json!({"expression": "({cookies:document.cookie,localStorage:Object.fromEntries(Object.entries(localStorage)),sessionStorage:Object.fromEntries(Object.entries(sessionStorage))})", "returnByValue": true})),
                    "performance" => ("Runtime.evaluate", serde_json::json!({"expression":"({navigation:performance.getEntriesByType('navigation').map(n=>({url:n.name,domReadyMs:Math.round(n.domContentLoadedEventEnd),loadMs:Math.round(n.loadEventEnd),responseMs:Math.round(n.responseEnd)})),resources:performance.getEntriesByType('resource').length,memory:performance.memory?{usedBytes:performance.memory.usedJSHeapSize,totalBytes:performance.memory.totalJSHeapSize}:null})", "returnByValue":true})),
                    _ => return,
                };
                let p = self.proxy.clone();
                self.with_active(|wv| unsafe {
                    if let Ok(core) = wv.controller().CoreWebView2() {
                        let handler = webview2_com::CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |res, value| {
                            let text = if res.is_ok() { value } else { format!("Ошибка DevTools: {res:?}") };
                            let _ = p.send_event(UserEvent::DevResult(text));
                            Ok(())
                        }));
                        let _ = core.CallDevToolsProtocolMethod(&windows_core::HSTRING::from(method), &windows_core::HSTRING::from(params.to_string()), &handler);
                    }
                });
            }
            Cmd::ZoomIn => self.change_zoom(1),
            Cmd::ZoomOut => self.change_zoom(-1),
            Cmd::ZoomReset => self.change_zoom(0),
            Cmd::DevTools => {
                if self.dev_open { self.cmd(Cmd::DevClose); }
                else { self.dev_open = true; self.panel.clear(); self.layout(); self.push_state(); self.cmd(Cmd::DevConnect); }
            },
            Cmd::NativeDevTools => self.with_active(|wv| unsafe {
                if let Ok(core) = wv.controller().CoreWebView2() { let _ = core.OpenDevToolsWindow(); }
            }),
            Cmd::Fullscreen => self.set_fullscreen(!self.fullscreen),
            Cmd::Escape => { if self.fullscreen { self.with_active(|wv| { let _ = wv.evaluate_script("document.exitFullscreen().catch(()=>{})"); }); self.set_fullscreen(false); } else { self.close_panel(); } },
            Cmd::Ready => self.push_state(),
            Cmd::NewTab => self.new_tab(None),
            Cmd::CloseTab { id } => {
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
                if matches!(name.as_str(), "history" | "settings" | "ext" | "music" | "dev") {
                    self.panel = name.clone();
                    self.layout();
                    self.push_state();
                    match name.as_str() {
                        "history" => self.push_history(),
                        "ext" => self.refresh_ext(),
                        "music" => self.push_music(),
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
                    && ACCENTS.contains(&accent.as_str())
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
                if data.starts_with("data:image/") && data.len() < BG_MAX_LEN {
                    if !self.incognito {
                        let _ = fs::write(&self.bg_path, &data);
                    }
                    self.bg_data = Some(data);
                    self.settings.bg_mode = "custom".into();
                    self.save_settings();
                    self.refresh_newtabs();
                    self.push_state();
                }
            }
            Cmd::ClearBackground => {
                let _ = fs::remove_file(&self.bg_path);
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
            Cmd::Recognize => self.start_recognize(),
            Cmd::SetMusic { token, source } => {
                let token = token.trim().to_string();
                let ok_token = token.len() < 200 && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                if ok_token && (source == "system" || source == "mic") {
                    self.settings.audd_token = token;
                    self.settings.music_source = source;
                    self.save_settings();
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
                Some((u, _)) if id == "mmioliijnhnoblpgimnlajmefafdfilb" => self.new_tab(Some(u)),
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
            Cmd::DetachTab { id, x, y } => {
                if let Some(pid) = transfer::target(self.incognito, x, y) {
                    if let Some(tab) = self.tabs.iter().find(|t| t.id == id) {
                        if !self.transferring.contains_key(&id) && transfer::send(pid, id, &tab.url) {
                            self.transferring.insert(id, Instant::now());
                        }
                    }
                    return;
                }
                if self.tabs.len() > 1 {
                    let url = self.tabs.iter().find(|t| t.id == id).map(|t| t.url.clone()).unwrap_or_default();
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
                        let _ = wv.evaluate_script(&format!("window.__lumenCfg&&(window.__lumenCfg.labels={on})"));
                    }
                }
                self.push_state();
            }
            Cmd::ExtRemove { id } => {
                if let Some(name) = self.ext_map.remove(&id) {
                    if !name.contains(['/', '\\', '.']) {
                        let dir = self.ext_dir.join(name);
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_secs(2));
                            let _ = fs::remove_dir_all(dir);
                        });
                    }
                    if let Ok(j) = serde_json::to_string(&self.ext_map) {
                        let _ = fs::write(&self.ext_map_path, j);
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
            UserEvent::DevTarget(id, target) => {
                if id == self.active && self.dev_open {
                    match target.ok().and_then(|id| devtools::frontend_url(self.debug_port, &id)) {
                        Some(url) => { let _ = self.dev.load_url(&url); },
                        None => { let _ = self.dev.evaluate_script("window.devtoolsError('Не удалось подключить страницу. Используйте отдельный инспектор или перезапустите Lumen.')"); },
                    }
                }
            },
            UserEvent::DevResult(s) => { if let Ok(j) = serde_json::to_string(&s) { let _ = self.dev.evaluate_script(&format!("window.onDevResult({j})")); } },
            UserEvent::Pulse => {
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
                    tab.url = u;
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
            UserEvent::NewTab(u) => self.new_tab(Some(u)),
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
                                let _ = fs::write(&self.ext_map_path, j);
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
                Shortcut::History => self.cmd(Cmd::OpenPanel { name: "history".into() }),
                Shortcut::Settings => self.cmd(Cmd::OpenPanel { name: "settings".into() }),
                Shortcut::Music => self.cmd(Cmd::Recognize),
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
            UserEvent::Music(m) => {
                self.music = m;
                self.push_music();
            }
        }
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
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
        assert!(matches!(map_key(0xBB, true, true, false), Some(Shortcut::ZoomIn)));
        assert!(matches!(map_key(0x30, true, false, false), Some(Shortcut::ZoomReset)));
        assert!(matches!(map_key(0x49, true, true, false), Some(Shortcut::DevTools)));
        assert!(matches!(map_key(0x7A, false, false, false), Some(Shortcut::Fullscreen)));
        assert!(map_key(0x49, true, false, false).is_none());
        assert!(map_key(0xBB, false, false, false).is_none());
    }
}

#[derive(Default)]
struct Cli {
    incognito: bool,
    secondary: bool,
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
            "--incognito" => c.incognito = true,
            "--secondary" => c.secondary = true,
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
            _ => {}
        }
        i += 1;
    }
    c
}

fn safe_url(u: &str) -> bool {
    u.starts_with("https://") || u.starts_with("http://") || u.starts_with("chrome-extension://")
}

/// Режим «всплывающего окна» расширения: маленькое окно без вкладок, закрывается при потере фокуса.
fn run_popup(url: String, size: (f64, f64), pos: Option<(i32, i32)>, args: String, profile: PathBuf) {
    let keep_open = url.starts_with("chrome-extension://mmioliijnhnoblpgimnlajmefafdfilb/");
    let event_loop = EventLoopBuilder::<()>::new().build();
    let icon = tao::window::Icon::from_rgba(ICON_RGBA.to_vec(), 64, 64).ok();
    let mut wb = WindowBuilder::new()
        .with_window_icon(icon)
        .with_title("Lumen — расширение")
        .with_inner_size(tao::dpi::LogicalSize::new(size.0, size.1))
        .with_always_on_top(!keep_open);
    if let Some((x, y)) = pos {
        wb = wb.with_position(tao::dpi::PhysicalPosition::new(x, y));
    }
    let window = wb.build(&event_loop).expect("не удалось создать окно");
    let mut ctx = WebContext::new(Some(profile));
    let webview = WebViewBuilder::with_web_context(&mut ctx)
        .with_initialization_script(include_str!("shazam-capture.js"))
        .with_devtools(false)
        .with_incognito(false)
        .with_browser_extensions_enabled(true)
        .with_additional_browser_args(&args)
        .with_url(&url)
        .build(&window)
        .expect("не удалось создать WebView2");
    let mut was_focused = false;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        let _ = &webview;
        if let Event::WindowEvent { event: we, .. } = event {
            match we {
                WindowEvent::CloseRequested => *flow = ControlFlow::Exit,
                WindowEvent::Focused(true) => was_focused = true,
                WindowEvent::Focused(false) if was_focused && !keep_open => *flow = ControlFlow::Exit,
                _ => {}
            }
        }
    });
}

fn main() {
    let cli = parse_cli();
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
    let settings: Settings = fs::read_to_string(&settings_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let debugging_profile = if incognito { std::env::temp_dir().join(format!("Lumen-incognito-{}", std::process::id())) } else { dir.join("profile") };
    let debug_port = devtools::port(&debugging_profile);
    let args = browser_args(settings.eco, debug_port);
    // WebView2 can ignore the debugging switch in per-controller options.
    // Set it before creating any environment; only this process and its children
    // inherit it, never the user's global Windows environment.
    std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", &args);

    if let Some(url) = cli.popup.clone().filter(|u| safe_url(u)) {
        run_popup(url, cli.size.unwrap_or((420.0, 620.0)), cli.pos, args, dir.join("profile"));
        return;
    }

    let profile_dir = if incognito {
        std::env::temp_dir().join(format!("Lumen-incognito-{}", std::process::id()))
    } else {
        dir.join("profile")
    };

    let links_path = dir.join("links.json");
    let session_path = dir.join("session.json");
    let ext_map_path = dir.join("ext_map.json");

    // история
    let history_path = dir.join("history.jsonl");
    let mut history: Vec<HistEntry> = if incognito {
        Vec::new()
    } else {
        fs::read_to_string(&history_path)
            .map(|s| s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
            .unwrap_or_default()
    };
    if history.len() > HISTORY_MAX {
        let cut = history.len() - HISTORY_MAX;
        history.drain(..cut);
    }

    let links: Vec<Link> = fs::read_to_string(&links_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let session: Session = if incognito || secondary {
        Session::default()
    } else {
        fs::read_to_string(&session_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    };
    let ext_map: HashMap<String, String> = fs::read_to_string(&ext_map_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let restore = !incognito && !secondary && settings.startup == "restore" && !session.tabs.is_empty();

    // своя фотография для главного экрана
    let bg_path = dir.join("background.txt");
    let bg_data = fs::read_to_string(&bg_path)
        .ok()
        .filter(|d| d.starts_with("data:image/") && d.len() < BG_MAX_LEN);

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
        .with_title(if incognito { "Lumen 0.3 — Инкогнито" } else { "Lumen 0.3" })
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
        .with_devtools(cfg!(debug_assertions))
        .with_incognito(incognito)
        .with_browser_extensions_enabled(!incognito) // у всех WebView одного профиля параметры должны совпадать
        .with_additional_browser_args(&args) // у всех WebView одного профиля аргументы должны совпадать
        .with_html(UI_HTML)
        .with_bounds(ui_rect(&window))
        .with_on_page_load_handler(move |ev: PageLoadEvent, _: String| {
            if matches!(ev, PageLoadEvent::Finished) {
                let _ = p_ready.send_event(UserEvent::Ui(r#"{"cmd":"ready"}"#.to_string()));
            }
        })
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            let _ = p_ui.send_event(UserEvent::Ui(req.body().clone()));
        })
        .build_as_child(&window)
        .expect("не удалось создать панель интерфейса");

    // Use the same native shortcuts in the toolbar and in page WebViews.
    // Handling here prevents Ctrl+Shift+T falling through to UI's Ctrl+T.
    let p_toolbar = proxy.clone();
    let _ = hook::install_shortcuts(&ui, move |vk, ctrl, shift, alt| {
        if vk == 0x1B { return false; } // panel Escape is handled by UI
        if let Some(sc) = map_key(vk, ctrl, shift, alt) {
            let _ = p_toolbar.send_event(UserEvent::Shortcut(sc));
            true
        } else { false }
    });

    let p_dev = proxy.clone();
    let p_dev_ready = proxy.clone();
    let p_dev_link = proxy.clone();
    let dev = WebViewBuilder::with_web_context(&mut ctx)
        .with_initialization_script(include_str!("devtools-theme.js"))
        .with_navigation_handler(move |url| devtools::navigation_allowed(debug_port, &url))
        .with_new_window_req_handler(move |url| {
            if url.starts_with("https://") || url.starts_with("http://") { let _ = p_dev_link.send_event(UserEvent::NewTab(url)); }
            false
        })
        .with_incognito(incognito)
        .with_browser_extensions_enabled(!incognito)
        .with_additional_browser_args(&args)
        .with_html(DEV_HTML)
        .with_visible(false)
        .with_bounds(content_rect(&window))
        .with_on_page_load_handler(move |ev, _| { if matches!(ev, PageLoadEvent::Finished) { let _ = p_dev_ready.send_event(UserEvent::Ui(r#"{"cmd":"ready"}"#.into())); } })
        .with_ipc_handler(move |req: wry::http::Request<String>| { let _ = p_dev.send_event(UserEvent::Ui(req.body().clone())); })
        .build_as_child(&window).expect("не удалось создать боковую панель");

    let mut app = App {
        debug_port,
        frame_dark: matches!(window.theme(), tao::window::Theme::Dark),
        dev,
        dev_open: false,
        dev_width: 560.0,
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
        profile_dir,
        blocker: Rc::new(Blocker::load(&filters_dir)),
        proxy,
        filters: Rc::new(RefCell::new(None)),
        filters_status: "Загрузка фильтров…".into(),
        lists_dir,
        user_filters_dir: filters_dir,
        ext_dir,
        music: MusicState::new("idle"),
        music_busy: Arc::new(AtomicBool::new(false)),
        stats: Arc::new(hook::Stats::new()),
        links,
        links_path,
        closed: Vec::new(),
        session_path,
        last_session: RefCell::new(String::new()),
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

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } | Event::UserEvent(UserEvent::Quit) => {
                animate_window(&app.window, false);
                transfer::cleanup();
                if app.incognito {
                    app.tabs.clear();
                    schedule_cleanup(&app.profile_dir);
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
