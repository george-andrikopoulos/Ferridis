//! Server-side WebSocket handler trait for Ferridis adapters.
//!
//! Adapters that declare a channel with `transport: "websocket"` in their
//! manifest implement [`WsHandler`] to process incoming frames. The
//! [`crate::AdapterServer`] will call the trait methods on each lifecycle
//! event; implementors are free to ignore any they don't need (all three
//! have default no-op bodies).

use async_trait::async_trait;

/// Opaque identifier for a single WebSocket connection.
///
/// Assigned by the server at upgrade time; stable for the lifetime of the
/// connection. The underlying representation is a monotonically-incrementing
/// `u64` — callers should treat it as an opaque token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WsConnId(u64);

impl WsConnId {
    /// Construct a `WsConnId` from a raw integer.
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// Return the raw numeric value.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for WsConnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ws-conn-{}", self.0)
    }
}

/// A single WebSocket frame received from a client.
#[derive(Debug, Clone)]
pub enum WsMessage {
    /// UTF-8 text frame.
    Text(String),
    /// Binary frame.
    Binary(Vec<u8>),
    /// Close frame — the connection is being torn down.
    Close,
}

/// Server-side WebSocket lifecycle callbacks for an adapter channel.
///
/// All methods have default no-op implementations so adapters only override
/// what they care about. Object-safe: can be stored as `Box<dyn WsHandler>`.
#[async_trait]
pub trait WsHandler: Send + Sync {
    /// Called once when a client completes the WebSocket upgrade handshake.
    async fn on_connect(&self, _id: WsConnId) {}

    /// Called for each frame received from the client.
    async fn on_message(&self, _id: WsConnId, _msg: WsMessage) {}

    /// Called once when the connection is closed (either side initiated).
    async fn on_disconnect(&self, _id: WsConnId) {}
}
