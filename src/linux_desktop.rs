//! Desktop integration shared by either Linux rendering backend.
use std::{io,path::Path};
const ID: &str = "io.github.L4NOTTE.Lumen.desktop";
fn desktop_entry(exe: &Path) -> io::Result<String> {
    let raw=exe.to_str().filter(|s|exe.is_absolute()&&!s.contains(['\n','\r','\0','=']))
        .ok_or_else(||io::Error::other("invalid executable path"))?;
    // Desktop Entry Exec has its own escaping rules; this is not shell quoting.
    let mut escaped=String::new();
    for ch in raw.chars() {
        match ch {
            '\\'=>escaped.push_str("\\\\\\\\"),
            '"'|'`'|'$'=>{escaped.push_str("\\\\");escaped.push(ch);},
            '%'=>escaped.push_str("%%"),
            _=>escaped.push(ch),
        }
    }
    Ok(format!("[Desktop Entry]\nVersion=1.0\nType=Application\nName=Lumen\nComment=Web browser written in Rust\nExec=\"{escaped}\" --url %u\nIcon=lumen\nTerminal=false\nCategories=Network;WebBrowser;\nMimeType=x-scheme-handler/http;x-scheme-handler/https;\nStartupNotify=true\n"))
}
#[cfg(target_os="linux")]
pub fn set_default() -> io::Result<()> {
    let exe=std::env::current_exe()?.canonicalize()?;
    let entry=desktop_entry(&exe)?;
    let folder=dirs::data_local_dir().ok_or_else(||io::Error::other("XDG data directory unavailable"))?.join("applications");
    std::fs::create_dir_all(&folder)?;
    crate::security::atomic_write(&folder.join(ID),entry.as_bytes())?;
    let status=std::process::Command::new("xdg-settings")
        .args(["set","default-web-browser",ID]).status()?;
    if !status.success(){return Err(io::Error::other("Системе не удалось изменить браузер по умолчанию"));}
    let result=std::process::Command::new("xdg-settings").args(["get","default-web-browser"]).output()?;
    if !result.status.success()||String::from_utf8_lossy(&result.stdout).trim()!=ID {
        return Err(io::Error::other("Рабочая среда не подтвердила выбор Lumen"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn exec_paths_are_escaped_and_urls_are_not_shell_commands() {
        use super::*;
        let path=if cfg!(windows){Path::new(r"C:\Program Files\Lumen 100%\lumen.exe")}else{Path::new("/home/user/Lumen 100%/lumen")};
        let entry=desktop_entry(path).unwrap();
        assert!(entry.contains("100%%"));
        assert!(entry.contains("\" --url %u\n"));
        assert!(!entry.contains("sh -c"));
        assert!(desktop_entry(Path::new("relative")).is_err());
        assert!(desktop_entry(Path::new("/tmp/injected\nExec=evil")).is_err());
    }
}
