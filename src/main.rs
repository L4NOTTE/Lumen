#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod blocker;

use std::{
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use blocker::Blocker;
use serde::{Deserialize, Serialize};
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{Window, WindowBuilder},
};
use wry::{
    dpi::{LogicalPosition, LogicalSize},
    Rect, WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows,
};

const TOOLBAR_H: f64 = 80.0;
const UI_HTML: &str = include_str!("../ui/ui.html");
const NEWTAB_HTML: &str = include_str!("../ui/newtab.html");
/// SmartScreen остаётся включённым (мы его не отключаем), перевод страниц выключен.
const BROWSER_ARGS: &str =
    "--enable-features=ParallelDownloading --disable-features=Translate,msWebOOUI,msPdfOOUI";

fn search_url(engine: &str) -> &'static str {
    match engine {
        "yandex" => "https://yandex.ru/search/?text=",
        "duckduckgo" => "https://duckduckgo.com/?q=",
        _ => "https://www.google.com/search?q=",
    }
}

// ---------- настройки ----------

#[derive(Serialize, Deserialize, Clone)]
struct Settings {
    engine: String,
    adblock: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { engine: "google".into(), adblock: true }
    }
}

// ---------- события ----------

enum UserEvent {
    Ui(String),
    Title(u32, String),
    Url(u32, String),
    Navigate(u32, String),
    Search(u32, String),
    NewTab(String),
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
}

// ---------- приложение ----------

struct Tab {
    id: u32,
    webview: WebView,
    title: String,
    url: String,
}

struct App {
    window: Window,
    ui: WebView,
    ctx: WebContext,
    tabs: Vec<Tab>,
    active: u32,
    next_id: u32,
    settings: Settings,
    settings_path: PathBuf,
    blocker: Rc<Blocker>,
    adblock: Arc<AtomicBool>,
    proxy: EventLoopProxy<UserEvent>,
}

fn logical_size(w: &Window) -> (f64, f64) {
    let s = w.inner_size().to_logical::<f64>(w.scale_factor());
    (s.width, s.height)
}

fn content_rect(w: &Window) -> Rect {
    let (cw, ch) = logical_size(w);
    Rect {
        position: LogicalPosition::new(0.0, TOOLBAR_H).into(),
        size: LogicalSize::new(cw, (ch - TOOLBAR_H).max(0.0)).into(),
    }
}

