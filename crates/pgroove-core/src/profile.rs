use serde::{Deserialize, Serialize};

/// A saved connection profile. The password is never stored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
}

impl Default for ConnectionProfile {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: "localhost".into(),
            port: 5432,
            database: "postgres".into(),
            user: "postgres".into(),
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
