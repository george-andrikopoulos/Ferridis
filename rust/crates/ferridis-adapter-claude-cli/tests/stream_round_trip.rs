//! End-to-end test for the Claude Code CLI adapter.
//!
//! Two tracks:
//!
//! 1. **Hermetic**: spawns the adapter against a tiny shell script
//!    that mimics `claude --output-format stream-json` by emitting a
//!    canned sequence of JSON lines on stdout. Asserts the SSE
//!    stream from the adapter produces one chunk per line in order
//!    and terminates cleanly.
//!
//! 2. **Live** (`#[ignore]`'d): spawns the adapter against a real
//!    `claude` install. Costs API tokens; run on demand with
//!    `cargo test -p ferridis-adapter-claude-cli -- --ignored`.

use std::net::SocketAddr;
use std::path::PathBuf;

use ferridis_adapter_claude_cli::{
    AllowedRoots, ClaudeCliCapability, ClaudeCliConfig, ModelAllowList,
};
use ferridis_adapter_sdk::AdapterServer;
use futures_util::StreamExt;
use tokio::net::TcpListener;

#[tokio::test]
async fn hermetic_round_trip_through_stub_claude() {
    let stub = write_claude_stub();
    let cfg = ClaudeCliConfig::new()
        .with_binary(stub.path())
        .with_allowed_cwds(AllowedRoots::empty())
        .with_allowed_models(ModelAllowList::standard());
    let addr = spawn(cfg).await;

    // Drive a streaming dispatch by hand (no client crate needed —
    // we want to keep this test inside the adapter crate's scope).
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/intents/submit-prompt"))
        .header("accept", "text/event-stream")
        .json(&serde_json::json!({
            "prompt": "doesn't matter — stub doesn't read it"
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/event-stream"),
        "adapter must respond with SSE"
    );

    let chunks = parse_sse_chunks(resp.bytes_stream()).await;
    // The stub emits five JSON lines: init, assistant (1st token),
    // assistant (2nd token), result, session_state idle.
    let chunk_events: Vec<&serde_json::Value> = chunks
        .iter()
        .filter(|(name, _)| name == "chunk")
        .map(|(_, v)| v)
        .collect();
    assert_eq!(chunk_events.len(), 5, "got chunks: {chunk_events:#?}");
    assert_eq!(chunk_events[0]["type"], "system");
    assert_eq!(chunk_events[0]["subtype"], "init");
    assert_eq!(chunk_events[3]["type"], "result");
    assert_eq!(chunk_events[3]["result"], "two tokens");
    // The SDK appends a final `end` event after stream exhaustion.
    assert!(
        chunks.iter().any(|(name, _)| name == "end"),
        "missing end event: {chunks:#?}"
    );
}

#[tokio::test]
async fn hermetic_rejects_undeclared_intent() {
    let stub = write_claude_stub();
    let cfg = ClaudeCliConfig::new().with_binary(stub.path());
    let addr = spawn(cfg).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/intents/something-fake"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn hermetic_rejects_disallowed_cwd() {
    let stub = write_claude_stub();
    // No allowed roots → every cwd is rejected.
    let cfg = ClaudeCliConfig::new()
        .with_binary(stub.path())
        .with_allowed_cwds(AllowedRoots::empty());
    let addr = spawn(cfg).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/intents/submit-prompt"))
        .header("accept", "text/event-stream")
        .json(&serde_json::json!({
            "prompt": "hi",
            "cwd": "/tmp"
        }))
        .send()
        .await
        .expect("post");
    // The SDK funnels DispatchError::InvalidRequest into the SSE
    // body as one `error` event followed by `end`. We get a 200 +
    // SSE stream (the upgrade already happened) but the only chunk
    // is the error.
    assert_eq!(resp.status(), 200);
    let chunks = parse_sse_chunks(resp.bytes_stream()).await;
    assert!(
        chunks.iter().any(|(name, _)| name == "error"),
        "expected an error chunk, got: {chunks:#?}"
    );
    assert!(chunks.iter().any(|(name, _)| name == "end"));
}

#[tokio::test]
#[ignore = "spawns a real `claude` and costs API tokens"]
async fn live_round_trip_against_real_claude() {
    let cwd = std::env::current_dir().unwrap();
    let allowed = AllowedRoots::empty().with_root(&cwd).unwrap();
    let cfg = ClaudeCliConfig::new()
        .with_allowed_cwds(allowed.clone())
        .with_default_cwd(cwd);
    let addr = spawn(cfg).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/intents/submit-prompt"))
        .header("accept", "text/event-stream")
        // Ask for a tiny deterministic-ish response to keep cost low.
        .json(&serde_json::json!({
            "prompt": "Reply with exactly the word PONG and nothing else.",
            "model": "haiku"
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200);

    let chunks = parse_sse_chunks(resp.bytes_stream()).await;
    let saw_result = chunks
        .iter()
        .any(|(name, v)| name == "chunk" && v["type"] == "result");
    let saw_end = chunks.iter().any(|(name, _)| name == "end");
    assert!(saw_result, "no result chunk: {chunks:#?}");
    assert!(saw_end, "no end event");
}

// ---- helpers ---------------------------------------------------

async fn spawn(cfg: ClaudeCliConfig) -> SocketAddr {
    let server = AdapterServer::new(ClaudeCliCapability::new(cfg));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = server.into_router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct StubScript {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl StubScript {
    fn path(&self) -> &PathBuf {
        &self.path
    }
}

fn write_claude_stub() -> StubScript {
    // Mimic the first/last events the real `claude --output-format
    // stream-json --verbose` emits, with two assistant tokens and a
    // final result.
    let lines = r##"{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000001","model":"stub"}
{"type":"assistant","message":{"content":[{"type":"text","text":"two"}]},"session_id":"00000000-0000-4000-8000-000000000001"}
{"type":"assistant","message":{"content":[{"type":"text","text":" tokens"}]},"session_id":"00000000-0000-4000-8000-000000000001"}
{"type":"result","subtype":"success","is_error":false,"result":"two tokens","session_id":"00000000-0000-4000-8000-000000000001"}
{"type":"system","subtype":"session_state_changed","state":"idle","session_id":"00000000-0000-4000-8000-000000000001"}"##;

    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("claude-stub.sh");
    let script = format!("#!/usr/bin/env bash\nset -e\ncat <<'EOF'\n{lines}\nEOF\n");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    StubScript { _dir: dir, path }
}

/// Minimal SSE parser tailored to the adapter SDK's wire format —
/// `event: <name>\ndata: <json-or-text>\n\n` blocks. Returns
/// `(event_name, parsed_data)` pairs in arrival order.
async fn parse_sse_chunks<S>(stream: S) -> Vec<(String, serde_json::Value)>
where
    S: futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin,
{
    let mut out = Vec::new();
    let mut buf = Vec::new();
    let mut s = Box::pin(stream);
    while let Some(chunk) = s.next().await {
        let chunk = chunk.expect("network");
        buf.extend_from_slice(&chunk);

        // Drain any complete event blocks (terminated by blank line).
        while let Some(idx) = find_double_newline(&buf) {
            let raw: Vec<u8> = buf.drain(..idx + 2).collect();
            // Strip the trailing blank line.
            let block =
                std::str::from_utf8(&raw[..raw.len() - 2]).expect("SSE block must be UTF-8");
            let mut name = "message".to_string();
            let mut data_parts: Vec<&str> = Vec::new();
            for line in block.split('\n') {
                if let Some(rest) = line.strip_prefix("event:") {
                    name = rest.trim().to_string();
                } else if let Some(rest) = line.strip_prefix("data:") {
                    data_parts.push(rest.trim_start());
                }
            }
            let data_str = data_parts.join("\n");
            let value: serde_json::Value = match serde_json::from_str(&data_str) {
                Ok(v) => v,
                Err(_) => serde_json::Value::String(data_str),
            };
            out.push((name, value));
        }
    }
    out
}

fn find_double_newline(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len().saturating_sub(1) {
        if &buf[i..i + 2] == b"\n\n" {
            return Some(i);
        }
    }
    None
}
