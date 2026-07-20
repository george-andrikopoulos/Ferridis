//! Tests for Client::discover_broker.

use ferridis_client::{Client, DiscoveryHandle};

#[tokio::test]
async fn discover_broker_returns_handle() {
    // Passes a URL that will fail to connect — verifies the method exists
    // and returns a handle without panicking when the broker is absent.
    let client = Client::ephemeral();
    let url: url::Url = "http://127.0.0.1:1/".parse().unwrap(); // allow:unwrap test-only literal
    let handle: DiscoveryHandle = client.discover_broker(url).await;
    drop(handle);
}

/// Verify that the snapshot is fetched synchronously: after `discover_broker`
/// returns the registry already contains the broker's services — no sleep needed.
#[tokio::test]
async fn discover_broker_snapshot_is_synchronous() {
    use axum::{
        Router,
        body::Body,
        routing::{get, post},
    };
    use bytes::Bytes;
    use futures_util::StreamExt as _;
    use serde_json::{Value, json};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    // ---- Minimal MCP SSE stub -----------------------------------------------
    // Handles the initialize → notifications/initialized → tools/list handshake.
    // A broadcast channel carries SSE event lines from the POST handler to the
    // long-lived SSE GET stream.
    let (sse_tx, _) = broadcast::channel::<String>(16);
    let sse_tx = Arc::new(sse_tx);

    let get_tx = Arc::clone(&sse_tx);
    let post_tx = Arc::clone(&sse_tx);

    let mcp_app = Router::new()
        .route(
            "/sse",
            get(move || {
                let tx = Arc::clone(&get_tx);
                async move {
                    let rx = tx.subscribe();

                    // Tell the client where to POST JSON-RPC requests.
                    let endpoint_event =
                        "event: endpoint\ndata: /messages?sessionId=test\n\n".to_string();

                    let first = futures_util::stream::once(async move {
                        Ok::<Bytes, std::convert::Infallible>(Bytes::from(endpoint_event))
                    });

                    let rest = futures_util::stream::unfold(rx, |mut r| async move {
                        match r.recv().await {
                            Ok(msg) => {
                                Some((Ok::<Bytes, std::convert::Infallible>(Bytes::from(msg)), r))
                            }
                            Err(_) => None,
                        }
                    });

                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .header("cache-control", "no-cache")
                        .body(Body::from_stream(first.chain(rest)))
                        .unwrap() // allow:unwrap test-only builder — all args are valid
                }
            }),
        )
        .route(
            "/messages",
            post(move |body: String| {
                let tx = Arc::clone(&post_tx);
                async move {
                    let payload: Value = serde_json::from_str(&body).unwrap_or_default();
                    let method = payload.get("method").and_then(|v| v.as_str()).unwrap_or(""); // allow:unwrap not used — using unwrap_or
                    let id = payload.get("id").and_then(|v| v.as_u64());

                    match (method, id) {
                        ("initialize", Some(req_id)) => {
                            let resp = json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": {
                                    "protocolVersion": "2024-11-05",
                                    "serverInfo": { "name": "test-svc", "version": "0.1.0" },
                                    "capabilities": {}
                                }
                            });
                            let event = format!("event: message\ndata: {}\n\n", resp);
                            let _ = tx.send(event);
                        }
                        ("notifications/initialized", _) => { /* notification — no SSE response */
                        }
                        ("tools/list", Some(req_id)) => {
                            let resp = json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": {
                                    "tools": [{
                                        "name": "test-tool",
                                        "description": "A test tool",
                                        "inputSchema": {
                                            "type": "object",
                                            "properties": {}
                                        }
                                    }]
                                }
                            });
                            let event = format!("event: message\ndata: {}\n\n", resp);
                            let _ = tx.send(event);
                        }
                        _ => {}
                    }

                    axum::http::StatusCode::OK
                }
            }),
        );

    let mcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); // allow:unwrap test-only bind
    let mcp_addr: SocketAddr = mcp_listener.local_addr().unwrap(); // allow:unwrap test-only
    tokio::spawn(axum::serve(mcp_listener, mcp_app).into_future());

    // ---- Broker snapshot stub -----------------------------------------------
    // Points to the MCP SSE stub spun above.
    let snapshot = format!(r#"[{{"kind":"mcp","name":"test-svc","url":"http://{mcp_addr}/sse"}}]"#);

    let broker_app = Router::new().route(
        "/discovery/services",
        get(move || {
            let s = snapshot.clone(); // test-only — one GET expected
            async move { s }
        }),
    );
    let broker_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); // allow:unwrap test-only bind
    let broker_addr: SocketAddr = broker_listener.local_addr().unwrap(); // allow:unwrap test-only
    tokio::spawn(axum::serve(broker_listener, broker_app).into_future());

    let client = Client::ephemeral();
    let broker_url: url::Url = format!("http://{broker_addr}/").parse().unwrap(); // allow:unwrap test-only literal

    let handle = client.discover_broker(broker_url).await;

    // Snapshot entries are in the registry immediately — no wait required.
    let registry = client.registry().lock().await;
    let found = registry.iter().any(|rec| {
        rec.manifest().id().contains("test-svc")
            || rec.capability().to_string().contains("test-svc")
    });
    drop(registry);
    drop(handle);

    assert!(
        found,
        "broker snapshot entry should be in the registry immediately after discover_broker returns"
    );
}
