//! Broker self-registration for Ferridis adapters.
//!
//! After an adapter binds its HTTP port it can announce itself to a running
//! `ferridis-discovery-broker` so that any Ferridis client watching the
//! broker learns about it immediately.
//!
//! # Typical usage
//!
//! ```rust,no_run
//! # use ferridis_adapter_sdk::broker::{BrokerConfig, BrokerUrl, ServiceName,
//! #     ServiceKind, AdapterUrl, RegistrationPersistence, BrokerRegistration};
//! # use std::net::SocketAddr;
//! # #[tokio::main] async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let bind: SocketAddr = "127.0.0.1:7821".parse()?;
//! let cfg = BrokerConfig::new(
//!     BrokerUrl::parse("http://127.0.0.1:7825")?,
//!     ServiceName::parse("ferridis-fs")?,
//!     ServiceKind::Ferridis,
//!     AdapterUrl::from_socket(bind),
//!     RegistrationPersistence::Ephemeral,
//! );
//! let _reg = BrokerRegistration::connect(cfg).await?;
//! # Ok(())
//! # }
//! ```

use std::net::SocketAddr;
use std::time::Duration;

use reqwest::Client;
use serde::Serialize;
use thiserror::Error;
use url::Url;

// ── Domain types ──────────────────────────────────────────────────────────────

/// Validated URL of a running Ferridis discovery broker.
///
/// Scheme must be `http` or `https`.
#[derive(Debug, Clone)]
pub struct BrokerUrl(Url);

impl BrokerUrl {
    /// Parse and validate a broker URL.
    pub fn parse(s: &str) -> Result<Self, BrokerUrlError> {
        let url = Url::parse(s).map_err(|e| BrokerUrlError::Malformed(s.to_owned(), e))?;
        match url.scheme() {
            "http" | "https" => Ok(Self(url)),
            other => Err(BrokerUrlError::InvalidScheme(other.to_owned())),
        }
    }
}

/// Validated service name for broker registration.
///
/// Non-empty; only `[a-z0-9-]` characters allowed.
#[derive(Debug, Clone)]
pub struct ServiceName(String);

impl ServiceName {
    /// Parse and validate a service name.
    pub fn parse(s: impl Into<String>) -> Result<Self, ServiceNameError> {
        let s = s.into();
        if s.is_empty() {
            return Err(ServiceNameError::Empty);
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(ServiceNameError::InvalidChars(s));
        }
        Ok(Self(s))
    }

    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Validated URL at which this adapter is reachable by broker clients.
///
/// Scheme must be `http` or `https`.
#[derive(Debug, Clone)]
pub struct AdapterUrl(Url);

impl AdapterUrl {
    /// Parse and validate an adapter URL.
    pub fn parse(s: &str) -> Result<Self, AdapterUrlError> {
        let url = Url::parse(s).map_err(|e| AdapterUrlError::Malformed(s.to_owned(), e))?;
        match url.scheme() {
            "http" | "https" => Ok(Self(url)),
            other => Err(AdapterUrlError::InvalidScheme(other.to_owned())),
        }
    }

    /// Construct from a bound [`SocketAddr`], yielding `http://{addr}/`.
    ///
    /// Infallible: a valid `SocketAddr` always produces a valid `http` URL.
    pub fn from_socket(addr: SocketAddr) -> Self {
        // allow:expect — SocketAddr always formats as a valid URL authority.
        let url =
            Url::parse(&format!("http://{addr}/")).expect("SocketAddr is a valid URL authority");
        Self(url)
    }
}

/// Whether a registration survives a broker restart.
///
/// Exhaustive enum — a `bool` loses the intent at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationPersistence {
    /// Expires after the broker's default TTL (~300 s). Refreshed automatically
    /// by the heartbeat task inside [`BrokerRegistration`].
    Ephemeral,
    /// Bypasses TTL; written to the broker's state file and survives restarts.
    Pinned,
}

/// Kind of service being registered with the broker.
///
/// Exhaustive enum — wire serialisation is an implementation detail hidden here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    /// An MCP-protocol service (SSE or stdio-bridged).
    Mcp,
    /// A native Ferridis adapter.
    Ferridis,
}

impl ServiceKind {
    fn as_wire_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Ferridis => "ferridis",
        }
    }
}

