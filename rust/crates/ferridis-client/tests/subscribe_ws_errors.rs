//! RED tests for Task 3 — Client::subscribe_ws error paths (v0.4).

use ferridis_client::{Client, ClientError};
use ferridis_core::{CapabilityRef, ChannelTransport};

/// subscribe_ws on an unregistered capability must return
/// `ClientError::CapabilityNotRegistered`.
#[tokio::test]
async fn subscribe_ws_unregistered_capability_returns_error() {
    let client = Client::ephemeral();
    let cap: CapabilityRef = "ferridis://personal.local/test/ws@v1".parse().unwrap();
    let err = match client.subscribe_ws(&cap, "events").await {
        Err(e) => e,
        Ok(_) => panic!("expected CapabilityNotRegistered error"),
    };
    assert!(
        matches!(err, ClientError::CapabilityNotRegistered(_)),
        "expected CapabilityNotRegistered, got {err:?}"
    );
}

/// WrongTransport is a distinct ClientError variant — callers can match it.
#[test]
fn wrong_transport_is_a_distinct_error_variant() {
    let err = ClientError::WrongTransport {
        channel: "state-changed".to_string(),
        declared: ChannelTransport::Sse,
        requested: ChannelTransport::WebSocket,
    };
    let msg = err.to_string();
    assert!(msg.contains("state-changed"), "error message should include channel name");
}
