//! Bidirectional event channels over WebSocket.
//!
//! The v0.2 [`events`](crate::events) module ships one-way SSE —
//! server → client only. This module adds the bidirectional half:
//! the client can also send typed [`ClientMessage`]s upstream
//! (subscribe / unsubscribe / ack / control).
//!
//! Both transports coexist. A capability's manifest declares which
//! transport each event channel uses; clients pick the appropriate
//! primitive based on that declaration. SSE remains the simpler,
//! lower-overhead default for pure push channels; WebSocket is the
//! choice when the channel needs upstream traffic (subscription
//! management, acks, capability-specific control messages).
//!
//! # Wire format
//!
//! Frames are **text** (JSON). Binary frames are rejected with a
//! [`ProtocolError::WebSocket`]. Each direction has a typed shape:
//!
//! - **Server → client**: [`ServerEvent`] — the same shape the SSE
//!   path produces, so consumer code can be transport-agnostic.
//! - **Client → server**: [`ClientMessage`] — an enum, so illegal
//!   shapes are unrepresentable.
//!
//! Ping / pong / close frames are handled by `tungstenite`
//! transparently; consumers never see them.
//!
//! # Authentication
//!
//! Bearer tokens are sent in the `Authorization` header on the
//! WebSocket upgrade. The connection is opened against the
//! capability's events URL by convention
//! (`wss://{endpoint}/events/{channel}` for TLS;
//! `ws://{endpoint}/events/{channel}` for plaintext localhost).
//!
//! # Lifetime
//!
//! The connection lives until either side closes it or the network
//! drops. Dropping the [`WsConnection`] (or both halves of a split)
//! closes the underlying socket. After a clean close the receiver
//! stream ends `Ready(None)`; after an abrupt close it ends with
//! a final `Err(ProtocolError::WebSocket)` item.

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

use crate::error::ProtocolError;
pub use crate::events::ServerEvent;

/// A typed message the client may send upstream over a WebSocket
/// event channel.
///
/// Adding a variant is a wire-compatible change as long as the new
/// variant's `type` tag is opt-in for the server (a capability that
/// does not understand `control` ignores it).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Subscribe to `channel` after the connection is open. Used
    /// when one WebSocket multiplexes multiple logical channels;
    /// capabilities that bind one channel per connection do not
    /// need this and may reject the message.
    Subscribe {
        /// The channel name to subscribe to. Same character class
        /// as an intent verb.
        channel: String,
    },
    /// Stop receiving events from `channel`.
    Unsubscribe {
        /// The channel to unsubscribe from.
        channel: String,
    },
    /// Acknowledge receipt of an event by id. Capabilities that
    /// implement event sourcing or at-least-once delivery use this
    /// to advance the cursor.
    Ack {
        /// The `id` field of the [`ServerEvent`] being acknowledged.
        id: String,
    },
    /// Capability-specific control payload. The schema is opaque to
    /// the protocol layer; capabilities define and document it.
    Control {
        /// The control payload. Opaque to this crate.
        payload: serde_json::Value,
    },
}

/// A live bidirectional WebSocket connection.
///
/// Held as a single value to start; call [`WsConnection::split`] to
/// get independent halves you can pass to two tasks (the common
/// pattern: one task forwards `ClientMessage`s upstream, another
/// consumes `ServerEvent`s downstream).
pub struct WsConnection {
    sender: WsSender,
    receiver: WsReceiver,
}

impl WsConnection {
    /// Split into two independently-owned halves.
    pub fn split(self) -> (WsSender, WsReceiver) {
        (self.sender, self.receiver)
    }

    /// Send one message upstream. Convenience for the unified form;
    /// equivalent to splitting and calling [`WsSender::send`].
    pub async fn send(&mut self, msg: &ClientMessage) -> Result<(), ProtocolError> {
        self.sender.send(msg).await
    }

    /// Receive the next event. Convenience for the unified form;
    /// equivalent to splitting and polling [`WsReceiver`].
    pub async fn next(&mut self) -> Option<Result<ServerEvent, ProtocolError>> {
        self.receiver.next().await
    }

    /// Close the connection cleanly. After this returns, the
    /// receiver half (if previously split off) yields `Ready(None)`.
    pub async fn close(mut self) -> Result<(), ProtocolError> {
        self.sender.close().await
    }
}

/// The send half of a [`WsConnection`].
pub struct WsSender {
    url: Url,
    tx: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
}

impl WsSender {
    /// Send one message upstream.
    pub async fn send(&mut self, msg: &ClientMessage) -> Result<(), ProtocolError> {
        let json = serde_json::to_string(msg).map_err(|e| ProtocolError::Json(e.to_string()))?;
        self.tx
            .send(Message::Text(json.into()))
            .await
            .map_err(|e| ProtocolError::WebSocket {
                url: self.url.clone(),
                detail: format!("send: {e}"),
            })
    }

