use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;

/// Password storage keyed by connection name.
pub trait SecretStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>>;
    fn set(&self, name: &str, password: &str) -> Result<()>;
    fn delete(&self, name: &str) -> Result<()>;
}

/// OS keychain backed store.
pub struct KeyringStore;

impl KeyringStore {
    fn entry(name: &str) -> Result<keyring::Entry> {
        Ok(keyring::Entry::new("pgroove", name)?)
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, name: &str) -> Result<Option<String>> {
        match Self::entry(name)?.get_password() {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, name: &str, password: &str) -> Result<()> {
        Ok(Self::entry(name)?.set_password(password)?)
    }

    fn delete(&self, name: &str) -> Result<()> {
        match Self::entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
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
}
