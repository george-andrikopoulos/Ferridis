//! End-to-end backpressure flow: an adapter that emits
//! `BackpressureSignal`s through `dispatch_stream_flow`, the SDK
//! translating them to `event: backpressure` on the SSE wire, and the
//! client honoring them — slow-down is advisory, halt surfaces as the
//! typed `ClientError::StreamHalted` and terminates the stream.

use async_trait::async_trait;
use ferridis_adapter_sdk::{
    AdapterServer, BackpressureSignal, Capability, DispatchError, FlowIntentStream, SchemaSource,
    StreamChunk,
};
use ferridis_client::{Client, ClientError};
use ferridis_core::{CapabilityRef, IntentVerb, Manifest};
use futures_util::StreamExt as _;
use tokio::net::TcpListener;
use url::Url;

const MANIFEST_JSON: &str = r#"{
    "ferridis_version": "0.1",
    "id": "flow.test.v1",
    "name": "Backpressure flow test",
    "category": "search",
    "summary": "Stream-kind intent with flow-control signals.",
    "intents": [
        {"verb": "tick", "kind": "stream",
         "chunk_schema_url": "https://x/tick.chunk.json"}
    ],
    "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
    "tiers": ["native"],
    "auth": { "type": "none" }
}"#;

/// Emits: chunk `a` (Continue), chunk `b` (SlowDown), then a chunk
/// tagged Halt — which the SDK must translate into a terminating
/// `backpressure: halt` without delivering the chunk's payload.
struct HaltingCap {
    manifest: Manifest,
}

#[async_trait]
impl Capability for HaltingCap {
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
                serde_json::json!({"id": "never-delivered"}),
                BackpressureSignal::Halt,
            ));
            // Anything after Halt must never reach the wire.
            yield Ok(StreamChunk::data_only(serde_json::json!({"id": "zombie"})));
        };
        Ok(Box::pin(s))
    }
}

async fn spin_and_register(client: &Client) -> CapabilityRef {
    let cap = HaltingCap {
        manifest: Manifest::parse(MANIFEST_JSON).expect("valid manifest"),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let router = AdapterServer::new(cap).into_router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve adapter");
    });

    let cap_ref = CapabilityRef::parse("ferridis://public.ferridis.io/flow/test@v1")
        .expect("valid capability ref");
    let manifest_url = Url::parse(&format!("http://{addr}/manifest.json")).expect("valid url");
    let base_url = Url::parse(&format!("http://{addr}/")).expect("valid url");
    client
        .register(cap_ref.clone(), manifest_url, base_url)
        .await
        .expect("register flow capability");
    cap_ref
}

/// Chunks before the halt arrive intact (slow-down is advisory in this
/// API); the halt surfaces as the typed `StreamHalted` and terminates
/// the stream — the halt-tagged chunk's payload is never delivered.
#[tokio::test]
async fn halt_signal_surfaces_as_typed_error_and_terminates() {
    let client = Client::ephemeral();
    let cap_ref = spin_and_register(&client).await;
    let tick = IntentVerb::parse("tick").expect("valid verb");

    let mut stream = Box::pin(
        client
            .dispatch_streaming(&cap_ref, tick.clone(), serde_json::json!({}))
            .await
            .expect("dispatch_streaming succeeds"),
    );

    let first = stream.next().await.expect("chunk a").expect("a is ok");
    assert_eq!(first["id"], "a");

    // Chunk b follows the slow-down signal — advisory, data flows on.
    let second = stream.next().await.expect("chunk b").expect("b is ok");
    assert_eq!(second["id"], "b");

    let halted = stream.next().await.expect("halt item");
    match halted {
        Err(ClientError::StreamHalted { capability, intent }) => {
            assert_eq!(capability, cap_ref);
            assert_eq!(intent, tick);
        }
        other => panic!("expected StreamHalted, got {other:?}"),
    }

    // Halt terminates: no zombie chunks, no clean `end`.
    assert!(
        stream.next().await.is_none(),
        "stream must terminate after halt"
    );
}
