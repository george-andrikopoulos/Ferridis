//! MCP transport abstractions.
//!
//! Two implementations:
//!
//! - [`StdioTransport`] — spawns an MCP server as a child process and
//!   speaks line-delimited JSON-RPC over its stdin/stdout. The shape
//!   most MCP servers use (including our own `ferridis-mcp-server`).
//!
//! - [`SseTransport`] — opens a long-lived `GET /sse` connection,
//!   parses the server's `event: endpoint` to get the per-session
//!   POST URL, then POSTs requests there and reads responses off the
//!   SSE stream. Legacy MCP transport, still used by Home Assistant's
//!   MCP server today.
//!
//! Both expose the same async interface: send a JSON-RPC line, await
//! the response whose `id` matches. Correlation is handled by the
//! transport so the upper-layer [`McpClient`](super::client::McpClient)
//! does not have to know about the wire shape.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};
use url::Url;

use crate::error::ClientError;

/// One JSON-RPC exchange channel for an MCP server connection.
#[async_trait]
pub trait McpTransport: Send + Sync + 'static {
    /// Send a JSON-RPC request (with `id`) and await the matching response.
    async fn request(&self, payload: Value, id: u64) -> Result<Value, ClientError>;

    /// Send a JSON-RPC notification (no response).
    async fn notify(&self, payload: Value) -> Result<(), ClientError>;

    /// Re-establish the underlying transport session.
    ///
    /// Stdio-backed transports are a no-op (the child process owns its
    /// session for life). SSE-backed transports re-handshake to obtain
    /// a fresh `sessionId`, which is the only way to recover after the
    /// upstream MCP server restarts or recycles sessions. The
    /// [`McpClient`](super::client::McpClient) layer calls this after
    /// catching [`ClientError::McpSessionExpired`] and follows with a
    /// fresh `initialize` handshake.
    async fn reconnect(&self) -> Result<(), ClientError> {
        Ok(())
    }
}

/// State shared between the transport's reader task and the request
/// senders that await response correlation by `id`.
#[derive(Default)]
struct Pending {
    by_id: HashMap<u64, oneshot::Sender<Result<Value, ClientError>>>,
}

// =========================================================================
// Stdio transport
// =========================================================================

/// MCP transport over a child process's stdin / stdout.
pub struct StdioTransport {
    stdin: Mutex<tokio::process::ChildStdin>,
    pending: Arc<Mutex<Pending>>,
    _child: tokio::process::Child,
}

impl StdioTransport {
    /// Spawn `command args...` and connect.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<Arc<Self>, ClientError> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| ClientError::WalletIo(format!("spawn MCP server `{command}`: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ClientError::WalletIo("MCP server child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClientError::WalletIo("MCP server child has no stdout".into()))?;
        let stderr = child.stderr.take();

        let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(Pending::default()));

        // Reader task: read lines from stdout, dispatch responses by id.
        {
            let pending = Arc::clone(&pending);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    handle_inbound_line(&line, &pending).await;
                }
            });
        }

        // Drain stderr into tracing so MCP server logs surface.
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "ferridis_client::mcp::stdio", "stderr: {line}");
                }
            });
        }

        Ok(Arc::new(Self {
            stdin: Mutex::new(stdin),
            pending,
            _child: child,
        }))
    }

    async fn write_line(&self, payload: &Value) -> Result<(), ClientError> {
        let mut line = serde_json::to_string(payload).map_err(ClientError::from)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(ClientError::from)?;
        stdin.flush().await.map_err(ClientError::from)?;
        Ok(())
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn request(&self, payload: Value, id: u64) -> Result<Value, ClientError> {
        let (tx, rx) = oneshot::channel();
        {
            let mut p = self.pending.lock().await;
            p.by_id.insert(id, tx);
        }
        self.write_line(&payload).await?;
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => Err(ClientError::WalletIo(
                "MCP server closed before responding".into(),
            )),
            Err(_) => {
                self.pending.lock().await.by_id.remove(&id);
                Err(ClientError::WalletIo(
                    "timed out waiting for MCP response".into(),
                ))
            }
        }
    }

    async fn notify(&self, payload: Value) -> Result<(), ClientError> {
        self.write_line(&payload).await
    }
}

