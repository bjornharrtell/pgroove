//! User preferences stored in `settings.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Look for a newer release when the app starts.
    #[serde(default = "yes")]
    pub check_for_updates: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            check_for_updates: true,
        }
    }
}

impl Settings {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("pgroove").join("settings.toml"))
    }

    /// Loads the settings; a missing or unreadable file yields the defaults.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
                log::warn!("ignoring settings file {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip_and_tolerance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/settings.toml");
        assert!(Settings::load(&path).check_for_updates);
        let off = Settings {
            check_for_updates: false,
        };
        off.save(&path).unwrap();
        assert_eq!(Settings::load(&path), off);
        std::fs::write(&path, "").unwrap();
        assert!(
            Settings::load(&path).check_for_updates,
            "missing key defaults to on"
        );
        std::fs::write(&path, "check_for_updates = 3").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        let _ = Settings::default_path();
    }
}
