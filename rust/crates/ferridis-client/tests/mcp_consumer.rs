//! MCP-consumer roundtrip self-test.
//!
//! Spawns the project's own `ferridis-mcp-server` binary (which is an
//! MCP publisher) and lets `ferridis-client` consume it through the
//! new MCP-consumer path. End to end, the picture is:
//!
//! ```text
//! ferridis-client (this test, consumer side)
//!     ↓ stdio MCP, handshake + tools/list + tools/call
//! ferridis-mcp-server (real binary, publisher side)
//!     ↓ ferridis-client (internal to the server)
//!     ↓ HTTP /intents/...
//! ferridis-adapter-fs (real adapter on 127.0.0.1)
//! ```
//!
//! Both halves of the Ferridis-↔-MCP interop shim talking to each
//! other proves they are wire-compatible. If this passes, an
//! independent MCP server (like Home Assistant's) is just a different
//! transport away.

use std::net::SocketAddr;

use ferridis_adapter_fs::{FilesystemCapability, Root};
use ferridis_adapter_sdk::AdapterServer;
use ferridis_client::Client;
use ferridis_core::IntentVerb;
use tempfile::TempDir;
use tokio::net::TcpListener;

async fn spin_fs_adapter(dir: &TempDir) -> SocketAddr {
    let root = Root::new(dir.path()).unwrap();
    let cap = FilesystemCapability::new(root).unwrap();
    let server = AdapterServer::new(cap);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = server.into_router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

fn mcp_server_binary() -> std::path::PathBuf {
    // `CARGO_BIN_EXE_*` is only set for bins of the crate under test, so
    // the cross-crate binary must be located by hand. Look in the same
    // target profile directory as this test executable (which honors
    // `CARGO_TARGET_DIR` and the platform's exe suffix), then in the
    // sibling `release` profile, then in the workspace-relative
    // `target/release` as a last resort.
    let name = format!("ferridis-mcp-server{}", std::env::consts::EXE_SUFFIX);
    if let Ok(mut profile_dir) = std::env::current_exe() {
        profile_dir.pop(); // deps
        profile_dir.pop(); // debug | release
        let candidate = profile_dir.join(&name);
        if candidate.exists() {
            return candidate;
        }
        if let Some(target_root) = profile_dir.parent() {
            let release = target_root.join("release").join(&name);
            if release.exists() {
                return release;
            }
        }
    }
    let p = std::env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .expect("CARGO_MANIFEST_DIR");
    // crates/ferridis-client → ../../target/release/<name>
    p.parent()
        .expect("crates dir")
        .parent()
        .expect("workspace root")
        .join("target")
        .join("release")
        .join(name)
}

#[tokio::test]
async fn consumes_our_own_mcp_server_over_stdio_end_to_end() {
    let work = TempDir::new().unwrap();
    let fs_root = TempDir::new().unwrap();
    std::fs::write(fs_root.path().join("hello.txt"), "hello via mcp roundtrip").unwrap();

    // 1. Stand up a Ferridis filesystem adapter on a real port.
    let addr = spin_fs_adapter(&fs_root).await;

    // 2. Write an adapters config for the MCP server to publish.
    let adapters_path = work.path().join("adapters.json");
    let adapters = serde_json::json!([{
        "capability": "ferridis://public.ferridis.io/ferridis/fs@v1",
        "manifestUrl": format!("http://{addr}/manifest.json"),
        "baseUrl": format!("http://{addr}/"),
    }]);
    std::fs::write(
        &adapters_path,
        serde_json::to_vec_pretty(&adapters).unwrap(),
    )
    .unwrap();

    // 3. Pre-condition: the mcp-server binary must exist.
    let binary = mcp_server_binary();
    if !binary.exists() {
        panic!(
            "test requires the ferridis-mcp-server binary (looked at {}) — run `cargo build -p ferridis-mcp-server` first",
            binary.display()
        );
    }

    // 4. Build the Ferridis client. Register the MCP server over stdio.
    //    v0.2 keychain refactor: ferridis-mcp-server takes no --wallet
    //    flag; tests opt into in-memory wallet storage via the env var.
    let client = Client::ephemeral();
    let args = vec![
        "--adapters-config".to_string(),
        adapters_path.to_string_lossy().into_owned(),
    ];
    let env = vec![("FERRIDIS_WALLET_MEMORY".to_string(), "1".to_string())];
    let capability = client
        .register_mcp_stdio(&binary.to_string_lossy(), &args, &env)
        .await
        .expect("register MCP server over stdio");

    // 5. The MCP server should report at least one tool (read-file).
    let read = IntentVerb::parse("ferridis-ferridis-fs-v1-read-file")
        .expect("projected intent verb parses");
    let candidates = client.candidates_for_intent(&read).await;
    assert!(
        candidates.contains(&capability),
        "expected the registered MCP capability to back the projected intent. cap={capability}, intent={read}, candidates={candidates:?}",
    );

    // 6. Dispatch through Ferridis — routes via the MCP backend, calls
    //    `tools/call ferridis_ferridis_fs_v1_read_file` under the hood,
    //    which the publisher routes to the filesystem adapter.
    let resp = client
        .dispatch(&capability, read, serde_json::json!({"path": "hello.txt"}))
        .await
        .expect("dispatch through MCP backend succeeds");

    // 7. The publisher wraps the dispatch result in MCP's tools/call
    //    text content; the consumer unwraps to JSON.
    assert_eq!(resp["content"], "hello via mcp roundtrip");
    assert_eq!(resp["path"], "hello.txt");
}
