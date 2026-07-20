//! Connection wallet — the persisted set of [`StoredConnection`]s.
//!
//! # v0.2 storage model
//!
//! The wallet no longer has a single on-disk JSON document. Each
//! [`StoredConnection`] is one entry in a pluggable [`WalletStore`]
//! (see [`wallet_store`](crate::wallet_store)). The store also holds
//! a single index entry — a JSON array of capability ref strings —
//! that records every key the wallet has written. The wallet uses
//! the index to enumerate entries on open and to keep itself in
//! sync.
//!
//! Production builds use [`KeychainStore`](crate::wallet_store::KeychainStore),
//! which talks to the OS credential manager (Linux Secret Service via
//! D-Bus / macOS Keychain / Windows Credential Manager). If no
//! keychain backend is reachable at startup the constructor errors
//! out — **there is no plaintext fallback**, and never will be. A
//! silent downgrade to disk would be a credential-exfil vector any
//! attacker who can disable the keychain service can exploit.
//!
//! Tests and embedded hosts that intentionally manage their own
//! persistence can use [`MemoryStore`](crate::wallet_store::MemoryStore)
//! via [`Wallet::with_store`]. The explicit construction means a
//! production caller cannot reach for an ephemeral wallet by accident.
//!
//! # Migration from v0.1 plaintext wallets
//!
//! v0.1 wallets were a single `wallet.json` file. v0.2 refuses to
//! read these. The recommended migration is to construct a
//! [`KeychainStore`], read the legacy file once with
//! [`Wallet::detect_legacy_plaintext`], import the connections, then
//! delete the file. Hosts that fail to migrate will see
//! [`ClientError::LegacyPlaintextWalletDetected`] at startup.
//!
//! # Concurrency
//!
//! [`Wallet`] is `Send + Sync`. The `Client` still wraps it in a
//! `tokio::sync::Mutex` because the in-memory `connections` Vec is
//! mutated by inserts/removes — even though each [`WalletStore`]
//! mutation is itself atomic, the read-modify-write of the in-memory
//! index needs serialization at the caller level.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferridis_core::{
    Authorized, CapabilityRef, Connection, Expired, Pending, Revoked, StoredConnection,
};

use crate::error::ClientError;
use crate::wallet_store::{INDEX_ACCOUNT, KeychainStore, MemoryStore, WalletStore};

/// The wallet format version this build of `ferridis-client` writes.
/// Stored alongside each [`StoredConnection`] so future schema
/// migrations are explicit. Bumped when the per-entry shape changes.
pub const WALLET_VERSION: u32 = 2;

/// The set of connections this host has accumulated.
pub struct Wallet {
    store: Arc<dyn WalletStore>,
    /// In-memory mirror of what's in the store. Rebuilt at open time
    /// from the index entry; kept in sync on every insert/remove.
    connections: Vec<StoredConnection>,
}

impl std::fmt::Debug for Wallet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wallet")
            .field("store", &self.store)
            .field("connections", &self.connections.len())
            .finish()
    }
}

impl Wallet {
    /// Open a keychain-backed wallet under `namespace`. The
    /// `namespace` becomes the `service` field of every keyring entry,
    /// so hosts running multiple parallel Ferridis instances can keep
    /// their wallets separated.
    ///
    /// Returns [`ClientError::WalletBackendUnavailable`] if no keychain
    /// backend is reachable. **There is no plaintext fallback** — see
    /// the module-level docs for the rationale.
    pub fn open_keychain(namespace: impl Into<String>) -> Result<Self, ClientError> {
        let store = KeychainStore::open(namespace)?;
        Self::with_store(Arc::new(store))
    }

    /// Build a wallet over an in-memory store. **Test / embedded use
    /// only.** Production callers must use [`Self::open_keychain`].
    pub fn ephemeral() -> Self {
        let store: Arc<dyn WalletStore> = Arc::new(MemoryStore::new());
        // Construction over an empty store cannot fail — the index
        // entry is missing, which we treat as "fresh wallet".
        Self::with_store(store).expect("ephemeral wallet has no failure modes")
    }

