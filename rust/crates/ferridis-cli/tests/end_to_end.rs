//! End-to-end test for the sidecar binary.
//!
//! Spawns the compiled `ferridis-cli` binary, drives it over stdio
//! with real JSON-RPC requests, spins a real [`ferridis-adapter-fs`]
//! on a real port, and asserts that the full path works:
//! VS Code (simulated here) → stdio → `ferridis-cli` → `ferridis-client`
//! → HTTP → `ferridis-adapter-fs` → filesystem.

use std::net::SocketAddr;
use std::process::Stdio;
use std::time::Duration;

use ferridis_adapter_fs::{FilesystemCapability, Root};
use ferridis_adapter_sdk::AdapterServer;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Command;

async fn spin_fs_adapter(dir: &TempDir) -> SocketAddr {
    let root = Root::new(dir.path()).expect("tempdir is absolute existing dir");
    let cap = FilesystemCapability::new(root).expect("default manifest parses");
    let server = AdapterServer::new(cap);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = server.into_router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

fn binary_path() -> std::path::PathBuf {
    // Cargo sets CARGO_BIN_EXE_<bin-name> for integration tests.
    let raw = env!("CARGO_BIN_EXE_ferridis-cli");
    std::path::PathBuf::from(raw)
}

struct Sidecar {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
}

impl Sidecar {
    async fn spawn() -> Self {
        // v0.2 wallet is keychain-backed; tests use the explicit
        // in-memory escape hatch via env var.
        let mut child = Command::new(binary_path())
            .env("FERRIDIS_WALLET_MEMORY", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()) // keep stderr off the test output
            .spawn()
            .expect("spawn ferridis-cli");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();

        let mut response = String::new();
        // The sidecar writes one response per request, line-delimited.
        let read = tokio::time::timeout(
            Duration::from_secs(5),
            self.stdout.read_line(&mut response),
        )
        .await
        .expect("response timeout")
        .expect("response read");
        assert!(read > 0, "sidecar closed stdout unexpectedly");
        serde_json::from_str(&response).expect("response is valid JSON")
    }

    async fn shutdown(mut self) {
        // Drop stdin to signal EOF to the sidecar.
        drop(self.stdin);
        let _ = tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await;
    }
}

#[tokio::test]
async fn list_capabilities_is_empty_on_fresh_wallet() {
    let mut sidecar = Sidecar::spawn().await;

    let resp = sidecar.call(1, "list_capabilities", json!(null)).await;
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"], json!([]));

    sidecar.shutdown().await;
}

#[tokio::test]
async fn full_path_register_dispatch_read_file() {
    let fs_root = TempDir::new().unwrap();
    std::fs::write(fs_root.path().join("hello.txt"), "hello from sidecar").unwrap();

    let addr = spin_fs_adapter(&fs_root).await;
    let mut sidecar = Sidecar::spawn().await;

    // 1. Register.
    let cap = "ferridis://public.ferridis.io/ferridis/fs@v1";
    let resp = sidecar
        .call(
            1,
            "register",
            json!({
                "capability": cap,
                "manifest_url": format!("http://{addr}/manifest.json"),
                "base_url": format!("http://{addr}/"),
            }),
        )
        .await;
    assert_eq!(resp["error"], Value::Null);
    let result = &resp["result"];
    assert_eq!(result["id"], "ferridis.fs.v1");
    assert!(
        result["intents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i == "read-file")
    );

    // 2. Candidates resolves for known intent.
    let resp = sidecar
        .call(2, "candidates_for_intent", json!({"intent": "read-file"}))
        .await;
    assert_eq!(resp["result"].as_array().unwrap().len(), 1);

    // 3. Dispatch read-file.
    let resp = sidecar
        .call(
            3,
            "dispatch",
            json!({
                "capability": cap,
                "intent": "read-file",
                "body": {"path": "hello.txt"},
            }),
        )
        .await;
    assert_eq!(resp["error"], Value::Null);
    assert_eq!(resp["result"]["content"], "hello from sidecar");

    // 4. List capabilities reflects the registration.
    let resp = sidecar.call(4, "list_capabilities", json!(null)).await;
    let list = resp["result"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], "ferridis.fs.v1");

    sidecar.shutdown().await;
}

#[tokio::test]
async fn unknown_method_returns_jsonrpc_error() {
    let mut sidecar = Sidecar::spawn().await;

    let resp = sidecar
        .call(9, "no_such_method", json!(null))
        .await;
    assert_eq!(resp["id"], 9);
    let err = &resp["error"];
    assert_eq!(err["code"], -32601);

    sidecar.shutdown().await;
}

#[tokio::test]
async fn malformed_json_returns_parse_error() {
    let mut sidecar = Sidecar::spawn().await;

    sidecar
        .stdin
        .write_all(b"not json at all\n")
        .await
        .unwrap();
    sidecar.stdin.flush().await.unwrap();

    let mut response = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        sidecar.stdout.read_line(&mut response),
    )
    .await
    .expect("response timeout")
    .expect("response read");
    let parsed: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(parsed["error"]["code"], -32700);

    sidecar.shutdown().await;
}
