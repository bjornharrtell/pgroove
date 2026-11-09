use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;

/// Password storage keyed by connection name.
pub trait SecretStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>>;
    fn set(&self, name: &str, password: &str) -> Result<()>;
    fn delete(&self, name: &str) -> Result<()>;
}

/// Installs the platform credential store once and reports why it failed, if it did.
fn init_keychain() -> Result<()> {
    static INIT: std::sync::OnceLock<std::result::Result<(), String>> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        #[cfg(target_os = "linux")]
        let store = dbus_secret_service_keyring_store::Store::new();
        #[cfg(target_os = "macos")]
        let store = apple_native_keyring_store::keychain::Store::new();
        #[cfg(target_os = "windows")]
        let store = windows_native_keyring_store::Store::new();
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        let store: std::result::Result<std::sync::Arc<keyring_core::mock::Store>, _> =
            keyring_core::mock::Store::new();
        store
            .map(|s| keyring_core::set_default_store(s))
            .map_err(|e| format!("no credential store is available: {e}"))
    })
    .clone()
    .map_err(anyhow::Error::msg)
}

/// OS keychain backed store.
pub struct KeyringStore;

impl KeyringStore {
    fn entry(name: &str) -> Result<keyring_core::Entry> {
        init_keychain()?;
        Ok(keyring_core::Entry::new("pgroove", name)?)
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, name: &str) -> Result<Option<String>> {
        match Self::entry(name)?.get_password() {
            Ok(p) => Ok(Some(p)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, name: &str, password: &str) -> Result<()> {
        Ok(Self::entry(name)?.set_password(password)?)
    }

    fn delete(&self, name: &str) -> Result<()> {
        match Self::entry(name)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// In-memory store for tests and keychain-less environments.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }

    fn set(&self, name: &str, password: &str) -> Result<()> {
        self.0.lock().unwrap().insert(name.into(), password.into());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_roundtrip() {
        let s = MemoryStore::default();
        assert_eq!(s.get("a").unwrap(), None);
        s.set("a", "pw").unwrap();
        assert_eq!(s.get("a").unwrap().as_deref(), Some("pw"));
        s.delete("a").unwrap();
        assert_eq!(s.get("a").unwrap(), None);
    }

    /// Needs a running keychain; enable with `PGROOVE_TEST_KEYRING=1`.
    #[tokio::test(flavor = "multi_thread")]
    async fn keyring_persists_across_entries() {
        if std::env::var("PGROOVE_TEST_KEYRING").is_err() {
            return;
        }
        let s = KeyringStore;
        let name = "pgroove-test-entry";
        s.set(name, "pw").unwrap();
        assert_eq!(s.get(name).unwrap().as_deref(), Some("pw"));
        s.delete(name).unwrap();
        assert_eq!(s.get(name).unwrap(), None);
    }
}
