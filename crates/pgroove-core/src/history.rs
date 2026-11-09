//! Persisted query history and saved snippets.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Oldest entries are dropped beyond this many.
pub const MAX_HISTORY: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub sql: String,
    /// Seconds since the Unix epoch.
    pub at: u64,
    pub millis: u64,
    pub ok: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct HistoryFile {
    #[serde(default, rename = "entry")]
    entries: Vec<HistoryEntry>,
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Describes how long ago `at` was, e.g. `just now`, `5 min ago`, `3 h ago`, `2 d ago`.
pub fn relative_time(at: u64, now: u64) -> String {
    let d = now.saturating_sub(at);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86399 => format!("{} h ago", d / 3600),
        _ => format!("{} d ago", d / 86400),
    }
}

fn read_text(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// Most recent queries, newest last.
pub struct History {
    path: PathBuf,
    entries: Vec<HistoryEntry>,
}

impl History {
    pub fn default_path() -> Option<PathBuf> {
        dirs::data_local_dir().map(|d| d.join("pgroove").join("history.toml"))
    }

    /// A history that is never written to disk.
    pub fn in_memory() -> Self {
        Self {
            path: PathBuf::new(),
            entries: Vec::new(),
        }
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let entries = match read_text(&path)? {
            Some(t) => {
                toml::from_str::<HistoryFile>(&t)
                    .with_context(|| format!("parsing {}", path.display()))?
                    .entries
            }
            None => Vec::new(),
        };
        Ok(Self { path, entries })
    }

    /// Newest first.
    pub fn recent(&self) -> impl Iterator<Item = &HistoryEntry> {
        self.entries.iter().rev()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records a run; an identical statement run again moves to the newest position.
    pub fn push(&mut self, entry: HistoryEntry) -> Result<()> {
        if entry.sql.trim().is_empty() {
            return Ok(());
        }
        self.entries.retain(|e| e.sql != entry.sql);
        self.entries.push(entry);
        if self.entries.len() > MAX_HISTORY {
            let extra = self.entries.len() - MAX_HISTORY;
            self.entries.drain(..extra);
        }
        self.save()
    }

    pub fn clear(&mut self) -> Result<()> {
        self.entries.clear();
        self.save()
    }

    fn save(&self) -> Result<()> {
        write_text(
            &self.path,
            &toml::to_string_pretty(&HistoryFile {
                entries: self.entries.clone(),
            })?,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub name: String,
    pub sql: String,
}

#[derive(Default, Serialize, Deserialize)]
struct SnippetFile {
    #[serde(default, rename = "snippet")]
    snippets: Vec<Snippet>,
}

/// Saved named snippets.
pub struct Snippets {
    path: PathBuf,
    items: Vec<Snippet>,
}

impl Snippets {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("pgroove").join("snippets.toml"))
    }

    /// Snippets that are never written to disk.
    pub fn in_memory() -> Self {
        Self {
            path: PathBuf::new(),
            items: Vec::new(),
        }
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let items = match read_text(&path)? {
            Some(t) => {
                toml::from_str::<SnippetFile>(&t)
                    .with_context(|| format!("parsing {}", path.display()))?
                    .snippets
            }
            None => Vec::new(),
        };
        Ok(Self { path, items })
    }

    pub fn items(&self) -> &[Snippet] {
        &self.items
    }

    /// Adds a snippet, replacing one with the same name.
    pub fn save(&mut self, name: &str, sql: &str) -> Result<()> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "Enter a name");
        anyhow::ensure!(!sql.trim().is_empty(), "There is no SQL to save");
        let snippet = Snippet {
            name: name.to_string(),
            sql: sql.to_string(),
        };
        match self.items.iter().position(|s| s.name == name) {
            Some(i) => self.items[i] = snippet,
            None => self.items.push(snippet),
        }
        self.persist()
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        self.items.retain(|s| s.name != name);
        self.persist()
    }

    fn persist(&self) -> Result<()> {
        write_text(
            &self.path,
            &toml::to_string_pretty(&SnippetFile {
                snippets: self.items.clone(),
            })?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sql: &str, at: u64) -> HistoryEntry {
        HistoryEntry {
            sql: sql.into(),
            at,
            millis: 5,
            ok: true,
        }
    }

    #[test]
    fn relative_times() {
        assert_eq!(relative_time(100, 100), "just now");
        assert_eq!(relative_time(0, 59), "just now");
        assert_eq!(relative_time(0, 300), "5 min ago");
        assert_eq!(relative_time(0, 7200), "2 h ago");
        assert_eq!(relative_time(0, 3 * 86400), "3 d ago");
        assert_eq!(relative_time(500, 100), "just now");
        assert!(now_secs() > 1_000_000_000);
    }

    #[test]
    fn history_persists_dedups_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/h.toml");
        let mut h = History::load(&path).unwrap();
        assert!(h.is_empty());
        h.push(entry("select 1", 1)).unwrap();
        h.push(entry("select 2", 2)).unwrap();
        h.push(entry("select 1", 3)).unwrap();
        h.push(entry("   ", 4)).unwrap();
        let sqls: Vec<_> = h.recent().map(|e| e.sql.as_str()).collect();
        assert_eq!(sqls, ["select 1", "select 2"]);
        let again = History::load(&path).unwrap();
        assert_eq!(again.len(), 2);
        assert_eq!(again.recent().next().unwrap().at, 3);

        let mut h = again;
        for i in 0..MAX_HISTORY + 5 {
            h.push(entry(&format!("select {i}"), i as u64)).unwrap();
        }
        assert_eq!(h.len(), MAX_HISTORY);
        assert_eq!(
            h.recent().next().unwrap().sql,
            format!("select {}", MAX_HISTORY + 4)
        );
        h.clear().unwrap();
        assert!(History::load(&path).unwrap().is_empty());
    }

    #[test]
    fn corrupt_files_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.toml");
        std::fs::write(&path, "entry = 3").unwrap();
        assert!(History::load(&path).is_err());
        std::fs::write(&path, "snippet = 3").unwrap();
        assert!(Snippets::load(&path).is_err());
        let _ = (History::default_path(), Snippets::default_path());
        let mut h = History::in_memory();
        h.push(entry("select 1", 1)).unwrap();
        assert_eq!(h.len(), 1);
        let mut s = Snippets::in_memory();
        s.save("a", "select 1").unwrap();
        assert_eq!(s.items().len(), 1);
    }

    #[test]
    fn snippets_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.toml");
        let mut s = Snippets::load(&path).unwrap();
        assert!(s.items().is_empty());
        s.save(" top orders ", "select * from orders limit 10")
            .unwrap();
        s.save("top orders", "select 2").unwrap();
        s.save("other", "select 3").unwrap();
        assert!(s.save("  ", "x").is_err());
        assert!(s.save("n", "  ").is_err());
        let loaded = Snippets::load(&path).unwrap();
        assert_eq!(loaded.items().len(), 2);
        assert_eq!(loaded.items()[0].sql, "select 2");
        let mut loaded = loaded;
        loaded.remove("top orders").unwrap();
        assert_eq!(Snippets::load(&path).unwrap().items()[0].name, "other");
    }
}
