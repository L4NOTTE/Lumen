//! Linux frontend: GTK 3 / WebKitGTK 4.1. No Windows or Chromium COM dependencies.
use crate::blocker::Blocker;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    cell::Cell,
    collections::HashMap,
    fs,
    path::PathBuf,
    rc::Rc,
    time::{SystemTime, UNIX_EPOCH},
};
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::unix::WindowExtUnix,
    window::{Fullscreen, Window, WindowBuilder},
};
use webkit2gtk::{SettingsExt as _, WebInspectorExt as _, WebViewExt as _};
use wry::{
    dpi::{LogicalPosition, LogicalSize},
    PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder, WebViewBuilderExtUnix,
    WebViewExtUnix,
};

const BAR: f64 = 80.0;
#[cfg(test)]
mod hardening_tests {
    #[test]
    fn glib_string_iterator_backport() {
        use gtk::glib::variant::ToVariant;
        let value = vec!["alpha", "beta", "gamma"].to_variant();
        let mut iter = value.array_iter_str().unwrap();
        assert_eq!(iter.next(), Some("alpha"));
        assert_eq!(iter.next_back(), Some("gamma"));
        assert_eq!(iter.last(), Some("beta"));
        assert_eq!(value.array_iter_str().unwrap().nth(1),Some("beta"));
    }
}
const UI: &str = include_str!(concat!(env!("OUT_DIR"),"/ui.html"));
const NEWTAB: &str = include_str!(concat!(env!("OUT_DIR"), "/newtab.html"));
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    engine: String,
    theme: String,
    accent: String,
    font: String,
    font_size: String,
    bg_mode: String,
    startup: String,
    adblock: bool,
    hide_labels: bool,
    site_zoom: HashMap<String, f64>,
    background: Option<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            engine: "google".into(),
            theme: "system".into(),
            accent: "indigo".into(),
            font: "system".into(),
            font_size: "normal".into(),
            bg_mode: "none".into(),
            startup: "restore".into(),
            adblock: true,
            hide_labels: true,
            site_zoom: HashMap::new(),
            background: None,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Link {
    t: String,
    u: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct History {
    t: String,
    u: String,
    ts: u64,
}
#[derive(Default, Serialize, Deserialize)]
struct Session {
    active: usize,
    tabs: Vec<Link>,
}
enum Msg {
    Notice(String),
    OpenTab(String,bool),
    DownloadPoll,
    Ui(Value),
    NewTabCommand(u32,Value),
    Title(u32, String),
    Load(u32, String, bool),
    Zoom(u32, f64),
    Fullscreen(bool),
    Inspector(u32, bool),
    Quit,
}
struct Tab {
    id: u32,
    wv: WebView,
    container: gtk::Box,
    url: String,
    title: String,
    loading: bool,
    zoom: f64,
}
struct App {
    newtab_token: String,
    downloads: crate::downloads::Shared,
    window: Window,
    root: gtk::Box,
    chrome: gtk::Box,
    stack: gtk::Stack,
    paned: gtk::Paned,
    inspector: gtk::Box,
    ctx: WebContext,
    ui: WebView,
    tabs: Vec<Tab>,
    active: u32,
    next: u32,
    config: Config,
    dir: PathBuf,
    private: bool,
    links: Vec<Link>,
    history: Vec<History>,
    closed: Vec<String>,
    panel: String,
    dev: bool,
    full: bool,
    proxy: EventLoopProxy<Msg>,
    blocker: Rc<Blocker>,
    smoke: bool,
    smoke_started: bool,
    last_state: String,
}
fn zoom_key(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.starts_with("about:") || raw.starts_with("data:") {
        return Some("lumen:newtab".into());
    }
    let u = url::Url::parse(raw).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.origin().ascii_serialization())
}
fn safe(raw: &str) -> bool {
    url::Url::parse(raw)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}
