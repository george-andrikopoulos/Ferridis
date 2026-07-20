//! Real-to-real integration test for the Ferridis stack.
//!
//! No wiremock. This test:
//!
//! 1. Spins a real [`ferridis_adapter_fs::FilesystemCapability`] inside
//!    a real [`ferridis_adapter_sdk::AdapterServer`] on a real port.
//! 2. Builds a real [`ferridis_client::Client`] with a real wallet.
//! 3. Registers the adapter through the client over real HTTP.
//! 4. Dispatches every intent (`write-file`, `read-file`, `list-dir`,
//!    `search-files`, `move-file`) through the full client stack.
//!
//! What this catches that mocks can't: serialization mismatches at
//! the seams, URL-building bugs, response-parsing bugs, JSON shape
//! drift between adapter and client.

use std::net::SocketAddr;

use ferridis_adapter_fs::{FilesystemCapability, Root};
use ferridis_adapter_sdk::AdapterServer;
use ferridis_client::Client;
use ferridis_core::{CapabilityRef, IntentVerb};
use tempfile::TempDir;
use tokio::net::TcpListener;
use url::Url;

/// Spin a fresh filesystem adapter rooted at `dir` and return its
/// bound socket address.
async fn spin_fs_adapter(dir: &TempDir) -> SocketAddr {
    let root = Root::new(dir.path()).expect("tempdir is an absolute existing directory");
    let cap = FilesystemCapability::new(root).expect("default manifest must parse");
    let server = AdapterServer::new(cap);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = server.into_router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

/// The CapabilityRef the filesystem adapter publishes (id
/// `ferridis.fs.v1` → public-mesh URL by the v0.1 convention).
fn fs_capability_ref() -> CapabilityRef {
    CapabilityRef::parse("ferridis://public.ferridis.io/ferridis/fs@v1").unwrap()
}

async fn register_fs(client: &Client, addr: SocketAddr) -> CapabilityRef {
    let cap = fs_capability_ref();
    let manifest_url = Url::parse(&format!("http://{addr}/manifest.json")).unwrap();
    let base_url = Url::parse(&format!("http://{addr}/")).unwrap();
    let manifest = client
        .register(cap.clone(), manifest_url, base_url)
        .await
        .expect("register should fetch + validate the manifest");
    assert_eq!(manifest.id(), "ferridis.fs.v1");
    cap
}

#[tokio::test]
async fn register_then_resolve_intent() {
    let dir = TempDir::new().unwrap();
    let addr = spin_fs_adapter(&dir).await;
    let client = Client::ephemeral();
    let cap = register_fs(&client, addr).await;

    let read = IntentVerb::parse("read-file").unwrap();
    let candidates = client.candidates_for_intent(&read).await;
    assert_eq!(candidates, vec![cap.clone()]);

    let unknown = IntentVerb::parse("send-message").unwrap();
    assert!(client.candidates_for_intent(&unknown).await.is_empty());
}

#[tokio::test]
async fn write_then_read_round_trips_through_client() {
    let dir = TempDir::new().unwrap();
    let addr = spin_fs_adapter(&dir).await;
    let client = Client::ephemeral();
    let cap = register_fs(&client, addr).await;

    let write = IntentVerb::parse("write-file").unwrap();
    client
        .dispatch(
            &cap,
            write,
            serde_json::json!({"path": "hello.txt", "content": "Hello from the client!"}),
        )
        .await
        .expect("write-file must succeed");

    let read = IntentVerb::parse("read-file").unwrap();
    let resp = client
        .dispatch(&cap, read, serde_json::json!({"path": "hello.txt"}))
        .await
        .expect("read-file must succeed");
    assert_eq!(resp["content"], "Hello from the client!");
}

#[tokio::test]
async fn list_dir_and_search_files_through_client() {
    let dir = TempDir::new().unwrap();
    let addr = spin_fs_adapter(&dir).await;
    let client = Client::ephemeral();
    let cap = register_fs(&client, addr).await;

    let write = IntentVerb::parse("write-file").unwrap();
    for name in ["alpha.txt", "beta.txt", "deep/nested-secret.md"] {
        client
            .dispatch(
                &cap,
                write.clone(),
                serde_json::json!({"path": name, "content": "data"}),
            )
            .await
            .expect("write-file must succeed");
    }

    let list = IntentVerb::parse("list-dir").unwrap();
    let entries = client
        .dispatch(&cap, list, serde_json::json!({"path": ""}))
        .await
        .expect("list-dir must succeed");
    let names: Vec<&str> = entries
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"alpha.txt"));
    assert!(names.contains(&"beta.txt"));

    let search = IntentVerb::parse("search-files").unwrap();
    let hits = client
        .dispatch(
            &cap,
            search,
            serde_json::json!({"path": "", "name_contains": "secret"}),
        )
        .await
        .expect("search-files must succeed");
    let hits_arr: Vec<&str> = hits
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.as_str().unwrap())
        .collect();
    assert_eq!(hits_arr.len(), 1);
    assert!(hits_arr[0].ends_with("nested-secret.md"));
}

