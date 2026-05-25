//! Shared HTTP client.
//!
//! [`Client`] wraps a [`reqwest::Client`] with Ferridis-specific defaults:
//! a user-agent string, request and connect timeouts, and a tight
//! connection-pool configuration. The protocol-version header is
//! attached per-call by the call site rather than here, because the
//! wire-version negotiation is request-scoped.
//!
//! # Connection-pool configuration (hardening from a v0.1 wedge)
//!
//! During VS Code demo testing we hit a real bug: when an adapter
//! process was killed and reqwest's pool still held a keepalive entry
//! for it, subsequent requests reused the stale connection and the
//! sidecar's `dispatch` calls hung past the 30-second `timeout` we
//! had set. The exact root cause inside reqwest's keepalive layer was
//! not fully diagnosed; the defensive configuration here is the v0.1
//! fix:
//!
//! - [`POOL_IDLE_TIMEOUT`] — drop idle pool entries quickly so a stale
//!   localhost connection can't linger for hours.
//! - [`POOL_MAX_IDLE_PER_HOST`] — small pool, so the blast radius of a
//!   stale entry is one or two requests, not a backlog.
//! - [`CONNECT_TIMEOUT`] — tight connect budget so a half-alive server
//!   that accepts but never responds errors fast.
//! - [`REQUEST_TIMEOUT`] — overall request budget, unchanged.

use std::time::Duration;

/// Total request timeout (connect + send + receive).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Connect-phase timeout (just the TCP/TLS handshake).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Drop idle keepalive entries from the pool after this long. Short so
/// a stale localhost connection cannot wedge a later request.
pub const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum idle keepalive entries per host. Small so a single bad
/// entry can affect at most one or two subsequent requests before
/// being purged.
pub const POOL_MAX_IDLE_PER_HOST: usize = 2;

/// Shared HTTP client used across the protocol crate.
///
/// `Client` is cheap to [`clone`](Clone): `reqwest::Client` is internally
/// reference-counted. The intended pattern is to construct one per
/// runtime and share it.
#[derive(Debug, Clone)]
pub struct Client {
    inner: reqwest::Client,
}

impl Client {
    /// Build a client with Ferridis defaults — including the
    /// connection-pool hardening described at the [crate level](crate::client).
    pub fn new() -> Self {
        let inner = reqwest::Client::builder()
            .user_agent(concat!("ferridis-protocol/", env!("CARGO_PKG_VERSION")))
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .pool_idle_timeout(Some(POOL_IDLE_TIMEOUT))
            .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
            .build()
            .expect("reqwest::Client::build with default config never fails");
        Self { inner }
    }

    /// Build a client from a pre-configured `reqwest::Client`.
    ///
    /// Useful for tests and for environments that need custom TLS, proxy,
    /// or connection-pool tuning that differs from the defaults.
    pub fn from_reqwest(inner: reqwest::Client) -> Self {
        Self { inner }
    }

    /// Borrow the underlying `reqwest::Client`.
    pub fn http(&self) -> &reqwest::Client {
        &self.inner
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}
