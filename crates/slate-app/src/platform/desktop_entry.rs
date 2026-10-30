//! "Add to app menu" (Linux): installs Slate's desktop file and icons into the
//! user's data directory, pointing at the running AppImage or binary. Bars and
//! docks find a window's icon through the desktop file named after its app id,
//! and the XDG portals (global shortcuts) refuse apps without one, so a bare
//! binary or a non-integrated AppImage otherwise shows a placeholder icon.

use std::path::{Path, PathBuf};

use super::APP_ID;

/// Icon sizes installed under `icons/hicolor/<size>/apps/<APP_ID>.png`.
const ICONS: [(&str, &[u8]); 3] = [
    (
        "64x64",
        include_bytes!("../../../../assets/icons/64x64.png"),
    ),
    (
        "128x128",
        include_bytes!("../../../../assets/icons/128x128.png"),
    ),
    (
        "512x512",
        include_bytes!("../../../../assets/icons/icon.png"),
    ),
];

fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))
}

/// What to launch: the AppImage itself when running from one (its mount
/// point changes every run), else this executable.
fn launcher() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| std::env::current_exe().ok())
}

fn desktop_file(exec: &Path) -> String {
    // Exec quoting per the Desktop Entry spec: quote, escape \ " ` $.
    let mut quoted = String::from("\"");
    for c in exec.to_string_lossy().chars() {
        if matches!(c, '\\' | '"' | '`' | '$') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    format!(
        "[Desktop Entry]\nType=Application\nName=Slate\nGenericName=TIDAL Music Client\n\
         Comment=Native TIDAL client\nExec={quoted} %u\nIcon={APP_ID}\nTerminal=false\n\
         Categories=AudioVideo;Audio;Music;Player;\nMimeType=x-scheme-handler/tidal;x-scheme-handler/slate;\n\
         StartupWMClass={APP_ID}\nStartupNotify=true\n"
    )
}

fn paths(base: &Path) -> (PathBuf, Vec<PathBuf>) {
    let desktop = base.join("applications").join(format!("{APP_ID}.desktop"));
    let icons = ICONS
        .iter()
        .map(|(size, _)| base.join(format!("icons/hicolor/{size}/apps/{APP_ID}.png")))
        .collect();
    (desktop, icons)
}

pub fn installed() -> bool {
    data_home().is_some_and(|b| paths(&b).0.is_file())
}

pub fn install() -> Result<(), String> {
    let base = data_home().ok_or("no home directory")?;
    let exec = launcher().ok_or("cannot tell where Slate is installed")?;
    install_into(&base, &exec).map_err(|e| e.to_string())
}

pub fn remove() -> Result<(), String> {
    let base = data_home().ok_or("no home directory")?;
    remove_from(&base).map_err(|e| e.to_string())
}

fn install_into(base: &Path, exec: &Path) -> std::io::Result<()> {
    let (desktop, icons) = paths(base);
    for (path, (_, bytes)) in icons.iter().zip(ICONS) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, bytes)?;
    }
    if let Some(dir) = desktop.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&desktop, desktop_file(exec))?;
    refresh(base);
    Ok(())
}

fn remove_from(base: &Path) -> std::io::Result<()> {
    let (desktop, icons) = paths(base);
    for p in std::iter::once(&desktop).chain(icons.iter()) {
        match std::fs::remove_file(p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    refresh(base);
    Ok(())
}

/// Best effort: menus and bars pick the entry up sooner. Missing tools are fine.
fn refresh(base: &Path) {
    let _ = std::process::Command::new("update-desktop-database")
        .arg(base.join("applications"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let _ = std::process::Command::new("gtk-update-icon-cache")
        .args(["-q", "-t", "-f"])
        .arg(base.join("icons/hicolor"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_and_removes_the_entry_and_icons() {
        let dir = tempfile::tempdir().unwrap();
        let exec = Path::new("/opt/My Apps/slate-0.2.0-x86_64.AppImage");
        install_into(dir.path(), exec).unwrap();
        let (desktop, icons) = paths(dir.path());
        let text = std::fs::read_to_string(&desktop).unwrap();
        assert!(
            text.contains("Exec=\"/opt/My Apps/slate-0.2.0-x86_64.AppImage\" %u"),
            "{text}"
        );
        assert!(text.contains(&format!("Icon={APP_ID}\n")));
        assert!(text.contains(&format!("StartupWMClass={APP_ID}\n")));
        for i in &icons {
            assert!(i.is_file(), "{}", i.display());
        }
        remove_from(dir.path()).unwrap();
        assert!(!desktop.exists() && icons.iter().all(|i| !i.exists()));
        remove_from(dir.path()).unwrap(); // removing twice is fine
    }

    #[test]
    fn exec_paths_are_escaped() {
        let text = desktop_file(Path::new("/x/a\"b$c"));
        assert!(text.contains(r#"Exec="/x/a\"b\$c" %u"#), "{text}");
    }
}
