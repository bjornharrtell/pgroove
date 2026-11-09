//! User preferences stored in `settings.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Rows a query returns unless the user picks another limit.
pub const DEFAULT_ROW_LIMIT: u32 = 200;
/// The largest limit; bigger results go through Export.
pub const MAX_ROW_LIMIT: u32 = 10_000;
/// Limits offered in the query tab.
pub const ROW_LIMIT_PRESETS: [u32; 7] = [50, 100, 200, 500, 1_000, 5_000, MAX_ROW_LIMIT];

/// Keeps a row limit within `1..=MAX_ROW_LIMIT`.
pub fn clamp_row_limit(n: u32) -> u32 {
    n.clamp(1, MAX_ROW_LIMIT)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Look for a newer release when the app starts.
    #[serde(default = "yes")]
    pub check_for_updates: bool,
    /// Rows shown per query run (the Run button always limits).
    #[serde(default = "default_limit")]
    pub query_row_limit: u32,
}

fn default_limit() -> u32 {
    DEFAULT_ROW_LIMIT
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            check_for_updates: true,
            query_row_limit: DEFAULT_ROW_LIMIT,
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
    fn row_limits_are_clamped() {
        assert_eq!(clamp_row_limit(0), 1);
        assert_eq!(clamp_row_limit(200), 200);
        assert_eq!(clamp_row_limit(50_000), MAX_ROW_LIMIT);
        assert!(ROW_LIMIT_PRESETS.contains(&DEFAULT_ROW_LIMIT));
        assert!(
            ROW_LIMIT_PRESETS
                .iter()
                .all(|n| (1..=MAX_ROW_LIMIT).contains(n))
        );
    }

    #[test]
    fn defaults_roundtrip_and_tolerance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/settings.toml");
        assert!(Settings::load(&path).check_for_updates);
        let off = Settings {
            check_for_updates: false,
            query_row_limit: 1000,
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