// =========================================================================
// SSE transport
// =========================================================================

/// MCP transport over a Server-Sent Events stream.
///
/// `endpoint_url` is read from the first `event: endpoint` the server
/// sends; subsequent requests are POSTed there as JSON-RPC bodies, and
/// responses are pushed back through the SSE event stream as
/// `event: message` frames.
pub struct SseTransport {
    /// The SSE root URL the transport was opened against. Cached
    /// alongside `http` so [`reconnect`](Self::reconnect) can
    /// re-open without needing the caller to pass it again.
    sse_url: Url,
    http: reqwest::Client,
    /// Shared with each spawned reader task so a `reconnect` can
    /// install a new task that writes to the same slot.
    post_url: Arc<Mutex<Option<Url>>>,
    /// Resolved when the SSE stream's first `endpoint` event arrives.
    endpoint_ready: Arc<tokio::sync::Notify>,
    pending: Arc<Mutex<Pending>>,
}

impl SseTransport {
    /// Open a long-lived SSE connection to `sse_url`.
    ///
    /// Returns once the connection's reader task is spawned. The caller
    /// must `await` [`SseTransport::wait_for_endpoint`] before issuing
    /// any [`request`](Self::request).
    pub async fn open(sse_url: Url) -> Result<Arc<Self>, ClientError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ferridis-client/", env!("CARGO_PKG_VERSION")))
            // No total-request timeout on the SSE stream — it stays
            // open for the lifetime of the connection. We rely on
            // per-request timeouts on the POST side.
            .build()
            .map_err(|e| ClientError::WalletIo(format!("build SSE client: {e}")))?;

        let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(Pending::default()));
        let endpoint_ready = Arc::new(tokio::sync::Notify::new());
        let post_url: Arc<Mutex<Option<Url>>> = Arc::new(Mutex::new(None));

        spawn_sse_reader(
            &http,
            &sse_url,
            Arc::clone(&pending),
            Arc::clone(&post_url),
            Arc::clone(&endpoint_ready),
        )
        .await?;

        let me = Arc::new(Self {
            sse_url,
            http,
            post_url,
            endpoint_ready,
            pending,
        });

        Ok(me)
    }

    /// Wait until the server has sent the `endpoint` event with the
    /// session POST URL. Errors if the connection drops first.
    pub async fn wait_for_endpoint(&self, base: &Url) -> Result<(), ClientError> {
        tokio::time::timeout(Duration::from_secs(10), self.endpoint_ready.notified())
            .await
            .map_err(|_| {
                ClientError::WalletIo(format!(
                    "SSE server at {base} did not send endpoint event within 10s"
                ))
            })?;
        Ok(())
    }

    /// Idempotent endpoint resolution against the SSE origin.
    ///
    /// As of the v0.2 SSE-reconnect work, the reader task resolves
    /// `endpoint:` data against `sse_origin` directly, so the
    /// `post_url` slot is always absolute by the time
    /// [`wait_for_endpoint`](Self::wait_for_endpoint) returns. This
    /// method is retained for backwards compatibility with callers
    /// that previously had to call it; it now verifies the slot is
    /// populated and otherwise does no work.
    pub async fn bind_origin(&self, _sse_url: &Url) -> Result<(), ClientError> {
        let slot = self.post_url.lock().await;
        if slot.is_none() {
            return Err(ClientError::WalletIo("endpoint not yet received".into()));
        }
        Ok(())
    }

    async fn post_url(&self) -> Result<Url, ClientError> {
        self.post_url
            .lock()
            .await
            .clone()
            .ok_or_else(|| ClientError::WalletIo("MCP endpoint not yet received".into()))
    }
}