fn address(raw: &str, engine: &str) -> String {
    let text = raw.trim();
    if text.is_empty() {
        return String::new();
    }
    if safe(text) {
        return text.into();
    }
    if !text.contains(char::is_whitespace)
        && !text.contains("://")
        && (text.contains('.') || text.starts_with("localhost"))
    {
        let candidate = format!("https://{text}");
        if safe(&candidate) {
            return candidate;
        }
    }
    let base = match engine {
        "yandex" => "https://yandex.ru/search/?text=",
        "duckduckgo" => "https://duckduckgo.com/?q=",
        _ => "https://www.google.com/search?q=",
    };
    format!("{base}{}", urlencoding::encode(text))
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect {
        position: LogicalPosition::new(x, y).into(),
        size: LogicalSize::new(w.max(1.0), h.max(1.0)).into(),
    }
}
fn accent(name: &str) -> (&str, &str) {
    if crate::security::hex_color(name) { return (name, name); }
    match name {
        "blue" => ("#2563eb", "#60a5fa"),
        "green" => ("#16a34a", "#4ade80"),
        "orange" => ("#ea580c", "#fb923c"),
        "pink" => ("#db2777", "#f472b6"),
        "red" => ("#dc2626", "#f87171"),
        _ => ("#4f46e5", "#8b8dff"),
    }
}
fn font(name: &str) -> &'static str {
    match name {
        "arial" => "Arial,'Liberation Sans',sans-serif",
        "verdana" => "Verdana,'DejaVu Sans',sans-serif",
        "georgia" => "Georgia,'Liberation Serif',serif",
        "trebuchet" => "'Trebuchet MS','DejaVu Sans',sans-serif",
        "consolas" => "Consolas,'DejaVu Sans Mono',monospace",
        _ => "system-ui,sans-serif",
    }
}
fn font_size(name: &str) -> u32 {
    match name {
        "small" => 12,
        "large" => 15,
        _ => 13,
    }
}
fn gradient(name: &str) -> &'static str {
    match name {
        "g1" => "linear-gradient(135deg,#4f46e5,#9333ea 50%,#ec4899)",
        "g2" => "linear-gradient(135deg,#0ea5e9,#22d3ee 50%,#34d399)",
        "g3" => "linear-gradient(160deg,#0f172a,#1e293b 50%,#334155)",
        "g4" => "linear-gradient(135deg,#f97316,#ef4444 50%,#7c3aed)",
        _ => "",
    }
}
fn read<T: serde::de::DeserializeOwned + Default>(path: PathBuf) -> T {
    crate::security::read_profile(&path,6_000_000)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
fn write<T: Serialize>(path: PathBuf, value: &T) {
    if let Ok(data) = serde_json::to_vec_pretty(value) {
        if let Err(error) = crate::security::atomic_write(&path,&data) {
            eprintln!("Lumen: cannot save {}: {error}",path.display());
        }
    }
}
fn mount_inspector(
    inspector: &webkit2gtk::WebInspector,
    host: &gtk::Box,
    proxy: &EventLoopProxy<Msg>,
    id: u32,
) -> bool {
    if let Some(view) = inspector.web_view() {
        if view.parent().as_ref() != Some(host.upcast_ref()) {
            if let Some(parent) = view
                .parent()
                .and_then(|p| p.downcast::<gtk::Container>().ok())
            {
                parent.remove(&view)
            }
            host.pack_start(&view, true, true, 0);
        }
        view.set_hexpand(true);
        view.set_vexpand(true);
        host.show_all();
        let _ = proxy.send_event(Msg::Inspector(id, true));
        return true;
    }
    false
}
impl App {
    fn save(&self) {
        if !self.private {
            write(self.dir.join("settings.json"), &self.config);
            write(self.dir.join("links.json"), &self.links);
            write(self.dir.join("history.json"), &self.history);
        }
    }
    fn save_session(&self) {
        if self.private {
            return;
        }
        let tabs: Vec<_> = self.tabs.iter().filter(|t| safe(&t.url)).collect();
        write(
            self.dir.join("session.json"),
            &Session {
                active: tabs.iter().position(|t| t.id == self.active).unwrap_or(0),
                tabs: tabs
                    .iter()
                    .map(|t| Link {
                        t: t.title.clone(),
                        u: crate::security::persisted_url(&t.url),
                    })
                    .collect(),
            },
        );
    }
    fn toast(&self, text: &str) {
        let _ = self
            .ui
            .evaluate_script(&format!("window.onToast({})", json!(text)));
    }
    fn newtab(&self) -> String {
        let (al, ad) = accent(&self.config.accent);
        let vars = format!(
            ":root{{--acc-l:{al};--acc-d:{ad};--font:{};--fs:{}px}}",
            font(&self.config.font),
            font_size(&self.config.font_size)
        );
        let bg = if self.private {
            String::new()
        } else if self.config.bg_mode == "custom" {
            self.config
                .background
                .as_ref()
                .filter(|d|crate::security::image_data(d))
                .map(|d| {
                    format!(
                        "body{{background:url({}) center/cover no-repeat fixed}}",
                        json!(d)
                    )
                })
                .unwrap_or_default()
        } else {
            let g = gradient(&self.config.bg_mode);
            if g.is_empty() {
                String::new()
            } else {
                format!("body{{background:{g} fixed}}")
            }
        };
        NEWTAB
            .replace("__NEWTAB_TOKEN__", &self.newtab_token)
            .replace(
                "/*__LINKS__*/[]",
                &if self.private {
                    "[]".into()
                } else {
                    json!(self.links.iter().enumerate().filter(|(_,link)|crate::security::web_link(&link.u)).map(|(i,link)|json!({"i":i,"t":link.t,"u":link.u})).collect::<Vec<_>>()).to_string().replace("</", "<\\/")
                },
            )
            .replace("__INC__", if self.private { "1" } else { "0" })
            .replace(
                "__THEME__",
                if self.private {
                    "dark"
                } else {
                    match self.config.theme.as_str() { "dark"=>"dark", "light"=>"light", _=>"system" }
                },
            )
            .replace("__BGATTR__", if bg.is_empty() { "0" } else { "1" })
            .replace("/*__VARS__*/", &vars)
            .replace("/*__BGCSS__*/", &bg)
    }
    fn tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == self.active)
    }
    fn open(&mut self, url: Option<String>) {
        self.open_tab(url,true);
    }
    fn open_tab(&mut self, url: Option<String>, foreground: bool) {
        let id = self.next;
        self.next += 1;
        let initial = url.unwrap_or_default();
        let html = self.newtab();
        let (p_ui, p_title, p_load) = (
            self.proxy.clone(),
            self.proxy.clone(),
            self.proxy.clone(),
        );
        let newtab = Rc::new(Cell::new(initial.is_empty()));
        let trusted = newtab.clone();
        let engine = self.config.engine.clone();
        let blocker = self.blocker.clone();
        let enabled = self.config.adblock;
        let newtab_token = self.newtab_token.clone();
        let navigation=move|u:String|{
                if let Ok(parsed)=url::Url::parse(&u){
                    if parsed.host_str()==Some("lumen.invalid"){
                        if trusted.get() && crate::security::newtab_request(&parsed,&newtab_token){let args:HashMap<_,_>=parsed.query_pairs().into_owned().collect();let cmd=match parsed.path(){"/s"=>json!({"cmd":"go","text":args.get("q").cloned().unwrap_or_default(),"engine":engine}),"/add"=>json!({"cmd":"add_link","title":args.get("t"),"url":args.get("u")}),"/del"=>json!({"cmd":"delete_link","index":args.get("i").and_then(|v|v.parse::<usize>().ok())}),_=>json!({"cmd":"noop"})};let _=p_ui.send_event(Msg::NewTabCommand(id,cmd));}return false
                    }
                    if matches!(parsed.scheme(),"about"|"data"){return trusted.get()}
                    if safe(&u){trusted.set(false);return !enabled||!blocker.is_blocked(&u)}
                }
                false
                    };
        let mut builder=WebViewBuilder::with_web_context(&mut self.ctx).with_devtools(false).with_incognito(self.private).with_focused(foreground)
            .with_bounds(rect(0.0,BAR,900.0,600.0))
            .with_document_title_changed_handler(move|t|{let _=p_title.send_event(Msg::Title(id,t));})
            .with_on_page_load_handler(move|ev,u|{let _=p_load.send_event(Msg::Load(id,u,matches!(ev,PageLoadEvent::Started)));})
;
        if enabled {
            builder = builder
                .with_initialization_script(self.blocker.script())
                .with_initialization_script(&format!(
                    "window.__lumenCfg={{labels:{}}};{}",
                    self.config.hide_labels,
                    include_str!("runtime.js")
                ));
        }
        builder = if initial.is_empty() {
            builder.with_html(html)
        } else {
            builder.with_url(&initial)
        };
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let wv = match builder.build_gtk(&container) {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("Lumen: {e}");
                self.toast("Не удалось открыть вкладку");
                return;
            }
        };
        self.stack.add_named(&container, &format!("tab-{id}"));
        container.show_all();
        crate::downloads::install(&wv, &self.downloads);
        let native = wv.webview();
        native.connect_permission_request(|_,request| {
            use webkit2gtk::PermissionRequestExt as _;
            request.deny();
            true
        });
        if let Some(manager)=native.website_data_manager(){
            use webkit2gtk::WebsiteDataManagerExt as _;
            manager.set_itp_enabled(true);
        }
        let p=self.proxy.clone();
        native.connect_decide_policy(move|_,decision,kind| {
            use webkit2gtk::{NavigationPolicyDecisionExt as _,PolicyDecisionExt as _,URIRequestExt as _};
            let Some(nav)=decision.downcast_ref::<webkit2gtk::NavigationPolicyDecision>() else {return false;};
            let Some(action)=nav.navigation_action() else {return false;};
            let middle=action.mouse_button()==2;
            if !middle && kind!=webkit2gtk::PolicyDecisionType::NewWindowAction {
                if kind!=webkit2gtk::PolicyDecisionType::NavigationAction {return false;}
                let allowed=action.request().and_then(|r|r.uri()).is_some_and(|uri|navigation(uri.to_string()));
                if allowed {decision.use_();} else {decision.ignore();}
                return true;
            }
            if let Some(uri)=action.request().and_then(|r|r.uri()) {
                if safe(&uri) {let _=p.send_event(Msg::OpenTab(uri.to_string(),!middle));}
            }
            decision.ignore();
            true
        });
        let p = self.proxy.clone();
        native.connect_zoom_level_notify(move |w| {
            let _ = p.send_event(Msg::Zoom(id, w.zoom_level()));
        });
        native.connect_scroll_event(|w, event| {
            if event.state().contains(gdk::ModifierType::CONTROL_MASK) {
                let step = match event.direction() {
                    gdk::ScrollDirection::Up => 0.1,
                    gdk::ScrollDirection::Down => -0.1,
                    _ => {
                        let (_, dy) = event.delta();
                        if dy < 0.0 {
                            0.1
                        } else {
                            -0.1
                        }
                    }
                };
                w.set_zoom_level((w.zoom_level() + step).clamp(0.25, 5.0));
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
        let p = self.proxy.clone();
        native.connect_enter_fullscreen(move |_| {
            let _ = p.send_event(Msg::Fullscreen(true));
            true
        });
        let p = self.proxy.clone();
        native.connect_leave_fullscreen(move |_| {
            let _ = p.send_event(Msg::Fullscreen(false));
            true
        });
        if let Some(inspector) = native.inspector() {
            let host = self.inspector.clone();
            let p = self.proxy.clone();
            inspector.connect_open_window(move |i| mount_inspector(i, &host, &p, id));
            let host = self.inspector.clone();
            let p = self.proxy.clone();
            inspector.connect_attach(move |i| mount_inspector(i, &host, &p, id));
            let p = self.proxy.clone();
            inspector.connect_closed(move |_| {
                let _ = p.send_event(Msg::Inspector(id, false));
            });
        }
        let factor = zoom_key(&initial)
            .and_then(|k| self.config.site_zoom.get(&k).copied())
            .filter(|z| z.is_finite() && (0.25..=5.0).contains(z))
            .unwrap_or(1.0);
        let _ = wv.zoom(factor);
        self.tabs.push(Tab {
            id,
            wv,
            container,
            url: initial,
            title: "Новая вкладка".into(),
            loading: false,
            zoom: factor,
        });
        if foreground {self.activate(id);} else {self.layout();self.push();self.save_session();}
    }
    fn close_inspector(&mut self) {
        if let Some(t) = self.tab() {
            if let Some(i) = t.wv.webview().inspector() {
                i.close();
            }
        }
        for child in self.inspector.children() {
            self.inspector.remove(&child);
        }
        self.dev = false;
        self.inspector.hide();
    }
    fn activate(&mut self, id: u32) {
        if !self.tabs.iter().any(|t| t.id == id) {
            return;
        }
        if self.dev {
            self.close_inspector()
        }
        self.active = id;
        self.panel.clear();
        self.layout();
        if let Some(t) = self.tab() {
            let _ = t.wv.focus();
        }
        self.push();
        self.save_session();
    }
    fn layout(&self) {
        let panel = !self.panel.is_empty() && !self.full;
        self.root
            .set_child_packing(&self.chrome, panel, true, 0, gtk::PackType::Start);
        self.chrome
            .set_size_request(-1, if panel { -1 } else { BAR as i32 });
        self.chrome.set_visible(!self.full);
        self.paned.set_visible(!panel);
        self.stack
            .set_visible_child_name(&format!("tab-{}", self.active));
        self.inspector.set_visible(self.dev && !panel && !self.full);
        if self.dev && self.paned.position() < 240 {
            let width = self
                .window
                .inner_size()
                .to_logical::<f64>(self.window.scale_factor())
                .width;
            self.paned.set_position((width * 0.55) as i32);
        }
    }
    fn push(&mut self) {
        let c = &self.config;
        let (al, ad) = accent(&c.accent);
        let tabs:Vec<_>=self.tabs.iter().map(|t|json!({"id":t.id,"title":t.title,"url":t.url,"loading":t.loading,"sleeping":false})).collect();
        let s=json!({"download_count":self.downloads.borrow().active_count(),"platform":"linux","version":env!("CARGO_PKG_VERSION"),"tabs":tabs,"active":self.active,"zoom":self.tab().map(|t|(t.zoom*100.0).round()).unwrap_or(100.0),"panel":self.panel,"dev_open":self.dev,"maximized":self.window.is_maximized(),"incognito":self.private,"engine":c.engine,"adblock":c.adblock,
          "set":{"theme":c.theme,"accent":c.accent,"font":c.font,"font_size":c.font_size,"bg_mode":c.bg_mode,"has_bg":c.background.is_some(),"startup":c.startup,"labels":c.hide_labels,"sleep":0,"eco":false},"look":{"al":al,"ad":ad,"font":font(&c.font),"fs":font_size(&c.font_size)},"filters":{"level":"basic","status":"Базовая блокировка и скрытие рекламных карточек. Некоторые объявления могут сохраняться."},"block":{"count":0,"recent":[]}}).to_string();
        if s != self.last_state {
            let _ = self.ui.evaluate_script(&format!("window.onState({s})"));
            self.last_state = s;
        }
    }
    fn push_downloads(&self) {
        let data = serde_json::to_string(self.downloads.borrow().records())
            .unwrap_or_else(|_| "[]".into());
        let _ = self
            .ui
            .evaluate_script(&format!("window.onDownloads({data})"));
    }
    fn refresh_newtabs(&mut self) {
        let html = self.newtab();
        for t in &self.tabs {
            if t.url.is_empty() {
                let _ = t.wv.load_html(&html);
            }
        }
    }
    fn zoom(&mut self, step: i32) {
        let factor = self
            .tab()
            .map(|t| {
                if step == 0 {
                    1.0
                } else {
                    (t.zoom + step as f64 * 0.1).clamp(0.25, 5.0)
                }
            })
            .unwrap_or(1.0);
        let id = self.active;
        self.user_zoom(id, factor);
        if let Some(t) = self.tab() {
            let _ = t.wv.zoom(factor);
        }
    }
    fn user_zoom(&mut self, id: u32, factor: f64) {
        if !factor.is_finite() || !(0.25..=5.0).contains(&factor) {
            return;
        }
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            t.zoom = factor;
            if let Some(key) = zoom_key(&t.url) {
                if (factor - 1.0).abs() < 0.0001 {
                    self.config.site_zoom.remove(&key);
                } else {
                    self.config.site_zoom.insert(key, factor);
                }
                self.save();
            }
        }
        self.push();
    }
    fn rebuild(&mut self) {
        self.close_inspector();
        let old: Vec<_> = self.tabs.iter().map(|t| t.url.clone()).collect();
        let active = self
            .tabs
            .iter()
            .position(|t| t.id == self.active)
            .unwrap_or(0);
        for tab in &self.tabs {
            self.stack.remove(&tab.container);
        }
        self.tabs.clear();
        for url in old {
            self.open(if url.is_empty() { None } else { Some(url) });
        }
        if let Some(id) = self.tabs.get(active).map(|t| t.id) {
            self.activate(id);
        }
    }
    fn command(&mut self, v: Value) {
        let text = |name: &str| v[name].as_str().unwrap_or("").to_string();
        let id = v["id"].as_u64().unwrap_or(self.active as u64) as u32;
        match v["cmd"].as_str().unwrap_or("") {
            "ready" => {
                self.last_state.clear();
                self.push();
                if self.panel == "downloads" {
                    self.push_downloads();
                }
                if self.smoke && !self.smoke_started {
                    self.smoke_started = true;
                    self.open(None);
                    self.zoom(1);
                    let id = self.tabs[0].id;
                    self.command(json!({"cmd":"drop_tab","id":id,"index":1}));
                    let _=self.ui.evaluate_script("setTimeout(()=>window.ipc.postMessage(JSON.stringify({cmd:'smoke_result',tabs:state.tabs.length,zoom:state.zoom,platform:state.platform,dom:document.querySelectorAll('#tabs .tab:not(.out)').length})),250)");
                }
            }
            "smoke_result" if self.smoke => {
                if v["tabs"] == 2 && v["dom"] == 2 && v["zoom"] == 110 && v["platform"] == "linux" {
                    println!("LUMEN_LINUX_SMOKE_OK");
                    let _ = self.proxy.send_event(Msg::Quit);
                } else {
                    eprintln!("Linux smoke failed: {v}");
                    std::process::exit(1)
                }
            }
            "download_action" => {
                self.downloads
                    .borrow_mut()
                    .action(&text("id"), &text("action"));
                self.push_downloads();
                self.push();
            }
            "clear_downloads" => {
                self.downloads.borrow_mut().clear();
                self.push_downloads();
            }
            "downloads_folder" => self.downloads.borrow().open_folder(),
            "default_browser" => {
                let proxy=self.proxy.clone();
                std::thread::spawn(move|| {
                    let message=match crate::linux_desktop::set_default() {
                        Ok(())=>"Lumen выбран браузером по умолчанию".into(),
                        Err(error)=>error.to_string(),
                    };
                    let _=proxy.send_event(Msg::Notice(message));
                });
            }
            "new_tab" => self.open(None),
            "switch" => self.activate(id),
            "close_tab" => {
                if let Some(pos) = self.tabs.iter().position(|t| t.id == id) {
                    if self.dev {
                        self.close_inspector()
                    }
                    let t = self.tabs.remove(pos);
                    self.stack.remove(&t.container);
                    self.closed.push(t.url);
                    if self.tabs.is_empty() {
                        self.open(None)
                    } else {
                        let next = self.tabs[pos.min(self.tabs.len() - 1)].id;
                        self.activate(if self.active == id { next } else { self.active });
                    }
                }
            }
            "drop_tab" => {
                if v["detach"].as_bool() == Some(true) {
                    self.toast("Перенос вкладок между окнами в Linux пока не поддерживается");
                }
                if let Some(from) = self.tabs.iter().position(|t| t.id == id) {
                    let t = self.tabs.remove(from);
                    let to = (v["index"].as_u64().unwrap_or(0) as usize).min(self.tabs.len());
                    self.tabs.insert(to, t);
                    self.push();
                    self.save_session();
                }
            }
            "go" | "open_url" => {
                let url = address(
                    &if v["cmd"] == "go" {
                        text("text")
                    } else {
                        text("url")
                    },
                    &self.config.engine,
                );
                if !url.is_empty() {
                    if v["cmd"] == "open_url" {
                        self.open(Some(url));
                        return;
                    }
                    self.panel.clear();
                    self.layout();
                    if let Some(t) = self.tab() {
                        let _ = t.wv.load_url(&url);
                    }
                }
            }
            "back" => {
                if let Some(t) = self.tab() {
                    t.wv.webview().go_back();
                }
            }
            "forward" => {
                if let Some(t) = self.tab() {
                    t.wv.webview().go_forward();
                }
            }
            "reload" => {
                if let Some(t) = self.tab() {
                    if t.url.is_empty() { let _ = t.wv.load_html(&self.newtab()); }
                    else { t.wv.webview().reload(); }
                }
            }
            "zoom_in" => self.zoom(1),
            "zoom_out" => self.zoom(-1),
            "zoom_reset" => self.zoom(0),
            "dev_tools" | "native_dev_tools" => {
                if self.private { return; }
                if let Some(tab) = self.tab() {
                    let native = tab.wv.webview();
                    if let (Some(settings), Some(inspector)) = (native.settings(), native.inspector()) {
                        settings.set_enable_developer_extras(true);
                        inspector.show();
                    }
                }
            }
            "dev_close" => {
                self.close_inspector();
                self.layout();
                self.push();
            }
            "open_panel" => {
                let panel = text("name");
                if panel == "dev" {
                    self.command(json!({"cmd":"dev_tools"}));
                    return;
                }
                if panel == "ext" {
                    self.toast("Расширения Chrome и Shazam требуют Windows/WebView2");
                    return;
                }
                self.panel = panel;
                if self.panel == "downloads" {
                    self.push_downloads();
                }
                if self.panel == "history" {
                    let _ = self
                        .ui
                        .evaluate_script(&format!("window.onHistory({})", json!(self.history)));
                }
                self.layout();
                self.push();
            }
            "close_panel" => {
                self.panel.clear();
                self.layout();
                self.push();
            }
            "set_engine" => {
                let engine = text("engine");
                if ["google", "yandex", "duckduckgo"].contains(&engine.as_str()) {
                    self.config.engine = engine;
                    self.save();
                    self.push();
                }
            }
            "set_theme" | "set_accent" | "set_font" | "set_font_size" | "set_look" => {
                for key in ["theme", "accent", "font", "font_size", "bg_mode"] {
                    if let Some(value) = v[key].as_str().or_else(|| v["look"][key].as_str()) {
                        if value.len() > 32 {
                            continue;
                        }
                        match key {
                            "theme" => self.config.theme = value.into(),
                            "accent" if crate::security::hex_color(value)
                                || matches!(value, "indigo" | "blue" | "green" | "orange" | "pink" | "red") => {
                                self.config.accent = value.into();
                            }
                            "font" => self.config.font = value.into(),
                            "font_size" => self.config.font_size = value.into(),
                            "bg_mode" => self.config.bg_mode = value.into(),
                            _ => {}
                        }
                    }
                }
                self.save();
                self.refresh_newtabs();
                self.push();
            }
            "set_bg_mode" => {
                let mode = text("mode");
                if ["none", "g1", "g2", "g3", "g4", "custom"].contains(&mode.as_str()) {
                    self.config.bg_mode = mode;
                    self.save();
                    self.refresh_newtabs();
                    self.push();
                }
            }
            "set_startup" => {
                let mode = text("mode");
                if ["new", "restore"].contains(&mode.as_str()) {
                    self.config.startup = mode;
                    self.save();
                    self.push();
                }
            }
            "set_adblock" | "toggle_adblock" => {
                self.config.adblock = v["on"].as_bool().unwrap_or(!self.config.adblock);
                self.save();
                self.rebuild();
            }
            "set_labels" => {
                self.config.hide_labels = v["on"].as_bool().unwrap_or(true);
                for t in &self.tabs {
                    let _ = t.wv.evaluate_script(&format!(
                        "window.__lumenSetLabels&&window.__lumenSetLabels({})",
                        self.config.hide_labels
                    ));
                }
                self.save();
                self.push();
            }
            "set_background" => {
                let data = text("data");
                if crate::security::image_data(&data) {
                    self.config.background = Some(data);
                    self.config.bg_mode = "custom".into();
                    self.save();
                    self.refresh_newtabs();
                    self.push();
                }
            }
            "clear_background" => {
                self.config.background = None;
                self.config.bg_mode = "none".into();
                self.save();
                self.refresh_newtabs();
                self.push();
            }
            "add_current_link" => {
                if let Some(t) = self.tab() {
                    if safe(&t.url) {
                        self.links.push(Link {
                            t: t.title.clone(),
                            u: crate::security::persisted_url(&t.url),
                        });
                        self.save();
                        self.refresh_newtabs();
                        self.toast("Ссылка добавлена на новую вкладку");
                    }
                }
            }
            "add_link" => {
                let u = address(&text("url"), &self.config.engine);
                if safe(&u) {
                    self.links.push(Link {
                        t: text("title"),
                        u,
                    });
                    self.save();
                    self.refresh_newtabs();
                }
            }
            "delete_link" => {
                if let Some(i) = v["index"].as_u64() {
                    if (i as usize) < self.links.len() {
                        self.links.remove(i as usize);
                        self.save();
                        self.refresh_newtabs();
                    }
                }
            }
            "delete_history" => {
                self.history
                    .retain(|h| !(h.ts == v["ts"].as_u64().unwrap_or(0) && h.u == text("u")));
                self.save();
                let _ = self
                    .ui
                    .evaluate_script(&format!("window.onHistory({})", json!(self.history)));
            }
            "clear_history" => {
                self.history.clear();
                self.save();
                let _ = self.ui.evaluate_script("window.onHistory([])");
            }
            "incognito" => {
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).arg("--incognito").spawn();
                }
            }
            "fullscreen" => {
                self.full = !self.full;
                self.window
                    .set_fullscreen(self.full.then_some(Fullscreen::Borderless(None)));
                self.layout();
            }
            "escape" => {
                if self.full {
                    if let Some(t) = self.tab() {
                        let _ = t.wv.evaluate_script(
                            "document.fullscreenElement&&document.exitFullscreen()",
                        );
                    }
                    self.full = false;
                    self.window.set_fullscreen(None);
                }
                self.panel.clear();
                self.layout();
                self.push();
            }
            "win_min" => self.window.set_minimized(true),
            "win_max" => {
                self.window.set_maximized(!self.window.is_maximized());
                self.push();
            }
            "win_close" => {
                let _ = self.proxy.send_event(Msg::Quit);
            }
            "win_resize" => {
                use tao::window::ResizeDirection::*;
                let edge = match text("edge").as_str() { "n"=>North,"ne"=>NorthEast,"nw"=>NorthWest,"e"=>East,"w"=>West,_=>return };
                let _ = self.window.drag_resize_window(edge);
            }
            "win_drag" => {
                let _ = self.window.drag_window();
            }
            "reopen" => {
                if let Some(url) = self.closed.pop() {
                    self.open(if url.is_empty() { None } else { Some(url) });
                }
            }
            "next_tab" | "prev_tab" => {
                if let Some(i) = self.tabs.iter().position(|t| t.id == self.active) {
                    let n = self.tabs.len();
                    let next = if v["cmd"] == "prev_tab" {
                        (i + n - 1) % n
                    } else {
                        (i + 1) % n
                    };
                    self.activate(self.tabs[next].id);
                }
            }
            "focus_url" => {
                self.panel.clear();
                self.layout();
                let _ = self.ui.focus();
                let _=self.ui.evaluate_script("document.getElementById('url').focus();document.getElementById('url').select()");
            }
            "frame_theme" | "noop" => {}
            _ => self.toast("Эта функция пока доступна только в Windows-версии"),
        }
    }
    fn event(&mut self, msg: Msg) {
        match msg {
            Msg::Notice(text)=>self.toast(&text),
            Msg::OpenTab(url,foreground)=>{if safe(&url){self.open_tab(Some(url),foreground);}},
            Msg::DownloadPoll => {
                let changed = self.downloads.borrow_mut().poll();
                if changed {
                    let notice = self.downloads.borrow_mut().take_notice();
                    if let Some(text) = notice {
                        self.toast(&text);
                    }
                    if self.panel == "downloads" {
                        self.push_downloads();
                    }
                    self.push();
                }
            }
            Msg::Ui(v) => self.command(v),
            Msg::NewTabCommand(id,v) => {if self.active == id {self.command(v);}},
            Msg::Title(id, title) => {
                if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                    t.title = title;
                    self.push();
                }
            }
            Msg::Load(id, url, loading) => {
                let u = if url.starts_with("about:") || url.starts_with("data:") {
                    String::new()
                } else {
                    url
                };
                if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                    t.url = u;
                    t.loading = loading;
                    let factor = zoom_key(&t.url)
                        .and_then(|k| self.config.site_zoom.get(&k).copied())
                        .filter(|z| z.is_finite() && (0.25..=5.0).contains(z))
                        .unwrap_or(1.0);
                    t.zoom = factor;
                    let _ = t.wv.zoom(factor);
                    if !loading && !self.private && safe(&t.url) {
                        self.history.push(History {
                            t: t.title.clone(),
                            u: crate::security::persisted_url(&t.url),
                            ts: SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                        });
                        if self.history.len() > 1000 {
                            self.history.remove(0);
                        }
                        self.save();
                    }
                    self.push();
                    self.save_session();
                }
            }
            Msg::Zoom(id, factor) => {
                if self
                    .tabs
                    .iter()
                    .any(|t| t.id == id && (t.zoom - factor).abs() > 0.0001)
                {
                    self.user_zoom(id, factor)
                }
            }
            Msg::Fullscreen(full) => {
                self.full = full;
                self.window
                    .set_fullscreen(full.then_some(Fullscreen::Borderless(None)));
                self.layout();
            }
            Msg::Inspector(id, open) => {
                if id == self.active {
                    self.dev = open;
                    self.layout();
                    self.push();
                }
            }
            Msg::Quit => {}
        }
    }
}
fn key_command(event: &gdk::EventKey) -> Option<Value> {
    use gdk::keys::constants as k;
    let ctrl = event.state().contains(gdk::ModifierType::CONTROL_MASK);
    let shift = event.state().contains(gdk::ModifierType::SHIFT_MASK);
    let alt = event.state().contains(gdk::ModifierType::MOD1_MASK);
    let key = event.keyval();
    let name = if key == k::F12 || (ctrl && shift && (key == k::I || key == k::i)) {
        "dev_tools"
    } else if key == k::F11 {
        "fullscreen"
    } else if key == k::Escape {
        "escape"
    } else if alt && key == k::Left {
        "back"
    } else if alt && key == k::Right {
        "forward"
    } else if ctrl && (key == k::t || key == k::T) {
        if shift {
            "reopen"
        } else {
            "new_tab"
        }
    } else if ctrl && (key == k::w || key == k::W) {
        "close_tab"
    } else if ctrl && (key == k::l || key == k::L) || key == k::F6 {
        "focus_url"
    } else if ctrl && (key == k::Tab || key == k::ISO_Left_Tab) {
        if shift {
            "prev_tab"
        } else {
            "next_tab"
        }
    } else if ctrl && (key == k::plus || key == k::equal || key == k::Up) {
        "zoom_in"
    } else if ctrl && (key == k::minus || key == k::Down) {
        "zoom_out"
    } else if ctrl && key == k::_0 {
        "zoom_reset"
    } else if ctrl && (key == k::r || key == k::R) || key == k::F5 {
        "reload"
    } else if ctrl && shift && (key == k::n || key == k::N) {
        "incognito"
    } else if ctrl && (key == k::j || key == k::J) {
        return Some(json!({"cmd":"open_panel","name":"downloads"}));
    } else if ctrl && (key == k::h || key == k::H) {
        return Some(json!({"cmd":"open_panel","name":"history"}));
    } else {
        return None;
    };
    Some(json!({"cmd":name}))
}
pub fn run() {
    std::env::remove_var("WEBKIT_INSPECTOR_SERVER");
    std::env::remove_var("WEBKIT_INSPECTOR_HTTP_SERVER");
    std::env::remove_var("WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS");
    let args: Vec<_> = std::env::args().collect();
    let private = args.iter().any(|s| s == "--incognito");
    let smoke = args.iter().any(|s| s == "--smoke-test");
    let dir = std::env::var_os("LUMEN_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("lumen")
        });
    crate::security::private_directory(&dir).expect("private settings directory");
    let mut config: Config = read(dir.join("settings.json"));
    config.background = config.background.filter(|data|crate::security::image_data(data));
    let links: Vec<Link> = read(dir.join("links.json"));
    let mut history: Vec<History> = if private {
        Vec::new()
    } else {
        read(dir.join("history.json"))
    };
    let mut session: Session = if private || smoke {
        Session::default()
    } else {
        read(dir.join("session.json"))
    };
    if !private && !smoke {
        let mut history_changed = false;
        for entry in &mut history {
            history_changed |= crate::security::redact_persisted_url(&mut entry.u);
        }
        if history_changed { write(dir.join("history.json"), &history); }
        let mut session_changed = false;
        for entry in &mut session.tabs {
            session_changed |= crate::security::redact_persisted_url(&mut entry.u);
        }
        if session_changed { write(dir.join("session.json"), &session); }
    }
    let event_loop = EventLoopBuilder::<Msg>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_window_icon(
            tao::window::Icon::from_rgba(include_bytes!("../assets/icon64.rgba").to_vec(), 64, 64)
                .ok(),
        )
        .with_title(format!(
            "Lumen {}{}",
            env!("CARGO_PKG_VERSION"),
            if private {
                " — Инкогнито"
            } else {
                ""
            }
        ))
        .with_decorations(false)
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .with_min_inner_size(LogicalSize::new(480.0, 360.0))
        .build(&event_loop)
        .expect("GTK window");
    let root = window.default_vbox().expect("GTK container").clone();
    let chrome = gtk::Box::new(gtk::Orientation::Vertical, 0);
    chrome.set_size_request(-1, BAR as i32);
    root.pack_start(&chrome, false, true, 0);
    chrome.show();
    let stack = gtk::Stack::new();
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    stack.show();
    let inspector = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.pack1(&stack, true, false);
    paned.pack2(&inspector, true, false);
    paned.set_position(700);
    root.pack_start(&paned, true, true, 0);
    paned.show();
    inspector.hide();
    let profile = if private {
        None
    } else {
        Some(
            dirs::data_local_dir()
                .unwrap_or_else(|| dir.clone())
                .join("lumen/webkit"),
        )
    };
    let profile = if std::env::var_os("LUMEN_DATA_DIR").is_some() && !private {
        Some(dir.join("webkit"))
    } else {
        profile
    };
    if let Some(path) = &profile { crate::security::private_directory(path).expect("private WebKit profile"); }
    let mut ctx = WebContext::new(profile);
    let p = proxy.clone();
    let ui = WebViewBuilder::with_web_context(&mut ctx)
        .with_incognito(private)
        .with_html(UI)
        .with_bounds(rect(0.0, 0.0, 1200.0, BAR))
        .with_navigation_handler(|u| crate::security::internal_document(&u, UI))
        .with_new_window_req_handler(|_| false)
        .with_ipc_handler(move |request| {
            if request.body().len() > crate::security::MAX_IPC
                || !crate::security::internal_document(&request.uri().to_string(), UI)
            {
                return;
            }
            if let Ok(v) = serde_json::from_str(request.body()) {
                let _ = p.send_event(Msg::Ui(v));
            }
        })
        .build_gtk(&chrome)
        .expect("Lumen toolbar");
    let p = proxy.clone();
    window
        .gtk_window()
        .connect_key_press_event(move |_, event| {
            if let Some(v) = key_command(event) {
                let _ = p.send_event(Msg::Ui(v));
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
    let blocker = Rc::new(Blocker::load(&dir.join("filters")));
    let download_proxy = proxy.clone();
    let downloads = crate::downloads::Manager::new(&dir, private, move || {
        download_proxy.send_event(Msg::DownloadPoll).is_ok()
    });
    let mut app = App {
        newtab_token: crate::security::newtab_token(),
        downloads,
        window,
        root,
        chrome,
        stack,
        paned,
        inspector,
        ctx,
        ui,
        tabs: Vec::new(),
        active: 0,
        next: 1,
        config,
        dir,
        private,
        links,
        history,
        closed: Vec::new(),
        panel: String::new(),
        dev: false,
        full: false,
        proxy,
        blocker,
        smoke,
        smoke_started: false,
        last_state: String::new(),
    };
    let first = args
        .iter()
        .position(|s| s == "--url")
        .and_then(|i| args.get(i + 1))
        .filter(|u| safe(u));
    if let Some(u) = first {
        app.open(Some(u.clone()));
    } else if app.config.startup == "restore" && !session.tabs.is_empty() {
        for tab in session.tabs.into_iter().take(50) {
            if safe(&tab.u) {
                app.open(Some(tab.u));
            }
        }
        if let Some(id) = app.tabs.get(session.active).map(|t| t.id) {
            app.activate(id);
        }
        if app.tabs.is_empty() {
            app.open(None);
        }
    } else {
        app.open(None);
    }
    let started = std::time::Instant::now();
    let updates=crate::updater::start(!private && !smoke);
    event_loop.run(move |event, _, flow| {
        *flow = if smoke {
            ControlFlow::WaitUntil(std::time::Instant::now() + std::time::Duration::from_secs(1))
        } else {
            ControlFlow::Wait
        };
        if smoke && started.elapsed() > std::time::Duration::from_secs(30) {
            eprintln!("Linux smoke timed out before UI acknowledgement");
            std::process::exit(1)
        }
        match event {
            Event::UserEvent(Msg::Quit)
            | Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                app.save();
                app.save_session();
                if app.downloads.borrow().active_count()==0 {
                    if let Ok(update)=updates.try_recv(){let _=update.apply();}
                }
                *flow = ControlFlow::Exit
            }
            Event::UserEvent(msg) => app.event(msg),
            Event::WindowEvent {
                event: WindowEvent::Resized(_),
                ..
            }
            | Event::WindowEvent {
                event: WindowEvent::ScaleFactorChanged { .. },
                ..
            } => {
                app.layout();
                app.push();
            }
            _ => {}
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn addresses_and_zoom_are_scoped() {
        assert!(!safe("javascript:alert(1)"));
        assert_eq!(address("example.com", "google"), "https://example.com");
        assert_eq!(
            zoom_key("https://EXAMPLE.com/a?q=x"),
            Some("https://example.com".into())
        );
        assert_ne!(
            zoom_key("http://localhost:1"),
            zoom_key("http://localhost:2")
        );
    }
    #[test]
    fn old_settings_and_zoom_roundtrip() {
        let mut c: Config = serde_json::from_str("{}").unwrap();
        c.site_zoom.insert("https://example.com".into(), 1.3);
        let loaded: Config = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(loaded.site_zoom["https://example.com"], 1.3);
    }
}
