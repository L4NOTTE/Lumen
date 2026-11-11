//! Register browser capabilities, never overwrite Windows UserChoice.
use std::{io, path::Path};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};
const CLIENT: &str = r"Software\Clients\StartMenuInternet\Lumen";
#[derive(Debug)]
struct Entry {
    path: String,
    name: &'static str,
    value: String,
}
fn entries(exe: &Path) -> io::Result<Vec<Entry>> {
    if !exe.is_absolute() || exe.to_str().is_none_or(|p| p.contains(['"', '\0'])) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Нужен абсолютный путь к lumen.exe",
        ));
    }
    let executable = exe.display().to_string();
    let icon = format!("\"{executable}\",0");
    let command = format!("\"{executable}\" -- \"%1\"");
    let mut out = Vec::new();
    let mut add = |path: String, name, value: String| out.push(Entry { path, name, value });
    add(CLIENT.into(), "", "Lumen".into());
    add(format!(r"{CLIENT}\DefaultIcon"), "", icon.clone());
    add(
        format!(r"{CLIENT}\shell\open\command"),
        "",
        format!("\"{executable}\""),
    );
    let capabilities = format!(r"{CLIENT}\Capabilities");
    add(capabilities.clone(), "ApplicationName", "Lumen".into());
    add(
        capabilities.clone(),
        "ApplicationDescription",
        "Lumen — браузер на Rust".into(),
    );
    add(capabilities.clone(), "ApplicationIcon", icon.clone());
    add(
        format!(r"{capabilities}\StartMenu"),
        "StartMenuInternet",
        "Lumen".into(),
    );
    for protocol in ["http", "https"] {
        add(
            format!(r"{capabilities}\URLAssociations"),
            protocol,
            "LumenURL".into(),
        );
    }
    add(
        r"Software\RegisteredApplications".into(),
        "Lumen",
        capabilities,
    );
    add(r"Software\Classes\LumenURL".into(), "", "Lumen URL".into());
    add(
        r"Software\Classes\LumenURL".into(),
        "URL Protocol",
        "".into(),
    );
    add(r"Software\Classes\LumenURL\DefaultIcon".into(), "", icon);
    add(
        r"Software\Classes\LumenURL\shell\open\command".into(),
        "",
        command,
    );
    Ok(out)
}
fn write(root: &RegKey, exe: &Path) -> io::Result<()> {
    for entry in entries(exe)? {
        let (key, _) = root.create_subkey(&entry.path)?;
        key.set_value(entry.name, &entry.value)?;
    }
    Ok(())
}
pub fn register(exe: &Path) -> io::Result<()> {
    write(&RegKey::predef(HKEY_CURRENT_USER), exe)
}
pub fn open_settings(exe: &Path) -> io::Result<()> {
    register(exe)?;
    launch_settings()
}
pub fn launch_settings() -> io::Result<()> {
    use windows::{core::w, Win32::{UI::{Shell::ShellExecuteW,WindowsAndMessaging::SW_SHOWNORMAL}}};
    // Invoke the registered URI handler directly. Explorer can interpret the URI
    // as a folder argument and fall back to Documents on some Windows builds.
    let result=unsafe {ShellExecuteW(None,w!("open"),w!("ms-settings:defaultapps?registeredAppUser=Lumen"),None,None,SW_SHOWNORMAL)};
    if result.0 as isize <= 32 {Err(io::Error::other(format!("Не удалось открыть параметры Windows ({})",result.0 as isize)))} else {Ok(())}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_commands_and_no_userchoice() {
        let exe = Path::new(r"C:\Program Files\Lumen\lumen.exe");
        let values = entries(exe).unwrap();
        assert_eq!(
            values
                .iter()
                .find(|e| e.path == r"Software\Classes\LumenURL\shell\open\command")
                .unwrap()
                .value,
            r#""C:\Program Files\Lumen\lumen.exe" -- "%1""#
        );
        assert!(values.iter().all(|e| !e.path.contains("UserChoice")));
        assert_eq!(
            values
                .iter()
                .filter(|e| e.name == "http" || e.name == "https")
                .count(),
            2
        );
        assert!(entries(Path::new("relative.exe")).is_err());
    }
    #[test]
    fn registry_round_trip_in_isolated_test_root() {
        let current = RegKey::predef(HKEY_CURRENT_USER);
        let namespace = format!(r"Software\LumenRegistrationTest-{}", std::process::id());
        let (root, _) = current.create_subkey(&namespace).unwrap();
        let exe = Path::new(r"C:\Lumen Test\lumen.exe");
        write(&root, exe).unwrap();
        let capabilities = root
            .open_subkey(format!(r"{CLIENT}\Capabilities\URLAssociations"))
            .unwrap();
        assert_eq!(
            capabilities.get_value::<String, _>("https").unwrap(),
            "LumenURL"
        );
        let registered = root
            .open_subkey(r"Software\RegisteredApplications")
            .unwrap();
        assert_eq!(
            registered.get_value::<String, _>("Lumen").unwrap(),
            format!(r"{CLIENT}\Capabilities")
        );
        drop(capabilities);
        drop(registered);
        drop(root);
        assert!(namespace.starts_with(r"Software\LumenRegistrationTest-"));
        current.delete_subkey_all(&namespace).unwrap();
    }
}