/// All parameters needed to register this adapter with a discovery broker.
///
/// Every field is a validated newtype or enum — invariants are enforced by
/// the constituent types, not by `BrokerConfig` itself.
#[derive(Debug, Clone)]
pub struct BrokerConfig {
    broker: BrokerUrl,
    name: ServiceName,
    kind: ServiceKind,
    adapter_url: AdapterUrl,
    persistence: RegistrationPersistence,
    heartbeat_interval: Duration,
}

impl BrokerConfig {
    /// Construct from validated constituent types.
    ///
    /// Infallible: all invariants are already enforced by the field types.
    /// The heartbeat interval defaults to 240 s — comfortably inside the
    /// broker's 300 s TTL.
    pub fn new(
        broker: BrokerUrl,
        name: ServiceName,
        kind: ServiceKind,
        adapter_url: AdapterUrl,
        persistence: RegistrationPersistence,
    ) -> Self {
        Self {
            broker,
            name,
            kind,
            adapter_url,
            persistence,
            heartbeat_interval: HEARTBEAT_INTERVAL,
        }
    }

    /// Builder-style heartbeat-interval override.
    ///
    /// Production callers should keep the default — it is tuned against
    /// the broker's TTL. Exists so tests can exercise the heartbeat loop
    /// without waiting minutes.
    #[must_use]
    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }
}

// ── Wire types (private — encode at the boundary only) ───────────────────────

#[derive(Serialize)]
struct RegisterRequest<'a> {
    name: &'a str,
    kind: &'a str,
    url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    persistent: Option<bool>,
}

// ── Error types ───────────────────────────────────────────────────────────────

/// Errors from [`BrokerUrl::parse`].
#[derive(Debug, Error)]
pub enum BrokerUrlError {
    /// The string is not a valid URL.
    #[error("malformed broker URL `{0}`: {1}")]
    Malformed(String, url::ParseError),
    /// The URL scheme is not `http` or `https`.
    #[error("broker URL scheme must be http or https, got `{0}`")]
    InvalidScheme(String),
}

/// Errors from [`ServiceName::parse`].
#[derive(Debug, Error)]
pub enum ServiceNameError {
    /// The service name is empty.
    #[error("service name must not be empty")]
    Empty,
    /// The service name contains characters outside `[a-z0-9-]`.
    #[error("service name `{0}` contains invalid characters (only a-z, 0-9, '-' allowed)")]
    InvalidChars(String),
}

/// Errors from [`AdapterUrl::parse`].
#[derive(Debug, Error)]
pub enum AdapterUrlError {
    /// The string is not a valid URL.
    #[error("malformed adapter URL `{0}`: {1}")]
    Malformed(String, url::ParseError),
    /// The URL scheme is not `http` or `https`.
    #[error("adapter URL scheme must be http or https, got `{0}`")]
    InvalidScheme(String),
}

/// Errors from [`BrokerRegistration::connect`].
#[derive(Debug, Error)]
pub enum BrokerError {
    /// The broker returned a non-2xx status.
    #[error("broker at {broker} rejected registration of `{name}` with HTTP {status}")]
    Rejected {
        /// URL of the broker that rejected the registration.
        broker: Url,
        /// Service name that was rejected.
        name: String,
        /// HTTP status code returned by the broker.
        status: u16,
    },
    /// A network-level error occurred.
    #[error("network error contacting broker at {broker}: {source}")]
    Network {
        /// URL of the broker that could not be reached.
        broker: Url,
        /// Underlying reqwest error.
        #[source]
        source: reqwest::Error,
    },
}

// ── Registration handle ───────────────────────────────────────────────────────

/// Heartbeat re-registration interval for ephemeral entries.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(240);

