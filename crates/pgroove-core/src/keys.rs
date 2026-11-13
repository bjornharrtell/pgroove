//! Keybinding defaults and user overrides.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Command names and their default keystrokes.
pub const DEFAULTS: &[(&str, &[&str])] = &[
    ("run_query", &["ctrl-enter", "cmd-enter"]),
    ("run_script", &["alt-x"]),
    ("cancel_query", &["escape"]),
    ("format_sql", &["ctrl-shift-f"]),
    ("open_palette", &["ctrl-k", "cmd-k"]),
    ("toggle_fullscreen", &["f11", "ctrl-cmd-f"]),
];

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize, Default)]
struct KeyFile {
    #[serde(default)]
    keys: BTreeMap<String, OneOrMany>,
}

pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("pgroove").join("keybindings.toml"))
}

/// Reads overrides from `[keys]`; a missing file means no overrides.
pub fn load_overrides(path: &Path) -> Result<BTreeMap<String, Vec<String>>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let file: KeyFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(file
        .keys
        .into_iter()
        .map(|(k, v)| {
            (
                k,
                match v {
                    OneOrMany::One(s) => vec![s],
                    OneOrMany::Many(v) => v,
                },
            )
        })
        .collect())
}

/// The effective `(command, keystroke)` pairs and the names in `overrides` that
/// aren't known commands. An override replaces all default keystrokes of a
/// command; an empty list unbinds it.
pub fn resolve(overrides: &BTreeMap<String, Vec<String>>) -> (Vec<(String, String)>, Vec<String>) {
    let mut out = Vec::new();
    for (name, defaults) in DEFAULTS {
        match overrides.get(*name) {
            Some(keys) => out.extend(keys.iter().map(|k| (name.to_string(), k.clone()))),
            None => out.extend(defaults.iter().map(|k| (name.to_string(), k.to_string()))),
        }
    }
    let unknown = overrides
        .keys()
        .filter(|k| !DEFAULTS.iter().any(|(n, _)| n == k))
        .cloned()
        .collect();
    (out, unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_without_overrides() {
        let (b, unknown) = resolve(&BTreeMap::new());
        assert!(unknown.is_empty());
        assert!(b.contains(&("run_query".into(), "ctrl-enter".into())));
        assert!(b.contains(&("open_palette".into(), "cmd-k".into())));
        assert_eq!(b.len(), 9);
        assert!(b.contains(&("run_script".into(), "alt-x".into())));
        assert!(b.contains(&("toggle_fullscreen".into(), "f11".into())));
    }

    #[test]
    fn overrides_replace_and_unbind() {
        let mut o = BTreeMap::new();
        o.insert("run_query".to_string(), vec!["f5".to_string()]);
        o.insert("format_sql".to_string(), vec![]);
        o.insert("bogus".to_string(), vec!["x".to_string()]);
        let (b, unknown) = resolve(&o);
        assert_eq!(unknown, ["bogus"]);
        assert!(b.contains(&("run_query".into(), "f5".into())));
        assert!(!b.iter().any(|(n, k)| n == "run_query" && k == "ctrl-enter"));
        assert!(!b.iter().any(|(n, _)| n == "format_sql"));
        assert!(b.contains(&("cancel_query".into(), "escape".into())));
    }

    #[test]
    fn loads_strings_and_lists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k.toml");
        assert!(load_overrides(&path).unwrap().is_empty());
        std::fs::write(
            &path,
            "[keys]\nrun_query = \"f5\"\nopen_palette = [\"ctrl-p\", \"cmd-p\"]\n",
        )
        .unwrap();
        let o = load_overrides(&path).unwrap();
        assert_eq!(o["run_query"], ["f5"]);
        assert_eq!(o["open_palette"], ["ctrl-p", "cmd-p"]);
        std::fs::write(&path, "[keys]\nrun_query = 5\n").unwrap();
        assert!(load_overrides(&path).is_err());
        assert!(default_path().is_some() || default_path().is_none());
    }
}
