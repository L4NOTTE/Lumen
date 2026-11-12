#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

#[cfg(target_os = "windows")]
include!("windows_main.rs");

mod downloads;
mod security;
mod updater;
#[cfg(any(target_os="linux",test))]
mod linux_desktop;

#[cfg(target_os = "linux")]
mod blocker;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
compile_error!("Lumen supports Windows and Linux.");
