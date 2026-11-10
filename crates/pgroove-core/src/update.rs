//! Self-update from GitHub releases: check, download with checksum verification, install.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
pub use reqwest::Client as HttpClient;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
pub use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Latest-release endpoint of the project.
pub const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/bjornharrtell/pgroove/releases/latest";

/// Never download more than this (the release archives are well under 100 MiB).
const MAX_DOWNLOAD: u64 = 300 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    pub notes: String,
    pub page: String,
    pub assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

/// The version this build was compiled as.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is valid semver")
}

/// Parses a release tag such as `v0.1.2`.
pub fn parse_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.trim().trim_start_matches('v')).ok()
}

impl Release {
    /// Whether this release is newer than `current` (pre-releases are never offered).
    pub fn is_newer_than(&self, current: &Version) -> bool {
        self.version.pre.is_empty() && self.version > *current
    }
}

/// An HTTP client for update checks and downloads (rustls with the `ring` provider).
pub fn http_client() -> HttpClient {
    // Installing twice is harmless: the first provider wins.
    let _ = rustls::crypto::ring::default_provider().install_default();
    HttpClient::builder()
        .user_agent(concat!("pgroove/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_default()
}

/// The outcome of a latest-release check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Latest {
    /// GitHub answered with a release and, when it sent one, its ETag for the next check.
    Release {
        release: Release,
        etag: Option<String>,
    },
    /// GitHub answered 304: nothing changed since the ETag the caller sent.
    NotModified,
}

/// Fetches the latest release from `url` (normally [`LATEST_RELEASE_URL`]).
///
/// Passing the `etag` of an earlier answer makes the request conditional, so an
/// unchanged release costs a 304 and no body.
pub async fn fetch_latest(http: &HttpClient, url: &str, etag: Option<&str>) -> Result<Latest> {
    let mut request = http
        .get(url)
        .header("Accept", "application/vnd.github+json");
    if let Some(etag) = etag {
        request = request.header("If-None-Match", etag);
    }
    let response = request
        .send()
        .await
        .context("asking GitHub for the latest release")?;
    let status = response.status();
    let new_etag = response
        .headers()
        .get("ETag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if status == StatusCode::NOT_MODIFIED {
        return Ok(Latest::NotModified);
    }
    if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
        return Err(rate_limited(status, response.headers()));
    }
    let api: ApiRelease = response
        .error_for_status()
        .context("asking GitHub for the latest release")?
        .json()
        .await?;
    let version = parse_tag(&api.tag_name)
        .ok_or_else(|| anyhow!("release tag {:?} is not a version", api.tag_name))?;
    Ok(Latest::Release {
        release: Release {
            version,
            tag: api.tag_name,
            notes: api.body.unwrap_or_default(),
            page: api.html_url,
            assets: api
                .assets
                .into_iter()
                .map(|a| Asset {
                    name: a.name,
                    url: a.browser_download_url,
                    size: a.size,
                })
                .collect(),
        },
        etag: new_etag,
    })
}

/// The error for a rate-limited answer, naming when the limit resets if GitHub said so.
fn rate_limited(status: StatusCode, headers: &HeaderMap) -> anyhow::Error {
    let reset = headers
        .get("X-RateLimit-Reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .and_then(|reset| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|now| reset.saturating_sub(now.as_secs()))
        });
    let wait = reset.map(|secs| {
        let minutes = secs.div_ceil(60).max(1);
        let plural = if minutes == 1 { "" } else { "s" };
        format!(" (the limit resets in about {minutes} minute{plural})")
    });
    anyhow!(
        "GitHub is rate limiting update checks (HTTP {}); try again later{}",
        status.as_u16(),
        wait.unwrap_or_default()
    )
}

/// How an asset is packaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Package {
    AppImage,
    Tarball,
    Zip,
}

/// The platform this process runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub os: &'static str,
    pub arch: &'static str,
    /// Running from an AppImage (then the AppImage file is what gets replaced).
    pub appimage: bool,
}

pub fn current_target() -> Target {
    Target {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        appimage: std::env::var_os("APPIMAGE").is_some(),
    }
}

