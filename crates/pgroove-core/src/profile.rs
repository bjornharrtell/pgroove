use serde::{Deserialize, Serialize};

/// How TLS is used for a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SslMode {
    /// Never use TLS.
    Disable,
    /// Use TLS when the server offers it, without verifying the certificate.
    #[default]
    Prefer,
    /// Require TLS, without verifying the certificate.
    Require,
    /// Require TLS and verify the certificate chain and host name.
    VerifyFull,
}

impl SslMode {
    pub const ALL: [SslMode; 4] = [
        SslMode::Disable,
        SslMode::Prefer,
        SslMode::Require,
        SslMode::VerifyFull,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Prefer => "prefer",
            SslMode::Require => "require",
            SslMode::VerifyFull => "verify-full",
        }
    }

    /// The next mode in `ALL`, wrapping around.
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// Parses a libpq `sslmode` value.
    pub fn from_libpq(v: &str) -> Option<Self> {
        Some(match v {
            "disable" => SslMode::Disable,
            "allow" | "prefer" => SslMode::Prefer,
            "require" => SslMode::Require,
            "verify-ca" | "verify-full" => SslMode::VerifyFull,
            _ => return None,
        })
    }
}

/// A saved connection profile. The password is never stored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    #[serde(default)]
    pub ssl: SslMode,
}

impl Default for ConnectionProfile {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: "localhost".into(),
            port: 5432,
            database: "postgres".into(),
            user: "postgres".into(),
            ssl: SslMode::default(),
        }
    }
}

impl ConnectionProfile {
    /// Short label shown in the UI, e.g. `user@host:5432/db`.
    pub fn label(&self) -> String {
        format!(
            "{}@{}:{}/{}",
            self.user, self.host, self.port, self.database
        )
    }

    /// Parses a `postgres://` or `postgresql://` URI; returns the profile and the password, if any.
    ///
    /// The profile name defaults to `host/database`.
    pub fn from_uri(uri: &str) -> Result<(Self, Option<String>), &'static str> {
        let url = url::Url::parse(uri.trim()).map_err(|_| "That isn't a valid connection URI")?;
        if !matches!(url.scheme(), "postgres" | "postgresql") {
            return Err("The URI must start with postgres:// or postgresql://");
        }
        let decode = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        };
        let mut p = ConnectionProfile {
            host: url
                .host_str()
                .map(decode)
                .unwrap_or_else(|| "localhost".into()),
            port: url.port().unwrap_or(5432),
            ..Default::default()
        };
        if !url.username().is_empty() {
            p.user = decode(url.username());
        }
        let db = decode(url.path().trim_start_matches('/'));
        if !db.is_empty() {
            p.database = db;
        }
        for (k, v) in url.query_pairs() {
            if k == "sslmode" {
                p.ssl = SslMode::from_libpq(&v).ok_or("Unknown sslmode in the URI")?;
            }
        }
        p.name = format!("{}/{}", p.host, p.database);
        Ok((p, url.password().map(decode)))
    }

    /// Returns a user-facing message for the first invalid field.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.name.trim().is_empty() {
            return Err("Enter a name");
        }
        if self.host.trim().is_empty() {
            return Err("Enter a host");
        }
        if self.port == 0 {
            return Err("Enter a port between 1 and 65535");
        }
        if self.database.trim().is_empty() {
            return Err("Enter a database");
        }
        if self.user.trim().is_empty() {
            return Err("Enter a user");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> ConnectionProfile {
        ConnectionProfile {
            name: "local".into(),
            ..Default::default()
        }
    }

    #[test]
    fn label_formats() {
        assert_eq!(valid().label(), "postgres@localhost:5432/postgres");
    }

    #[test]
    fn ssl_modes() {
        assert_eq!(SslMode::Disable.next(), SslMode::Prefer);
        assert_eq!(SslMode::VerifyFull.next(), SslMode::Disable);
        for m in SslMode::ALL {
            assert_eq!(SslMode::from_libpq(m.label()), Some(m));
        }
        assert_eq!(SslMode::from_libpq("allow"), Some(SslMode::Prefer));
        assert_eq!(SslMode::from_libpq("verify-ca"), Some(SslMode::VerifyFull));
        assert_eq!(SslMode::from_libpq("bogus"), None);
    }

    #[test]
    fn uri_parsing() {
        let (p, pw) = ConnectionProfile::from_uri(
            "postgresql://me:p%40ss@db.example:6543/shop?sslmode=require",
        )
        .unwrap();
        assert_eq!(
            (
                p.host.as_str(),
                p.port,
                p.user.as_str(),
                p.database.as_str()
            ),
            ("db.example", 6543, "me", "shop")
        );
        assert_eq!(p.ssl, SslMode::Require);
        assert_eq!(p.name, "db.example/shop");
        assert_eq!(pw.as_deref(), Some("p@ss"));

        let (p, pw) = ConnectionProfile::from_uri("postgres://localhost").unwrap();
        assert_eq!(
            (p.port, p.user.as_str(), p.database.as_str()),
            (5432, "postgres", "postgres")
        );
        assert_eq!(pw, None);
        assert!(ConnectionProfile::from_uri("mysql://x").is_err());
        assert!(ConnectionProfile::from_uri("not a uri").is_err());
        assert!(ConnectionProfile::from_uri("postgres://h/d?sslmode=nope").is_err());
    }

    #[test]
    fn validate_ok() {
        assert!(valid().validate().is_ok());
    }

    #[test]
    fn validate_reports_each_field() {
        type Case = (fn(&mut ConnectionProfile), &'static str);
        let cases: [Case; 5] = [
            (|p| p.name = " ".into(), "Enter a name"),
            (|p| p.host.clear(), "Enter a host"),
            (|p| p.port = 0, "Enter a port between 1 and 65535"),
            (|p| p.database.clear(), "Enter a database"),
            (|p| p.user.clear(), "Enter a user"),
        ];
        for (mutate, msg) in cases {
            let mut p = valid();
            mutate(&mut p);
            assert_eq!(p.validate(), Err(msg));
        }
    }
}
