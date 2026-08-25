//! Credential storage for provider API keys. Keys are held in the OS keyring, never in the
//! settings file or any other plaintext location. Selection code depends only on the
//! [`CredentialStore`] trait so it is testable without a live keyring.

use std::collections::HashMap;
use std::sync::Mutex;

/// A store that maps a provider id (e.g. `"openai"`) to its API key.
pub trait CredentialStore: Send + Sync {
    /// The stored key for `provider`, or `None` when absent. `Err` signals a store failure
    /// (e.g. the OS keyring is unavailable) rather than "not found".
    fn get(&self, provider: &str) -> anyhow::Result<Option<String>>;
    /// Store (or replace) the key for `provider`.
    fn set(&self, provider: &str, key: &str) -> anyhow::Result<()>;
    /// Remove the key for `provider`; a missing key is a no-op.
    fn delete(&self, provider: &str) -> anyhow::Result<()>;
}

const SERVICE: &str = "light-factory";

/// The OS keyring backend. One entry per provider: service `light-factory`, account the provider
/// id. On Linux this is the Secret Service (GNOME Keyring / KWallet); on macOS the Keychain; on
/// Windows the Credential Manager.
///
/// The `keyring` backend is synchronous and performs D-Bus/Keychain/Credential-Manager I/O in the
/// calling thread. The TUI only calls it at startup and on explicit `/provider`/`/key` commands
/// (a handful of lookups per command), so the bounded blocking is accepted for now rather than
/// moving the store to an async backend.
pub struct KeyringStore;

impl CredentialStore for KeyringStore {
    fn get(&self, provider: &str) -> anyhow::Result<Option<String>> {
        let entry = keyring::Entry::new(SERVICE, provider)?;
        match entry.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, provider: &str, key: &str) -> anyhow::Result<()> {
        let entry = keyring::Entry::new(SERVICE, provider)?;
        entry.set_password(key)?;
        Ok(())
    }

    fn delete(&self, provider: &str) -> anyhow::Result<()> {
        let entry = keyring::Entry::new(SERVICE, provider)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// In-memory store for tests and headless callers that want an ephemeral store.
#[derive(Default)]
#[doc(hidden)]
pub struct MemStore(Mutex<HashMap<String, String>>);

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemStore {
    fn get(&self, provider: &str) -> anyhow::Result<Option<String>> {
        Ok(self.0.lock().expect("poisoned").get(provider).cloned())
    }

    fn set(&self, provider: &str, key: &str) -> anyhow::Result<()> {
        self.0
            .lock()
            .expect("poisoned")
            .insert(provider.to_string(), key.to_string());
        Ok(())
    }

    fn delete(&self, provider: &str) -> anyhow::Result<()> {
        self.0.lock().expect("poisoned").remove(provider);
        Ok(())
    }
}

/// A store whose every operation fails, for the store-failure branch [`MemStore`] cannot reach.
///
/// It models a keyring that is present but unusable — a locked wallet, a dead D-Bus session —
/// which fails for every operation rather than for one, so `set` and `delete` fail too.
///
/// `#[doc(hidden)] pub` for the same reason as [`MemStore`]: `selection.rs`, `modal.rs`, and
/// `app.rs` live in the binary crate and can only reach a double this library exports.
#[doc(hidden)]
pub struct FailingStore {
    message: String,
}

impl FailingStore {
    /// A store that fails with `message`, so a test can assert that the store's own words reach
    /// the user.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Default for FailingStore {
    fn default() -> Self {
        Self::new("credential store unavailable")
    }
}

impl CredentialStore for FailingStore {
    fn get(&self, _provider: &str) -> anyhow::Result<Option<String>> {
        Err(anyhow::anyhow!("{}", self.message))
    }

    fn set(&self, _provider: &str, _key: &str) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("{}", self.message))
    }

    fn delete(&self, _provider: &str) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("{}", self.message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_set_get_delete() {
        let store = MemStore::new();
        assert_eq!(store.get("openai").unwrap(), None);
        store.set("openai", "sk-test").unwrap();
        assert_eq!(store.get("openai").unwrap(), Some("sk-test".to_string()));
        store.delete("openai").unwrap();
        assert_eq!(store.get("openai").unwrap(), None);
    }

    #[test]
    fn delete_missing_key_is_a_noop() {
        let store = MemStore::new();
        store.delete("never-stored").unwrap();
    }

    #[test]
    fn set_replaces_the_existing_key() {
        let store = MemStore::new();
        store.set("gemini", "first").unwrap();
        store.set("gemini", "second").unwrap();
        assert_eq!(store.get("gemini").unwrap(), Some("second".to_string()));
    }

    /// `MemStore` always answers `Ok`, so no test could reach the `Err` branch that
    /// `CredentialStore::get`'s contract makes load-bearing. This double is that branch.
    #[test]
    fn failing_store_fails_every_operation() {
        let store = FailingStore::default();
        assert!(store.get("openai").is_err());
        assert!(store.set("openai", "sk-test").is_err());
        assert!(store.delete("openai").is_err());
    }

    /// The store's own words are what the user ends up reading, so a test must be able to pin
    /// them.
    #[test]
    fn failing_store_reports_the_message_it_was_given() {
        let store = FailingStore::new("no D-Bus session");
        let err = store.get("openai").expect_err("get must fail");
        assert_eq!(format!("{err:#}"), "no D-Bus session");
    }

    #[test]
    fn failing_store_default_names_the_store() {
        let err = FailingStore::default()
            .get("openai")
            .expect_err("get must fail");
        assert_eq!(format!("{err:#}"), "credential store unavailable");
    }
}