fn ui_rect(w: &Window) -> Rect {
    let (cw, _) = logical_size(w);
    Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: LogicalSize::new(cw, TOOLBAR_H).into(),
    }
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
        if let Ok(s) = serde_json::to_string_pretty(&self.settings) {
            let _ = fs::write(&self.settings_path, s);
        }
    }

    fn make_webview(&mut self, id: u32, url: Option<String>) -> WebView {
        let rect = content_rect(&self.window);
        let (p_title, p_url, p_win, p_nav) = (
            self.proxy.clone(),
            self.proxy.clone(),
            self.proxy.clone(),
            self.proxy.clone(),
        );
        let blocker = self.blocker.clone();
        let adblock = self.adblock.clone();

        let mut b = WebViewBuilder::new_with_web_context(&mut self.ctx)
            .with_devtools(cfg!(debug_assertions))
            .with_additional_browser_args(BROWSER_ARGS)
            .with_bounds(rect)
            .with_document_title_changed_handler(move |t| {
                let _ = p_title.send_event(UserEvent::Title(id, t));
            })
            .with_on_page_load_handler(move |_, u| {
                let _ = p_url.send_event(UserEvent::Url(id, u));
            })
            .with_new_window_req_handler(move |u| {
                let _ = p_win.send_event(UserEvent::NewTab(u));
                false
            })
            .with_navigation_handler(move |u| {
                let Ok(mut parsed) = url::Url::parse(&u) else { return true };

                // поисковая строка стартовой страницы
                if parsed.host_str() == Some("lumen.invalid") {
                    if let Some((_, q)) = parsed.query_pairs().find(|(k, _)| k == "q") {
                        let _ = p_nav.send_event(UserEvent::Search(id, q.into_owned()));
                    }
                    return false;
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

        if self.settings.adblock {
            b = b.with_initialization_script(self.blocker.script());
        }
        b = match url {
            Some(u) => b.with_url(u),
            None => b.with_html(NEWTAB_HTML),
        };
        b.build_as_child(&self.window).expect("не удалось создать WebView2")
    }

    fn new_tab(&mut self, url: Option<String>) {
        let id = self.next_id;
        self.next_id += 1;
        let webview = self.make_webview(id, url);
        self.tabs.push(Tab { id, webview, title: "Новая вкладка".into(), url: String::new() });
        self.active = id;
        self.layout();
        self.push_state();
    }

    fn layout(&self) {
        let _ = self.ui.set_bounds(ui_rect(&self.window));
        for t in &self.tabs {
            let _ = t.webview.set_bounds(content_rect(&self.window));
            let _ = t.webview.set_visible(t.id == self.active);
        }
        if let Some(t) = self.tabs.iter().find(|t| t.id == self.active) {
            let _ = t.webview.focus();
        }
    }

    fn push_state(&self) {
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .map(|t| serde_json::json!({ "id": t.id, "title": t.title, "url": t.url }))
            .collect();
        let state = serde_json::json!({
            "tabs": tabs,
            "active": self.active,
            "engine": self.settings.engine,
            "adblock": self.settings.adblock,
        });
        let _ = self.ui.evaluate_script(&format!("window.onState({state})"));
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == self.active)
    }

    fn rebuild_tabs(&mut self) {
        let old: Vec<(u32, String)> = self.tabs.iter().map(|t| (t.id, t.url.clone())).collect();
        self.tabs.clear();
        for (id, url) in old {
            let u = if url.is_empty() { None } else { Some(url) };
            let webview = self.make_webview(id, u);
            self.tabs.push(Tab { id, webview, title: "Новая вкладка".into(), url: String::new() });
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
                } else {
                    if self.active == id {
                        self.active = self.tabs.last().map(|t| t.id).unwrap_or(0);
                    }
                    self.layout();
                    self.push_state();
                }
            }
            Cmd::Switch { id } => {
                self.active = id;
                self.layout();
                self.push_state();
            }
            Cmd::Go { text } => {
                let u = self.resolve(&text);
                if let Some(t) = self.active_tab() {
                    let _ = t.webview.load_url(&u);
                }
            }
            Cmd::Back => {
                if let Some(t) = self.active_tab() {
                    let _ = t.webview.evaluate_script("history.back()");
                }
            }
            Cmd::Forward => {
                if let Some(t) = self.active_tab() {
                    let _ = t.webview.evaluate_script("history.forward()");
                }
            }
            Cmd::Reload => {
                if let Some(t) = self.active_tab() {
                    let _ = t.webview.evaluate_script("location.reload()");
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
        }
    }

    fn handle(&mut self, ev: UserEvent) {
        match ev {
            UserEvent::Ui(s) => {
                if let Ok(c) = serde_json::from_str::<Cmd>(&s) {
                    self.cmd(c);
                }
            }
            UserEvent::Title(id, t) => {
                if let Some(tab) = self.tabs.iter_mut().find(|x| x.id == id) {
                    tab.title = t;
                }
                self.push_state();
            }
            UserEvent::Url(id, u) => {
                let u = if u.starts_with("about:") || u.starts_with("data:") { String::new() } else { u };
                if let Some(tab) = self.tabs.iter_mut().find(|x| x.id == id) {
                    tab.url = u;
                }
                self.push_state();
            }
            UserEvent::Navigate(id, u) => {
                if let Some(t) = self.tabs.iter().find(|x| x.id == id) {
                    let _ = t.webview.load_url(&u);
                }
            }
            UserEvent::Search(id, q) => {
                let u = self.resolve(&q);
                if let Some(t) = self.tabs.iter().find(|x| x.id == id) {
                    let _ = t.webview.load_url(&u);
                }
            }
            UserEvent::NewTab(u) => self.new_tab(Some(u)),
        }
    }
}

fn main() {
    // профиль и настройки: %LOCALAPPDATA%\Lumen
    let dir = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Lumen");
    let filters_dir = dir.join("filters");
    let _ = fs::create_dir_all(&filters_dir);

    let settings_path = dir.join("settings.json");
    let settings: Settings = fs::read_to_string(&settings_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("Lumen")
        .with_inner_size(tao::dpi::LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(640.0, 400.0))
        .build(&event_loop)
        .expect("не удалось создать окно");

    let mut ctx = WebContext::new(Some(dir.join("profile")));

    let p_ui = proxy.clone();
    let ui = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_html(UI_HTML)
        .with_bounds(ui_rect(&window))
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
        adblock: Arc::new(AtomicBool::new(settings.adblock)),
        settings,
        settings_path,
        blocker: Rc::new(Blocker::load(&filters_dir)),
        proxy,
    };
    app.new_tab(None);

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
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
