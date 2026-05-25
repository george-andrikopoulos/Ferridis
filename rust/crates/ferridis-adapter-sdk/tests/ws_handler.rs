//! RED tests for Task 4 — Adapter-SDK WsHandler trait (v0.4).

use async_trait::async_trait;
use ferridis_adapter_sdk::ws_handler::{WsConnId, WsHandler, WsMessage};

/// A minimal no-op handler used to verify the trait is implementable.
struct EchoHandler;

#[async_trait]
impl WsHandler for EchoHandler {
    async fn on_connect(&self, _id: WsConnId) {}
    async fn on_message(&self, _id: WsConnId, _msg: WsMessage) {}
    async fn on_disconnect(&self, _id: WsConnId) {}
}

/// WsConnId is a distinct newtype — two IDs of the same value compare equal.
#[test]
fn ws_conn_id_equality() {
    let a = WsConnId::new(1);
    let b = WsConnId::new(1);
    assert_eq!(a, b);
}

/// WsMessage::Text carries a UTF-8 string.
#[test]
fn ws_message_text_carries_payload() {
    let msg = WsMessage::Text("hello".to_string());
    assert!(matches!(msg, WsMessage::Text(ref s) if s == "hello"));
}

/// WsMessage::Binary carries raw bytes.
#[test]
fn ws_message_binary_carries_bytes() {
    let msg = WsMessage::Binary(vec![0x01, 0x02]);
    assert!(matches!(msg, WsMessage::Binary(ref b) if b == &[0x01, 0x02]));
}

/// WsMessage::Close is a distinct variant.
#[test]
fn ws_message_close_variant_exists() {
    let msg = WsMessage::Close;
    assert!(matches!(msg, WsMessage::Close));
}

/// WsHandler can be boxed as a trait object (object-safe check).
#[test]
fn ws_handler_is_object_safe() {
    let _handler: Box<dyn WsHandler> = Box::new(EchoHandler);
}