#[async_trait]
impl McpTransport for SseTransport {
    async fn request(&self, payload: Value, id: u64) -> Result<Value, ClientError> {
        let (tx, rx) = oneshot::channel();
        {
            let mut p = self.pending.lock().await;
            p.by_id.insert(id, tx);
        }
        let url = self.post_url().await?;
        let resp = self
            .http
            .post(url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ClientError::WalletIo(format!("POST MCP request: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            self.pending.lock().await.by_id.remove(&id);
            // 404 means the upstream forgot our session. Surface as a
            // typed error so the McpClient layer can re-handshake and
            // retry. Other non-success codes stay as opaque I/O.
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(ClientError::McpSessionExpired(
                    "POST returned 404 — upstream forgot sessionId".to_string(),
                ));
            }
            return Err(ClientError::WalletIo(format!("MCP POST returned {status}")));
        }
        // Response arrives via SSE; await the oneshot.
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => Err(ClientError::WalletIo(
                "MCP server dropped SSE stream before responding".into(),
            )),
            Err(_) => {
                self.pending.lock().await.by_id.remove(&id);
                Err(ClientError::WalletIo(
                    "timed out waiting for MCP SSE response".into(),
                ))
            }
        }
    }

    async fn notify(&self, payload: Value) -> Result<(), ClientError> {
        let url = self.post_url().await?;
        let resp = self
            .http
            .post(url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ClientError::WalletIo(format!("POST MCP notification: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(ClientError::McpSessionExpired(
                    "notify POST returned 404 — upstream forgot sessionId".to_string(),
                ));
            }
            return Err(ClientError::WalletIo(format!(
                "MCP notification POST returned {status}"
            )));
        }
        Ok(())
    }

    async fn reconnect(&self) -> Result<(), ClientError> {
        // Drain in-flight waiters so they don't block forever — the
        // responses they were waiting on can't arrive (server forgot
        // the session).
        {
            let mut p = self.pending.lock().await;
            for (_, tx) in p.by_id.drain() {
                let _ = tx.send(Err(ClientError::McpSessionExpired(
                    "session re-handshake invalidated in-flight request".into(),
                )));
            }
        }
        spawn_sse_reader(
            &self.http,
            &self.sse_url,
            Arc::clone(&self.pending),
            Arc::clone(&self.post_url),
            Arc::clone(&self.endpoint_ready),
        )
        .await?;
        // Wait for the new `endpoint` event (fresh sessionId).
        tokio::time::timeout(Duration::from_secs(10), self.endpoint_ready.notified())
            .await
            .map_err(|_| {
                ClientError::WalletIo(format!(
                    "SSE reconnect: server at {} did not send endpoint event within 10s",
                    self.sse_url
                ))
            })?;
        self.bind_origin(&self.sse_url.clone()).await?;
        tracing::info!(
            target: "ferridis_client::mcp::sse",
            sse_url = %self.sse_url,
            "SSE session re-handshaked"
        );
        Ok(())
    }
}

/// Open the SSE GET stream and spawn the reader task. Clears the
/// `post_url_slot` first so concurrent requests block on
/// `endpoint_ready` until the new session arrives.
async fn spawn_sse_reader(
    http: &reqwest::Client,
    sse_url: &Url,
    pending: Arc<Mutex<Pending>>,
    post_url_slot: Arc<Mutex<Option<Url>>>,
    endpoint_ready: Arc<tokio::sync::Notify>,
) -> Result<(), ClientError> {
    *post_url_slot.lock().await = None;
    let resp = http
        .get(sse_url.clone())
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .send()
        .await
        .map_err(|e| ClientError::WalletIo(format!("SSE GET: {e}")))?;
    if !resp.status().is_success() {
        return Err(ClientError::WalletIo(format!(
            "SSE GET returned {}",
            resp.status()
        )));
    }
    let sse_origin = sse_url.clone();
    tokio::spawn(async move {
        let mut byte_stream = resp.bytes_stream();
        let mut buf = Vec::<u8>::new();
        while let Some(chunk) = byte_stream.next().await {
            let Ok(bytes) = chunk else { break };
            buf.extend_from_slice(&bytes);
            while let Some(pos) = find_event_boundary(&buf) {
                let event_bytes = buf.drain(..pos.end).collect::<Vec<u8>>();
                let event_text = String::from_utf8_lossy(&event_bytes[..pos.end - 2]).into_owned();
                let (event_name, data) = parse_sse_event(&event_text);
                handle_event_shared(
                    &event_name,
                    &data,
                    &pending,
                    &post_url_slot,
                    &endpoint_ready,
                    &sse_origin,
                )
                .await;
            }
        }
        tracing::debug!(target: "ferridis_client::mcp::sse", "SSE reader task exiting (stream closed)");
    });
    Ok(())
}

