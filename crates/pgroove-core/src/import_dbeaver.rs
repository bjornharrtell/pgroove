//! One-time import of connections from DBeaver's workspace (read only).
//!
//! DBeaver keeps connection definitions in `.dbeaver/data-sources*.json` and the saved user
//! names and passwords in `.dbeaver/credentials-config.json`, encrypted with AES-128-CBC under
//! a fixed, publicly documented key (obfuscation, not protection).

use std::path::{Path, PathBuf};

use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::profile::{ConnectionProfile, ProfileColor, SslMode};

const KEY: [u8; 16] = [
    0xba, 0xbb, 0x4a, 0x9f, 0x77, 0x4a, 0xb8, 0x53, 0xc9, 0x6c, 0x2d, 0x65, 0x3d, 0xfe, 0x54, 0x4a,
];

/// A connection found in DBeaver, ready to import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub profile: ConnectionProfile,
    /// The saved password, when there is one and it could be read.
    pub password: Option<String>,
    /// Something the user should know, e.g. why there is no password.
    pub note: Option<String>,
}

/// What a project (or all projects) yielded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub candidates: Vec<Candidate>,
    /// Connections left out (other databases, unreadable entries).
    pub skipped: usize,
}

/// Directories that may hold `DBeaverData`: native, Flatpak and Snap installs.
fn data_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(d) = dirs::data_dir() {
        roots.push(d.join("DBeaverData"));
    }
    if let Some(h) = dirs::home_dir() {
        roots.push(h.join("Library/DBeaverData"));
        roots.push(h.join(".var/app/io.dbeaver.DBeaverCommunity/data/DBeaverData"));
        roots.push(h.join("snap/dbeaver-ce/current/.local/share/DBeaverData"));
    }
    if let Some(d) = dirs::config_dir() {
        roots.push(d.join("DBeaverData"));
    }
    roots.sort();
    roots.dedup();
    roots
}

/// The `.dbeaver` folders of every DBeaver project that has connection definitions.
pub fn find_projects() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in data_roots() {
        let Ok(entries) = std::fs::read_dir(root.join("workspace6")) else {
            continue;
        };
        for e in entries.flatten() {
            let dir = e.path().join(".dbeaver");
            if !data_source_files(&dir).is_empty() {
                out.push(dir);
            }
        }
    }
    out.sort();
    out
}

fn data_source_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("data-sources") && n.ends_with(".json"))
        })
        .collect();
    files.sort();
    files
}

/// Decrypts `credentials-config.json`: a 16-byte IV followed by AES-128-CBC data.
pub fn decrypt_credentials(bytes: &[u8]) -> Result<Value> {
    if bytes.len() < 32 {
        return Err(anyhow!("credentials file is too short"));
    }
    let (iv, data) = bytes.split_at(16);
    let plain = cbc::Decryptor::<aes::Aes128>::new(&KEY.into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .map_err(|_| anyhow!("could not decrypt the credentials"))?;
    serde_json::from_slice(&plain).context("credentials are not valid JSON")
}

/// Host, port, database, user and sslmode from a `jdbc:postgresql://` URL.
fn parse_jdbc(url: &str) -> Option<ConnectionProfile> {
    let rest = url.strip_prefix("jdbc:")?;
    let parsed = url::Url::parse(rest).ok()?;
    if !matches!(parsed.scheme(), "postgresql" | "postgres") {
        return None;
    }
    let mut p = ConnectionProfile {
        host: parsed.host_str()?.to_string(),
        port: parsed.port().unwrap_or(5432),
        ..Default::default()
    };
    let db = parsed.path().trim_start_matches('/');
    if !db.is_empty() {
        p.database = db.to_string();
    }
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "sslmode" => p.ssl = SslMode::from_libpq(&v).unwrap_or_default(),
            "ssl" if v == "true" => p.ssl = SslMode::Require,
            "user" => p.user = v.into_owned(),
            _ => {}
        }
    }
    Some(p)
}

fn color_for(kind: &str) -> Option<ProfileColor> {
    match kind {
        "prod" => Some(ProfileColor::Red),
        "test" => Some(ProfileColor::Yellow),
        _ => None,
    }
}

