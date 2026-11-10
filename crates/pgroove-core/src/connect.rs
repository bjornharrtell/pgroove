use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio_postgres::config::SslMode as DriverSsl;
use tokio_postgres::{Client, Config};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::{ConnectionProfile, SslMode};

/// Builds the driver config. The password is passed separately and never logged.
pub fn build_config(profile: &ConnectionProfile, password: Option<&str>) -> Config {
    let mut cfg = Config::new();
    cfg.host(&profile.host)
        .port(profile.port)
        .dbname(&profile.database)
        .user(&profile.user)
        .application_name("pgroove")
        .connect_timeout(Duration::from_secs(10))
        .ssl_mode(match profile.ssl {
            SslMode::Disable => DriverSsl::Disable,
            SslMode::Prefer => DriverSsl::Prefer,
            SslMode::Require | SslMode::VerifyFull => DriverSsl::Require,
        });
    if let Some(pw) = password {
        cfg.password(pw);
    }
    cfg
}

/// Accepts any server certificate (libpq `require` / `prefer` semantics).
#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Builds the TLS connector for a mode.
pub fn tls_connector(mode: SslMode) -> Result<MakeRustlsConnect> {
    let provider = Arc::new(ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?;
    let config = if mode == SslMode::VerifyFull {
        let mut roots = RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        for e in &native.errors {
            log::warn!("loading system certificates: {e}");
        }
        let (added, ignored) = roots.add_parsable_certificates(native.certs);
        log::debug!("loaded {added} system certificates, ignored {ignored}");
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
            .with_no_client_auth()
    };
    Ok(MakeRustlsConnect::new(config))
}

/// Connects and spawns the connection driver task on the current tokio runtime.
/// Returns the client and the server version string.
pub async fn connect(
    profile: &ConnectionProfile,
    password: Option<&str>,
) -> Result<(Client, String)> {
    let (client, conn) = build_config(profile, password)
        .connect(tls_connector(profile.ssl)?)
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

/// Test helper: connects using `PGROOVE_TEST_PG=host:port:user:password:db`, if set.
#[cfg(test)]
pub(crate) async fn test_connection() -> Option<(Client, String)> {
    let spec = std::env::var("PGROOVE_TEST_PG").ok()?;
    let f: Vec<&str> = spec.splitn(5, ':').collect();
    let p = ConnectionProfile {
        name: "live".into(),
        host: f[0].into(),
        port: f[1].parse().unwrap(),
        user: f[2].into(),
        database: f[4].into(),
        ssl: SslMode::Prefer,
        ..Default::default()
    };
    let pw = (!f[3].is_empty()).then_some(f[3]);
    Some(connect(&p, pw).await.unwrap())
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
            ssl: SslMode::VerifyFull,
            ..Default::default()
        };
        let cfg = build_config(&p, Some("pw"));
        assert_eq!(cfg.get_ssl_mode(), DriverSsl::Require);
        assert_eq!(
            build_config(&ConnectionProfile::default(), None).get_ssl_mode(),
            DriverSsl::Prefer
        );
        assert_eq!(
            build_config(
                &ConnectionProfile {
                    ssl: SslMode::Disable,
                    ..Default::default()
                },
                None
            )
            .get_ssl_mode(),
            DriverSsl::Disable
        );
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
        let Some((client, version)) = test_connection().await else {
            return;
        };
        assert!(!version.is_empty());
        let row = client.query_one("select 1::int", &[]).await.unwrap();
        assert_eq!(row.get::<_, i32>(0), 1);

        client
            .batch_execute(
                "create schema if not exists pgroove_t; \
                 create table if not exists pgroove_t.a (id int); \
                 create or replace view pgroove_t.v as select 1 as x",
            )
            .await
            .unwrap();
        assert!(
            crate::catalog::schemas(&client)
                .await
                .unwrap()
                .contains(&"pgroove_t".to_string())
        );
        let objs = crate::catalog::objects(&client, "pgroove_t").await.unwrap();
        let names: Vec<_> = objs.iter().map(|o| (o.name.as_str(), o.kind)).collect();
        assert_eq!(
            names,
            [
                ("a", crate::catalog::ObjectKind::Table),
                ("v", crate::catalog::ObjectKind::View)
            ]
        );
        client
            .batch_execute("drop schema pgroove_t cascade")
            .await
            .unwrap();
    }

    /// Needs a TLS-enabled server: `PGROOVE_TEST_PG_TLS=host:port:user:password:db`.
    #[tokio::test]
    async fn connect_tls_live() {
        let Ok(spec) = std::env::var("PGROOVE_TEST_PG_TLS") else {
            return;
        };
        let f: Vec<&str> = spec.splitn(5, ':').collect();
        let mut p = ConnectionProfile {
            name: "tls".into(),
            host: f[0].into(),
            port: f[1].parse().unwrap(),
            user: f[2].into(),
            database: f[4].into(),
            ssl: SslMode::Require,
            ..Default::default()
        };
        let pw = (!f[3].is_empty()).then_some(f[3]);
        let ssl_in_use = |client: Client| async move {
            let row = client
                .query_one(
                    "select ssl from pg_stat_ssl where pid = pg_backend_pid()",
                    &[],
                )
                .await
                .unwrap();
            row.get::<_, bool>(0)
        };
        let (c, _) = connect(&p, pw).await.unwrap();
        assert!(ssl_in_use(c).await, "require uses TLS");
        p.ssl = SslMode::Prefer;
        let (c, _) = connect(&p, pw).await.unwrap();
        assert!(ssl_in_use(c).await, "prefer uses TLS when offered");
        p.ssl = SslMode::Disable;
        let (c, _) = connect(&p, pw).await.unwrap();
        assert!(!ssl_in_use(c).await, "disable stays plain");
        // A self-signed certificate must fail verification.
        p.ssl = SslMode::VerifyFull;
        assert!(connect(&p, pw).await.is_err());
    }
}
