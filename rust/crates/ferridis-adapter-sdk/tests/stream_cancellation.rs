//! Cancellation semantics for streamed intents: when the consumer
//! drops the connection, the adapter's intent stream must be dropped —
//! releasing whatever the capability holds (child processes, cursors,
//! file handles). This is the guarantee that lets `kill_on_drop`-style
//! cleanup in adapters (e.g. the Claude CLI adapter) actually fire.
//!
//! Also pins the wire shape of the backpressure events the SDK emits
//! from `dispatch_stream_flow`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use ferridis_adapter_sdk::{
    AdapterServer, BackpressureSignal, Capability, DispatchError, FlowIntentStream, IntentStream,
    SchemaSource, StreamChunk,
};
use ferridis_core::{IntentVerb, Manifest};
use futures_util::StreamExt as _;
use tokio::net::TcpListener;

const MANIFEST_JSON: &str = r#"{
    "ferridis_version": "0.1",
    "id": "cancel.test.v1",
    "name": "Cancellation test",
    "category": "search",
    "summary": "Infinite stream-kind intent for cancellation tests.",
    "intents": [
        {"verb": "tick", "kind": "stream",
         "chunk_schema_url": "https://x/tick.chunk.json"}
    ],
    "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
    "tiers": ["native"],
    "auth": { "type": "none" }
}"#;

/// Sets its flag when dropped — stands in for a child process /
/// cursor / handle owned by a real capability's stream.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Ticks forever (every 20 ms), holding a [`DropFlag`] inside the
/// stream so the test can observe when the SDK drops it.
struct TickingCap {
    manifest: Manifest,
    stream_dropped: Arc<AtomicBool>,
}

#[async_trait]
impl Capability for TickingCap {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn schema(&self) -> SchemaSource {
        SchemaSource::Embedded {
            content_type: "application/yaml".into(),
            body: "openapi: 3.0.0\n".into(),
        }
    }
    async fn dispatch(
        &self,
        intent: &IntentVerb,
        _body: serde_json::Value,
    ) -> Result<serde_json::Value, DispatchError> {
        Err(DispatchError::UnsupportedIntent(intent.clone())) // clone: error carries an owned verb
    }
    async fn dispatch_stream(
        &self,
        _intent: &IntentVerb,
        _body: serde_json::Value,
    ) -> Result<IntentStream, DispatchError> {
        let guard = DropFlag(Arc::clone(&self.stream_dropped));
        let s = async_stream::stream! {
            let _guard = guard;
            let mut n = 0u64;
            loop {
                tokio::time::sleep(Duration::from_millis(20)).await;
                n += 1;
                yield Ok(serde_json::json!({"n": n}));
            }
        };
        Ok(Box::pin(s))
    }
}

async fn spin(cap: impl Capability + 'static) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let router = AdapterServer::new(cap).into_router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve adapter");
    });
    addr
}

/// Dropping the consumer's connection mid-stream drops the adapter's
/// intent stream (and everything it owns) promptly — the producer is
/// active, so the SDK notices the dead connection on the next write.
#[tokio::test]
async fn consumer_drop_reaches_the_adapter_stream_drop() {
    let stream_dropped = Arc::new(AtomicBool::new(false));
    let cap = TickingCap {
        manifest: Manifest::parse(MANIFEST_JSON).expect("valid manifest"),
        stream_dropped: Arc::clone(&stream_dropped),
    };
    let addr = spin(cap).await;

    // Open the streamed intent and read at least one chunk so the
    // stream is provably live before the cancel.
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/intents/tick"))
        .header("accept", "text/event-stream")
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("open streamed intent");
    assert!(resp.status().is_success());
    let mut bytes = resp.bytes_stream();
    let first = bytes
        .next()
        .await
        .expect("first SSE bytes")
        .expect("bytes ok");
    assert!(!first.is_empty());
    assert!(
        !stream_dropped.load(Ordering::SeqCst),
        "stream must be live before cancel"
    );

    // Cancel: drop the response mid-stream.
    drop(bytes);

    // The adapter's stream (and its DropFlag) must be dropped promptly.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !stream_dropped.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "adapter stream was not dropped after consumer cancellation"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ── Wire shape of backpressure events ─────────────────────────────────────────

struct FlowWireCap {
    manifest: Manifest,
}

#[async_trait]
impl Capability for FlowWireCap {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn schema(&self) -> SchemaSource {
        SchemaSource::Embedded {
            content_type: "application/yaml".into(),
            body: "openapi: 3.0.0\n".into(),
        }
    }
    async fn dispatch(
        &self,
        intent: &IntentVerb,
        _body: serde_json::Value,
    ) -> Result<serde_json::Value, DispatchError> {
        Err(DispatchError::UnsupportedIntent(intent.clone())) // clone: error carries an owned verb
    }
    async fn dispatch_stream_flow(
        &self,
        _intent: &IntentVerb,
        _body: serde_json::Value,
    ) -> Result<FlowIntentStream, DispatchError> {
        let s = async_stream::stream! {
            yield Ok(StreamChunk::data_only(serde_json::json!({"id": "a"})));
            yield Ok(StreamChunk::new(
                serde_json::json!({"id": "b"}),
                BackpressureSignal::SlowDown,
            ));
            yield Ok(StreamChunk::new(
                serde_json::json!({"id": "x"}),
                BackpressureSignal::Halt,
            ));
        };
        Ok(Box::pin(s))
    }
}

/// The wire carries: chunk(a), backpressure(slow-down) once (on the
/// state change), chunk(b), backpressure(halt), then the connection
/// closes with no `end` and without the halt-tagged chunk's payload.
#[tokio::test]
async fn backpressure_events_have_the_documented_wire_shape() {
    let cap = FlowWireCap {
        manifest: Manifest::parse(MANIFEST_JSON).expect("valid manifest"),
    };
    let addr = spin(cap).await;

    let body = reqwest::Client::new()
        .post(format!("http://{addr}/intents/tick"))
        .header("accept", "text/event-stream")
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("open streamed intent")
        .text()
        .await
        .expect("stream closes after halt, body completes");

    let events: Vec<&str> = body
        .lines()
        .filter(|l| l.starts_with("event: "))
        .map(|l| l.trim_start_matches("event: "))
        .collect();
    assert_eq!(
        events,
        vec!["chunk", "backpressure", "chunk", "backpressure"],
        "unexpected event sequence in body:\n{body}"
    );
    assert!(body.contains(r#"{"signal":"slow-down"}"#), "body:\n{body}");
    assert!(body.contains(r#"{"signal":"halt"}"#), "body:\n{body}");
    assert!(
        !body.contains("event: end"),
        "halt must not be followed by end:\n{body}"
    );
    assert!(
        !body.contains(r#""id":"x""#),
        "halt-tagged chunk payload must not leak:\n{body}"
    );
}
