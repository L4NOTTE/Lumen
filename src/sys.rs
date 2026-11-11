//! Мелкие системные помощники: скрытый запуск процессов, загрузка файлов через curl.exe
//! (curl входит в состав Windows 10/11).

use std::{os::windows::process::CommandExt, path::Path, process::Command};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A narrowly scoped child mode replaces shell-built recursive deletion.
pub fn cleanup_mode() -> bool {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|v| v != "--cleanup-profile") {
        return false;
    }
    if args.len() != 4 {
        return true;
    }
    let Ok(pid) = args[3].to_string_lossy().parse::<u32>() else {
        return true;
    };
    let Ok(root) = std::env::temp_dir().canonicalize() else {
        return true;
    };
    let requested = std::path::PathBuf::from(&args[2]);
    for _ in 0..12 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let Ok(path) = requested.canonicalize() else {
            return true;
        };
        if path.parent() != Some(root.as_path())
            || path.file_name().and_then(|v| v.to_str()) != Some(&format!("Lumen-incognito-{pid}"))
        {
            return true;
        }
        if std::fs::symlink_metadata(&requested).is_ok_and(|m| m.file_type().is_symlink()) {
            return true;
        }
        // remove_dir_all does not follow child symlinks. The final absolute root
        // was checked above against the exact disposable profile for this PID.
        if std::fs::remove_dir_all(&path).is_ok() {
            return true;
        }
    }
    true
}
pub fn schedule_cleanup(path: &Path) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe)
            .arg("--cleanup-profile")
            .arg(path)
            .arg(std::process::id().to_string())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}

pub fn hidden(program: &str) -> Command {
    let system = std::path::PathBuf::from(
        std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()),
    )
    .join("System32")
    .join(program);
    let mut c = Command::new(system);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// Скачивает `url` в `dest`. Возвращает true при успехе.
pub fn curl_download(url: &str, dest: &Path) -> bool {
    hidden("curl.exe")
        .args([
            "-L",
            "-f",
            "-sS",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "5",
            "--max-filesize",
            "67108864",
            "--connect-timeout",
            "8",
            "--max-time",
            "90",
            "-o",
        ])
        .arg(dest)
        .arg("--")
        .arg(url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
