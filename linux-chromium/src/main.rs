#[cfg(target_os="linux")]
mod runtime;

fn main() {
    #[cfg(target_os="linux")]
    runtime::run();
    #[cfg(not(target_os="linux"))]
    eprintln!("This isolated Chromium compatibility probe must run on Linux.");
}
