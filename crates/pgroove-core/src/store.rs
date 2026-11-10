use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{ConnectionProfile, atomic};

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "connection")]
    connections: Vec<ConnectionProfile>,
}

/// Saved profiles, persisted as TOML.
pub struct ProfileStore {
    path: PathBuf,
    profiles: Vec<ConnectionProfile>,
}

impl ProfileStore {
    /// Default location: `<config dir>/pgroove/connections.toml`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("pgroove").join("connections.toml"))
    }

    /// Loads the store; a missing file yields an empty store and an unparseable one is
    /// moved aside to `<path>.bad` so the app still starts.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let profiles = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<File>(&text) {
                Ok(file) => file.connections,
                Err(e) => {
                    let bad = set_aside(&path)?;
                    log::warn!(
                        "ignoring connections file {}: {e}; moved to {}",
                        path.display(),
                        bad.display()
                    );
                    Vec::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self { path, profiles })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn profiles(&self) -> &[ConnectionProfile] {
        &self.profiles
    }

    /// Adds (`original_name` is `None`) or replaces the profile named `original_name`, then saves.
    pub fn upsert(
        &mut self,
        original_name: Option<&str>,
        profile: ConnectionProfile,
    ) -> Result<()> {
        profile.validate().map_err(anyhow::Error::msg)?;
        let idx = original_name.and_then(|k| self.profiles.iter().position(|p| p.name == k));
        let clash = self
            .profiles
            .iter()
            .enumerate()
            .any(|(i, p)| p.name == profile.name && Some(i) != idx);
        if clash {
            bail!("A connection named {} already exists", profile.name);
        }
        match idx {
            Some(i) => self.profiles[i] = profile,
            None => self.profiles.push(profile),
        }
        self.save()
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        self.profiles.retain(|p| p.name != name);
        self.save()
    }

    fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(&File {
            connections: self.profiles.clone(),
        })?;
        atomic::write(&self.path, text.as_bytes())
    }
}

/// Renames `path` to a sibling `<path>.bad`, replacing an older one, and returns it.
fn set_aside(path: &Path) -> Result<PathBuf> {
    let bad = atomic::sibling(path, ".bad");
    let _ = std::fs::remove_file(&bad);
    std::fs::rename(path, &bad)
        .with_context(|| format!("moving {} to {}", path.display(), bad.display()))?;
    Ok(bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> ConnectionProfile {
        ConnectionProfile {
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let s = ProfileStore::load(dir.path().join("x.toml")).unwrap();
        assert!(s.profiles().is_empty());
        assert!(s.path().ends_with("x.toml"));
    }

    #[test]
    fn roundtrip_upsert_rename_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/c.toml");
        let mut s = ProfileStore::load(&path).unwrap();
        s.upsert(None, p("a")).unwrap();
        s.upsert(None, p("b")).unwrap();
        s.upsert(Some("a"), p("c")).unwrap();
        let s2 = ProfileStore::load(&path).unwrap();
        let names: Vec<_> = s2.profiles().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["c", "b"]);
        let mut s2 = s2;
        s2.remove("c").unwrap();
        assert_eq!(ProfileStore::load(&path).unwrap().profiles().len(), 1);
    }

    #[test]
    fn rejects_duplicates_and_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = ProfileStore::load(dir.path().join("c.toml")).unwrap();
        s.upsert(None, p("a")).unwrap();
        s.upsert(None, p("b")).unwrap();
        assert!(s.upsert(None, p("a")).is_err());
        assert!(s.upsert(Some("b"), p("a")).is_err());
        assert!(s.upsert(None, p("")).is_err());
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        let bad = dir.path().join("c.toml.bad");
        std::fs::write(&bad, "older").unwrap();
        std::fs::write(&path, "connection = 3").unwrap();
        let s = ProfileStore::load(&path).unwrap();
        assert!(s.profiles().is_empty(), "a bad file yields an empty store");
        assert!(!path.exists(), "the broken file is gone from its place");
        assert_eq!(
            std::fs::read_to_string(&bad).unwrap(),
            "connection = 3",
            "it replaces an older .bad file"
        );
    }

    #[test]
    fn save_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        let mut s = ProfileStore::load(&path).unwrap();
        s.upsert(None, p("a")).unwrap();
        s.upsert(None, p("b")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[connection]]"), "{text}");
        assert!(!dir.path().join("c.toml.part").exists());
    }

    #[test]
    fn unreadable_file_still_errors() {
        let dir = tempfile::tempdir().unwrap();
        // Reading a directory as a file fails for a reason other than not-found.
        assert!(ProfileStore::load(dir.path()).is_err());
    }
}