#[tokio::test]
async fn move_file_through_client() {
    let dir = TempDir::new().unwrap();
    let addr = spin_fs_adapter(&dir).await;
    let client = Client::ephemeral();
    let cap = register_fs(&client, addr).await;

    let write = IntentVerb::parse("write-file").unwrap();
    client
        .dispatch(
            &cap,
            write,
            serde_json::json!({"path": "src.txt", "content": "moved-content"}),
        )
        .await
        .unwrap();

    let mv = IntentVerb::parse("move-file").unwrap();
    client
        .dispatch(
            &cap,
            mv,
            serde_json::json!({"from": "src.txt", "to": "dst/final.txt"}),
        )
        .await
        .expect("move-file must succeed");

    let read = IntentVerb::parse("read-file").unwrap();
    let resp = client
        .dispatch(&cap, read, serde_json::json!({"path": "dst/final.txt"}))
        .await
        .unwrap();
    assert_eq!(resp["content"], "moved-content");
}

#[tokio::test]
async fn dispatch_against_unregistered_capability_fails() {
    let dir = TempDir::new().unwrap();
    let _addr = spin_fs_adapter(&dir).await;
    let client = Client::ephemeral();

    let cap = fs_capability_ref();
    let read = IntentVerb::parse("read-file").unwrap();
    let err = client
        .dispatch(&cap, read, serde_json::json!({"path": "x"}))
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not registered"),
        "expected CapabilityNotRegistered, got: {msg}"
    );
}

#[tokio::test]
async fn dispatch_with_intent_not_in_manifest_fails() {
    let dir = TempDir::new().unwrap();
    let addr = spin_fs_adapter(&dir).await;
    let client = Client::ephemeral();
    let cap = register_fs(&client, addr).await;

    let send = IntentVerb::parse("send-message").unwrap();
    let err = client
        .dispatch(&cap, send, serde_json::json!({}))
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("does not declare intent"),
        "expected IntentNotSupported, got: {msg}"
    );
}

#[tokio::test]
async fn wallet_round_trip_preserves_inserted_connection() {
    use ferridis_client::{MemoryStore, Wallet, WalletStore};
    use ferridis_core::{AccessToken, Connection, Pending, StoredConnection, Tier};
    use std::sync::Arc;
    use time::{Duration, OffsetDateTime};

    // v0.2 wallet: persistence is per-entry through a WalletStore.
    // Use a shared MemoryStore across two Wallet/Client instances to
    // prove the same round-trip semantics that the v0.1 file path
    // provided. Production uses KeychainStore behind the same trait.
    let store: Arc<dyn WalletStore> = Arc::new(MemoryStore::new());

    let cap = CapabilityRef::parse("ferridis://public.ferridis.io/test/example@v1").unwrap();
    let pending = Connection::<Pending>::new(cap.clone(), Tier::Native, "http://x/", "csrf");
    let authed = pending.complete(
        AccessToken::new("end-to-end-token"),
        None,
        OffsetDateTime::now_utc() + Duration::hours(1),
    );
    let stored = StoredConnection::from_authorized(&authed);

    {
        let wallet = Wallet::with_store(store.clone()).unwrap();
        let client = Client::with_wallet(wallet);
        client.insert_connection(stored).await.unwrap();
    }

    let wallet2 = Wallet::with_store(store).unwrap();
    let client2 = Client::with_wallet(wallet2);
    let recovered = client2.wallet().lock().await.authorized(&cap);
    let conn = recovered.expect("authorized connection must survive wallet-store round-trip");
    assert_eq!(conn.access_token().expose(), "end-to-end-token");
}

