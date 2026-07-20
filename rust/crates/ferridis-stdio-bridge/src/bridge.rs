use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use bytes::Bytes;
use futures_util::{StreamExt as _, stream};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::types::{SessionId, SpawnConfig};

// ── Session handle ─────────────────────────────────────────────────────────

struct SessionHandle {
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    _child: tokio::process::Child,
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        // Send kill signal synchronously — no async available in Drop.
        let _ = self._child.start_kill();
    }
}

// ── Session store ──────────────────────────────────────────────────────────

/// Shared map of live sessions, keyed by [`SessionId`].
///
/// Each entry owns the child process handle; removing an entry drops the
/// handle, which sends the kill signal to the child.
#[derive(Clone, Default)]
pub struct SessionStore(Arc<Mutex<HashMap<SessionId, SessionHandle>>>);

impl SessionStore {
    async fn insert(&self, id: SessionId, handle: SessionHandle) {
        self.0.lock().await.insert(id, handle);
    }

    /// Clone the stdin Arc for a session without holding the store lock.
    async fn borrow_stdin(&self, id: SessionId) -> Option<Arc<Mutex<tokio::process::ChildStdin>>> {
        self.0.lock().await.get(&id).map(|h| Arc::clone(&h.stdin))
    }

    async fn remove(&self, id: SessionId) {
        self.0.lock().await.remove(&id);
    }
}

// ── App state ─────────────────────────────────────────────────────────────

/// Router state: the session store plus the spawn configuration cloned
/// for every new SSE session.
#[derive(Clone)]
pub struct AppState {
    sessions: SessionStore,
    spawn_cfg: SpawnConfig,
}

impl AppState {
    /// Build fresh state around a validated [`SpawnConfig`].
    pub fn new(spawn_cfg: SpawnConfig) -> Self {
        Self {
            sessions: SessionStore::default(),
            spawn_cfg,
        }
    }
}

// ── Router ────────────────────────────────────────────────────────────────

/// Build the bridge's axum router: `GET /sse` (session-per-connection)
/// and `POST /messages?sessionId=<id>`.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/sse", get(get_sse))
        .route("/messages", post(post_message))
        .with_state(state)
}

// ── Route: GET /sse ───────────────────────────────────────────────────────

async fn get_sse(
    State(state): State<AppState>,
) -> Result<Sse<impl stream::Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    let session_id = SessionId::new();

    let spawned = state.spawn_cfg.spawn().map_err(|e| {
        tracing::error!(error = %e, "failed to spawn MCP child");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let (child, stdin, stdout, stderr) = spawned.into_parts();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let stdin = Arc::new(Mutex::new(stdin));

    state
        .sessions
        .insert(
            session_id,
            SessionHandle {
                stdin: Arc::clone(&stdin),
                _child: child,
            },
        )
        .await;

    // Stdout reader task: forward child lines to the SSE channel.
    {
        let sessions = state.sessions.clone(); // clone: SessionStore is Arc-backed — cheap pointer copy for the reader task
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if events_tx.send(line).is_err() {
                    break; // SSE client disconnected
                }
            }
            sessions.remove(session_id).await;
            tracing::debug!(%session_id, "session closed");
        });
    }

    // Drain stderr to tracing so MCP server logs surface.
    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "ferridis_stdio_bridge::child", %session_id, "{line}");
            }
        });
    }

    let endpoint = format!("/messages?sessionId={session_id}");
    let first = stream::once(async move {
        Ok::<Event, Infallible>(Event::default().event("endpoint").data(endpoint))
    });
    // events_rx is moved into the closure (captured once); poll_recv borrows &mut self on each call.
    let rest = stream::poll_fn(move |cx| {
        events_rx.poll_recv(cx).map(|opt| {
            opt.map(|line| Ok::<Event, Infallible>(Event::default().event("message").data(line)))
        })
    });

    Ok(Sse::new(first.chain(rest)))
}

// ── Route: POST /messages ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct MessageParams {
    #[serde(rename = "sessionId")]
    session_id: String,
}

async fn post_message(
    State(state): State<AppState>,
    Query(params): Query<MessageParams>,
    body: Bytes,
) -> StatusCode {
    let uuid = match params.session_id.parse::<uuid::Uuid>() {
        Ok(u) => u,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    let session_id = SessionId::from_raw(uuid);

    let Some(stdin) = state.sessions.borrow_stdin(session_id).await else {
        return StatusCode::NOT_FOUND;
    };

    let mut line = body.to_vec();
    line.push(b'\n');

    let mut guard = stdin.lock().await;
    if guard.write_all(&line).await.is_err() || guard.flush().await.is_err() {
        return StatusCode::BAD_GATEWAY;
    }

    StatusCode::ACCEPTED
}
