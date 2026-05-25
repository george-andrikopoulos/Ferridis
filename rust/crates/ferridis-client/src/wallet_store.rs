//! Backing store for the connection wallet.
//!
//! [`WalletStore`] is the seam between [`Wallet`](crate::Wallet) and
//! whatever durable medium actually persists the serialized
//! [`StoredConnection`](ferridis_core::StoredConnection)s. v0.2 ships
//! two implementations:
//!
//! - [`KeychainStore`] — production. Talks to the OS credential
//!   manager (Linux Secret Service via D-Bus / macOS Keychain /
//!   Windows Credential Manager) through the `keyring` crate.
//!   Refuses to construct if no backend is reachable — there is **no
//!   plaintext fallback**. A silent fallback would be a downgrade
//!   vector: an attacker who can disable the keychain service would
//!   force secrets onto disk where they can be read.
//! - [`MemoryStore`] — explicit, opt-in only. For tests and for hosts
//!   that intentionally manage their own persistence. The name and
//!   public construction makes it impossible to enable by accident.
//!
//! Per-entry storage: the wallet writes one record per
//! [`StoredConnection`], keyed by the connection's capability ref.
//! It also writes one well-known index record listing every key it
//! has stored, because `keyring` has no enumerate API.
//!
//! All values are UTF-8 JSON; binary tokens (none today) would need
//! base64 encoding at the value boundary.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::ClientError;

/// The reserved account name used to store the wallet's index of
/// capability refs. Not a valid capability ref itself (starts with
/// underscores), so it cannot collide with a real entry.
pub const INDEX_ACCOUNT: &str = "__ferridis_wallet_index__";

/// Pluggable backing store for [`Wallet`](crate::Wallet).
///
/// Implementations must be safe to share across threads. Errors from
/// any method should be surfaced through [`ClientError`] — the wallet
/// will propagate them to its caller without retry.
pub trait WalletStore: Send + Sync + std::fmt::Debug {
    /// Fetch the value stored under `account`, or `None` if no entry
    /// exists for that key.
    fn get(&self, account: &str) -> Result<Option<String>, ClientError>;

    /// Insert or replace the value at `account`. Atomic with respect
    /// to other callers of the same store.
    fn set(&self, account: &str, value: &str) -> Result<(), ClientError>;

    /// Remove the entry at `account`. Returns `Ok(())` whether or not
    /// an entry existed.
    fn remove(&self, account: &str) -> Result<(), ClientError>;
}

/// In-memory store. Loses data on drop.
///
/// **Test / embedded use only.** Construct explicitly — the wallet
/// will never reach for this unless the caller passes it in. There is
/// no automatic fallback from a missing keychain to memory.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<HashMap<String, String>>,
}

impl MemoryStore {
    /// Build an empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl WalletStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>, ClientError> {
        Ok(self.inner.lock().expect("memorystore mutex poisoned").get(account).cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<(), ClientError> {
        self.inner
            .lock()
            .expect("memorystore mutex poisoned")
            .insert(account.to_string(), value.to_string());
        Ok(())
    }

    fn remove(&self, account: &str) -> Result<(), ClientError> {
        self.inner
            .lock()
            .expect("memorystore mutex poisoned")
            .remove(account);
        Ok(())
    }
}

/// OS keychain-backed store. Production default.
///
/// Wraps `keyring::Entry` for each (service, account) pair. The
/// `service` is a single constant per Ferridis instance — the
/// `namespace` arg to [`KeychainStore::new`] — so hosts running
/// multiple parallel Ferridis instances (e.g. dev + prod) can
/// keep their wallets separated.
///
/// Construction performs a reachability probe by issuing a no-op
/// `get_password` on the index account. Any
/// [`keyring::Error::PlatformFailure`] / `NoStorageAccess` / etc.
/// surfaces immediately as a [`ClientError::WalletBackendUnavailable`].
/// Subsequent operations are assumed to succeed; transient backend
/// errors are surfaced as `WalletBackendError`.
#[derive(Debug)]
pub struct KeychainStore {
    namespace: String,
}

impl KeychainStore {
    /// Open the keychain-backed store under `namespace` (used as the
    /// `service` field in keyring entries). Returns an error if no
    /// keychain backend is reachable.
    ///
    /// **Refuses to fall back to plaintext under any circumstance.**
    /// A reachable but locked keychain (e.g., Secret Service running
    /// but no session unlocked) is treated as unreachable.
    pub fn open(namespace: impl Into<String>) -> Result<Self, ClientError> {
        let namespace = namespace.into();
        let probe = keyring::Entry::new(&namespace, INDEX_ACCOUNT)
            .map_err(|e| ClientError::WalletBackendUnavailable(e.to_string()))?;
        match probe.get_password() {
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(Self { namespace }),
            Err(e) => Err(ClientError::WalletBackendUnavailable(e.to_string())),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, ClientError> {
        keyring::Entry::new(&self.namespace, account)
            .map_err(|e| ClientError::WalletBackendError(e.to_string()))
    }
}

impl WalletStore for KeychainStore {
    fn get(&self, account: &str) -> Result<Option<String>, ClientError> {
        match self.entry(account)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ClientError::WalletBackendError(e.to_string())),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<(), ClientError> {
        self.entry(account)?
            .set_password(value)
            .map_err(|e| ClientError::WalletBackendError(e.to_string()))
    }

    fn remove(&self, account: &str) -> Result<(), ClientError> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(ClientError::WalletBackendError(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trip() {
        let s = MemoryStore::new();
        assert_eq!(s.get("foo").unwrap(), None);
        s.set("foo", "bar").unwrap();
        assert_eq!(s.get("foo").unwrap(), Some("bar".into()));
        s.set("foo", "baz").unwrap();
        assert_eq!(s.get("foo").unwrap(), Some("baz".into()));
        s.remove("foo").unwrap();
        assert_eq!(s.get("foo").unwrap(), None);
        // Removing a missing key is a no-op, not an error.
        s.remove("never-existed").unwrap();
    }

    #[test]
    fn memory_store_isolates_keys() {
        let s = MemoryStore::new();
        s.set("a", "1").unwrap();
        s.set("b", "2").unwrap();
        assert_eq!(s.get("a").unwrap().as_deref(), Some("1"));
        assert_eq!(s.get("b").unwrap().as_deref(), Some("2"));
        s.remove("a").unwrap();
        assert_eq!(s.get("a").unwrap(), None);
        assert_eq!(s.get("b").unwrap().as_deref(), Some("2"));
    }
}