/// End-to-end SSE event subscription against a hand-rolled axum
/// adapter. Exercises M6 v0.2: SSE one-way notifications discovered
/// by URL convention `{base_url}/events/{channel}`.
#[tokio::test]
async fn subscribe_receives_ordered_events_over_sse() {
    use axum::Router;
    use axum::response::Sse;
    use axum::response::sse::{Event as SseEvent, KeepAlive};
    use axum::routing::get;
    use futures_util::StreamExt;
    use std::convert::Infallible;
    use std::time::Duration as StdDuration;

    // Tiny standalone adapter: just enough Ferridis surface for the
    // client to register against, plus the SSE endpoint under test.
    async fn spin_event_adapter() -> SocketAddr {
        // Three pre-baked events emitted in order, then the stream
        // closes. The third one includes an `id` so we exercise that
        // field round-trip too.
        let stream = futures_util::stream::iter(vec![
            Ok::<_, Infallible>(
                SseEvent::default()
                    .event("notice")
                    .data(r#"{"seq":1,"msg":"first"}"#),
            ),
            Ok(SseEvent::default()
                .event("notice")
                .data(r#"{"seq":2,"msg":"second"}"#)),
            Ok(SseEvent::default()
                .event("done")
                .id("final-event")
                .data(r#"{"seq":3,"msg":"third"}"#)),
        ]);

        let manifest_body = serde_json::json!({
            "ferridis_version": "0.1",
            "id": "test.events.v1",
            "name": "Events test capability",
            "category": "test",
            "summary": "SSE event subscription test capability.",
            "intents": ["read-events"],
            "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
            "tiers": ["native"],
            "auth": { "type": "none" }
        });

        let manifest_route = {
            let body = manifest_body.clone();
            move || async move { axum::Json(body) }
        };

        let app = Router::new()
            .route("/manifest.json", get(manifest_route))
            .route(
                "/events/notifications",
                get(move || async {
                    Sse::new(stream).keep_alive(
                        KeepAlive::new()
                            .interval(StdDuration::from_secs(60))
                            .text("keepalive"),
                    )
                }),
            );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    let addr = spin_event_adapter().await;
    let client = Client::ephemeral();
    let cap = CapabilityRef::parse("ferridis://public.ferridis.io/test/events@v1").unwrap();
    let manifest_url = Url::parse(&format!("http://{addr}/manifest.json")).unwrap();
    let base_url = Url::parse(&format!("http://{addr}/")).unwrap();
    client
        .register(cap.clone(), manifest_url, base_url)
        .await
        .expect("register the test events capability");

    // Subscribe, drain three events, assert shape + order.
    let mut stream = Box::pin(
        client
            .subscribe(&cap, "notifications")
            .await
            .expect("subscribe must succeed against the test adapter"),
    );

    let first = stream
        .next()
        .await
        .expect("stream not exhausted")
        .expect("event not error");
    assert_eq!(first.name, "notice");
    assert_eq!(first.data["seq"], 1);
    assert_eq!(first.data["msg"], "first");
    assert!(first.id.is_none());

    let second = stream.next().await.unwrap().unwrap();
    assert_eq!(second.name, "notice");
    assert_eq!(second.data["seq"], 2);

    let third = stream.next().await.unwrap().unwrap();
    assert_eq!(third.name, "done");
    assert_eq!(third.data["seq"], 3);
    assert_eq!(third.id.as_deref(), Some("final-event"));

    // axum's `Sse` will keep the connection open even after the inner
    // stream finishes (the `KeepAlive` ping is what would arrive
    // next). Drop the stream rather than waiting for None — the
    // contract is "events delivered in order", not "stream closes
    // after last event."
}

/// End-to-end mesh registration: spin a mock mesh on axum, sign a
/// real manifest with a fresh ECDSA P-256 key, fetch + verify
/// through `register_from_mesh`, confirm the record lands in the
/// Public tier.
#[tokio::test]
async fn register_from_mesh_verifies_signed_manifest_and_tags_public() {
    use axum::Router;
    use axum::body::Bytes;
    use axum::http::header;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use base64::Engine;
    use ferridis_client::RegistryTier;
    use sigstore::cosign::bundle::{Bundle as RekorBundle, Payload, SignedArtifactBundle};
    use sigstore::crypto::signing_key::ecdsa::{ECDSAKeys, EllipticCurve};

    // 1. The manifest the mesh is going to host. Has to round-trip
    // through `Manifest::parse`, so it needs the full v0.1 shape.
    let manifest_json = serde_json::json!({
        "ferridis_version": "0.1",
        "id": "mesh-test.fs.v1",
        "name": "Mesh Test FS",
        "category": "files",
        "summary": "Mesh-registered test capability.",
        "intents": ["read-file"],
        "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
        "tiers": ["native"],
        "auth": { "type": "none" }
    });
    let manifest_bytes = serde_json::to_vec(&manifest_json).unwrap();

    // 2. Generate a real ECDSA P-256 keypair and sign the manifest.
    let keys = ECDSAKeys::new(EllipticCurve::P256).unwrap();
    let signer = keys.to_sigstore_signer().unwrap();
    let sig = signer.sign(&manifest_bytes).unwrap();
    let b64_sig = base64::engine::general_purpose::STANDARD.encode(&sig);
    let pubkey_pem = keys.as_inner().public_key_to_pem().unwrap();

    let bundle = SignedArtifactBundle {
        base64_signature: b64_sig,
        cert: pubkey_pem,
        rekor_bundle: RekorBundle {
            signed_entry_timestamp: String::new(),
            payload: Payload {
                body: String::new(),
                integrated_time: 0,
                log_index: 0,
                log_id: String::from("mesh-test-log"),
            },
        },
    };
    let bundle_bytes = serde_json::to_vec(&bundle).unwrap();

    // 3. Spin a mock mesh that serves both artifacts at the
    // convention path. The capability ref will be
    // `ferridis://mesh.local/test/fs@v1`, so the URL convention puts
    // the artifacts under `/mesh.local/test/fs/v1/`.
    let manifest_owned = manifest_bytes.clone();
    let bundle_owned = bundle_bytes.clone();
    let app = Router::new()
        .route(
            "/mesh.local/test/fs/v1/manifest.json",
            get(move || {
                let m = manifest_owned.clone();
                async move {
                    ([(header::CONTENT_TYPE, "application/json")], Bytes::from(m)).into_response()
                }
            }),
        )
        .route(
            "/mesh.local/test/fs/v1/manifest.json.cosign.bundle",
            get(move || {
                let b = bundle_owned.clone();
                async move {
                    ([(header::CONTENT_TYPE, "application/json")], Bytes::from(b)).into_response()
                }
            }),
        );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mesh_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // 4. Register through the client.
    let client = Client::ephemeral();
    let cap = CapabilityRef::parse("ferridis://mesh.local/test/fs@v1").unwrap();
    let mesh_root = Url::parse(&format!("http://{mesh_addr}/")).unwrap();
    let base_url = Url::parse("http://does-not-matter.invalid/").unwrap();

    // Manifest doesn't yet carry endpoint_url, so the fallback is used.
    let manifest = client
        .register_from_mesh(cap.clone(), mesh_root, Some(base_url))
        .await
        .expect("mesh fetch + verify + register must succeed");
    assert_eq!(manifest.id(), "mesh-test.fs.v1");

    // 5. Verify the record landed in the Public tier.
    let registry = client.registry().lock().await;
    let record = registry.get(&cap).expect("capability is registered");
    assert_eq!(record.tier(), RegistryTier::Public);
}

/// End-to-end SSE session-expiry recovery. Spin a mock MCP-over-SSE
/// server that hands out a fresh `sessionId` on each `GET /sse` and
/// 404s POSTs against any but the current id. Drive an `SseTransport`
/// through `request → 404 → reconnect → request → success` and
/// assert the reconnect path produces a working response.
#[tokio::test]
async fn sse_transport_reconnects_after_session_expiry() {
    use axum::Router;
    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use axum::response::Sse;
    use axum::response::sse::{Event as SseEvent, KeepAlive};
    use axum::routing::{get, post};
    use ferridis_client::mcp::{McpTransport, SseTransport};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::time::Duration;
    use tokio::sync::broadcast;

    #[derive(Clone)]
    struct AppState {
        // Session counter. Increments on every /sse GET; only the
        // latest value is "alive". POSTs to older sessions 404.
        current_session: StdArc<AtomicU64>,
        // Broadcast channel for response events to push to whichever
        // SSE stream is currently active. The reader subscribes; the
        // POST handler publishes.
        responses: broadcast::Sender<String>,
    }

    async fn open_sse(
        State(state): State<AppState>,
    ) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
        use futures_util::StreamExt;
        let id = state.current_session.fetch_add(1, AtomicOrdering::SeqCst) + 1;
        // First-event-then-passthrough: emit the endpoint immediately,
        // then forward any broadcast responses for this session's
        // lifetime.
        let endpoint = SseEvent::default()
            .event("endpoint")
            .data(format!("/messages?session={id}"));
        let mut rx = state.responses.subscribe();
        let response_stream = async_stream::stream! {
            while let Ok(json_line) = rx.recv().await {
                yield Ok(SseEvent::default().event("message").data(json_line));
            }
        };
        let stream = futures_util::stream::once(async move { Ok(endpoint) }).chain(response_stream);
        Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(60)))
    }

    async fn post_messages(
        State(state): State<AppState>,
        Query(params): Query<HashMap<String, String>>,
        body: String,
    ) -> Result<&'static str, StatusCode> {
        let req_session: u64 = params
            .get("session")
            .and_then(|s| s.parse().ok())
            .ok_or(StatusCode::BAD_REQUEST)?;
        let current = state.current_session.load(AtomicOrdering::SeqCst);
        if req_session != current {
            // Stale session — exactly what HA's MCP server does after
            // a restart.
            return Err(StatusCode::NOT_FOUND);
        }
        // Push a JSON-RPC response on the SSE stream so the
        // transport's `request` future resolves. Echo whatever `id`
        // arrived in the payload.
        let parsed: serde_json::Value =
            serde_json::from_str(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
        let id = parsed.get("id").cloned().unwrap_or(json!(0));
        let response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"ok": true, "session": current}
        });
        let _ = state.responses.send(response.to_string());
        Ok("accepted")
    }

    let (tx, _) = broadcast::channel(64);
    let state = AppState {
        current_session: StdArc::new(AtomicU64::new(0)),
        responses: tx,
    };
    let app = Router::new()
        .route("/sse", get(open_sse))
        .route("/messages", post(post_messages))
        .with_state(state.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let sse_url = Url::parse(&format!("http://{addr}/sse")).unwrap();

    let transport = SseTransport::open(sse_url.clone()).await.unwrap();
    transport.wait_for_endpoint(&sse_url).await.unwrap();
    transport.bind_origin(&sse_url).await.unwrap();

    // First request works.
    let resp = transport
        .request(json!({"jsonrpc":"2.0","id":1,"method":"x"}), 1)
        .await
        .expect("first request must succeed on session 1");
    assert_eq!(resp["result"]["session"], 1);

    // Force the server to recycle the session out from under us.
    // Bump current_session forward without anyone opening /sse — this
    // simulates the HA-VM-restart situation precisely.
    state.current_session.store(99, AtomicOrdering::SeqCst);

    // The next request should now see a 404 from POST. The transport
    // surfaces `McpSessionExpired`; without auto-reconnect the
    // caller would be stuck. The caller (typically McpClient)
    // catches it and calls `reconnect()`.
    let err = transport
        .request(json!({"jsonrpc":"2.0","id":2,"method":"x"}), 2)
        .await
        .unwrap_err();
    match err {
        ferridis_client::ClientError::McpSessionExpired(_) => {}
        other => panic!("expected McpSessionExpired, got {other:?}"),
    }

    // Reconnect: opens new /sse → new endpoint → fresh sessionId.
    transport.reconnect().await.expect("reconnect must succeed");

    // Request on the fresh session should now succeed.
    let resp = transport
        .request(json!({"jsonrpc":"2.0","id":3,"method":"x"}), 3)
        .await
        .expect("post-reconnect request must succeed");
    assert!(resp["result"]["ok"].as_bool().unwrap_or(false));
    // The mock incremented current_session twice (once for the initial
    // open, again on reconnect), and we forced it to 99 in between.
    // The reconnect's GET /sse bumps from 99 → 100.
    assert_eq!(resp["result"]["session"], 100);
}

/// Federated mesh: two mesh roots in priority order. The first
/// (org-style) returns 404 for the requested capability; the
/// second (public-style) hosts it. `FederatedMesh::fetch_and_verify`
/// must fall through and return the manifest from the second mesh.
#[tokio::test]
async fn federated_mesh_falls_through_on_404_and_returns_from_next_tier() {
    use axum::Router;
    use axum::body::Bytes;
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use base64::Engine;
    use ferridis_protocol::{Client as HttpClient, FederatedMesh, MeshClient};
    use sigstore::cosign::bundle::{Bundle as RekorBundle, Payload, SignedArtifactBundle};
    use sigstore::crypto::signing_key::ecdsa::{ECDSAKeys, EllipticCurve};

    // The capability the federation is going to look up.
    let cap_str = "ferridis://mesh.local/test/fs@v1";

    // Manifest body (only the public mesh hosts it).
    let manifest_body = serde_json::json!({
        "ferridis_version": "0.1",
        "id": "fed-test.fs.v1",
        "name": "Fed Test FS",
        "category": "files",
        "summary": "Federated-mesh fall-through test.",
        "intents": ["read-file"],
        "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
        "tiers": ["native"],
        "auth": { "type": "none" }
    });
    let manifest_bytes = serde_json::to_vec(&manifest_body).unwrap();

    // Sign with a fresh ECDSA key. The federation's
    // `fetch_and_verify` only checks signature-vs-cert (no
    // TrustRoot), so we don't need full Sigstore trust material here.
    let keys = ECDSAKeys::new(EllipticCurve::P256).unwrap();
    let signer = keys.to_sigstore_signer().unwrap();
    let sig = signer.sign(&manifest_bytes).unwrap();
    let b64_sig = base64::engine::general_purpose::STANDARD.encode(&sig);
    let pubkey_pem = keys.as_inner().public_key_to_pem().unwrap();
    let bundle = SignedArtifactBundle {
        base64_signature: b64_sig,
        cert: pubkey_pem,
        rekor_bundle: RekorBundle {
            signed_entry_timestamp: String::new(),
            payload: Payload {
                body: String::new(),
                integrated_time: 0,
                log_index: 0,
                log_id: String::from("fed-test"),
            },
        },
    };
    let bundle_bytes = serde_json::to_vec(&bundle).unwrap();

    // Mesh A — returns 404 for every artifact path. Stand-in for
    // a higher-priority mesh that doesn't carry this capability.
    let mesh_a_app = Router::new().fallback(get(|| async {
        (StatusCode::NOT_FOUND, "not on this mesh").into_response()
    }));
    let mesh_a_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mesh_a_addr = mesh_a_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(mesh_a_listener, mesh_a_app).await.unwrap();
    });

    // Mesh B — serves the signed manifest at the convention path.
    let manifest_owned = manifest_bytes.clone();
    let bundle_owned = bundle_bytes.clone();
    let mesh_b_app = Router::new()
        .route(
            "/mesh.local/test/fs/v1/manifest.json",
            get(move || {
                let m = manifest_owned.clone();
                async move {
                    ([(header::CONTENT_TYPE, "application/json")], Bytes::from(m)).into_response()
                }
            }),
        )
        .route(
            "/mesh.local/test/fs/v1/manifest.json.cosign.bundle",
            get(move || {
                let b = bundle_owned.clone();
                async move {
                    ([(header::CONTENT_TYPE, "application/json")], Bytes::from(b)).into_response()
                }
            }),
        );
    let mesh_b_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mesh_b_addr = mesh_b_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(mesh_b_listener, mesh_b_app).await.unwrap();
    });

    let http = HttpClient::new();
    let mesh_a = MeshClient::new(
        http.clone(),
        Url::parse(&format!("http://{mesh_a_addr}/")).unwrap(),
    );
    let mesh_b = MeshClient::new(http, Url::parse(&format!("http://{mesh_b_addr}/")).unwrap());
    let federation = FederatedMesh::new(vec![mesh_a, mesh_b]);

    let cap = CapabilityRef::parse(cap_str).unwrap();
    let artifact = federation
        .fetch_and_verify(&cap)
        .await
        .expect("federation must fall through to mesh B and succeed");
    assert_eq!(artifact.manifest_bytes, manifest_bytes);
}

