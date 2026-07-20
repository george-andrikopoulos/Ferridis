//! Regression pins for manifest-declared event channels at the subscribe
//! boundary: a manifest that declares channels rejects subscriptions to
//! undeclared ones with the typed `EventChannelNotDeclared` — before any
//! network I/O happens.

use ferridis_client::registry::CapabilityRecord;
use ferridis_client::{Client, ClientError};
use ferridis_core::{CapabilityRef, Manifest};
use url::Url;

const MANIFEST_WITH_CHANNELS: &str = r#"{
    "ferridis_version": "0.1",
    "id": "test.events.v1",
    "name": "Events test capability",
    "category": "test",
    "summary": "Event-emitting test capability.",
    "intents": ["read-events"],
    "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
    "event_channels": [
        {"name": "state-changed"},
        {"name": "service-called", "chunk_schema_url": "https://x/svc.json"}
    ],
    "tiers": ["native"],
    "auth": { "type": "none" }
}"#;

async fn client_with_channel_capability() -> (Client, CapabilityRef) {
    let client = Client::ephemeral();
    let cap: CapabilityRef = "ferridis://personal.local/test/events@v1"
        .parse()
        .expect("valid capability ref");
    let manifest = Manifest::parse(MANIFEST_WITH_CHANNELS).expect("valid manifest");
    // 127.0.0.1:9 (discard port) — the declaration check must fire before
    // any connection is attempted, so this URL is never dialled in the
    // rejection test.
    let base_url = Url::parse("http://127.0.0.1:9/").expect("valid url");
    client
        .registry()
        .lock()
        .await
        .insert(CapabilityRecord::new(cap.clone(), manifest, base_url));
    (client, cap)
}

/// Subscribing to a channel the manifest does not declare returns the
/// typed error carrying the declared-channel list.
#[tokio::test]
async fn subscribe_rejects_undeclared_channel_with_typed_error() {
    let (client, cap) = client_with_channel_capability().await;
    let err = match client.subscribe(&cap, "never-declared").await {
        Err(e) => e,
        Ok(_) => panic!("expected EventChannelNotDeclared"),
    };
    match err {
        ClientError::EventChannelNotDeclared {
            capability,
            channel,
            declared,
        } => {
            assert_eq!(capability, cap);
            assert_eq!(channel, "never-declared");
            assert_eq!(declared, vec!["state-changed", "service-called"]);
        }
        other => panic!("expected EventChannelNotDeclared, got {other:?}"),
    }
}

/// A declared channel passes the declaration gate — the failure that
/// follows (unreachable adapter) must be anything but
/// `EventChannelNotDeclared`.
#[tokio::test]
async fn subscribe_accepts_declared_channel_past_the_gate() {
    let (client, cap) = client_with_channel_capability().await;
    // Any transport-level failure (or, improbably, a success against the
    // discard port) means the gate passed — which is the guarantee under
    // test. Only the declaration rejection itself is a failure.
    if let Err(ClientError::EventChannelNotDeclared { .. }) =
        client.subscribe(&cap, "state-changed").await
    {
        panic!("declared channel must not be rejected by the declaration gate")
    }
}

/// `subscribe_ws` shares the declaration gate.
#[tokio::test]
async fn subscribe_ws_rejects_undeclared_channel_with_typed_error() {
    let (client, cap) = client_with_channel_capability().await;
    let err = match client.subscribe_ws(&cap, "never-declared").await {
        Err(e) => e,
        Ok(_) => panic!("expected EventChannelNotDeclared"),
    };
    assert!(
        matches!(err, ClientError::EventChannelNotDeclared { .. }),
        "expected EventChannelNotDeclared, got {err:?}"
    );
}