    /// Close the connection cleanly.
    pub async fn close(&mut self) -> Result<(), ProtocolError> {
        self.tx.close().await.map_err(|e| ProtocolError::WebSocket {
            url: self.url.clone(),
            detail: format!("close: {e}"),
        })
    }
}

/// The receive half of a [`WsConnection`].
///
/// Implements [`Stream`] yielding [`ServerEvent`]s. Ping / pong / close
/// frames from the peer are handled transparently; binary frames are
/// surfaced as a final `Err` item before the stream ends.
pub struct WsReceiver {
    url: Url,
    rx: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
}

impl Stream for WsReceiver {
    type Item = Result<ServerEvent, ProtocolError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.rx).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(ProtocolError::WebSocket {
                        url: this.url.clone(),
                        detail: format!("recv: {e}"),
                    })));
                }
                Poll::Ready(Some(Ok(Message::Text(txt)))) => {
                    return Poll::Ready(Some(parse_server_event(&this.url, txt.as_str())));
                }
                Poll::Ready(Some(Ok(Message::Binary(_)))) => {
                    return Poll::Ready(Some(Err(ProtocolError::WebSocket {
                        url: this.url.clone(),
                        detail: "binary frame received; Ferridis WebSocket channels are \
                                 text/JSON only"
                            .to_string(),
                    })));
                }
                // Close frame: the peer is closing. The next poll
                // will return Ready(None); fall through and loop.
                Poll::Ready(Some(Ok(Message::Close(_)))) => return Poll::Ready(None),
                // Ping / Pong / Frame: tungstenite handles control
                // frames internally on the next send; skip and
                // re-poll for the next data frame.
                Poll::Ready(Some(Ok(_))) => continue,
            }
        }
    }
}

fn parse_server_event(url: &Url, txt: &str) -> Result<ServerEvent, ProtocolError> {
    serde_json::from_str::<ServerEvent>(txt).map_err(|e| ProtocolError::WebSocket {
        url: url.clone(),
        detail: format!("malformed ServerEvent JSON: {e}"),
    })
}

/// Open a WebSocket connection to `url`.
///
/// `bearer` is sent as `Authorization: Bearer <token>` on the upgrade
/// request when provided. `url` must use the `ws://` or `wss://`
/// scheme; other schemes are rejected with [`ProtocolError::InvalidUrl`].
pub async fn connect_ws(url: &Url, bearer: Option<&str>) -> Result<WsConnection, ProtocolError> {
    match url.scheme() {
        "ws" | "wss" => {}
        other => {
            return Err(ProtocolError::InvalidUrl(format!(
                "WebSocket URL must use ws:// or wss://, got {other}://"
            )));
        }
    }

    let mut req = url
        .as_str()
        .into_client_request()
        .map_err(|e| ProtocolError::WebSocket {
            url: url.clone(),
            detail: format!("invalid upgrade request: {e}"),
        })?;

    if let Some(token) = bearer {
        let value = format!("Bearer {token}")
            .parse()
            .map_err(|e| ProtocolError::WebSocket {
                url: url.clone(),
                detail: format!("invalid bearer token: {e}"),
            })?;
        req.headers_mut().insert("Authorization", value);
    }

    let (ws_stream, _response) =
        tokio_tungstenite::connect_async(req)
            .await
            .map_err(|e| ProtocolError::WebSocket {
                url: url.clone(),
                detail: format!("handshake: {e}"),
            })?;

    let (tx, rx) = ws_stream.split();
    Ok(WsConnection {
        sender: WsSender {
            url: url.clone(),
            tx,
        },
        receiver: WsReceiver {
            url: url.clone(),
            rx,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_round_trips_through_json() {
        let cases = vec![
            ClientMessage::Subscribe {
                channel: "events".into(),
            },
            ClientMessage::Unsubscribe {
                channel: "events".into(),
            },
            ClientMessage::Ack { id: "42".into() },
            ClientMessage::Control {
                payload: serde_json::json!({"k": "v"}),
            },
        ];
        for msg in cases {
            let json = serde_json::to_string(&msg).unwrap();
            let back: ClientMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(msg, back);
        }
    }

    #[test]
    fn client_message_uses_snake_case_type_tag() {
        let m = ClientMessage::Subscribe {
            channel: "x".into(),
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"type\":\"subscribe\""), "got: {s}");
    }

    #[tokio::test]
    async fn connect_ws_rejects_non_ws_schemes() {
        let url = Url::parse("http://example.invalid/").unwrap();
        match connect_ws(&url, None).await {
            Err(ProtocolError::InvalidUrl(_)) => {}
            Err(other) => panic!("expected InvalidUrl, got {other:?}"),
            Ok(_) => panic!("expected an error for http:// scheme"),
        }
    }
}