#[tokio::test]
async fn federated_mesh_capability_not_found_when_no_tier_has_it() {
    use axum::Router;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use ferridis_protocol::ProtocolError;
    use ferridis_protocol::{Client as HttpClient, FederatedMesh, MeshClient};

    // Both meshes 404 for everything.
    let app = || {
        Router::new().fallback(get(|| async {
            (StatusCode::NOT_FOUND, "404").into_response()
        }))
    };
    let l1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a1 = l1.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l1, app()).await.unwrap() });
    let l2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a2 = l2.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l2, app()).await.unwrap() });

    let http = HttpClient::new();
    let federation = FederatedMesh::new(vec![
        MeshClient::new(http.clone(), Url::parse(&format!("http://{a1}/")).unwrap()),
        MeshClient::new(http, Url::parse(&format!("http://{a2}/")).unwrap()),
    ]);
    let cap = CapabilityRef::parse("ferridis://mesh.local/x/y@v1").unwrap();
    match federation.fetch_and_verify(&cap).await {
        Err(ProtocolError::CapabilityNotFound(_)) => {}
        other => panic!("expected CapabilityNotFound when no tier has the cap, got {other:?}"),
    }
}

/// End-to-end streaming dispatch: a mock adapter declares a
/// `stream`-kind intent in its manifest, responds to POST with
/// SSE `event: chunk` lines, terminates with `event: end`. Client
/// `dispatch_streaming` consumes the chunks in order.
#[tokio::test]
async fn dispatch_streaming_consumes_ordered_chunks() {
    use axum::Router;
    use axum::response::Sse;
    use axum::response::sse::{Event as SseEvent, KeepAlive};
    use axum::routing::{get, post};
    use futures_util::StreamExt;
    use std::convert::Infallible;
    use std::time::Duration as StdDuration;

    let manifest_body = serde_json::json!({
        "ferridis_version": "0.1",
        "id": "stream.test.v1",
        "name": "Streaming test capability",
        "category": "search",
        "summary": "Manifest with one stream-kind intent.",
        "intents": [
            {"verb": "search", "kind": "stream",
             "chunk_schema_url": "https://x/search.chunk.json"}
        ],
        "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
        "tiers": ["native"],
        "auth": { "type": "none" }
    });

    let manifest_route = {
        let body = manifest_body.clone();
        move || async move { axum::Json(body) }
    };

    let stream_handler = move || async move {
        let events = futures_util::stream::iter(vec![
            Ok::<_, Infallible>(
                SseEvent::default()
                    .event("chunk")
                    .data(r#"{"id":"d1","score":0.9}"#),
            ),
            Ok(SseEvent::default()
                .event("chunk")
                .data(r#"{"id":"d2","score":0.8}"#)),
            Ok(SseEvent::default()
                .event("chunk")
                .data(r#"{"id":"d3","score":0.7}"#)),
            Ok(SseEvent::default().event("end").data(r#"{"total":3}"#)),
        ]);
        Sse::new(events).keep_alive(
            KeepAlive::new()
                .interval(StdDuration::from_secs(60))
                .text("keepalive"),
        )
    };

    let app = Router::new()
        .route("/manifest.json", get(manifest_route))
        .route("/intents/search", post(stream_handler));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = Client::ephemeral();
    let cap = CapabilityRef::parse("ferridis://public.ferridis.io/stream/test@v1").unwrap();
    let manifest_url = Url::parse(&format!("http://{addr}/manifest.json")).unwrap();
    let base_url = Url::parse(&format!("http://{addr}/")).unwrap();
    client
        .register(cap.clone(), manifest_url, base_url)
        .await
        .expect("register stream-test capability");

    let search = IntentVerb::parse("search").unwrap();
    let mut stream = Box::pin(
        client
            .dispatch_streaming(&cap, search.clone(), serde_json::json!({"query": "x"}))
            .await
            .expect("dispatch_streaming on a stream-kind intent succeeds"),
    );

    let chunk1 = stream
        .next()
        .await
        .expect("first chunk")
        .expect("chunk not error");
    assert_eq!(chunk1["id"], "d1");
    assert_eq!(chunk1["score"], 0.9);

    let chunk2 = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk2["id"], "d2");

    let chunk3 = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk3["id"], "d3");

    // `event: end` terminates the stream.
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn dispatch_on_stream_kind_intent_errors_with_intent_requires_streaming() {
    use ferridis_client::ClientError;

    let manifest_body = serde_json::json!({
        "ferridis_version": "0.1",
        "id": "stream.guard.v1",
        "name": "Stream guard test",
        "category": "search",
        "summary": "Intent-kind mismatch guard.",
        "intents": [
            {"verb": "stream-things", "kind": "stream",
             "chunk_schema_url": "https://x/c.json"}
        ],
        "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
        "tiers": ["native"],
        "auth": { "type": "none" }
    });
    use axum::Router;
    use axum::routing::get;
    let manifest_route = move || {
        let body = manifest_body.clone();
        async move { axum::Json(body) }
    };
    let app = Router::new().route("/manifest.json", get(manifest_route));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = Client::ephemeral();
    let cap = CapabilityRef::parse("ferridis://public.ferridis.io/sg/test@v1").unwrap();
    let manifest_url = Url::parse(&format!("http://{addr}/manifest.json")).unwrap();
    let base_url = Url::parse(&format!("http://{addr}/")).unwrap();
    client
        .register(cap.clone(), manifest_url, base_url)
        .await
        .unwrap();

    let stream_intent = IntentVerb::parse("stream-things").unwrap();
    let err = client
        .dispatch(&cap, stream_intent, serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::IntentRequiresStreaming { .. }),
        "dispatch on a Stream intent must error IntentRequiresStreaming, got {err:?}"
    );
}

/// `Client::register_from_federated_mesh` should consume the same
/// kind of FederatedMesh `fetch_and_verify` exposes — mesh A 404s,
/// mesh B serves the signed manifest, the resulting record is
/// tagged Public.
#[tokio::test]
async fn register_from_federated_mesh_falls_through_to_next_tier_and_tags_public() {
    use axum::Router;
    use axum::body::Bytes;
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use base64::Engine;
    use ferridis_client::RegistryTier;
    use ferridis_protocol::{Client as HttpClient, FederatedMesh, MeshClient};
    use sigstore::cosign::bundle::{Bundle as RekorBundle, Payload, SignedArtifactBundle};
    use sigstore::crypto::signing_key::ecdsa::{ECDSAKeys, EllipticCurve};

    let cap_str = "ferridis://mesh.local/fed-reg/test@v1";

    let manifest_body = serde_json::json!({
        "ferridis_version": "0.1",
        "id": "fed-reg.fs.v1",
        "name": "Federated registration test",
        "category": "files",
        "summary": "Exercises Client::register_from_federated_mesh.",
        "intents": ["read-file"],
        "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
        "tiers": ["native"],
        "auth": { "type": "none" }
    });
    let manifest_bytes = serde_json::to_vec(&manifest_body).unwrap();

    let keys = ECDSAKeys::new(EllipticCurve::P256).unwrap();
    let signer = keys.to_sigstore_signer().unwrap();
    let sig = signer.sign(&manifest_bytes).unwrap();
    let b64_sig = base64::engine::general_purpose::STANDARD.encode(&sig);
    let pubkey_pem = keys.as_inner().public_key_to_pem().unwrap();
    let bundle = SignedArtifactBundle {
        base64_signature: b64_sig,
        cert: pubkey_pem,
        rekor_bundle: RekorBundle {
            signed_entry_timestamp: String::new(),
            payload: Payload {
                body: String::new(),
                integrated_time: 0,
                log_index: 0,
                log_id: String::from("fed-reg-test"),
            },
        },
    };
    let bundle_bytes = serde_json::to_vec(&bundle).unwrap();

    // Mesh A — 404s everything (high-priority tier without this cap).
    let mesh_a_app = Router::new().fallback(get(|| async {
        (StatusCode::NOT_FOUND, "not on this mesh").into_response()
    }));
    let mesh_a_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mesh_a_addr = mesh_a_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(mesh_a_listener, mesh_a_app).await.unwrap();
    });

    // Mesh B — serves the signed manifest at the convention path.
    let manifest_owned = manifest_bytes.clone();
    let bundle_owned = bundle_bytes.clone();
    let mesh_b_app = Router::new()
        .route(
            "/mesh.local/fed-reg/test/v1/manifest.json",
            get(move || {
                let m = manifest_owned.clone();
                async move {
                    ([(header::CONTENT_TYPE, "application/json")], Bytes::from(m)).into_response()
                }
            }),
        )
        .route(
            "/mesh.local/fed-reg/test/v1/manifest.json.cosign.bundle",
            get(move || {
                let b = bundle_owned.clone();
                async move {
                    ([(header::CONTENT_TYPE, "application/json")], Bytes::from(b)).into_response()
                }
            }),
        );
    let mesh_b_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mesh_b_addr = mesh_b_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(mesh_b_listener, mesh_b_app).await.unwrap();
    });

    let http = HttpClient::new();
    let mesh_a = MeshClient::new(
        http.clone(),
        Url::parse(&format!("http://{mesh_a_addr}/")).unwrap(),
    );
    let mesh_b = MeshClient::new(http, Url::parse(&format!("http://{mesh_b_addr}/")).unwrap());
    let federation = FederatedMesh::new(vec![mesh_a, mesh_b]);

    let cap = CapabilityRef::parse(cap_str).unwrap();
    let fallback = Url::parse("http://does-not-matter.invalid/").unwrap();
    let client = Client::ephemeral();
    let manifest = client
        .register_from_federated_mesh(cap.clone(), &federation, Some(fallback))
        .await
        .expect("federated registration must fall through to mesh B and tag Public");
    assert_eq!(manifest.id(), "fed-reg.fs.v1");

    let registry = client.registry().lock().await;
    let record = registry.get(&cap).expect("registered");
    assert_eq!(record.tier(), RegistryTier::Public);
}

/// Full SDK ↔ client round-trip for a stream-kind intent. Adapter
/// implements `Capability::dispatch_stream` returning a real
/// stream; AdapterServer wraps it as SSE; client consumes via
/// `dispatch_streaming`. Both sides shipping in v0.3 means the
/// streaming surface is symmetric end-to-end without any
/// hand-rolled wire-format code on either side.
#[tokio::test]
async fn adapter_dispatch_stream_round_trips_through_dispatch_streaming() {
    use async_trait::async_trait;
    use ferridis_adapter_sdk::{
        AdapterServer, Capability, DispatchError, IntentStream, SchemaSource,
    };
    use ferridis_core::Manifest;
    use futures_util::StreamExt;

    // The manifest declares one stream-kind intent.
    const MANIFEST_JSON: &str = r#"{
        "ferridis_version": "0.1",
        "id": "sdk-stream.test.v1",
        "name": "SDK streaming test",
        "category": "search",
        "summary": "Stream-kind intent exercised end-to-end.",
        "intents": [
            {"verb": "search", "kind": "stream",
             "chunk_schema_url": "https://x/search.chunk.json"}
        ],
        "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
        "tiers": ["native"],
        "auth": { "type": "none" }
    }"#;

    struct StreamSearchCap {
        manifest: Manifest,
    }

    #[async_trait]
    impl Capability for StreamSearchCap {
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
            intent: &ferridis_core::IntentVerb,
            _body: serde_json::Value,
        ) -> Result<serde_json::Value, DispatchError> {
            // This capability's only intent is stream-kind; the SDK
            // routes it through dispatch_stream and never reaches
            // here. Surface a clear error if it ever does.
            Err(DispatchError::UnsupportedIntent(intent.clone()))
        }
        async fn dispatch_stream(
            &self,
            _intent: &ferridis_core::IntentVerb,
            _body: serde_json::Value,
        ) -> Result<IntentStream, DispatchError> {
            // Hand-baked stream of three chunks. Real adapters would
            // pull from a database cursor / async iterator / etc.
            let s = async_stream::stream! {
                yield Ok(serde_json::json!({"doc": "alpha", "score": 0.91}));
                yield Ok(serde_json::json!({"doc": "beta",  "score": 0.83}));
                yield Ok(serde_json::json!({"doc": "gamma", "score": 0.77}));
            };
            Ok(Box::pin(s))
        }
    }

    // Spin the adapter on a real port.
    let cap = StreamSearchCap {
        manifest: Manifest::parse(MANIFEST_JSON).unwrap(),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = AdapterServer::new(cap).into_router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    // Register the capability through ferridis-client, then exercise
    // dispatch_streaming.
    let client = Client::ephemeral();
    let cap_ref = CapabilityRef::parse("ferridis://public.ferridis.io/sdk-stream/test@v1").unwrap();
    let manifest_url = Url::parse(&format!("http://{addr}/manifest.json")).unwrap();
    let base_url = Url::parse(&format!("http://{addr}/")).unwrap();
    client
        .register(cap_ref.clone(), manifest_url, base_url)
        .await
        .expect("register SDK streaming capability");

    let search = IntentVerb::parse("search").unwrap();
    let mut stream = Box::pin(
        client
            .dispatch_streaming(&cap_ref, search, serde_json::json!({"q": "anything"}))
            .await
            .expect("dispatch_streaming on the stream-kind intent succeeds"),
    );

    let c1 = stream.next().await.unwrap().unwrap();
    assert_eq!(c1["doc"], "alpha");
    let c2 = stream.next().await.unwrap().unwrap();
    assert_eq!(c2["doc"], "beta");
    let c3 = stream.next().await.unwrap().unwrap();
    assert_eq!(c3["doc"], "gamma");
    // SDK appends an `event: end` after the adapter's stream
    // exhausts; the client consumer treats that as a clean
    // termination.
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn subscribe_errors_on_unregistered_capability() {
    use ferridis_client::ClientError;

    let client = Client::ephemeral();
    let cap = CapabilityRef::parse("ferridis://public.ferridis.io/never/registered@v1").unwrap();
    // The returned `impl Stream` isn't Debug, so use match rather
    // than unwrap_err.
    match client.subscribe(&cap, "anything").await {
        Ok(_) => panic!("subscribe should error for an unregistered capability"),
        Err(ClientError::CapabilityNotRegistered(_)) => {}
        Err(other) => panic!("wrong error variant: {other:?}"),
    }
}