/// Picks the release asset for a platform, if one is published.
pub fn pick_asset<'a>(release: &'a Release, target: &Target) -> Option<(&'a Asset, Package)> {
    let (suffix, package) = match (target.os, target.arch) {
        ("linux", "x86_64") if target.appimage => ("-x86_64.AppImage", Package::AppImage),
        ("linux", "x86_64") => ("-linux-x86_64.tar.gz", Package::Tarball),
        ("macos", "aarch64") => ("-macos-aarch64.tar.gz", Package::Tarball),
        ("windows", "x86_64") => ("-windows-x86_64.zip", Package::Zip),
        _ => return None,
    };
    release
        .assets
        .iter()
        .find(|a| a.name.starts_with("pgroove-") && a.name.ends_with(suffix))
        .map(|a| (a, package))
}

/// Lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Checks `bytes` against the first token of a `sha256sum`-style line.
pub fn verify_sha256(bytes: &[u8], sidecar: &str) -> Result<()> {
    let expected = sidecar
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("the checksum file is empty"))?
        .to_ascii_lowercase();
    let actual = sha256_hex(bytes);
    if expected != actual {
        bail!("checksum mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

async fn get_bytes(http: &HttpClient, url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut resp = http.get(url).send().await?.error_for_status()?;
    if resp.content_length().is_some_and(|n| n > limit) {
        bail!("the download is larger than expected");
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        out.extend_from_slice(&chunk);
        if out.len() as u64 > limit {
            bail!("the download is larger than expected");
        }
    }
    Ok(out)
}

/// Downloads an asset and verifies it against the `<name>.sha256` file published next to it.
pub async fn download_verified(
    http: &HttpClient,
    release: &Release,
    asset: &Asset,
) -> Result<Vec<u8>> {
    let sidecar_name = format!("{}.sha256", asset.name);
    let sidecar = release
        .assets
        .iter()
        .find(|a| a.name == sidecar_name)
        .ok_or_else(|| {
            anyhow!(
                "{} has no published checksum, refusing to install",
                asset.name
            )
        })?;
    let sums = get_bytes(http, &sidecar.url, 4096).await?;
    let bytes = get_bytes(http, &asset.url, MAX_DOWNLOAD.max(asset.size)).await?;
    verify_sha256(&bytes, &String::from_utf8_lossy(&sums))
        .with_context(|| format!("verifying {}", asset.name))?;
    Ok(bytes)
}

/// Extracts the application binary from a package.
pub fn extract_binary(bytes: &[u8], package: Package, binary: &str) -> Result<Vec<u8>> {
    match package {
        Package::AppImage => Ok(bytes.to_vec()),
        Package::Tarball => {
            let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
            for entry in archive.entries()? {
                let mut entry = entry?;
                let is_binary = entry
                    .path()?
                    .file_name()
                    .is_some_and(|n| n == std::ffi::OsStr::new(binary));
                if is_binary && entry.header().entry_type().is_file() {
                    let mut out = Vec::new();
                    entry.read_to_end(&mut out)?;
                    return Ok(out);
                }
            }
            bail!("{binary} is not in the archive")
        }
        Package::Zip => {
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
            for i in 0..zip.len() {
                let mut file = zip.by_index(i)?;
                let matches = file
                    .enclosed_name()
                    .and_then(|p| p.file_name().map(|n| n.to_os_string()))
                    .is_some_and(|n| n == std::ffi::OsStr::new(binary));
                if matches && file.is_file() {
                    let mut out = Vec::new();
                    file.read_to_end(&mut out)?;
                    return Ok(out);
                }
            }
            bail!("{binary} is not in the archive")
        }
    }
}

/// Replaces the file at `dest` with `bytes`, atomically, keeping it executable.
pub fn replace_file(dest: &Path, bytes: &[u8]) -> Result<()> {
    let dir = dest
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", dest.display()))?;
    let tmp = dir.join(format!(
        ".{}.update",
        dest.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pgroove")
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow!("replacing {}: {e}", dest.display())
    })
}

/// The `.app` bundle that contains `exe` (`X.app/Contents/MacOS/pgroove`), if any.
pub fn app_bundle_of(exe: &Path) -> Option<PathBuf> {
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension()? == "app"
        && exe.parent()?.file_name()? == "MacOS"
        && cfg!(target_os = "macos"))
    .then(|| app.to_path_buf())
}

/// Installs an extracted binary (or AppImage) over the running program.
pub fn install(bytes: &[u8], package: Package) -> Result<()> {
    match package {
        Package::AppImage => {
            let path = std::env::var_os("APPIMAGE")
                .ok_or_else(|| anyhow!("not running from an AppImage"))?;
            replace_file(Path::new(&path), bytes)
        }
        Package::Tarball | Package::Zip => {
            let exe = std::env::current_exe()?;
            let dir = exe
                .parent()
                .ok_or_else(|| anyhow!("the executable has no directory"))?;
            let tmp = dir.join(".pgroove.new");
            std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
            }
            let result = self_replace::self_replace(&tmp);
            let _ = std::fs::remove_file(&tmp);
            result.context("replacing the running program")?;
            // A swapped binary breaks the bundle's seal; seal it again (ad hoc).
            if let Some(app) = app_bundle_of(&exe) {
                let signed = std::process::Command::new("/usr/bin/codesign")
                    .args(["--force", "--deep", "--sign", "-"])
                    .arg(&app)
                    .status();
                if !signed.is_ok_and(|s| s.success()) {
                    log::warn!("could not re-sign {}", app.display());
                }
            }
            Ok(())
        }
    }
}

