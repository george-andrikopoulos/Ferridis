//! Hermetic end-to-end tests for the stdio-to-SSE bridge.
//!
//! A tiny stub "MCP server" (line echo over stdio, plus arg/env
//! announcement and an `exit` trigger) is compiled once per test run with
//! `rustc` — no shell scripts, so the suite runs identically on Windows
//! and Unix. The bridge router is served in-process on an ephemeral port
//! and consumed through `ferridis-protocol`'s own SSE parser.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::OnceLock;

use ferridis_protocol::ProtocolError;
use ferridis_protocol::events::{ServerEvent, subscribe};
use ferridis_stdio_bridge::bridge::{AppState, build_router};
use ferridis_stdio_bridge::types::{Command, SpawnConfig};
use futures_util::StreamExt as _;
use url::Url;

/// Stub child: announces args/env, then echoes stdin lines; a line
/// consisting of `exit` makes it terminate (EOF path for the bridge).
const STUB_SOURCE: &str = r#"
use std::io::{BufRead, Write};
fn main() {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        writeln!(out, "args:{}", args.join(",")).unwrap();
        out.flush().unwrap();
    }
    if let Ok(v) = std::env::var("FERRIDIS_STUB_ANNOUNCE") {
        writeln!(out, "env:{v}").unwrap();
        out.flush().unwrap();
    }
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        if line.trim() == "exit" {
            return;
        }
        writeln!(out, "{line}").unwrap();
        out.flush().unwrap();
    }
}
"#;

fn stub_binary() -> &'static PathBuf {
    static STUB: OnceLock<PathBuf> = OnceLock::new();
    STUB.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        let src = dir.join("ferridis_stub_mcp.rs");
        let exe = dir.join(if cfg!(windows) {
            "ferridis_stub_mcp.exe"
        } else {
            "ferridis_stub_mcp"
        });
        std::fs::write(&src, STUB_SOURCE).expect("write stub source");
        let out = std::process::Command::new("rustc")
            .arg("--edition")
            .arg("2021")
            .arg("-o")
            .arg(&exe)
            .arg(&src)
            .output()
            .expect("rustc must be present alongside cargo");
        assert!(
            out.status.success(),
            "stub compilation failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        exe
    })
}

async fn spawn_bridge(spawn_cfg: SpawnConfig) -> SocketAddr {
    let router = build_router(AppState::new(spawn_cfg));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve bridge");
    });
    addr
}

fn stub_config() -> SpawnConfig {
    let command = Command::new(stub_binary().to_string_lossy()).expect("stub path is non-empty");
    SpawnConfig::new(command)
}

type EventStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<ServerEvent, ProtocolError>>>>;

/// Connect to `/sse`, return the event stream plus the `sessionId`
/// extracted from the mandatory first `endpoint` event.
async fn open_session(addr: SocketAddr) -> (EventStream, String) {
    let client = ferridis_protocol::Client::new();
    let url = Url::parse(&format!("http://{addr}/sse")).expect("valid url");
    let stream = subscribe(&client, &url).await.expect("SSE handshake");
    let mut stream: EventStream = Box::pin(stream);
    let first = stream
        .next()
        .await
        .expect("stream yields an event")
        .expect("first event parses");
    assert_eq!(first.name, "endpoint", "first SSE event must be `endpoint`");
    let session_id = first
        .data
        .as_str()
        .expect("endpoint data is a plain string")
        .split("sessionId=")
        .nth(1)
        .expect("endpoint data carries sessionId")
        .to_string();
    (stream, session_id)
}

/// The handshake event advertises a per-session messages endpoint with a
/// UUID session id.
#[tokio::test]
async fn sse_handshake_advertises_endpoint_with_uuid_session() {
    let addr = spawn_bridge(stub_config()).await;
    let (_stream, session_id) = open_session(addr).await;
    session_id
        .parse::<uuid::Uuid>()
        .expect("sessionId must be a UUID");
}

/// A message POSTed to the session endpoint reaches the child's stdin and
/// the child's stdout line comes back as an SSE `message` event.
#[tokio::test]
async fn message_round_trips_through_the_child() {
    let addr = spawn_bridge(stub_config()).await;
    let (mut stream, session_id) = open_session(addr).await;

    let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/messages?sessionId={session_id}"))
        .body(body)
        .send()
        .await
        .expect("POST /messages");
    assert_eq!(resp.status().as_u16(), 202);

    let event = stream
        .next()
        .await
        .expect("echo arrives")
        .expect("event parses");
    assert_eq!(event.name, "message");
    let expected: serde_json::Value = serde_json::from_str(body).expect("body is JSON");
    assert_eq!(event.data, expected);
}

/// `--arg` and `--env` configuration reaches the spawned child.
#[tokio::test]
async fn spawn_config_args_and_env_reach_the_child() {
    let cfg = stub_config()
        .with_arg("alpha")
        .with_arg("beta")
        .with_env("FERRIDIS_STUB_ANNOUNCE", "hello-env");
    let addr = spawn_bridge(cfg).await;
    let (mut stream, _session_id) = open_session(addr).await;

    let first = stream.next().await.expect("args line").expect("parses");
    assert_eq!(first.data.as_str(), Some("args:alpha,beta"));
    let second = stream.next().await.expect("env line").expect("parses");
    assert_eq!(second.data.as_str(), Some("env:hello-env"));
}

/// POST to a syntactically valid but unknown session returns 404.
#[tokio::test]
async fn post_to_unknown_session_returns_404() {
    let addr = spawn_bridge(stub_config()).await;
    let random = uuid::Uuid::new_v4();
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/messages?sessionId={random}"))
        .body("{}")
        .send()
        .await
        .expect("POST /messages");
    assert_eq!(resp.status().as_u16(), 404);
}

/// POST with a malformed session id returns 400 before touching any child.
#[tokio::test]
async fn post_with_malformed_session_id_returns_400() {
    let addr = spawn_bridge(stub_config()).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/messages?sessionId=not-a-uuid"))
        .body("{}")
        .send()
        .await
        .expect("POST /messages");
    assert_eq!(resp.status().as_u16(), 400);
}

/// When the child exits, the bridge removes the session — subsequent
/// POSTs get 404 instead of writing into a dead process.
#[tokio::test]
async fn child_exit_removes_the_session() {
    let addr = spawn_bridge(stub_config()).await;
    let (_stream, session_id) = open_session(addr).await;
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/messages?sessionId={session_id}");

    let resp = client
        .post(&url)
        .body("exit")
        .send()
        .await
        .expect("POST exit");
    assert_eq!(resp.status().as_u16(), 202);

    // The reader task notices EOF asynchronously; poll until the session
    // disappears (bounded, deterministic exit condition).
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
    loop {
        let status = client
            .post(&url)
            .body("{}")
            .send()
            .await
            .expect("POST after exit")
            .status()
            .as_u16();
        if status == 404 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session was not cleaned up after child exit (last status {status})"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
}

/// A spawn failure (nonexistent binary) surfaces as HTTP 500 on `/sse`,
/// not a hung connection.
#[tokio::test]
async fn spawn_failure_returns_500_on_sse() {
    let command = Command::new("ferridis-definitely-not-a-real-binary-7826")
        .expect("non-empty command string");
    let addr = spawn_bridge(SpawnConfig::new(command)).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/sse"))
        .send()
        .await
        .expect("GET /sse");
    assert_eq!(resp.status().as_u16(), 500);
}