/// A live registration with a discovery broker.
///
/// For [`RegistrationPersistence::Ephemeral`] registrations the handle owns a
/// background heartbeat task that re-registers before the broker's TTL expires.
/// Drop to abort the heartbeat.
///
/// For [`RegistrationPersistence::Pinned`] the broker persists the entry; no
/// heartbeat is needed.
#[must_use = "dropping BrokerRegistration aborts the heartbeat — bind to a variable"]
pub struct BrokerRegistration {
    _heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl BrokerRegistration {
    /// Register with the broker and start a heartbeat for ephemeral entries.
    pub async fn connect(cfg: BrokerConfig) -> Result<Self, BrokerError> {
        let client = Client::new();
        register_once(&client, &cfg).await?;
        tracing::info!(
            broker = %cfg.broker.0,
            name = cfg.name.as_str(),
            kind = cfg.kind.as_wire_str(),
            adapter_url = %cfg.adapter_url.0,
            persistence = ?cfg.persistence,
            "registered with discovery broker"
        );
        let heartbeat = if cfg.persistence == RegistrationPersistence::Ephemeral {
            Some(spawn_heartbeat(client, cfg))
        } else {
            None
        };
        Ok(Self {
            _heartbeat: heartbeat,
        })
    }
}

impl Drop for BrokerRegistration {
    fn drop(&mut self) {
        if let Some(h) = &self._heartbeat {
            h.abort();
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

async fn register_once(client: &Client, cfg: &BrokerConfig) -> Result<(), BrokerError> {
    // allow:expect — BrokerUrl is validated; joining a fixed path cannot fail.
    let url = cfg
        .broker
        .0
        .join("discovery/register")
        .expect("validated BrokerUrl");

    let persistent = match cfg.persistence {
        RegistrationPersistence::Ephemeral => None,
        RegistrationPersistence::Pinned => Some(true),
    };

    let body = RegisterRequest {
        name: cfg.name.as_str(),
        kind: cfg.kind.as_wire_str(),
        url: cfg.adapter_url.0.as_str(),
        persistent,
    };

    let resp = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| BrokerError::Network {
            broker: cfg.broker.0.clone(), // clone: Url into owned error variant — Url is not Copy
            source: e,
        })?;

    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    Err(BrokerError::Rejected {
        broker: cfg.broker.0.clone(), // clone: Url into owned error variant — Url is not Copy
        name: cfg.name.as_str().to_owned(),
        status: status.as_u16(),
    })
}

fn spawn_heartbeat(client: Client, cfg: BrokerConfig) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(cfg.heartbeat_interval);
        ticker.tick().await; // skip first — initial registration already done
        loop {
            ticker.tick().await;
            match register_once(&client, &cfg).await {
                Ok(()) => tracing::debug!(name = cfg.name.as_str(), "broker heartbeat ok"),
                Err(e) => tracing::warn!(
                    name = cfg.name.as_str(),
                    error = %e,
                    "broker heartbeat failed — will retry next interval"
                ),
            }
        }
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_url_accepts_http() {
        assert!(BrokerUrl::parse("http://127.0.0.1:7825").is_ok());
    }

    #[test]
    fn broker_url_accepts_https() {
        assert!(BrokerUrl::parse("https://broker.example.com").is_ok());
    }

    #[test]
    fn broker_url_rejects_invalid_scheme() {
        assert!(matches!(
            BrokerUrl::parse("ftp://broker.example.com").unwrap_err(),
            BrokerUrlError::InvalidScheme(_)
        ));
    }

    #[test]
    fn broker_url_rejects_malformed() {
        assert!(matches!(
            BrokerUrl::parse("not a url").unwrap_err(),
            BrokerUrlError::Malformed(_, _)
        ));
    }

    #[test]
    fn service_name_accepts_valid() {
        assert!(ServiceName::parse("ferridis-fs").is_ok());
        assert!(ServiceName::parse("my-service-123").is_ok());
    }

    #[test]
    fn service_name_rejects_empty() {
        assert!(matches!(
            ServiceName::parse("").unwrap_err(),
            ServiceNameError::Empty
        ));
    }

    #[test]
    fn service_name_rejects_uppercase() {
        assert!(matches!(
            ServiceName::parse("MyService").unwrap_err(),
            ServiceNameError::InvalidChars(_)
        ));
    }

    #[test]
    fn service_name_rejects_spaces() {
        assert!(matches!(
            ServiceName::parse("my service").unwrap_err(),
            ServiceNameError::InvalidChars(_)
        ));
    }

    #[test]
    fn adapter_url_from_socket_is_http() {
        let addr: SocketAddr = "127.0.0.1:7821".parse().expect("literal addr is valid"); // allow:expect
        let u = AdapterUrl::from_socket(addr);
        assert_eq!(u.0.scheme(), "http");
        assert_eq!(u.0.host_str(), Some("127.0.0.1"));
        assert_eq!(u.0.port(), Some(7821));
    }

    #[test]
    fn adapter_url_rejects_invalid_scheme() {
        assert!(matches!(
            AdapterUrl::parse("ws://localhost:7821").unwrap_err(),
            AdapterUrlError::InvalidScheme(_)
        ));
    }

    #[test]
    fn service_kind_wire_strings() {
        assert_eq!(ServiceKind::Mcp.as_wire_str(), "mcp");
        assert_eq!(ServiceKind::Ferridis.as_wire_str(), "ferridis");
    }
}