/// Starts a new instance of the (updated) program. The caller should then quit.
pub fn restart() -> Result<()> {
    let program = match std::env::var_os("APPIMAGE") {
        Some(p) => p.into(),
        None => std::env::current_exe()?,
    };
    std::process::Command::new(program)
        .args(std::env::args_os().skip(1))
        .spawn()
        .context("starting the updated program")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.into(),
            url: format!("https://example.test/{name}"),
            size: 1,
        }
    }

    fn release(tag: &str) -> Release {
        Release {
            version: parse_tag(tag).unwrap(),
            tag: tag.into(),
            notes: String::new(),
            page: String::new(),
            assets: vec![
                asset("pgroove-v0.2.0-linux-x86_64.tar.gz"),
                asset("pgroove-v0.2.0-x86_64.AppImage"),
                asset("pgroove-v0.2.0-macos-aarch64.tar.gz"),
                asset("pgroove-v0.2.0-windows-x86_64.zip"),
            ],
        }
    }

    fn target(os: &'static str, arch: &'static str, appimage: bool) -> Target {
        Target { os, arch, appimage }
    }

    #[test]
    fn version_logic() {
        assert_eq!(parse_tag("v0.1.2"), Some(Version::new(0, 1, 2)));
        assert_eq!(parse_tag(" 1.0.0 "), Some(Version::new(1, 0, 0)));
        assert_eq!(parse_tag("nightly"), None);
        let cur = Version::new(0, 1, 1);
        assert!(release("v0.1.2").is_newer_than(&cur));
        assert!(!release("v0.1.1").is_newer_than(&cur));
        assert!(!release("v0.1.0").is_newer_than(&cur));
        assert!(
            !release("v0.2.0-rc.1").is_newer_than(&cur),
            "no pre-releases"
        );
        assert!(current_version() >= Version::new(0, 1, 0));
    }

    #[test]
    fn asset_selection() {
        let r = release("v0.2.0");
        let pick = |t: Target| pick_asset(&r, &t).map(|(a, p)| (a.name.clone(), p));
        assert_eq!(
            pick(target("linux", "x86_64", true)),
            Some(("pgroove-v0.2.0-x86_64.AppImage".into(), Package::AppImage))
        );
        assert_eq!(
            pick(target("linux", "x86_64", false)),
            Some((
                "pgroove-v0.2.0-linux-x86_64.tar.gz".into(),
                Package::Tarball
            ))
        );
        assert_eq!(
            pick(target("macos", "aarch64", false)),
            Some((
                "pgroove-v0.2.0-macos-aarch64.tar.gz".into(),
                Package::Tarball
            ))
        );
        assert_eq!(
            pick(target("windows", "x86_64", false)),
            Some(("pgroove-v0.2.0-windows-x86_64.zip".into(), Package::Zip))
        );
        assert_eq!(pick(target("macos", "x86_64", false)), None);
        assert_eq!(pick(target("linux", "aarch64", false)), None);
        let _ = current_target();
    }

    #[test]
    fn finds_the_app_bundle() {
        let exe = Path::new("/Applications/pgroove.app/Contents/MacOS/pgroove");
        assert_eq!(
            app_bundle_of(exe),
            cfg!(target_os = "macos").then(|| PathBuf::from("/Applications/pgroove.app"))
        );
        assert_eq!(app_bundle_of(Path::new("/usr/bin/pgroove")), None);
        assert_eq!(app_bundle_of(Path::new("/x/y.app/bin/pgroove")), None);
    }

    #[test]
    fn checksums() {
        let data = b"hello";
        let hex = sha256_hex(data);
        assert_eq!(
            hex,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        verify_sha256(data, &format!("{hex}  pgroove.tar.gz\n")).unwrap();
        verify_sha256(data, &hex.to_uppercase()).unwrap();
        assert!(verify_sha256(data, &format!("{}  x", "0".repeat(64))).is_err());
        assert!(verify_sha256(data, "").is_err());
    }

    #[test]
    fn extracts_from_tarball_and_zip() {
        // tar.gz
        let mut tar_bytes = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut tar_bytes, flate2::Compression::fast());
            let mut b = tar::Builder::new(enc);
            for (name, body) in [("README.md", &b"doc"[..]), ("pgroove", &b"BINARY"[..])] {
                let mut h = tar::Header::new_gnu();
                h.set_size(body.len() as u64);
                h.set_mode(0o755);
                h.set_cksum();
                b.append_data(&mut h, name, body).unwrap();
            }
            b.into_inner().unwrap().finish().unwrap();
        }
        assert_eq!(
            extract_binary(&tar_bytes, Package::Tarball, "pgroove").unwrap(),
            b"BINARY"
        );
        assert!(extract_binary(&tar_bytes, Package::Tarball, "missing").is_err());

        // zip
        let mut zip_bytes = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_bytes));
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("README.md", opts).unwrap();
            w.write_all(b"doc").unwrap();
            w.start_file("pgroove.exe", opts).unwrap();
            w.write_all(b"EXE").unwrap();
            w.finish().unwrap();
        }
        assert_eq!(
            extract_binary(&zip_bytes, Package::Zip, "pgroove.exe").unwrap(),
            b"EXE"
        );
        assert!(extract_binary(&zip_bytes, Package::Zip, "nope.exe").is_err());
        assert_eq!(
            extract_binary(b"raw", Package::AppImage, "ignored").unwrap(),
            b"raw"
        );
    }

    #[test]
    fn replaces_a_file_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("pgroove.AppImage");
        std::fs::write(&dest, b"old").unwrap();
        replace_file(&dest, b"new").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "stays executable");
        }
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "no temp file left behind");
        assert!(replace_file(Path::new("/"), b"x").is_err());
    }

    /// Writes one HTTP/1.1 response and closes the socket.
    async fn reply(
        mut sock: tokio::net::TcpStream,
        status: &str,
        headers: &[(&str, String)],
        body: Vec<u8>,
    ) {
        use tokio::io::AsyncWriteExt;
        let mut head = format!("HTTP/1.1 {status}\r\n");
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        if !body.is_empty() {
            head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        head.push_str("Connection: close\r\n\r\n");
        let _ = sock.write_all(head.as_bytes()).await;
        let _ = sock.write_all(&body).await;
    }

    /// Serves canned responses by path on a local port.
    async fn serve(routes: Vec<(&'static str, Vec<u8>)>) -> String {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let routes = routes.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let (status, body) = match routes.iter().find(|(p, _)| *p == path) {
                        Some((_, b)) => ("200 OK", b.clone()),
                        None => ("404 Not Found", Vec::new()),
                    };
                    reply(sock, status, &[], body).await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// Serves one canned response to every request.
    async fn serve_canned(
        status: &'static str,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    ) -> String {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let headers = headers.clone();
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    reply(sock, status, &headers, body).await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// Serves `body` with `etag`, or 304 when the request sends `If-None-Match: <etag>`.
    async fn serve_conditional(etag: &'static str, body: Vec<u8>) -> String {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                    let conditional = request.contains(&format!("if-none-match: {etag}"));
                    let (status, body) = if conditional {
                        ("304 Not Modified", Vec::new())
                    } else {
                        ("200 OK", body)
                    };
                    reply(sock, status, &[("ETag", etag.to_string())], body).await;
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn fetches_and_downloads_verified() {
        let payload = b"the program".to_vec();
        let sum = format!("{}  a.AppImage\n", sha256_hex(&payload));
        // The asset URLs are filled in once the port is known.
        let base = serve(vec![
            ("/a.AppImage", payload.clone()),
            ("/a.AppImage.sha256", sum.into_bytes()),
            ("/bad.AppImage", b"tampered".to_vec()),
            (
                "/bad.AppImage.sha256",
                format!("{}  bad", sha256_hex(b"original")).into_bytes(),
            ),
        ])
        .await;
        let latest = serde_json::json!({
            "tag_name": "v9.9.9",
            "body": "notes",
            "html_url": "https://example.test/rel",
            "assets": [
                {"name": "a.AppImage", "browser_download_url": format!("{base}/a.AppImage"), "size": 11},
                {"name": "a.AppImage.sha256", "browser_download_url": format!("{base}/a.AppImage.sha256"), "size": 80},
                {"name": "bad.AppImage", "browser_download_url": format!("{base}/bad.AppImage"), "size": 8},
                {"name": "bad.AppImage.sha256", "browser_download_url": format!("{base}/bad.AppImage.sha256"), "size": 80},
                {"name": "nosum.AppImage", "browser_download_url": format!("{base}/nosum"), "size": 1}
            ]
        });
        let api = serve(vec![("/latest", latest.to_string().into_bytes())]).await;
        let http = http_client();
        let Latest::Release { release: rel, etag } =
            fetch_latest(&http, &format!("{api}/latest"), None)
                .await
                .unwrap()
        else {
            panic!("expected a release");
        };
        assert_eq!(etag, None, "this server sends no ETag");
        assert_eq!(rel.version, Version::new(9, 9, 9));
        assert_eq!(rel.notes, "notes");
        assert!(rel.is_newer_than(&current_version()));
        assert_eq!(rel.assets.len(), 5);

        let a = rel.assets.iter().find(|a| a.name == "a.AppImage").unwrap();
        assert_eq!(download_verified(&http, &rel, a).await.unwrap(), payload);
        let bad = rel
            .assets
            .iter()
            .find(|a| a.name == "bad.AppImage")
            .unwrap();
        let err = download_verified(&http, &rel, bad).await.unwrap_err();
        assert!(format!("{err:#}").contains("checksum mismatch"), "{err:#}");
        let nosum = rel
            .assets
            .iter()
            .find(|a| a.name == "nosum.AppImage")
            .unwrap();
        assert!(
            download_verified(&http, &rel, nosum)
                .await
                .unwrap_err()
                .to_string()
                .contains("no published checksum")
        );
        assert!(
            fetch_latest(&http, &format!("{api}/missing"), None)
                .await
                .is_err()
        );
        let notver = serve(vec![("/x", br#"{"tag_name":"nightly"}"#.to_vec())]).await;
        assert!(
            fetch_latest(&http, &format!("{notver}/x"), None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn rate_limiting_is_explained() {
        let http = http_client();
        let reset = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 600;
        for (status, code) in [
            ("403 Forbidden", StatusCode::FORBIDDEN),
            ("429 Too Many Requests", StatusCode::TOO_MANY_REQUESTS),
        ] {
            let base = serve_canned(
                status,
                vec![("X-RateLimit-Reset", reset.to_string())],
                Vec::new(),
            )
            .await;
            let err = format!("{:#}", fetch_latest(&http, &base, None).await.unwrap_err());
            assert!(err.contains("rate limiting"), "{err}");
            assert!(err.contains(&format!("HTTP {}", code.as_u16())), "{err}");
            assert!(err.contains("try again later"), "{err}");
            assert!(err.contains("resets in about 10 minutes"), "{err}");
        }
        let base = serve_canned("403 Forbidden", Vec::new(), Vec::new()).await;
        let err = format!("{:#}", fetch_latest(&http, &base, None).await.unwrap_err());
        assert_eq!(
            err,
            "GitHub is rate limiting update checks (HTTP 403); try again later"
        );
    }

    #[tokio::test]
    async fn conditional_check_reuses_the_etag() {
        let body = serde_json::json!({
            "tag_name": "v9.9.9",
            "body": "notes",
            "assets": []
        })
        .to_string()
        .into_bytes();
        let base = serve_conditional("\"v9\"", body).await;
        let http = http_client();
        let Latest::Release { release, etag } = fetch_latest(&http, &base, None).await.unwrap()
        else {
            panic!("expected a release");
        };
        assert_eq!(release.version, Version::new(9, 9, 9));
        assert_eq!(release.notes, "notes");
        assert_eq!(etag.as_deref(), Some("\"v9\""));
        assert_eq!(
            fetch_latest(&http, &base, etag.as_deref()).await.unwrap(),
            Latest::NotModified,
            "a 304 is neither a failure nor a new release"
        );
    }
}
