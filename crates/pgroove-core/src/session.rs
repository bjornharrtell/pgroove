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

/// A schema that was expanded in the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionExpanded {
    pub connection: String,
    pub database: String,
    pub schema: String,
}

/// Which tab was in front.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ActiveTab {
    /// The n-th query tab (0-based).
    Query { index: usize },
    Table {
        connection: String,
        database: String,
        schema: String,
        name: String,
    },
    Activity {
        connection: String,
        database: String,
    },
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
    /// Width of the sidebar, in logical pixels.
    #[serde(default)]
    pub sidebar_width: Option<f32>,
    /// Height of the SQL editor pane, in logical pixels.
    #[serde(default)]
    pub editor_height: Option<f32>,
    /// Open activity monitors as `(profile name, database)`.
    #[serde(default)]
    pub activity: Vec<(String, String)>,
    /// Schemas expanded in the tree.
    #[serde(default)]
    pub expanded: Vec<SessionExpanded>,
    /// The tab that was in front.
    #[serde(default)]
    pub active: Option<ActiveTab>,
    /// Text in the tree's search box.
    #[serde(default)]
    pub tree_filter: Option<String>,
    /// Connection groups collapsed in the sidebar (lowercase names).
    #[serde(default)]
    pub collapsed_groups: Vec<String>,
}

/// Default and allowed sizes of the resizable panes, in logical pixels.
pub const SIDEBAR_WIDTH: (f32, f32, f32) = (260.0, 180.0, 560.0);
pub const EDITOR_HEIGHT: (f32, f32, f32) = (170.0, 80.0, 1200.0);

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

    /// The sidebar width to use: the saved one if sensible, else the default.
    pub fn sidebar_width_or_default(&self) -> f32 {
        sane(self.sidebar_width, SIDEBAR_WIDTH)
    }

    /// The editor pane height to use: the saved one if sensible, else the default.
    pub fn editor_height_or_default(&self) -> f32 {
        sane(self.editor_height, EDITOR_HEIGHT)
    }

    /// A window size worth restoring (at least the minimum, not absurd).
    pub fn usable_window(&self) -> Option<(f32, f32)> {
        self.window.filter(|(w, h)| {
            (MIN_WINDOW.0..=10_000.0).contains(w) && (MIN_WINDOW.1..=10_000.0).contains(h)
        })
    }
}

/// A saved size when it is finite and within `(default, min, max)`; otherwise the default.
pub fn sane(saved: Option<f32>, (default, min, max): (f32, f32, f32)) -> f32 {
    saved
        .filter(|v| v.is_finite() && (min..=max).contains(v))
        .unwrap_or(default)
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
            sidebar_width: Some(300.0),
            editor_height: Some(220.0),
            activity: vec![("prod".into(), "shop".into())],
            expanded: vec![SessionExpanded {
                connection: "prod".into(),
                database: "shop".into(),
                schema: "public".into(),
            }],
            active: Some(ActiveTab::Table {
                connection: "prod".into(),
                database: "shop".into(),
                schema: "public".into(),
                name: "orders".into(),
            }),
            tree_filter: Some("ord".into()),
            collapsed_groups: vec!["work".into()],
        };
        s.save(&path).unwrap();
        assert_eq!(SessionState::load(&path), s);
        // Each kind of active tab survives a round trip.
        for active in [
            ActiveTab::Query { index: 2 },
            ActiveTab::Activity {
                connection: "a".into(),
                database: "b".into(),
            },
        ] {
            let t = SessionState {
                active: Some(active),
                ..SessionState::default()
            };
            t.save(&path).unwrap();
            assert_eq!(SessionState::load(&path), t);
        }
        std::fs::write(&path, "queries = 3").unwrap();
        assert_eq!(SessionState::load(&path), SessionState::default());
        let _ = SessionState::default_path();
    }

    #[test]
    fn pane_sizes_fall_back_when_unreasonable() {
        let mut s = SessionState::default();
        assert_eq!(s.sidebar_width_or_default(), SIDEBAR_WIDTH.0);
        assert_eq!(s.editor_height_or_default(), EDITOR_HEIGHT.0);
        s.sidebar_width = Some(320.0);
        s.editor_height = Some(400.0);
        assert_eq!(
            (s.sidebar_width_or_default(), s.editor_height_or_default()),
            (320.0, 400.0)
        );
        for bad in [0.0, -5.0, 9999.0, f32::NAN, f32::INFINITY] {
            s.sidebar_width = Some(bad);
            s.editor_height = Some(bad);
            assert_eq!(s.sidebar_width_or_default(), SIDEBAR_WIDTH.0, "{bad}");
            assert_eq!(s.editor_height_or_default(), EDITOR_HEIGHT.0, "{bad}");
        }
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
