use std::time::Duration;

use anyhow::{Context, Result};
use tokio_postgres::{Client, Config, NoTls};

use crate::ConnectionProfile;

/// Builds the driver config. The password is passed separately and never logged.
pub fn build_config(profile: &ConnectionProfile, password: Option<&str>) -> Config {
    let mut cfg = Config::new();
    cfg.host(&profile.host)
        .port(profile.port)
        .dbname(&profile.database)
        .user(&profile.user)
        .application_name("pgroove")
        .connect_timeout(Duration::from_secs(10));
    if let Some(pw) = password {
        cfg.password(pw);
    }
    cfg
}

/// Connects and spawns the connection driver task on the current tokio runtime.
/// Returns the client and the server version string.
pub async fn connect(
    profile: &ConnectionProfile,
    password: Option<&str>,
) -> Result<(Client, String)> {
    let (client, conn) = build_config(profile, password)
        .connect(NoTls)
        .await
        .with_context(|| format!("connecting to {}", profile.label()))?;
    let name = profile.name.clone();
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            log::warn!("connection {name} closed: {e}");
        }
    });
    let row = client.query_one("show server_version", &[]).await?;
    Ok((client, row.get(0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_postgres::config::Host;

    #[test]
    fn config_from_profile() {
        let p = ConnectionProfile {
            name: "n".into(),
            host: "db.example".into(),
            port: 6543,
            database: "shop".into(),
            user: "me".into(),
        };
        let cfg = build_config(&p, Some("pw"));
        assert_eq!(cfg.get_hosts(), [Host::Tcp("db.example".into())]);
        assert_eq!(cfg.get_ports(), [6543]);
        assert_eq!(cfg.get_dbname(), Some("shop"));
        assert_eq!(cfg.get_user(), Some("me"));
        assert_eq!(cfg.get_password(), Some(&b"pw"[..]));
        assert_eq!(build_config(&p, None).get_password(), None);
    }

    #[tokio::test]
    async fn connect_failure_is_error() {
        let p = ConnectionProfile {
            name: "n".into(),
            port: 1,
            host: "127.0.0.1".into(),
            ..Default::default()
        };
        let err = connect(&p, None).await.unwrap_err();
        assert!(err.to_string().contains("connecting to"));
    }

    /// Runs against a live server when `PGROOVE_TEST_PG=host:port:user:password:db` is set.
    #[tokio::test]
    async fn connect_live_server() {
        let Ok(spec) = std::env::var("PGROOVE_TEST_PG") else {
            return;
        };
        let f: Vec<&str> = spec.splitn(5, ':').collect();
        let p = ConnectionProfile {
            name: "live".into(),
            host: f[0].into(),
            port: f[1].parse().unwrap(),
            user: f[2].into(),
            database: f[4].into(),
        };
        let pw = (!f[3].is_empty()).then_some(f[3]);
        let (client, version) = connect(&p, pw).await.unwrap();
        assert!(!version.is_empty());
        let row = client.query_one("select 1::int", &[]).await.unwrap();
        assert_eq!(row.get::<_, i32>(0), 1);
    }
}
