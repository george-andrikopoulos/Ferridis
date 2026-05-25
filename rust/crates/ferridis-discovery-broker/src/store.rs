//! In-memory TTL service registry with broadcast for SSE push and optional
//! JSON state-file persistence so registrations survive process restarts.

use ferridis_protocol::discovery::{DiscoveredService, ServiceKind};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, Mutex};

const DEFAULT_TTL: Duration = Duration::from_secs(300);

// ── Wire / state-file types ───────────────────────────────────────────────────

/// JSON body for `POST /discovery/register` and the state-file entry format.
///
/// Fields are private; construction goes through [`RegisterRequest::new`] so
/// all state-file round-trips pass through a single validated path.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct RegisterRequest {
    name: String,
    kind: String,
    url: String,
    /// `true` → pinned; TTL is ignored and the entry survives process restarts.
    /// Omitting the field is equivalent to `false` (backwards-compatible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    persistent: Option<bool>,
}

impl RegisterRequest {
    /// Construct a register request (used by tests and the axum JSON extractor).
    pub fn new(name: impl Into<String>, kind: impl Into<String>, url: impl Into<String>) -> Self {
        Self { name: name.into(), kind: kind.into(), url: url.into(), persistent: None }
    }

    /// Mark this request as persistent (pinned — survives TTL and restarts).
    pub fn persistent(mut self, p: bool) -> Self {
        self.persistent = Some(p);
        self
    }
}

// ── Internal store types ──────────────────────────────────────────────────────

/// Models the two mutually-exclusive lifetime strategies for a registered service.
///
/// Using an enum (Pattern 5) rather than `pinned: bool` makes the two cases
/// explicit in the type system — it is impossible to create an entry that is
/// neither ephemeral nor pinned, and callers can match exhaustively.
#[derive(Debug, Clone, Copy)]
enum Lifetime {
    Ephemeral { expires_at: Instant },
    Pinned,
}

impl Lifetime {
    fn is_live(&self, now: Instant) -> bool {
        match self {
            Lifetime::Ephemeral { expires_at } => *expires_at > now,
            Lifetime::Pinned => true,
        }
    }

    fn is_pinned(&self) -> bool {
        matches!(self, Lifetime::Pinned)
    }
}

#[derive(Debug)]
struct Entry {
    service: DiscoveredService,
    lifetime: Lifetime,
}

// ── Public return type ────────────────────────────────────────────────────────

/// One item returned by [`ServiceStore::list`].
///
/// Private fields with accessor methods follow Pattern 6 (smart constructors).
pub struct ServiceEntry {
    service: DiscoveredService,
    persistent: bool,
}

impl ServiceEntry {
    fn new(service: DiscoveredService, persistent: bool) -> Self {
        Self { service, persistent }
    }

    pub fn service(&self) -> &DiscoveredService {
        &self.service
    }

    pub fn persistent(&self) -> bool {
        self.persistent
    }
}

// ── Store ─────────────────────────────────────────────────────────────────────

/// Thread-safe in-memory store with TTL eviction, broadcast push, and optional
/// atomic JSON state-file persistence.
#[derive(Clone)]
pub struct ServiceStore {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
    tx: broadcast::Sender<DiscoveredService>,
    /// Path to the JSON state file, or `None` for ephemeral (no persistence).
    state_path: Option<Arc<PathBuf>>,
    ttl: Duration,
}

impl ServiceStore {
    /// Create a new ephemeral store (no state file, default TTL).
    pub fn new() -> Self {
        Self::new_with_ttl(DEFAULT_TTL)
    }

