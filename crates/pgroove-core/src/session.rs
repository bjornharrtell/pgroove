//! Window/session state restored at startup.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    /// Profile name connected when the app last closed.
    #[serde(default)]
    pub connection: Option<String>,
    /// Text of each query tab, in tab order.
    #[serde(default)]
    pub queries: Vec<String>,
    /// Open table tabs as `schema.table`, reopened after reconnecting.
    #[serde(default)]
    pub tables: Vec<(String, String)>,
    /// Window size in logical pixels.
    #[serde(default)]
    pub window: Option<(f32, f32)>,
}

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

    /// A window size worth restoring (not tiny, not absurd).
    pub fn usable_window(&self) -> Option<(f32, f32)> {
        self.window
            .filter(|(w, h)| (400.0..=10_000.0).contains(w) && (300.0..=10_000.0).contains(h))
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
            connection: Some("prod".into()),
            queries: vec!["select 1".into(), "".into()],
            tables: vec![("public".into(), "orders".into())],
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
        s.window = Some((20000.0, 800.0));
        assert_eq!(s.usable_window(), None);
        s.window = Some((900.0, 700.0));
        assert_eq!(s.usable_window(), Some((900.0, 700.0)));
    }
}