/// Reads one `data-sources*.json` with the decrypted credentials (if any).
pub fn parse_data_sources(json: &str, credentials: Option<&Value>) -> Result<Found> {
    let doc: Value = serde_json::from_str(json).context("data-sources is not valid JSON")?;
    let mut found = Found::default();
    let Some(conns) = doc.get("connections").and_then(Value::as_object) else {
        return Ok(found);
    };
    for (id, c) in conns {
        let is_pg = c.get("provider").and_then(Value::as_str) == Some("postgresql");
        let conf = c.get("configuration").unwrap_or(&Value::Null);
        let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let mut profile = if is_pg && text(conf, "configurationType") == "URL" {
            parse_jdbc(&text(conf, "url"))
        } else {
            None
        }
        .or_else(|| {
            is_pg.then(|| ConnectionProfile {
                host: text(conf, "host"),
                port: text(conf, "port").parse().unwrap_or(5432),
                database: Some(text(conf, "database"))
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| "postgres".into()),
                ..Default::default()
            })
        })
        .filter(|p| !p.host.is_empty());
        let Some(p) = profile.as_mut() else {
            found.skipped += 1;
            continue;
        };
        p.name = Some(text(c, "name"))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("{}/{}", p.host, p.database));
        p.group = text(c, "folder");
        p.color = color_for(&text(conf, "type"));
        let cred = credentials
            .and_then(|v| v.get(id))
            .and_then(|v| v.get("#connection"));
        let mut note = None;
        if let Some(u) = cred.and_then(|v| v.get("user")).and_then(Value::as_str) {
            p.user = u.to_string();
        }
        let password = cred
            .and_then(|v| v.get("password"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty());
        if password.is_none() {
            note = Some(
                if c.get("save-password").and_then(Value::as_bool) == Some(true) {
                    "no saved password could be read".to_string()
                } else {
                    "no saved password".to_string()
                },
            );
        }
        found.candidates.push(Candidate {
            profile: profile.take().unwrap_or_default(),
            password,
            note,
        });
    }
    Ok(found)
}

/// Reads one project's `.dbeaver` folder.
pub fn load_project(dir: &Path) -> Result<Found> {
    let credentials = std::fs::read(dir.join("credentials-config.json"))
        .ok()
        .and_then(|b| match decrypt_credentials(&b) {
            Ok(v) => Some(v),
            Err(e) => {
                log::warn!("DBeaver credentials unreadable: {e:#}");
                None
            }
        });
    let mut all = Found::default();
    for file in data_source_files(dir) {
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let f = parse_data_sources(&text, credentials.as_ref())?;
        all.candidates.extend(f.candidates);
        all.skipped += f.skipped;
    }
    Ok(all)
}

