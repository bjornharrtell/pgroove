//! Window/session state restored at startup.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// An open table tab and the connection it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTable {
    pub connection: String,
    pub database: String,
    pub schema: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    /// Open databases as `(profile name, database)`, reconnected at startup.
    #[serde(default)]
    pub connections: Vec<(String, String)>,
    /// Text of each query tab, in tab order.
    #[serde(default)]
    pub queries: Vec<String>,
    /// Open table tabs, reopened once their database is connected.
    #[serde(default)]
    pub tables: Vec<SessionTable>,
    /// Window size in logical pixels.
    #[serde(default)]
    pub window: Option<(f32, f32)>,
}

/// Smallest window the layout stays usable at, in logical pixels.
pub const MIN_WINDOW: (f32, f32) = (720.0, 480.0);

impl SessionState {
    pub fn default_path() -> Option<PathBuf> {
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .map(|d| d.join("pgroove").join("session.toml"))
    }

    /// Loads the state; a missing or unreadable file yields the default state.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
                log::warn!("ignoring session file {}: {e}", path.display());
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

    /// A window size worth restoring (at least the minimum, not absurd).
    pub fn usable_window(&self) -> Option<(f32, f32)> {
        self.window.filter(|(w, h)| {
            (MIN_WINDOW.0..=10_000.0).contains(w) && (MIN_WINDOW.1..=10_000.0).contains(h)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/s.toml");
        assert_eq!(SessionState::load(&path), SessionState::default());
        let s = SessionState {
            connections: vec![
                ("prod".into(), "shop".into()),
                ("prod".into(), "audit".into()),
            ],
            queries: vec!["select 1".into(), "".into()],
            tables: vec![SessionTable {
                connection: "prod".into(),
                database: "shop".into(),
                schema: "public".into(),
                name: "orders".into(),
            }],
            window: Some((1200.0, 800.0)),
        };
        s.save(&path).unwrap();
        assert_eq!(SessionState::load(&path), s);
        std::fs::write(&path, "queries = 3").unwrap();
        assert_eq!(SessionState::load(&path), SessionState::default());
        let _ = SessionState::default_path();
    }

    #[test]
    fn window_sanity() {
        let mut s = SessionState::default();
        assert_eq!(s.usable_window(), None);
        s.window = Some((10.0, 10.0));
        assert_eq!(s.usable_window(), None);
        s.window = Some((719.0, 800.0));
        assert_eq!(s.usable_window(), None, "narrower than the minimum");
        s.window = Some((MIN_WINDOW.0, MIN_WINDOW.1));
        assert!(s.usable_window().is_some());
        s.window = Some((20000.0, 800.0));
        assert_eq!(s.usable_window(), None);
        s.window = Some((900.0, 700.0));
        assert_eq!(s.usable_window(), Some((900.0, 700.0)));
    }
}
