//! RED tests for Task 1 — ChannelTransport enum (v0.4).
//!
//! These tests drive the addition of `ChannelTransport` to `EventChannel`
//! and `Manifest`. They fail to compile until the type is added (the
//! compilation error IS the red state for new-type TDD in Rust).

use ferridis_core::{ChannelTransport, Manifest};

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
        {"name": "via-websocket", "transport": "websocket"}
    ],
    "tiers": ["native"],
    "auth": { "type": "none" }
}"#;

#[test]
fn channel_transport_defaults_to_sse_when_absent() {
    let m = Manifest::parse(MANIFEST_WITH_CHANNELS).unwrap();
    let ch = m.event_channels().iter().find(|c| c.name() == "state-changed").unwrap();
    assert_eq!(ch.transport(), ChannelTransport::Sse);
}

#[test]
fn channel_transport_parses_websocket() {
    let m = Manifest::parse(MANIFEST_WITH_CHANNELS).unwrap();
    let ch = m.event_channels().iter().find(|c| c.name() == "via-websocket").unwrap();
    assert_eq!(ch.transport(), ChannelTransport::WebSocket);
}

#[test]
fn manifest_channel_transport_lookup() {
    let m = Manifest::parse(MANIFEST_WITH_CHANNELS).unwrap();
    assert_eq!(m.channel_transport("state-changed"), Some(ChannelTransport::Sse));
    assert_eq!(m.channel_transport("via-websocket"), Some(ChannelTransport::WebSocket));
    assert_eq!(m.channel_transport("not-declared"), None);
}

#[test]
fn channel_transport_default_is_sse() {
    assert_eq!(ChannelTransport::default(), ChannelTransport::Sse);
}

#[test]
fn channel_transport_roundtrips_through_serde() {
    let sse = serde_json::to_string(&ChannelTransport::Sse).unwrap();
    let ws = serde_json::to_string(&ChannelTransport::WebSocket).unwrap();
    assert_eq!(sse, r#""sse""#);
    assert_eq!(ws, r#""websocket""#);
    let back: ChannelTransport = serde_json::from_str(&sse).unwrap();
    assert_eq!(back, ChannelTransport::Sse);
    let back: ChannelTransport = serde_json::from_str(&ws).unwrap();
    assert_eq!(back, ChannelTransport::WebSocket);
}
