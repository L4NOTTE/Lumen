//! Мелкие системные помощники: скрытый запуск процессов, загрузка файлов через curl.exe
//! (curl входит в состав Windows 10/11).

use std::{os::windows::process::CommandExt, path::Path, process::Command};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn hidden(program: &str) -> Command {
    let mut c = Command::new(program);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// Скачивает `url` в `dest`. Возвращает true при успехе.
pub fn curl_download(url: &str, dest: &Path) -> bool {
    hidden("curl.exe")
        .args(["-L", "-f", "-sS", "--max-time", "90", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
