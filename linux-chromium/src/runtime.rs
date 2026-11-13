//! Disposable Chrome-style CEF compatibility gate, not the production browser.
use cef::*;
use std::{io::{Read,Write},net::TcpListener,sync::{Arc,atomic::{AtomicBool,Ordering}}};

wrap_display_handler! {
    struct ProbeDisplay { passed: Arc<AtomicBool>, expected: String, url: String }
    impl DisplayHandler {
        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            let Some(browser)=browser else{return;};
            if browser.main_frame().map(|f|CefString::from(&f.url()).to_string()).as_deref()!=Some(self.url.as_str()){return;}
            let text=title.map(ToString::to_string).unwrap_or_default();
            let Some(report)=text.strip_prefix("LUMEN_PROBE:") else{return;};
            let Ok(value)=serde_json::from_str::<serde_json::Value>(report) else{return;};
            let ok=value["id"].as_str()==Some(&self.expected)
                && ["storage","serviceWorker","tabs","tabCapture","offscreen"].iter().all(|k|value[*k]==true);
            println!("LUMEN_EXTENSION_REPORT:{value}");
            self.passed.store(ok,Ordering::SeqCst);
            if let Some(host)=browser.host(){host.close_browser(1);}
        }
    }
}
wrap_life_span_handler! {
    struct ProbeLife;
    impl LifeSpanHandler {
        fn on_before_close(&self,_browser:Option<&mut Browser>){quit_message_loop();}
    }
}
wrap_client! {
    struct ProbeClient { passed: Arc<AtomicBool>, expected: String, url: String }
    impl Client {
        fn display_handler(&self)->Option<DisplayHandler>{Some(ProbeDisplay::new(self.passed.clone(),self.expected.clone(),self.url.clone()))}
        fn life_span_handler(&self)->Option<LifeSpanHandler>{Some(ProbeLife::new())}
    }
}
wrap_browser_process_handler! {
    struct ProbeProcess { passed:Arc<AtomicBool>, expected:String, url:String }
    impl BrowserProcessHandler {
        fn on_context_initialized(&self){
            let mut client=ProbeClient::new(self.passed.clone(),self.expected.clone(),self.url.clone());
            let info=WindowInfo::default();
            let settings=BrowserSettings::default();
            let url=CefString::from(self.url.as_str());
            if browser_host_create_browser(Some(&info),Some(&mut client),Some(&url),Some(&settings),None,None)!=1 {
                eprintln!("CEF browser creation failed");quit_message_loop();
            }
        }
    }
}
wrap_app! {
    struct ProbeApp { passed:Arc<AtomicBool>, expected:String, url:String, fixture:String }
    impl App {
        fn browser_process_handler(&self)->Option<BrowserProcessHandler>{Some(ProbeProcess::new(self.passed.clone(),self.expected.clone(),self.url.clone()))}
        fn on_before_command_line_processing(&self,process_type:Option<&CefString>,command:Option<&mut CommandLine>){
            if process_type.is_some_and(|s|!s.to_string().is_empty()){return;}
            if let Some(command)=command {
                command.append_switch_with_value(Some(&CefString::from("load-extension")),Some(&CefString::from(self.fixture.as_str())));
            }
        }
    }
}
pub fn run(){
    let _=api_hash(sys::CEF_API_VERSION_LAST,0);
    let args=cef::args::Args::new();
    let code=execute_process(Some(args.as_main_args()),None,std::ptr::null_mut());
    if code>=0 {std::process::exit(code);}
    for argument in std::env::args().skip(1) {
        if argument.starts_with("--no-sandbox")||argument.starts_with("--disable-")||argument.starts_with("--remote-debugging") {
            eprintln!("Unsafe flags are not accepted by this compatibility gate");std::process::exit(2);
        }
    }
    let fixture=std::env::var_os("LUMEN_CEF_FIXTURE").map(std::path::PathBuf::from)
        .unwrap_or_else(||std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixture"))
        .canonicalize().expect("probe fixture directory");
    let expected=std::fs::read_to_string(fixture.join("id.txt")).expect("fixture extension id").trim().to_owned();
    let profile=tempfile::tempdir().expect("isolated profile");
    let listener=TcpListener::bind("127.0.0.1:0").expect("local fixture server");
    let url=format!("http://127.0.0.1:{}/",listener.local_addr().unwrap().port());
    std::thread::spawn(move||{for mut stream in listener.incoming().flatten(){
        let _=stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
        let mut request=[0u8;8192];let _=stream.read(&mut request);
        let body="<!doctype html><title>Lumen extension gate</title><p>Waiting for the isolated compatibility extension.</p>";
        let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());let _=stream.write_all(response.as_bytes());
    }});
    let passed=Arc::new(AtomicBool::new(false));
    let mut app=ProbeApp::new(passed.clone(),expected,url,fixture.display().to_string());
    let settings=Settings{no_sandbox:0,remote_debugging_port:0,root_cache_path:CefString::from(profile.path().to_str().unwrap()),..Default::default()};
    if initialize(Some(args.as_main_args()),Some(&settings),Some(&mut app),std::ptr::null_mut())!=1 {std::process::exit(1);}
    run_message_loop();shutdown();
    if !passed.load(Ordering::SeqCst){eprintln!("LUMEN_EXTENSION_GATE_FAILED");std::process::exit(1);}
    println!("LUMEN_EXTENSION_GATE_OK");
}