    /// Build a wallet over an explicit [`WalletStore`]. Used by both
    /// [`open_keychain`](Self::open_keychain) and
    /// [`ephemeral`](Self::ephemeral); embedders with their own store
    /// implementation can call this directly.
    ///
    /// Loads any existing entries from the store via the index
    /// account. A missing index entry is treated as a fresh wallet.
    pub fn with_store(store: Arc<dyn WalletStore>) -> Result<Self, ClientError> {
        let keys = load_index(store.as_ref())?;
        let mut connections = Vec::with_capacity(keys.len());
        for key in keys {
            let Some(value) = store.get(&key)? else {
                // Index referenced a missing entry — possible if the
                // user manually deleted a keychain item. Skip; the
                // next save will heal the index.
                tracing::warn!(account = %key, "wallet index references missing keychain entry; skipping");
                continue;
            };
            let stored: StoredConnection = serde_json::from_str(&value)
                .map_err(|e| ClientError::WalletParse(e.to_string()))?;
            connections.push(stored);
        }
        Ok(Self { store, connections })
    }

    /// Refuse to open if a legacy v0.1 plaintext wallet exists at
    /// `path`. Hosts should call this at startup with their previous
    /// wallet path, before constructing the v0.2 [`Wallet`], so users
    /// get a clear error instead of silent secret loss.
    pub fn detect_legacy_plaintext(path: impl AsRef<Path>) -> Result<(), ClientError> {
        let path = path.as_ref();
        if path.exists() {
            return Err(ClientError::LegacyPlaintextWalletDetected {
                path: path.display().to_string(),
            });
        }
        Ok(())
    }

    /// Number of connections currently in the wallet.
    pub fn len(&self) -> usize {
        self.connections.len()
    }

    /// Whether the wallet is empty.
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }

    /// Insert or replace a connection. Writes to the underlying store
    /// immediately, then refreshes the index. If either store write
    /// fails the wallet's in-memory state is unchanged.
    ///
    /// Connections are keyed by [`StoredConnection::capability`]. If a
    /// connection for the same capability already exists, the existing
    /// one is replaced and returned.
    pub fn insert(
        &mut self,
        stored: StoredConnection,
    ) -> Result<Option<StoredConnection>, ClientError> {
        let key = stored.capability().to_string();
        let value = serde_json::to_string(&stored).map_err(|e| ClientError::Json(e.to_string()))?;
        self.store.set(&key, &value)?;
        let prior_idx = self
            .connections
            .iter()
            .position(|c| c.capability().to_string() == key);
        let prior = if let Some(idx) = prior_idx {
            Some(std::mem::replace(&mut self.connections[idx], stored))
        } else {
            self.connections.push(stored);
            None
        };
        self.save_index()?;
        Ok(prior)
    }

    /// Remove and return the connection for the given capability.
    /// Deletes the corresponding store entry and refreshes the index.
    pub fn remove(
        &mut self,
        capability: &CapabilityRef,
    ) -> Result<Option<StoredConnection>, ClientError> {
        let key = capability.to_string();
        let idx = self
            .connections
            .iter()
            .position(|c| c.capability().to_string() == key);
        let Some(idx) = idx else {
            return Ok(None);
        };
        self.store.remove(&key)?;
        let removed = self.connections.remove(idx);
        self.save_index()?;
        Ok(Some(removed))
    }

    /// Iterate all stored connections.
    pub fn iter(&self) -> impl Iterator<Item = &StoredConnection> + '_ {
        self.connections.iter()
    }

    fn find(&self, capability: &CapabilityRef) -> Option<&StoredConnection> {
        self.connections
            .iter()
            .find(|c| c.capability() == capability)
    }

    /// Project the stored connection for `capability` into a
    /// [`Connection<Authorized>`], if one exists in that state.
    pub fn authorized(&self, capability: &CapabilityRef) -> Option<Connection<Authorized>> {
        self.find(capability)
            .and_then(StoredConnection::as_authorized)
    }

    /// Project the stored connection for `capability` into a
    /// [`Connection<Pending>`], if one exists in that state.
    pub fn pending(&self, capability: &CapabilityRef) -> Option<Connection<Pending>> {
        self.find(capability).and_then(StoredConnection::as_pending)
    }

    /// Project the stored connection for `capability` into a
    /// [`Connection<Expired>`], if one exists in that state.
    pub fn expired(&self, capability: &CapabilityRef) -> Option<Connection<Expired>> {
        self.find(capability).and_then(StoredConnection::as_expired)
    }

    /// Project the stored connection for `capability` into a
    /// [`Connection<Revoked>`], if one exists in that state.
    pub fn revoked(&self, capability: &CapabilityRef) -> Option<Connection<Revoked>> {
        self.find(capability).and_then(StoredConnection::as_revoked)
    }

    /// Returns `None` always; the old file path is no longer
    /// meaningful with a [`WalletStore`]-backed wallet. Retained
    /// briefly during v0.2 to keep external call sites compiling;
    /// will be removed in v0.3.
    #[deprecated = "the v0.2 wallet has no file path; use WalletStore-backed introspection instead"]
    pub fn path(&self) -> Option<&PathBuf> {
        None
    }

    fn save_index(&self) -> Result<(), ClientError> {
        let keys: Vec<String> = self
            .connections
            .iter()
            .map(|c| c.capability().to_string())
            .collect();
        let json = serde_json::to_string(&keys).map_err(|e| ClientError::Json(e.to_string()))?;
        self.store.set(INDEX_ACCOUNT, &json)
    }
}

