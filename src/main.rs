#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod blocker;
mod ext;
mod filters;
mod hook;
mod music;
mod sys;

use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
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
const NEWTAB_HTML: &str = include_str!("../ui/newtab.html");

/// SmartScreen остаётся включённым, перевод страниц выключен,
/// число процессов-рендереров ограничено (экономия оперативной памяти).
const BROWSER_ARGS: &str = "--enable-features=ParallelDownloading \
    --disable-features=Translate,msWebOOUI,msPdfOOUI \
    --renderer-process-limit=4";

/// Через сколько фоновая вкладка «засыпает» (выгружается из памяти).
const SLEEP_AFTER: Duration = Duration::from_secs(5 * 60);
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
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Cmd {
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
}

fn toolbar_px(w: &Window) -> u32 {
    (TOOLBAR_H * w.scale_factor()).round() as u32
}

fn content_rect(w: &Window) -> Rect {
    let sz = w.inner_size();
    let top = toolbar_px(w);
    Rect {
        position: PhysicalPosition::new(0i32, top as i32).into(),
        size: PhysicalSize::new(sz.width, sz.height.saturating_sub(top)).into(),
    }
}

fn ui_rect(w: &Window) -> Rect {
    let sz = w.inner_size();
    Rect {
        position: PhysicalPosition::new(0i32, 0i32).into(),
        size: PhysicalSize::new(sz.width, toolbar_px(w).min(sz.height)).into(),
    }
}

fn full_rect(w: &Window) -> Rect {
    let sz = w.inner_size();
    Rect {
        position: PhysicalPosition::new(0i32, 0i32).into(),
        size: PhysicalSize::new(sz.width, sz.height).into(),
    }
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
        if self.incognito {
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
        if self.incognito {
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
        NEWTAB_HTML
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
            .with_devtools(cfg!(debug_assertions))
            .with_incognito(incognito)
            .with_browser_extensions_enabled(!incognito)
            .with_additional_browser_args(BROWSER_ARGS)
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
                    if let Some((_, q)) = parsed.query_pairs().find(|(k, _)| k == "q") {
                        let _ = p_nav.send_event(UserEvent::Search(id, q.into_owned()));
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
        b = match url {
            Some(u) => b.with_url(u),
            None => b.with_html(newtab),
        };
        let wv = b.build_as_child(&self.window).expect("не удалось создать WebView2");
        let _ = hook::install_blocker(&wv, self.filters.clone(), self.adblock.clone(), page);
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
        let open = !self.panel.is_empty();
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
                    let _ = wv.set_bounds(content_rect(&self.window));
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
            "active": self.active,
            "panel": self.panel,
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
            },
            "look": { "al": al, "ad": ad, "font": font_stack(&s.font), "fs": font_px(&s.font_size) },
            "filters": { "level": s.adblock_level, "status": self.filters_status },
            "music_cfg": { "token": s.audd_token, "source": s.music_source },
        });
        let _ = self.ui.evaluate_script(&format!("window.onState({state})"));
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

    fn cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Ready => self.push_state(),
            Cmd::NewTab => self.new_tab(None),
            Cmd::CloseTab { id } => {
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
            Cmd::Reload => self.with_active(|wv| {
                let _ = wv.evaluate_script("location.reload()");
            }),
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
                if matches!(name.as_str(), "history" | "settings" | "ext" | "music") {
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
            Cmd::ExtRemove { id } => {
                let p = self.proxy.clone();
                ext::remove(&self.ui, id, move || {
                    let _ = p.send_event(UserEvent::ExtChanged("Расширение удалено".into()));
                });
            }
        }
    }

    fn handle(&mut self, ev: UserEvent) {
        match ev {
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
                    self.push_state();
                }
            }
            UserEvent::Url(id, u) => {
                let u = if u.starts_with("about:") || u.starts_with("data:") { String::new() } else { u };
                if let Some(tab) = self.tabs.iter_mut().find(|x| x.id == id && x.webview.is_some()) {
                    tab.url = u;
                    self.push_state();
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
                        std::thread::sleep(Duration::from_millis(2500));
                        let _ = p.send_event(UserEvent::Cosmetic(id));
                    });
                }
                self.push_state();
            }
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
                for t in self.tabs.iter_mut() {
                    if t.id != active
                        && t.webview.is_some()
                        && !t.url.is_empty()
                        && t.last_active.elapsed() > SLEEP_AFTER
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
                let _ = self.ui.evaluate_script(&format!("window.onExt({json})"));
            }
            UserEvent::ExtInstalled(res) => match res {
                Ok(dir) => {
                    let p = self.proxy.clone();
                    ext::add(&self.ui, &dir, move |ok| {
                        let msg = if ok {
                            "Расширение установлено"
                        } else {
                            "Не удалось добавить расширение (оно может не поддерживаться WebView2)"
                        };
                        let _ = p.send_event(UserEvent::ExtChanged(msg.to_string()));
                    });
                }
                Err(e) => {
                    let _ = self.proxy.send_event(UserEvent::ExtChanged(e));
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

fn main() {
    let incognito = std::env::args().any(|a| a == "--incognito");

    // профиль и настройки: %LOCALAPPDATA%\Lumen
    let dir = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Lumen");
    let filters_dir = dir.join("filters");
    let lists_dir = filters_dir.join("lists");
    let ext_dir = dir.join("extensions");
    let _ = fs::create_dir_all(&lists_dir);
    let _ = fs::create_dir_all(&ext_dir);

    let profile_dir = if incognito {
        std::env::temp_dir().join(format!("Lumen-incognito-{}", std::process::id()))
    } else {
        dir.join("profile")
    };

    let settings_path = dir.join("settings.json");
    let settings: Settings = fs::read_to_string(&settings_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

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

    // своя фотография для главного экрана
    let bg_path = dir.join("background.txt");
    let bg_data = fs::read_to_string(&bg_path)
        .ok()
        .filter(|d| d.starts_with("data:image/") && d.len() < BG_MAX_LEN);

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

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

    let window = WindowBuilder::new()
        .with_title(if incognito { "Lumen — Инкогнито" } else { "Lumen" })
        .with_inner_size(tao::dpi::LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(640.0, 400.0))
        .build(&event_loop)
        .expect("не удалось создать окно");

    let mut ctx = WebContext::new(Some(profile_dir.clone()));

    let p_ui = proxy.clone();
    let p_ready = proxy.clone();
    let ui = WebViewBuilder::with_web_context(&mut ctx)
        .with_devtools(cfg!(debug_assertions))
        .with_incognito(incognito)
        .with_browser_extensions_enabled(!incognito) // у всех WebView одного профиля параметры должны совпадать
        .with_additional_browser_args(BROWSER_ARGS) // у всех WebView одного профиля аргументы должны совпадать
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

    let mut app = App {
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
    };
    app.new_tab(None);
    app.spawn_filters(false);

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                if app.incognito {
                    app.tabs.clear();
                    schedule_cleanup(&app.profile_dir);
                }
                *flow = ControlFlow::Exit
            }
            Event::WindowEvent { event: WindowEvent::Resized(_), .. }
            | Event::WindowEvent { event: WindowEvent::ScaleFactorChanged { .. }, .. } => {
                app.layout()
            }
            Event::UserEvent(ev) => app.handle(ev),
            _ => {}
        }
    });
}
