//! Atomic replacement of a file's contents.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// A sibling path of `path` with `suffix` appended, e.g. `connections.toml.part`.
pub fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Writes `bytes` to `path` so the target is either the old or the new complete file.
///
/// The data goes to a sibling `.part` file in the same directory, is flushed and synced,
/// and is then renamed over `path`. Parent directories are created as needed. An empty
/// path writes nothing. On failure the `.part` file is removed and the error names `path`.
pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    write_part(path, bytes).with_context(|| format!("writing {}", path.display()))
}

fn write_part(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = sibling(path, ".part");
    if let Err(e) = write_and_rename(&part, path, bytes) {
        let _ = std::fs::remove_file(&part);
        return Err(e.into());
    }
    Ok(())
}

fn write_and_rename(part: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(part)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(part, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part_of(path: &Path) -> PathBuf {
        sibling(path, ".part")
    }

    #[test]
    fn replaces_content_and_leaves_no_part() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.toml");
        std::fs::write(&path, "old").unwrap();
        write(&path, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert!(!part_of(&path).exists());
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x/y/z.toml");
        write(&path, b"data").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "data");
        assert!(!part_of(&path).exists());
    }

    #[test]
    fn empty_path_is_a_no_op() {
        write(Path::new(""), b"data").unwrap();
        assert!(!sibling(Path::new(""), ".part").exists());
    }

    #[test]
    fn failed_write_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.toml");
        std::fs::write(&path, "old").unwrap();
        // A directory where the part file goes makes creating it fail.
        std::fs::create_dir(part_of(&path)).unwrap();
        let err = write(&path, b"new").unwrap_err();
        assert!(format!("{err:#}").contains("a.toml"), "{err:#}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    }

    #[test]
    fn directory_target_fails_and_leaves_no_part() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.toml");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("keep"), "old").unwrap();
        assert!(write(&path, b"new").is_err());
        assert!(path.is_dir(), "the existing entry survives");
        assert_eq!(
            std::fs::read_to_string(path.join("keep")).unwrap(),
            "old",
            "its contents survive"
        );
        assert!(!part_of(&path).exists());
    }
}