    /// Create an ephemeral store with a custom TTL (useful in tests to simulate expiry instantly).
    pub fn new_with_ttl(ttl: Duration) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { inner: Arc::new(Mutex::new(HashMap::new())), tx, state_path: None, ttl }
    }

    /// Create a store backed by a JSON state file at `path` (default TTL).
    ///
    /// The file is written atomically (tmp → rename) after every mutation.
    /// Call [`load_state`] at startup to restore persisted registrations.
    pub fn with_state(path: PathBuf) -> Self {
        Self::with_state_and_ttl(path, DEFAULT_TTL)
    }

    /// Create a store backed by a state file with a custom TTL (useful in tests).
    pub fn with_state_and_ttl(path: PathBuf, ttl: Duration) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            tx,
            state_path: Some(Arc::new(path)),
            ttl,
        }
    }

    /// Restore registrations from the state file.
    ///
    /// All loaded entries use `Lifetime::Pinned` — they bypass TTL until
    /// explicitly deleted, matching the semantics of the state file (only
    /// pinned entries are ever written there). Entries with unrecognised kinds
    /// or unparseable URLs are silently skipped so old state files survive
    /// format evolution.
    ///
    /// Returns the number of entries loaded. Returns `Ok(0)` if the file does
    /// not exist yet or when the store has no state path.
    pub async fn load_state(&self) -> Result<usize, BrokerError> {
        let Some(path) = self.state_path.as_ref() else {
            return Ok(0);
        };
        let content = match tokio::fs::read_to_string(path.as_ref()).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(BrokerError::Io(e)),
        };
        let requests: Vec<RegisterRequest> =
            serde_json::from_str(&content).map_err(BrokerError::Json)?;
        let count = requests.len();
        let mut map = self.inner.lock().await;
        for req in requests {
            let kind = match parse_kind(&req.kind) {
                Ok(k) => k,
                Err(_) => continue,
            };
            let url = match url::Url::parse(&req.url) {
                Ok(u) => u,
                Err(_) => continue,
            };
            let svc = DiscoveredService::new(req.name.clone(), kind, url); // clone: name moved into map key
            map.insert(req.name, Entry { service: svc, lifetime: Lifetime::Pinned });
        }
        Ok(count)
    }

    /// Register or refresh a service. Broadcasts to all active SSE subscribers
    /// and atomically persists to the state file (if configured).
    pub async fn register(&self, req: RegisterRequest) -> Result<(), BrokerError> {
        let kind = parse_kind(&req.kind)?;
        let url =
            url::Url::parse(&req.url).map_err(|e| BrokerError::InvalidUrl(e.to_string()))?;
        let lifetime = match req.persistent.unwrap_or(false) {
            true => Lifetime::Pinned,
            false => Lifetime::Ephemeral { expires_at: Instant::now() + self.ttl },
        };
        let svc = DiscoveredService::new(req.name.clone(), kind, url); // clone: name moved into map key below
        let entry = Entry {
            service: svc.clone(), // clone: stored in map and sent to broadcast channel separately
            lifetime,
        };
        self.inner.lock().await.insert(req.name, entry);
        let _ = self.tx.send(svc);
        self.persist().await?;
        Ok(())
    }

    /// Remove a service by name. Returns `true` if it existed, `false` if not found.
    ///
    /// Atomically persists after removal so the deletion survives restarts.
    pub async fn remove(&self, name: &str) -> Result<bool, BrokerError> {
        let existed = self.inner.lock().await.remove(name).is_some();
        if existed {
            self.persist().await?;
        }
        Ok(existed)
    }

    /// All live services. Pinned entries are always included; ephemeral entries
    /// are included only while their TTL has not elapsed.
    pub async fn list(&self) -> Vec<ServiceEntry> {
        let now = Instant::now();
        self.inner
            .lock()
            .await
            .values()
            .filter(|e| e.lifetime.is_live(now))
            .map(|e| ServiceEntry::new(
                e.service.clone(), // clone: returning owned Vec from behind mutex guard
                e.lifetime.is_pinned(),
            ))
            .collect()
    }

    /// Subscribe to new registration events.
    pub fn subscribe(&self) -> broadcast::Receiver<DiscoveredService> {
        self.tx.subscribe()
    }

    /// Atomically write all pinned entries to the state file.
    ///
    /// Only pinned entries are written — ephemeral registrations expire
    /// naturally and must not be revived on restart. Writes to `<path>.tmp`
    /// then renames — crash-safe. No-ops when `state_path` is `None`.
    async fn persist(&self) -> Result<(), BrokerError> {
        let Some(path) = self.state_path.as_ref() else {
            return Ok(());
        };
        let requests: Vec<RegisterRequest> = {
            let map = self.inner.lock().await;
            map.values()
                .filter(|e| e.lifetime.is_pinned())
                .map(|e| RegisterRequest {
                    name: e.service.name().to_string(),
                    kind: kind_to_str(e.service.kind()).to_string(),
                    url: e.service.url().to_string(),
                    persistent: Some(true),
                })
                .collect()
        };
        let json = serde_json::to_vec_pretty(&requests).map_err(BrokerError::Json)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(BrokerError::Io)?;
        }
        let tmp_path = PathBuf::from(format!("{}.tmp", path.display()));
        tokio::fs::write(&tmp_path, &json).await.map_err(BrokerError::Io)?;
        tokio::fs::rename(&tmp_path, path.as_ref()).await.map_err(BrokerError::Io)?;
        Ok(())
    }
}

impl Default for ServiceStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_kind(s: &str) -> Result<ServiceKind, BrokerError> {
    match s {
        "mcp" => Ok(ServiceKind::Mcp),
        "ferridis" => Ok(ServiceKind::Ferridis),
        other => Err(BrokerError::UnknownKind(other.to_string())),
    }
}

fn kind_to_str(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Mcp => "mcp",
        ServiceKind::Ferridis => "ferridis",
    }
}

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors from the broker store.
#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    /// Service kind string was not `mcp` or `ferridis`.
    #[error("unknown service kind `{0}` — expected `mcp` or `ferridis`")]
    UnknownKind(String),
    /// The URL in the register request could not be parsed.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    /// State file I/O failure.
    #[error("state file I/O: {0}")]
    Io(std::io::Error),
    /// State file JSON serialisation/deserialisation failure.
    #[error("state file JSON: {0}")]
    Json(serde_json::Error),
}
