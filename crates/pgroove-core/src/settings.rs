//! User preferences stored in `settings.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Which theme variant the app shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the desktop's light or dark setting.
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "follow the system",
            Appearance::Light => "always light",
            Appearance::Dark => "always dark",
        }
    }
}

pub const DEFAULT_LIGHT_THEME: &str = "Catppuccin Latte";
pub const DEFAULT_DARK_THEME: &str = "Catppuccin Mocha";

/// Folder where users can drop their own theme files (`*.json`).
pub fn themes_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("pgroove").join("themes"))
}

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
    /// Rows per page in the data viewer.
    #[serde(default = "default_limit")]
    pub table_page_size: u32,
    #[serde(default)]
    pub appearance: Appearance,
    /// Theme used when the appearance is light (or the system is light).
    #[serde(default = "default_light")]
    pub light_theme: String,
    /// Theme used when the appearance is dark (or the system is dark).
    #[serde(default = "default_dark")]
    pub dark_theme: String,
    /// The one-time offer to import DBeaver connections has been shown.
    #[serde(default)]
    pub dbeaver_import_offered: bool,
}

fn default_light() -> String {
    DEFAULT_LIGHT_THEME.to_string()
}

fn default_dark() -> String {
    DEFAULT_DARK_THEME.to_string()
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
            table_page_size: DEFAULT_ROW_LIMIT,
            appearance: Appearance::System,
            light_theme: default_light(),
            dark_theme: default_dark(),
            dbeaver_import_offered: false,
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
            table_page_size: 500,
            appearance: Appearance::Dark,
            light_theme: "My Light".into(),
            dark_theme: "Catppuccin Frappe".into(),
            dbeaver_import_offered: true,
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
