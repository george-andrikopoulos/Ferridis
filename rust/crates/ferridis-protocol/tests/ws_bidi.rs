//! End-to-end test for the WebSocket bidi transport against a real
//! axum WebSocket server.
//!
//! Verifies:
//! - The upgrade handshake completes.
//! - Client → server `ClientMessage` frames arrive intact.
//! - Server → client `ServerEvent` frames arrive intact.
//! - Two halves of a split connection work concurrently.
//! - `Authorization: Bearer` header propagates on the upgrade.

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use ferridis_protocol::ws::{ClientMessage, ServerEvent, connect_ws};
use futures_util::StreamExt;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use url::Url;

#[derive(Default)]
struct Captured {
    messages: Vec<ClientMessage>,
    bearer: Option<String>,
}

#[tokio::test]
async fn bidi_round_trip_against_real_server() {
    let captured = Arc::new(Mutex::new(Captured::default()));
    let addr = spawn_ws_server(captured.clone()).await;

    let ws_url = Url::parse(&format!("ws://{addr}/events/test")).unwrap();
    let mut conn = connect_ws(&ws_url, Some("test-bearer-abc"))
        .await
        .expect("connect");

    // Client → server.
    conn.send(&ClientMessage::Subscribe {
        channel: "test".into(),
    })
    .await
    .expect("send subscribe");
    conn.send(&ClientMessage::Ack { id: "42".into() })
        .await
        .expect("send ack");

    // Server → client. The test server replies with one event per
    // received client message, echoing the type back.
    let ev1 = conn.next().await.expect("event 1").expect("event 1 ok");
    let ev2 = conn.next().await.expect("event 2").expect("event 2 ok");
    assert_eq!(ev1.name, "echo");
    assert_eq!(ev1.data["echoed"], "subscribe");
    assert_eq!(ev2.name, "echo");
    assert_eq!(ev2.data["echoed"], "ack");

    conn.close().await.expect("close");

    // Server-side captured state.
    let cap = captured.lock().await;
    assert_eq!(cap.bearer.as_deref(), Some("Bearer test-bearer-abc"));
    assert_eq!(cap.messages.len(), 2);
    match &cap.messages[0] {
        ClientMessage::Subscribe { channel } => assert_eq!(channel, "test"),
        other => panic!("expected Subscribe, got {other:?}"),
    }
    match &cap.messages[1] {
        ClientMessage::Ack { id } => assert_eq!(id, "42"),
        other => panic!("expected Ack, got {other:?}"),
    }
}

#[tokio::test]
async fn split_halves_run_concurrently() {
    let captured = Arc::new(Mutex::new(Captured::default()));
    let addr = spawn_ws_server(captured.clone()).await;

    let ws_url = Url::parse(&format!("ws://{addr}/events/split")).unwrap();
    let conn = connect_ws(&ws_url, None).await.expect("connect");
    let (mut sender, mut receiver) = conn.split();

    // Spawn a reader task while we send on the original task. This
    // is the canonical pattern for bidi WebSocket consumers; the
    // type API must support it.
    let reader = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(item) = receiver.next().await {
            events.push(item.expect("event"));
            if events.len() == 3 {
                break;
            }
        }
        events
    });

    for i in 0..3 {
        sender
            .send(&ClientMessage::Control {
                payload: serde_json::json!({"n": i}),
            })
            .await
            .expect("send");
    }
    sender.close().await.expect("close");

    let events = reader.await.expect("reader joins");
    assert_eq!(events.len(), 3);
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev.name, "echo");
        assert_eq!(ev.data["echoed"], "control");
        assert_eq!(ev.data["n"], i);
    }
}

// ---- Test server scaffolding ---------------------------------

async fn spawn_ws_server(captured: Arc<Mutex<Captured>>) -> SocketAddr {
    let app = Router::new()
        .route("/events/:channel", get(ws_handler))
        .layer(middleware::from_fn_with_state(
            captured.clone(),
            capture_bearer,
        ))
        .with_state(captured);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

async fn capture_bearer(
    State(captured): State<Arc<Mutex<Captured>>>,
    req: Request,
    next: Next,
) -> Response {
    if let Some(v) = req.headers().get("authorization")
        && let Ok(s) = v.to_str()
    {
        captured.lock().await.bearer = Some(s.to_string());
    }
    next.run(req).await
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(captured): State<Arc<Mutex<Captured>>>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, captured))
}

async fn handle_socket(mut socket: WebSocket, captured: Arc<Mutex<Captured>>) {
    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            Message::Text(txt) => {
                let parsed: ClientMessage = match serde_json::from_str(&txt) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let echoed = match &parsed {
                    ClientMessage::Subscribe { .. } => "subscribe",
                    ClientMessage::Unsubscribe { .. } => "unsubscribe",
                    ClientMessage::Ack { .. } => "ack",
                    ClientMessage::Control { .. } => "control",
                };
                let n = match &parsed {
                    ClientMessage::Control { payload } => payload.get("n").cloned(),
                    _ => None,
                };
                captured.lock().await.messages.push(parsed);
                let mut data = serde_json::json!({"echoed": echoed});
                if let Some(n) = n {
                    data["n"] = n;
                }
                let event = ServerEvent {
                    name: "echo".into(),
                    data,
                    id: None,
                };
                let json = serde_json::to_string(&event).unwrap();
                if socket.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => continue,
        }
    }
}
