//! Error types for the wire layer.
//!
//! These are the categories defined in [`wire-protocol.md`'s error model],
//! plus transport-level errors that can occur before a Ferridis-shaped
//! response is even reachable.
//!
//! [`wire-protocol.md`'s error model]: ../../../../wire-protocol.md

use ferridis_core::FerridisError;
use thiserror::Error;
use url::Url;

/// Errors raised by [`crate`] at the transport / OAuth / call layers.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// The underlying HTTP transport failed (DNS, TCP, TLS, timeout).
    #[error("transport error fetching {url}: {source}")]
    Transport {
        /// The URL the request was aimed at.
        url: Url,
        /// The underlying `reqwest` error.
        #[source]
        source: reqwest::Error,
    },

    /// The server returned a non-success status code for a Ferridis fetch.
    #[error("HTTP {status} from {url}: {body}")]
    BadStatus {
        /// The URL that returned the bad status.
        url: Url,
        /// The HTTP status code.
        status: u16,
        /// Body excerpt (truncated).
        body: String,
    },

    /// A manifest fetched from the wire failed validation.
    #[error("manifest from {url} is invalid: {source}")]
    InvalidManifest {
        /// The manifest URL.
        url: Url,
        /// The underlying validation error from `ferridis-core`.
        #[source]
        source: FerridisError,
    },

    /// OAuth-layer error (consent denied, code expired, bad redirect, etc.).
    #[error("OAuth error: {0}")]
    OAuth(String),

    /// The CSRF `state` round-trip failed: the broker has no record of it.
    #[error("OAuth state token does not match any pending connection")]
    UnknownState,

    /// Connection token has expired and must be refreshed or re-authorized.
    #[error("connection token has expired")]
    ConnectionExpired,

    /// Connection was revoked by the user or the service.
    #[error("connection has been revoked")]
    ConnectionRevoked,

    /// The capability could not be found at the given URL.
    #[error("capability not found at {0}")]
    CapabilityNotFound(Url),

    /// The intent the runtime tried to invoke is not supported by the capability.
    #[error("intent `{0}` not supported by this capability")]
    IntentNotSupported(String),

    /// The requested tier is not in the manifest's supported tier set.
    #[error("requested tier not supported by capability")]
    TierUnavailable,

    /// The user has not granted consent for this operation.
    #[error("consent required")]
    ConsentRequired,

    /// The service is rate-limiting this connection.
    #[error("rate limited (retry after {retry_after_secs} seconds)")]
    RateLimited {
        /// Seconds to wait before retrying.
        retry_after_secs: u64,
    },

    /// JSON serialization or deserialization failed at the protocol layer.
    #[error("JSON error: {0}")]
    Json(String),

    /// The URL was malformed.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// A WebSocket connection failed (handshake, frame parse, send,
    /// receive). Carries a human-readable detail; the underlying
    /// `tungstenite` error is not exposed because it is not the
    /// stable surface this crate promises.
    #[error("WebSocket error at {url}: {detail}")]
    WebSocket {
        /// The WebSocket URL.
        url: Url,
        /// Human-readable detail.
        detail: String,
    },
}

impl From<serde_json::Error> for ProtocolError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e.to_string())
    }
}