/// Reader-task event handler. Stateless aside from the Arc'd handles,
/// so a freshly-spawned reconnect task uses the same routine as the
/// original `open` task. Resolves `endpoint:` events against the SSE
/// origin and signals `endpoint_ready`; routes `message:` events to
/// the matching pending oneshot by `id`.
async fn handle_event_shared(
    event_name: &str,
    data: &str,
    pending: &Mutex<Pending>,
    post_url_slot: &Mutex<Option<Url>>,
    endpoint_ready: &tokio::sync::Notify,
    sse_origin: &Url,
) {
    match event_name {
        "endpoint" => {
            // `data` is typically a relative path like
            // `/messages?sessionId=…`. Resolve against the SSE
            // origin so the POST URL is absolute.
            let resolved = if let Ok(abs) = Url::parse(data) {
                Some(abs)
            } else {
                sse_origin.join(data).ok()
            };
            if let Some(u) = resolved {
                *post_url_slot.lock().await = Some(u);
            }
            endpoint_ready.notify_waiters();
        }
        "message" => {
            if let Ok(value) = serde_json::from_str::<Value>(data)
                && let Some(id) = value.get("id").and_then(|v| v.as_u64())
            {
                let waiter = {
                    let mut p = pending.lock().await;
                    p.by_id.remove(&id)
                };
                if let Some(w) = waiter {
                    let _ = w.send(Ok(value));
                } else {
                    tracing::debug!(target: "ferridis_client::mcp::sse", "sse response for unknown id={id}");
                }
            }
        }
        other => {
            tracing::debug!(target: "ferridis_client::mcp::sse", "unhandled SSE event: {other}");
        }
    }
}

// =========================================================================
// Shared helpers
// =========================================================================

async fn handle_inbound_line(line: &str, pending: &Mutex<Pending>) {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        tracing::debug!(target: "ferridis_client::mcp", "ignoring non-JSON line from MCP server: {line}");
        return;
    };
    let Some(id) = value.get("id").and_then(|v| v.as_u64()) else {
        // Notification from server — not handled in v0.2 first cut.
        return;
    };
    let waiter = {
        let mut p = pending.lock().await;
        p.by_id.remove(&id)
    };
    if let Some(w) = waiter {
        let _ = w.send(Ok(value));
    }
}

/// Position of the first `\n\n` event boundary in `buf`, returned as a
/// range whose `end` is one past the second `\n`. Returns `None` if no
/// complete event is buffered yet.
fn find_event_boundary(buf: &[u8]) -> Option<std::ops::Range<usize>> {
    for (i, w) in buf.windows(2).enumerate() {
        if w == b"\n\n" {
            return Some(0..(i + 2));
        }
    }
    None
}

/// Parse a single SSE event block (without the trailing `\n\n`) into
/// `(event_name, data)`. Multi-line `data:` fields are concatenated
/// with `\n` per the SSE spec.
fn parse_sse_event(block: &str) -> (String, String) {
    let mut event_name = String::from("message");
    let mut data_parts: Vec<&str> = Vec::new();
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_name = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_parts.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
        // Other SSE fields (id, retry, comments) ignored.
    }
    (event_name, data_parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_sse_event() {
        let block = "event: endpoint\ndata: /messages?sessionId=abc";
        let (name, data) = parse_sse_event(block);
        assert_eq!(name, "endpoint");
        assert_eq!(data, "/messages?sessionId=abc");
    }

    #[test]
    fn defaults_to_message_event_name() {
        let block = "data: hello";
        let (name, data) = parse_sse_event(block);
        assert_eq!(name, "message");
        assert_eq!(data, "hello");
    }

    #[test]
    fn finds_event_boundary() {
        let buf = b"event: foo\ndata: bar\n\nevent: baz";
        let range = find_event_boundary(buf).unwrap();
        assert_eq!(&buf[range.clone()], b"event: foo\ndata: bar\n\n");
        assert_eq!(range.end, 22);
    }
}
