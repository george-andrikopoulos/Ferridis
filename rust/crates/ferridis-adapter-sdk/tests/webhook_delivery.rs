//! Delivery tests for the webhook `EventPublisher` — the Layer-4 publishing
//! contract: an [`Event`] POSTed as JSON to the subscriber URL, non-2xx
//! surfaced as a typed [`PublishError::BadStatus`].

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use ferridis_adapter_sdk::events::{Event, EventPublisher, PublishError, WebhookPublisher};
use ferridis_core::IntentVerb;
use tokio::sync::Mutex;
use url::Url;

type Received = Arc<Mutex<Vec<serde_json::Value>>>;

async fn spawn(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("serve test receiver");
    });
    addr
}

fn sample_event() -> Event {
    Event {
        ferridis_version: "0.1".to_string(),
        event: IntentVerb::parse("state-changed").expect("valid verb"),
        occurred_at: time::OffsetDateTime::now_utc(),
        payload: serde_json::json!({"entity": "light.kitchen", "state": "on"}),
    }
}

/// The webhook publisher POSTs the event envelope as JSON and the
/// subscriber receives every field intact.
#[tokio::test]
async fn webhook_delivers_event_json_to_subscriber() {
    let received: Received = Arc::new(Mutex::new(Vec::new()));
    let router = Router::new()
        .route(
            "/hook",
            post(
                |State(received): State<Received>, Json(body): Json<serde_json::Value>| async move {
                    received.lock().await.push(body);
                    StatusCode::NO_CONTENT
                },
            ),
        )
        .with_state(Arc::clone(&received));
    let addr = spawn(router).await;

    let url = Url::parse(&format!("http://{addr}/hook")).expect("valid url");
    let publisher = WebhookPublisher::new(url);
    publisher
        .publish(&sample_event())
        .await
        .expect("delivery succeeds");

    let got = received.lock().await;
    assert_eq!(got.len(), 1, "exactly one delivery expected");
    assert_eq!(got[0]["ferridis_version"], "0.1");
    assert_eq!(got[0]["event"], "state-changed");
    assert_eq!(got[0]["payload"]["entity"], "light.kitchen");
    assert_eq!(got[0]["payload"]["state"], "on");
    assert!(got[0]["occurred_at"].is_string() || got[0]["occurred_at"].is_array());
}

/// A subscriber returning a non-2xx status surfaces as the typed
/// `PublishError::BadStatus` — never a silent success.
#[tokio::test]
async fn webhook_surfaces_subscriber_error_status() {
    let router = Router::new().route(
        "/hook",
        post(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
    );
    let addr = spawn(router).await;

    let url = Url::parse(&format!("http://{addr}/hook")).expect("valid url");
    let publisher = WebhookPublisher::new(url);
    let err = publisher
        .publish(&sample_event())
        .await
        .expect_err("500 must surface as an error");
    assert!(
        matches!(err, PublishError::BadStatus(500)),
        "expected BadStatus(500), got {err:?}"
    );
}

/// An unreachable subscriber surfaces as `PublishError::Transport`.
#[tokio::test]
async fn webhook_surfaces_transport_failure() {
    // Bind-then-drop to obtain a port that is closed at publish time.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind probe listener");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);

    let url = Url::parse(&format!("http://{addr}/hook")).expect("valid url");
    let publisher = WebhookPublisher::new(url);
    let err = publisher
        .publish(&sample_event())
        .await
        .expect_err("closed port must surface as an error");
    assert!(
        matches!(err, PublishError::Transport(_)),
        "expected Transport, got {err:?}"
    );
}
