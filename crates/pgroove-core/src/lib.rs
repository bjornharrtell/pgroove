//! UI-independent logic: connections, catalog, query execution.

/// A saved connection profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionProfile {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
}

impl ConnectionProfile {
    /// Short label shown in the UI, e.g. `user@host:5432/db`.
    pub fn label(&self) -> String {
        format!(
            "{}@{}:{}/{}",
            self.user, self.host, self.port, self.database
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_formats() {
        let p = ConnectionProfile {
            name: "local".into(),
            host: "localhost".into(),
            port: 5432,
            database: "shop".into(),
            user: "postgres".into(),
        };
        assert_eq!(p.label(), "postgres@localhost:5432/shop");
    }
}
