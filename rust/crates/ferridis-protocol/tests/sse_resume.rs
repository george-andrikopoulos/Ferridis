//! Resume-behavior e2e for SSE reconnect: a dropped stream reconnected
//! via `subscribe_with_cursor` sends `Last-Event-ID` and the server
//! resumes after the acknowledged event — no replay, no gap.

use std::convert::Infallible;

use axum::Router;
use axum::http::HeaderMap;
use axum::response::sse::{Event, Sse};
use axum::routing::get;
use ferridis_protocol::Client;
use ferridis_protocol::events::{ReconnectCursor, ServerEvent, subscribe_with_cursor};
use futures_util::{StreamExt as _, stream};
use url::Url;

/// Serves two events per connection, starting after the `Last-Event-ID`
/// cursor (or from 1 when absent), then closes the stream — simulating a
/// server-side drop mid-feed.
async fn resumable_events(
    headers: HeaderMap,
) -> Sse<impl stream::Stream<Item = Result<Event, Infallible>>> {
    let start: u64 = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let events = stream::iter((start + 1..=start + 2).map(|n| {
        Ok(Event::default()
            .id(n.to_string())
            .data(format!("{{\"n\":{n}}}")))
    }));
    Sse::new(events)
}

async fn spawn_server() -> Url {
    let router = Router::new().route("/events", get(resumable_events));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve events");
    });
    Url::parse(&format!("http://{addr}/events")).expect("valid url")
}

async fn collect(client: &Client, url: &Url, cursor: Option<&ReconnectCursor>) -> Vec<ServerEvent> {
    let stream = subscribe_with_cursor(client, url, cursor)
        .await
        .expect("SSE handshake");
    let mut stream = Box::pin(stream);
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.expect("well-formed event"));
    }
    events
}

/// Drop mid-feed, reconnect with the last seen id as cursor, and the
/// stream resumes exactly after it.
#[tokio::test]
async fn reconnect_with_cursor_resumes_after_last_seen_event() {
    let url = spawn_server().await;
    let client = Client::new();

    // Initial connection (no cursor): events 1 and 2, then the server
    // closes the stream — the mid-feed drop.
    let first_batch = collect(&client, &url, None).await;
    assert_eq!(first_batch.len(), 2);
    assert_eq!(first_batch[0].id.as_deref(), Some("1"));
    assert_eq!(first_batch[1].id.as_deref(), Some("2"));

    // Reconnect from the last acknowledged id.
    let last_id = first_batch[1].id.as_deref().expect("server sent an id");
    let cursor = ReconnectCursor::new(last_id).expect("valid cursor");
    let resumed = collect(&client, &url, Some(&cursor)).await;

    assert_eq!(resumed.len(), 2);
    assert_eq!(
        resumed[0].id.as_deref(),
        Some("3"),
        "no replay of event 2, no gap"
    );
    assert_eq!(resumed[0].data["n"], 3);
    assert_eq!(resumed[1].id.as_deref(), Some("4"));
}

/// Without a cursor no `Last-Event-ID` header is sent — the server
/// starts from the beginning.
#[tokio::test]
async fn initial_connection_without_cursor_starts_from_the_beginning() {
    let url = spawn_server().await;
    let client = Client::new();
    let events = collect(&client, &url, None).await;
    assert_eq!(events.first().and_then(|e| e.id.as_deref()), Some("1"));
}
