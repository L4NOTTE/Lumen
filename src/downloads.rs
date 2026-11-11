//! Download history and native operation polling. No downloaded file is executed automatically.
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub type Shared = Rc<RefCell<Manager>>;
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Record {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub url: String,
    pub received: u64,
    pub total: Option<u64>,
    pub status: String,
    pub created: u64,
    pub error: Option<String>,
}
pub struct Progress {
    pub received: u64,
    pub total: Option<u64>,
    pub status: &'static str,
    pub error: Option<String>,
}
trait Task {
    fn progress(&self) -> Progress;
    fn cancel(&self);
}
// Keep earlier native operation wrappers alive during a transport retry.
#[cfg(target_os = "windows")]
struct RetryTask {
    current: Box<dyn Task>,
    _previous: Box<dyn Task>,
}
#[cfg(target_os = "windows")]
impl Task for RetryTask {
    fn progress(&self) -> Progress {
        self.current.progress()
    }
    fn cancel(&self) {
        self.current.cancel();
    }
}
pub struct Manager {
    records: Vec<Record>,
    tasks: HashMap<String, Box<dyn Task>>,
    history: PathBuf,
    folder: PathBuf,
    private: bool,
    next: u64,
    dirty: bool,
    notice: Option<String>,
    active: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    #[cfg(target_os = "linux")]
    contexts: Vec<gtk::glib::WeakRef<webkit2gtk::WebContext>>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn filename(suggested: &str) -> String {
    let leaf = suggested.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name: String = leaf
        .chars()
        .filter(|c| !c.is_control() && !matches!(c,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'))
        .map(|c| if "<>:\"|?*".contains(c) { '_' } else { c })
        .take(180)
        .collect();
    name = name.trim().trim_end_matches(['.', ' ']).to_string();
    if name.is_empty() || name == "." || name == ".." {
        name = "download.bin".into()
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        name.insert(0, '_');
    }
    name
}
impl Manager {
    pub fn new(dir: &Path, private: bool, tick: impl Fn() -> bool + Send + 'static) -> Shared {
        let history = dir.join("downloads.json");
        let mut records: Vec<Record> = if private {
            Vec::new()
        } else {
            fs::read(&history)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default()
        };
        records.truncate(200);
        for r in &mut records {
            if r.status == "running" {
                r.status = "interrupted".into();
                r.error = Some("Загрузка была прервана при закрытии браузера".into());
            }
        }
        let folder = std::env::var_os("LUMEN_DOWNLOAD_DIR")
            .map(PathBuf::from)
            .or_else(dirs::download_dir)
            .unwrap_or_else(|| dir.join("Downloads"));
        let active = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let (a, b) = (active.clone(), alive.clone());
        std::thread::spawn(move || {
            while b.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
                if a.load(Ordering::Relaxed) && !tick() {
                    break;
                }
            }
        });
        Rc::new(RefCell::new(Self {
            records,
            tasks: HashMap::new(),
            history,
            folder,
            private,
            next: 0,
            dirty: false,
            notice: None,
            active,
            alive,
            #[cfg(target_os = "linux")]
            contexts: Vec::new(),
        }))
    }
    pub fn records(&self) -> &[Record] {
        &self.records
    }
    pub fn active_count(&self) -> usize {
        self.tasks.len()
    }
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }
    fn destination(&self, suggested: &str) -> std::io::Result<PathBuf> {
        fs::create_dir_all(&self.folder)?;
        let name = filename(suggested);
        let base = Path::new(&name);
        let stem = base.file_stem().unwrap_or_default().to_string_lossy();
        let extension = base
            .extension()
            .map(|s| format!(".{}", s.to_string_lossy()))
            .unwrap_or_default();
        for n in 0..10000 {
            let path = self.folder.join(if n == 0 {
                name.clone()
            } else {
                format!("{stem} ({n}){extension}")
            });
            if !path.exists()
                && !self
                    .records
                    .iter()
                    .any(|r| r.status == "running" && r.path == path)
            {
                return Ok(path);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "Слишком много файлов с одинаковым именем",
        ))
    }
    fn add(&mut self, url: String, path: PathBuf, task: Box<dyn Task>) -> String {
        self.next += 1;
        let id = format!("{}-{}-{}", now(), std::process::id(), self.next);
        let url = if url.starts_with("data:") {
            String::from("data:…")
        } else {
            url.chars().take(4096).collect()
        };
        self.records.insert(
            0,
            Record {
                id: id.clone(),
                name: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                path,
                url,
                received: 0,
                total: None,
                status: "running".into(),
                created: now(),
                error: None,
            },
        );
        self.tasks.insert(id.clone(), task);
        self.dirty = true;
        self.notice = Some(format!("Загрузка: {}", self.records[0].name));
        self.active.store(true, Ordering::Relaxed);
        self.save();
        id
    }
    fn save(&self) {
        if self.private {
            return;
        }
        let mut all: HashMap<String, Record> = fs::read(&self.history)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Record>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|r| (r.id.clone(), r))
            .collect();
        for r in &self.records {
            all.insert(r.id.clone(), r.clone());
        }
        let mut rows: Vec<_> = all.into_values().collect();
        rows.sort_by(|a, b| b.created.cmp(&a.created));
        rows.truncate(200);
        if let Ok(data) = serde_json::to_vec(&rows) {
            let temp = self
                .history
                .with_extension(format!("{}.tmp", std::process::id()));
            if fs::write(&temp, data).is_ok() {
                let _ = fs::rename(temp, &self.history);
            }
        }
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = std::mem::take(&mut self.dirty);
        let mut ended = false;
        for r in &mut self.records {
            if let Some(task) = self.tasks.get(&r.id) {
                let p = task.progress();
                if r.received != p.received
                    || r.total != p.total
                    || r.status != p.status
                    || r.error != p.error
                {
                    r.received = p.received;
                    r.total = p.total;
                    r.status = p.status.into();
                    r.error = p.error;
                    changed = true;
                }
                if r.status != "running" {
                    self.notice = Some(format!(
                        "{}: {}",
                        if r.status == "completed" {
                            "Загружено"
                        } else {
                            "Загрузка прервана"
                        },
                        r.name
                    ));
                    self.tasks.remove(&r.id);
                    ended = true;
                }
            }
        }
        self.active.store(!self.tasks.is_empty(), Ordering::Relaxed);
        if ended {
            self.save()
        }
        changed
    }
    pub fn action(&mut self, id: &str, action: &str) {
        if action == "cancel" {
            if let Some(task) = self.tasks.remove(id) {
                task.cancel();
                if let Some(r) = self.records.iter_mut().find(|r| r.id == id) {
                    r.status = "cancelled".into();
                    r.error = None;
                }
                self.active.store(!self.tasks.is_empty(), Ordering::Relaxed);
                self.save();
            }
            return;
        }
        let Some(row) = self.records.iter().find(|r| r.id == id) else {
            return;
        };
        if action == "open" && row.status == "completed" && row.path.is_file() {
            open_path(&row.path, false)
        }
        if action == "folder" {
            open_path(&row.path, true)
        }
    }
    pub fn clear(&mut self) {
        self.records.retain(|r| r.status == "running");
        if !self.private {
            let _ = fs::write(
                &self.history,
                serde_json::to_vec(&self.records).unwrap_or_default(),
            );
        }
    }
    pub fn open_folder(&self) {
        let _ = fs::create_dir_all(&self.folder);
        open_path(&self.folder, false);
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}
fn open_path(path: &Path, folder: bool) {
    #[cfg(target_os = "windows")]
    {
        let mut c = std::process::Command::new("explorer.exe");
        if folder {
            c.arg("/select,");
        }
        let _ = c.arg(path).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let target = if folder {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        let _ = std::process::Command::new("xdg-open").arg(target).spawn();
    }
}

#[cfg(target_os = "windows")]
mod native_windows {
    use super::*;
    use webview2_com::{
        take_pwstr, DownloadStartingEventHandler,
        Microsoft::Web::WebView2::Win32::*, NavigationStartingEventHandler,
    };
    use windows_core::{Interface, HSTRING, PWSTR};
    use wry::WebViewExtWindows;
    // Poll on the GUI thread; keep the operation alive even when callbacks are missed.
    struct Native(ICoreWebView2DownloadOperation);
    impl Task for Native {
        fn progress(&self) -> Progress {
            unsafe {
                let (mut received, mut total) = (0i64, 0i64);
                let mut state = COREWEBVIEW2_DOWNLOAD_STATE::default();
                let _ = self.0.BytesReceived(&mut received);
                let _ = self.0.TotalBytesToReceive(&mut total);
                let ok = self.0.State(&mut state).is_ok();
                let status = if !ok {
                    "failed"
                } else if state == COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED {
                    "completed"
                } else if state == COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED {
                    "failed"
                } else {
                    "running"
                };
                Progress {
                    received: received.max(0) as u64,
                    total: (total > 0).then_some(total as u64),
                    status,
                    error: if status == "failed" {
                        let mut reason = COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON::default();
                        let _ = self.0.InterruptReason(&mut reason);
                        Some(format!("Загрузка прервана ({})", reason.0))
                    } else {
                        None
                    },
                }
            }
        }
        fn cancel(&self) {
            unsafe {
                let _ = self.0.Cancel();
            }
        }
    }
    pub fn install(wv: &wry::WebView, manager: &Shared) -> windows_core::Result<()> {
        let weak = Rc::downgrade(manager);
        unsafe {
            let navigation = Rc::new(std::cell::Cell::new(0u64));
            let nav = navigation.clone();
            let core = wv.controller().CoreWebView2()?;
            let mut nav_token = Default::default();
            core.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        let mut id = 0;
                        args.NavigationId(&mut id)?;
                        nav.set(id);
                    }
                    Ok(())
                })),
                &mut nav_token,
            )?;
            // A transport retry can emit DownloadStarting again, without a new
            // navigation or a terminal event on the previous operation object.
            let mut attempts: HashMap<(u64, String), (String, PathBuf)> = HashMap::new();
            let mut token = Default::default();
            core.cast::<ICoreWebView2_4>()?.add_DownloadStarting(
                &DownloadStartingEventHandler::create(Box::new(move |_, args| {
                    let (Some(args), Some(manager)) = (args, weak.upgrade()) else {
                        return Ok(());
                    };
                    let op = args.DownloadOperation()?;
                    let mut url = PWSTR::null();
                    op.Uri(&mut url)?;
                    let url = take_pwstr(url);
                    let mut suggested = PWSTR::null();
                    args.ResultFilePath(&mut suggested)?;
                    let suggested = take_pwstr(suggested);
                    attempts.retain(|_, (id, _)| manager.borrow().tasks.contains_key(id));
                    let key = (navigation.get(), url.clone());
                    let retry = attempts.get(&key).cloned();
                    let destination = match &retry {
                        Some((_, path)) => Ok(path.clone()),
                        None => manager.borrow().destination(&suggested),
                    };
                    match destination {
                        Ok(path) => {
                            args.SetResultFilePath(&HSTRING::from(path.as_os_str()))?;
                            args.SetHandled(true)?;
                            let task = Box::new(Native(op));
                            if let Some((id, _)) = retry {
                                let previous = manager.borrow_mut().tasks.remove(&id).unwrap();
                                manager.borrow_mut().tasks.insert(
                                    id,
                                    Box::new(RetryTask {
                                        current: task,
                                        _previous: previous,
                                    }),
                                );
                            } else {
                                let id = manager.borrow_mut().add(url, path.clone(), task);
                                attempts.insert(key, (id, path));
                            }
                        }
                        Err(_) => {
                            args.SetCancel(true)?;
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }
}
#[cfg(target_os = "windows")]
pub use native_windows::install;

#[cfg(target_os = "linux")]
mod native_linux {
    use super::*;
    use gtk::prelude::*;
    use std::cell::Cell;
    use webkit2gtk::{DownloadExt, URIRequestExt, URIResponseExt, WebContextExt, WebViewExt};
    use wry::WebViewExtUnix;
    struct Native {
        download: webkit2gtk::Download,
        finished: Rc<Cell<bool>>,
        error: Rc<RefCell<Option<String>>>,
    }
    impl Task for Native {
        fn progress(&self) -> Progress {
            let total = self
                .download
                .response()
                .map(|r| r.content_length())
                .filter(|n| *n > 0);
            let error = self.error.borrow().clone();
            Progress {
                received: self.download.received_data_length(),
                total,
                status: if error.is_some() {
                    "failed"
                } else if self.finished.get() {
                    "completed"
                } else {
                    "running"
                },
                error,
            }
        }
        fn cancel(&self) {
            self.download.cancel();
        }
    }
    pub fn install(wv: &wry::WebView, manager: &Shared) {
        let Some(context) = wv.webview().context() else {
            return;
        };
        {
            let mut m = manager.borrow_mut();
            m.contexts.retain(|c| c.upgrade().is_some());
            if m.contexts
                .iter()
                .any(|c| c.upgrade().as_ref() == Some(&context))
            {
                return;
            }
            m.contexts.push(context.downgrade());
        }
        let weak = Rc::downgrade(manager);
        context.connect_download_started(move |_, download| {
            let weak = weak.clone();
            download.connect_decide_destination(move |d, suggested| {
                let Some(manager) = weak.upgrade() else {
                    d.cancel();
                    return true;
                };
                let path = match manager.borrow().destination(suggested) {
                    Ok(p) => p,
                    Err(_) => {
                        d.cancel();
                        return true;
                    }
                };
                let Ok(uri) = url::Url::from_file_path(&path) else {
                    d.cancel();
                    return true;
                };
                d.set_allow_overwrite(false);
                d.set_destination(uri.as_str());
                let finished = Rc::new(Cell::new(false));
                let error = Rc::new(RefCell::new(None));
                let flag = finished.clone();
                d.connect_finished(move |_| flag.set(true));
                let e = error.clone();
                d.connect_failed(move |_, error| {
                    *e.borrow_mut() = Some(error.to_string());
                });
                let url = d
                    .request()
                    .and_then(|r| r.uri())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                manager.borrow_mut().add(
                    url,
                    path,
                    Box::new(Native {
                        download: d.clone(),
                        finished,
                        error,
                    }),
                );
                true
            });
        });
    }
}
#[cfg(target_os = "linux")]
pub use native_linux::install;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filenames_cannot_escape_folder() {
        assert_eq!(filename("../../test.zip"), "test.zip");
        assert_eq!(filename("C:\\folder\\file.zip"), "file.zip");
        assert_eq!(filename(".."), "download.bin");
        assert_eq!(filename("CON.txt"), "_CON.txt");
        assert!(!filename("a:b?.txt").contains(':'));
    }
    #[test]
    fn progress_and_cancel_do_not_delete_files() {
        struct Fake;
        impl Task for Fake {
            fn progress(&self) -> Progress {
                Progress {
                    received: 25,
                    total: Some(100),
                    status: "running",
                    error: None,
                }
            }
            fn cancel(&self) {}
        }
        let dir = std::env::temp_dir().join(format!(
            "lumen-download-test-{}-{}",
            std::process::id(),
            now()
        ));
        let _ = fs::create_dir_all(&dir);
        let m = Manager::new(&dir, true, || true);
        let path = dir.join("sample.txt");
        fs::write(&path, b"keep").unwrap();
        m.borrow_mut().add(
            "https://example.com/file".into(),
            path.clone(),
            Box::new(Fake),
        );
        assert!(m.borrow_mut().poll());
        assert_eq!(m.borrow().records()[0].received, 25);
        let id = m.borrow().records()[0].id.clone();
        m.borrow_mut().action(&id, "cancel");
        assert_eq!(m.borrow().records()[0].status, "cancelled");
        m.borrow_mut().clear();
        assert!(path.exists());
        assert!(!dir.join("downloads.json").exists());
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(dir);
    }
}