fn load_index(store: &dyn WalletStore) -> Result<Vec<String>, ClientError> {
    let Some(raw) = store.get(INDEX_ACCOUNT)? else {
        return Ok(Vec::new());
    };
    serde_json::from_str::<Vec<String>>(&raw).map_err(|e| ClientError::WalletParse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferridis_core::{AccessToken, Tier};
    use tempfile::TempDir;
    use time::{Duration, OffsetDateTime};

    fn cap(seg: &str) -> CapabilityRef {
        CapabilityRef::parse(&format!("ferridis://public.ferridis.io/test/{seg}@v1")).unwrap()
    }

    fn authed(seg: &str) -> StoredConnection {
        let pending = Connection::<Pending>::new(cap(seg), Tier::Native, "u", "s");
        let a = pending.complete(
            AccessToken::new(format!("token-{seg}")),
            None,
            OffsetDateTime::now_utc() + Duration::hours(1),
        );
        StoredConnection::from_authorized(&a)
    }

    #[test]
    fn ephemeral_wallet_round_trips_in_memory() {
        let mut w = Wallet::ephemeral();
        assert!(w.is_empty());
        assert!(w.insert(authed("a")).unwrap().is_none());
        assert!(w.insert(authed("b")).unwrap().is_none());
        assert_eq!(w.len(), 2);
        assert!(w.authorized(&cap("a")).is_some());
        assert!(w.authorized(&cap("missing")).is_none());
    }

    #[test]
    fn insert_replaces_existing_connection_for_same_capability() {
        let mut w = Wallet::ephemeral();
        let first = authed("a");
        let first_id = first.id();
        assert!(w.insert(first).unwrap().is_none());

        let second = authed("a");
        let replaced = w
            .insert(second)
            .unwrap()
            .expect("first conn must be returned");
        assert_eq!(replaced.id(), first_id);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn store_round_trip_preserves_connections() {
        // Use a single MemoryStore across two Wallet instances to
        // prove the persistence contract (load_index + per-entry get).
        let store: Arc<dyn WalletStore> = Arc::new(MemoryStore::new());
        {
            let mut w = Wallet::with_store(store.clone()).unwrap();
            w.insert(authed("a")).unwrap();
            w.insert(authed("b")).unwrap();
        }

        let w2 = Wallet::with_store(store).unwrap();
        assert_eq!(w2.len(), 2);
        assert_eq!(
            w2.authorized(&cap("a")).unwrap().access_token().expose(),
            "token-a"
        );
        assert_eq!(
            w2.authorized(&cap("b")).unwrap().access_token().expose(),
            "token-b"
        );
    }

    #[test]
    fn remove_drops_entry_and_updates_index() {
        let store: Arc<dyn WalletStore> = Arc::new(MemoryStore::new());
        {
            let mut w = Wallet::with_store(store.clone()).unwrap();
            w.insert(authed("a")).unwrap();
            w.insert(authed("b")).unwrap();
            let removed = w.remove(&cap("a")).unwrap();
            assert!(removed.is_some());
            assert_eq!(w.len(), 1);
        }
        let w2 = Wallet::with_store(store).unwrap();
        assert_eq!(w2.len(), 1);
        assert!(w2.authorized(&cap("a")).is_none());
        assert!(w2.authorized(&cap("b")).is_some());
    }

    #[test]
    fn detect_legacy_plaintext_errors_when_file_present() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("wallet.json");
        std::fs::write(&path, r#"{"wallet_version":1,"connections":[]}"#).unwrap();
        let err = Wallet::detect_legacy_plaintext(&path).unwrap_err();
        assert!(matches!(
            err,
            ClientError::LegacyPlaintextWalletDetected { .. }
        ));
    }

    #[test]
    fn detect_legacy_plaintext_passes_when_file_absent() {
        let dir = TempDir::new().unwrap();
        Wallet::detect_legacy_plaintext(dir.path().join("nope.json")).unwrap();
    }
}