/// The `.dbeaver` folders a user-chosen path stands for: a project folder, its `.dbeaver`
/// folder, a `data-sources*.json` file, or a workspace holding several projects.
pub fn projects_in(path: &Path) -> Vec<PathBuf> {
    let has_sources = |d: &Path| !data_source_files(d).is_empty();
    let path = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    if has_sources(path) {
        return vec![path.to_path_buf()];
    }
    let mut out = Vec::new();
    for base in [path.to_path_buf(), path.join("workspace6")] {
        if has_sources(&base.join(".dbeaver")) {
            out.push(base.join(".dbeaver"));
        }
        if let Ok(entries) = std::fs::read_dir(&base) {
            for e in entries.flatten() {
                let dir = e.path().join(".dbeaver");
                if has_sources(&dir) {
                    out.push(dir);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Everything importable from every DBeaver project on this machine.
pub fn detect() -> Found {
    load_projects(&find_projects())
}

/// Everything importable from the given `.dbeaver` folders.
pub fn load_projects(dirs: &[PathBuf]) -> Found {
    let mut all = Found::default();
    for dir in dirs {
        match load_project(dir) {
            Ok(f) => {
                all.candidates.extend(f.candidates);
                all.skipped += f.skipped;
            }
            Err(e) => log::warn!("skipping DBeaver project {}: {e:#}", dir.display()),
        }
    }
    all
}

/// Drops candidates that are already saved (same name, or same server, database and user) and
/// makes the remaining names unique.
pub fn only_new(candidates: Vec<Candidate>, existing: &[ConnectionProfile]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for mut c in candidates {
        let same_server = |p: &ConnectionProfile| {
            p.host == c.profile.host
                && p.port == c.profile.port
                && p.database == c.profile.database
                && p.user == c.profile.user
        };
        if existing.iter().any(same_server) || out.iter().any(|o| same_server(&o.profile)) {
            continue;
        }
        let base = c.profile.name.clone();
        let mut n = 1;
        while existing.iter().any(|p| p.name == c.profile.name)
            || out.iter().any(|o| o.profile.name == c.profile.name)
        {
            n += 1;
            c.profile.name = format!("{base} ({n})");
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;

    fn encrypt(plain: &[u8]) -> Vec<u8> {
        let iv = [7u8; 16];
        let ct = cbc::Encryptor::<aes::Aes128>::new(&KEY.into(), &iv.into())
            .encrypt_padded_vec_mut::<Pkcs7>(plain);
        [iv.to_vec(), ct].concat()
    }

    const SOURCES: &str = r#"{
      "connections": {
        "a": {"provider":"postgresql","name":"docker 5433","save-password":true,"folder":"Local",
              "configuration":{"host":"localhost","port":"5432","database":"postgres",
              "url":"jdbc:postgresql://db.example:5433/shop?sslmode=require","configurationType":"URL","type":"prod"}},
        "b": {"provider":"postgresql","name":"manual","save-password":true,
              "configuration":{"host":"h2","port":"6543","database":"","configurationType":"MANUAL","type":"dev"}},
        "c": {"provider":"mysql","name":"other","configuration":{"host":"x"}},
        "d": {"provider":"postgresql","name":"nohost","configuration":{"host":""}}
      }}"#;

    #[test]
    fn parses_connections_and_credentials() {
        let creds = serde_json::json!({
            "a": {"#connection": {"user": "alice", "password": "s3cret"}},
            "b": {"#connection": {"user": "bob"}}
        });
        let f = parse_data_sources(SOURCES, Some(&creds)).unwrap();
        assert_eq!(f.skipped, 2);
        assert_eq!(f.candidates.len(), 2);
        let a = f
            .candidates
            .iter()
            .find(|c| c.profile.name == "docker 5433")
            .unwrap();
        // The URL wins over the (stale) host and port fields.
        assert_eq!(
            (a.profile.host.as_str(), a.profile.port),
            ("db.example", 5433)
        );
        assert_eq!(a.profile.database, "shop");
        assert_eq!(a.profile.ssl, SslMode::Require);
        assert_eq!(
            (a.profile.user.as_str(), a.password.as_deref()),
            ("alice", Some("s3cret"))
        );
        assert_eq!(
            (a.profile.group.as_str(), a.profile.color),
            ("Local", Some(ProfileColor::Red))
        );
        let b = f
            .candidates
            .iter()
            .find(|c| c.profile.name == "manual")
            .unwrap();
        assert_eq!((b.profile.host.as_str(), b.profile.port), ("h2", 6543));
        assert_eq!(b.profile.database, "postgres");
        assert_eq!(b.profile.user, "bob");
        assert_eq!(b.password, None);
        assert_eq!(b.note.as_deref(), Some("no saved password could be read"));
    }

    #[test]
    fn works_without_credentials_and_with_bad_input() {
        let f = parse_data_sources(SOURCES, None).unwrap();
        assert_eq!(f.candidates.len(), 2);
        assert!(
            parse_data_sources("{}", None)
                .unwrap()
                .candidates
                .is_empty()
        );
        assert!(parse_data_sources("nope", None).is_err());
    }

    #[test]
    fn decrypts_credentials() {
        let json = br##"{"a":{"#connection":{"user":"u","password":"p"}}}"##;
        let v = decrypt_credentials(&encrypt(json)).unwrap();
        assert_eq!(v["a"]["#connection"]["password"], "p");
        assert!(decrypt_credentials(b"short").is_err());
        assert!(decrypt_credentials(&[1u8; 48]).is_err());
        assert!(decrypt_credentials(&encrypt(b"not json")).is_err());
    }

    #[test]
    fn jdbc_urls() {
        assert!(parse_jdbc("jdbc:mysql://h/db").is_none());
        assert!(parse_jdbc("postgres://h/db").is_none());
        let p = parse_jdbc("jdbc:postgresql://h/").unwrap();
        assert_eq!((p.port, p.database.as_str()), (5432, "postgres"));
        assert_eq!(
            parse_jdbc("jdbc:postgresql://h/d?ssl=true&user=zed").map(|p| (p.ssl, p.user)),
            Some((SslMode::Require, "zed".to_string()))
        );
        assert_eq!(color_for("test"), Some(ProfileColor::Yellow));
        assert_eq!(color_for("dev"), None);
    }

    #[test]
    fn skips_existing_and_renames_clashes() {
        let mk = |name: &str, host: &str| Candidate {
            profile: ConnectionProfile {
                name: name.into(),
                host: host.into(),
                ..Default::default()
            },
            password: None,
            note: None,
        };
        let existing = vec![ConnectionProfile {
            name: "taken".into(),
            host: "known".into(),
            ..Default::default()
        }];
        let out = only_new(
            vec![
                mk("a", "known"),
                mk("taken", "x"),
                mk("taken", "y"),
                mk("dup", "x"),
            ],
            &existing,
        );
        let names: Vec<&str> = out.iter().map(|c| c.profile.name.as_str()).collect();
        assert_eq!(names, ["taken (2)", "taken (3)"]);
    }

    #[test]
    fn resolves_chosen_paths() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("workspace6/General/.dbeaver");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("data-sources.json"), SOURCES).unwrap();
        let other = root.path().join("Other/.dbeaver");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("data-sources-2.json"), SOURCES).unwrap();
        // The .dbeaver folder, a file in it, the project, a workspace and the data root.
        assert_eq!(projects_in(&proj), vec![proj.clone()]);
        assert_eq!(
            projects_in(&proj.join("data-sources.json")),
            vec![proj.clone()]
        );
        assert_eq!(projects_in(proj.parent().unwrap()), vec![proj.clone()]);
        assert_eq!(
            projects_in(&root.path().join("workspace6")),
            vec![proj.clone()]
        );
        assert_eq!(projects_in(root.path()), vec![other.clone(), proj.clone()]);
        assert!(projects_in(&root.path().join("nothing")).is_empty());
        let all = load_projects(&projects_in(root.path()));
        assert_eq!(all.candidates.len(), 4);
        assert_eq!(all.skipped, 4);
    }

    #[test]
    fn loads_a_project_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data-sources.json"), SOURCES).unwrap();
        std::fs::write(
            dir.path().join("credentials-config.json"),
            encrypt(br##"{"a":{"#connection":{"user":"alice","password":"pw"}}}"##),
        )
        .unwrap();
        let f = load_project(dir.path()).unwrap();
        assert_eq!(f.candidates.len(), 2);
        assert!(
            f.candidates
                .iter()
                .any(|c| c.password.as_deref() == Some("pw"))
        );
        assert_eq!(data_source_files(dir.path()).len(), 1);
        // A garbled credentials file only loses the passwords.
        std::fs::write(dir.path().join("credentials-config.json"), [0u8; 40]).unwrap();
        assert!(
            load_project(dir.path())
                .unwrap()
                .candidates
                .iter()
                .all(|c| c.password.is_none())
        );
        assert!(
            load_project(&dir.path().join("missing"))
                .unwrap()
                .candidates
                .is_empty()
        );
    }

    /// Reads this machine's real DBeaver workspace; prints counts only, never passwords.
    #[test]
    fn live_workspace() {
        if std::env::var_os("PGROOVE_TEST_DBEAVER").is_none() {
            return;
        }
        let f = detect();
        let with_pw = f.candidates.iter().filter(|c| c.password.is_some()).count();
        println!(
            "dbeaver: {} projects, {} connections, {} with a password, {} skipped",
            find_projects().len(),
            f.candidates.len(),
            with_pw,
            f.skipped
        );
        assert!(!f.candidates.is_empty());
    }
}
