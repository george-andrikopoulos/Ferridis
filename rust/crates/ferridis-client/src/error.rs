//! Error type for the consumer-side library.

use ferridis_core::{CapabilityRef, FerridisError, IntentVerb};
use ferridis_protocol::ProtocolError;
use thiserror::Error;

/// Errors raised by [`Client`](crate::Client) and the wallet/registry it owns.
#[derive(Debug, Error)]
pub enum ClientError {
    /// The capability has not been registered with this client.
    /// Call [`Client::register`](crate::Client::register) first.
    #[error("capability `{0}` is not registered with this client")]
    CapabilityNotRegistered(CapabilityRef),

    /// The capability is registered, but its manifest does not declare
    /// the intent the caller asked to dispatch.
    #[error("capability `{capability}` does not declare intent `{intent}`")]
    IntentNotSupported {
        /// The capability that was checked.
        capability: CapabilityRef,
        /// The intent verb the caller tried to invoke.
        intent: IntentVerb,
    },

    /// The capability requires `auth: oauth2` but the wallet has no
    /// authorized connection for it. The caller should run the OAuth
    /// flow and insert the resulting connection.
    #[error("no authorized connection in wallet for capability `{0}`")]
    NoAuthorizedConnection(CapabilityRef),

    /// The capability's manifest declares an auth method this client
    /// does not yet support (e.g., `ApiKey` in v0.1).
    #[error("auth method `{0}` is not supported by ferridis-client in this version")]
    UnsupportedAuthMethod(&'static str),

    /// I/O error reading or writing the wallet on disk.
    #[error("wallet I/O error: {0}")]
    WalletIo(String),

    /// The wallet's backing keychain is unreachable at open time.
    /// v0.2 onwards has no plaintext fallback: a missing backend is
    /// fatal at startup rather than a silent downgrade to disk. The
    /// caller should surface this to the operator with instructions
    /// for unlocking / installing the platform keychain.
    #[error("wallet keychain backend is unavailable: {0}")]
    WalletBackendUnavailable(String),

    /// A keychain operation failed after the backend was reachable
    /// at open time (e.g., locked partway through a session,
    /// transient D-Bus error). Distinct from
    /// [`WalletBackendUnavailable`](Self::WalletBackendUnavailable)
    /// which is reserved for the startup probe.
    #[error("wallet keychain backend error: {0}")]
    WalletBackendError(String),

    /// The dispatch body failed validation against the capability's
    /// declared input schema for this intent. v0.2 of ferridis-client
    /// pre-validates outbound dispatches at the client boundary so
    /// callers see a typed error rather than an opaque transport
    /// failure from the adapter side. `details` is a human-readable
    /// rendering of the JSON-Schema validation errors.
    #[error("invalid arguments for intent `{intent}` on `{capability}`: {details}")]
    InvalidArgs {
        /// The capability the dispatch was targeting.
        capability: CapabilityRef,
        /// The intent the dispatch was invoking.
        intent: IntentVerb,
        /// Human-readable validation errors.
        details: String,
    },

    /// A legacy plaintext wallet file was detected on disk. v0.2 no
    /// longer reads plaintext wallets — the caller must migrate the
    /// file's contents into the OS keychain (one-time, manual) and
    /// then remove the file. The error carries the offending path so
    /// the operator knows what to clean up.
    #[error(
        "legacy plaintext wallet at {path}: v0.2 requires keychain-only storage; migrate and delete the file"
    )]
    LegacyPlaintextWalletDetected {
        /// The on-disk path of the legacy file.
        path: String,
    },

    /// The wallet file on disk could not be parsed.
    #[error("wallet file is malformed: {0}")]
    WalletParse(String),

    /// The wallet file is in a future format version this build does
    /// not understand.
    #[error("wallet file version {found} is newer than supported version {supported}")]
    WalletVersionTooNew {
        /// The wallet version found on disk.
        found: u32,
        /// The latest version this build can read.
        supported: u32,
    },

    /// An upstream `ferridis-core` validation failure surfaced through
    /// the client.
    #[error("core error: {0}")]
    Core(#[from] FerridisError),

    /// A wire-layer error from [`ferridis_protocol`].
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),

    /// JSON serialization or deserialization failed.
    #[error("JSON error: {0}")]
    Json(String),

    /// A URL was malformed when constructing a dispatch URL.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// The caller invoked [`Client::dispatch`] on an intent whose
    /// manifest declares `kind: "stream"`. Stream-kind intents
    /// return an ordered sequence of chunks rather than a single
    /// response; the caller must use
    /// [`Client::dispatch_streaming`](crate::Client::dispatch_streaming).
    #[error("intent `{intent}` on `{capability}` is stream-kind; use dispatch_streaming")]
    IntentRequiresStreaming {
        /// The capability the dispatch was targeting.
        capability: CapabilityRef,
        /// The stream-kind intent.
        intent: IntentVerb,
    },

    /// The caller invoked
    /// [`Client::dispatch_streaming`](crate::Client::dispatch_streaming)
    /// on an intent whose manifest declares (or defaults to)
    /// `kind: "request"`. Single-shot intents go through plain
    /// [`Client::dispatch`](crate::Client::dispatch).
    #[error(
        "intent `{intent}` on `{capability}` is request-kind; use dispatch (not dispatch_streaming)"
    )]
    IntentNotStreaming {
        /// The capability the dispatch was targeting.
        capability: CapabilityRef,
        /// The request-kind intent.
        intent: IntentVerb,
    },

    /// The caller asked to subscribe to an event channel that the
    /// capability's manifest does not declare in its
    /// `event_channels` field. v0.3 enforces channel declarations
    /// where present; manifests that declare no channels at all
    /// fall through to a permissive legacy path.
    #[error(
        "capability `{capability}` does not declare event channel `{channel}` (declared: {declared:?})"
    )]
    EventChannelNotDeclared {
        /// The capability the caller tried to subscribe against.
        capability: CapabilityRef,
        /// The channel name that was not declared.
        channel: String,
        /// The channels the manifest does declare.
        declared: Vec<String>,
    },

    /// The caller requested `subscribe_ws` on a channel declared as SSE
    /// (or vice-versa). Use the transport that matches the manifest's
    /// `transport` field for the channel.
    #[error("channel `{channel}` is declared as {declared:?} but {requested:?} was requested")]
    WrongTransport {
        /// The channel name.
        channel: String,
        /// The transport the manifest declares for this channel.
        declared: ferridis_core::ChannelTransport,
        /// The transport the caller tried to use.
        requested: ferridis_core::ChannelTransport,
    },

    /// The MCP server's SSE session expired — typically because the
    /// upstream restarted or recycled its sessions. The remote returns
    /// `404 Not Found` to POSTs against the cached session URL.
    /// Triggers re-handshake + retry at the [`crate::mcp::McpClient`]
    /// layer; if the retry also fails it propagates to the caller.
    #[error("MCP SSE session expired (re-handshake required): {0}")]
    McpSessionExpired(String),

    /// Two MCP tools projected to the same Ferridis intent verb after
    /// normalization. MCP allows dots, underscores, and uppercase in
    /// tool names; Ferridis intent verbs do not. If two MCP names
    /// collapse to the same verb, dispatch would be ambiguous, so
    /// registration fails fast.
    #[error("MCP tools `{first}` and `{second}` both normalize to intent verb `{verb}`")]
    McpToolNameCollision {
        /// The MCP tool name encountered first.
        first: String,
        /// The MCP tool name that collided with `first`.
        second: String,
        /// The intent verb both names map to.
        verb: IntentVerb,
    },
}

impl From<serde_json::Error> for ClientError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e.to_string())
    }
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        Self::WalletIo(e.to_string())
    }
}
