//! Desktop integration for an AppImage: a launcher entry and icon in the user's data dir,
//! so the app shows up in menus, can be pinned to a dock and gets its icon.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const ICON_PNG: &[u8] = include_bytes!("../../../packaging/pgroove.png");
const ICON_SVG: &[u8] = include_bytes!("../../../packaging/pgroove.svg");

/// The window class the app sets; the launcher entry's name and `StartupWMClass` match it.
pub const APP_ID: &str = "pgroove";

/// Quotes a path for the `Exec` key of a desktop entry.
fn exec_quote(path: &str) -> String {
    let mut out = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The launcher entry that starts the AppImage at `appimage`.
pub fn desktop_entry(appimage: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=pgroove\nGenericName=PostgreSQL admin\n\
         Comment=Browse, query and monitor PostgreSQL databases\nExec={} %U\nIcon={APP_ID}\n\
         Terminal=false\nCategories=Development;Database;\nKeywords=postgres;postgresql;sql;database;\n\
         StartupWMClass={APP_ID}\n",
        exec_quote(&appimage.to_string_lossy())
    )
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

/// Installs the launcher entry and icons under `data_dir` (`~/.local/share`); true when
/// anything was written. Safe to call on every start.
pub fn integrate(appimage: &Path, data_dir: &Path) -> Result<bool> {
    let mut changed = false;
    changed |= write_if_changed(
        &data_dir
            .join("applications")
            .join(format!("{APP_ID}.desktop")),
        desktop_entry(appimage).as_bytes(),
    )?;
    let icons = data_dir.join("icons").join("hicolor");
    changed |= write_if_changed(
        &icons.join("256x256/apps").join(format!("{APP_ID}.png")),
        ICON_PNG,
    )?;
    changed |= write_if_changed(
        &icons.join("scalable/apps").join(format!("{APP_ID}.svg")),
        ICON_SVG,
    )?;
    Ok(changed)
}

/// Integrates when running from an AppImage (unless `PGROOVE_NO_INTEGRATION` is set).
pub fn integrate_current() {
    let Some(appimage) = std::env::var_os("APPIMAGE").map(PathBuf::from) else {
        return;
    };
    if std::env::var_os("PGROOVE_NO_INTEGRATION").is_some() {
        return;
    }
    let Some(data) = dirs::data_dir() else {
        return;
    };
    match integrate(&appimage, &data) {
        Ok(true) => log::info!("installed the launcher entry for {}", appimage.display()),
        Ok(false) => {}
        Err(e) => log::warn!("desktop integration failed: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_exec_paths() {
        assert_eq!(exec_quote("/a b/p.AppImage"), "\"/a b/p.AppImage\"");
        assert_eq!(exec_quote("/a\"$`\\%"), "\"/a\\\"\\$\\`\\\\%%\"");
    }

    #[test]
    fn entry_points_at_the_appimage() {
        let e = desktop_entry(Path::new("/opt/pg root/pgroove.AppImage"));
        assert!(e.contains("Exec=\"/opt/pg root/pgroove.AppImage\" %U\n"));
        assert!(e.contains("Icon=pgroove\n") && e.contains("StartupWMClass=pgroove\n"));
    }

    #[test]
    fn writes_once_then_updates_when_moved() {
        let dir = tempfile::tempdir().unwrap();
        assert!(integrate(Path::new("/a/pgroove.AppImage"), dir.path()).unwrap());
        assert!(!integrate(Path::new("/a/pgroove.AppImage"), dir.path()).unwrap());
        assert!(integrate(Path::new("/b/pgroove.AppImage"), dir.path()).unwrap());
        let desktop =
            std::fs::read_to_string(dir.path().join("applications/pgroove.desktop")).unwrap();
        assert!(desktop.contains("/b/pgroove.AppImage"));
        assert!(
            dir.path()
                .join("icons/hicolor/256x256/apps/pgroove.png")
                .is_file()
        );
        assert!(
            dir.path()
                .join("icons/hicolor/scalable/apps/pgroove.svg")
                .is_file()
        );
    }
}
