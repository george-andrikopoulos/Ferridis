//! `ferridis-cli` — sidecar binary bridging non-Rust editor hosts to
//! [`ferridis-client`](../ferridis_client/index.html).
//!
//! # Wire protocol
//!
//! The sidecar speaks [JSON-RPC 2.0](https://www.jsonrpc.org/specification)
//! over line-delimited stdin/stdout. Each line is one JSON object.
//! Logs go to stderr so they do not interfere with the protocol channel.
//!
//! # Methods
//!
//! - `register({capability, manifest_url, base_url})` — fetch a manifest
//!   and register it with the personal-tier registry. Returns a flat
//!   summary of the validated manifest.
//! - `candidates_for_intent({intent})` — list capability references
//!   whose registered manifest declares that intent.
//! - `dispatch({capability, intent, body})` — invoke an intent against
//!   a registered capability. Returns the parsed JSON response.
//! - `insert_connection(<StoredConnection JSON>)` — add or replace a
//!   connection in the wallet; persists if the wallet is on-disk.
//! - `list_capabilities()` — full inspection of the personal registry.
//!
//! # Wallet
//!
//! v0.2: connections live in the OS keychain via `ferridis-client`'s
//! `KeychainStore` — no plaintext file anywhere. The keychain `service`
//! field defaults to `"ferridis"` and can be overridden with
//! `--keychain-namespace <name>` (or env `FERRIDIS_KEYCHAIN_NAMESPACE`).
//! If the OS keychain is not reachable the binary refuses to start —
//! there is no fallback to disk.
//!
//! For tests and embedded hosts that intentionally manage their own
//! persistence, set `FERRIDIS_WALLET_MEMORY=1` to use an ephemeral
//! in-memory wallet. **Production deployments must not set this.**
//!
//! # Migration from v0.1
//!
//! If a legacy `wallet.json` is found at the conventional path
//! (`$HOME/.ferridis/wallet.json`), the binary refuses to start and
//! prints instructions to migrate-then-delete. v0.2 does not migrate
//! plaintext wallets automatically — see the `ferridis-client` wallet
//! docs.

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Same rationale as `ferridis-client`: ClientError carries rich
// context and we don't want to box it everywhere just to hush clippy.
#![allow(clippy::result_large_err)]

mod rpc;

use std::sync::Arc;

use ferridis_client::{Client, ClientError, Wallet};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tracing::{error, info};

use crate::rpc::{JsonRpcRequest, JsonRpcResponse, codes, dispatch};

#[tokio::main]
async fn main() {
    init_tracing();

    let namespace = resolve_keychain_namespace();
    let use_memory = std::env::var("FERRIDIS_WALLET_MEMORY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    info!(namespace = %namespace, use_memory, "ferridis-cli starting");

    if !use_memory && let Err(e) = check_legacy_wallet() {
        error!(error = %e, "legacy plaintext wallet detected");
        std::process::exit(1);
    }

    let client = if use_memory {
        info!("FERRIDIS_WALLET_MEMORY=1 \u{2014} ephemeral in-memory wallet (TEST USE ONLY)");
        Arc::new(Client::ephemeral())
    } else {
        match Client::open_keychain(&namespace) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                error!(error = %e, "failed to open keychain-backed wallet");
                std::process::exit(1);
            }
        }
    };

    serve(client).await;
}

fn init_tracing() {
    // Logs to stderr so they do not corrupt the JSON-RPC channel on stdout.
    let filter = std::env::var("FERRIDIS_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "ferridis_cli=info".to_string());
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .try_init();
}

fn resolve_keychain_namespace() -> String {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--keychain-namespace"
            && let Some(v) = args.next()
        {
            return v;
        }
    }
    if let Ok(v) = std::env::var("FERRIDIS_KEYCHAIN_NAMESPACE") {
        return v;
    }
    "ferridis".to_string()
}

/// Detect legacy v0.1 plaintext wallet at `$HOME/.ferridis/wallet.json`
/// and refuse to start if found. The v0.2 keychain-only storage is
/// non-negotiable; users with a plaintext wallet must migrate manually.
fn check_legacy_wallet() -> Result<(), ClientError> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let legacy = format!("{home}/.ferridis/wallet.json");
    Wallet::detect_legacy_plaintext(&legacy)
}

async fn serve(client: Arc<Client>) {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let mut writer = BufWriter::new(stdout);

    loop {
        let line = match reader.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => {
                info!("stdin closed, exiting");
                break;
            }
            Err(e) => {
                error!(error = %e, "stdin read failed");
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(req) => dispatch(&client, req).await,
            Err(e) => JsonRpcResponse::error(
                serde_json::Value::Null,
                codes::PARSE_ERROR,
                format!("parse error: {e}"),
            ),
        };

        let response_str = match serde_json::to_string(&response) {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "failed to serialize response");
                continue;
            }
        };

        if let Err(e) = writer.write_all(response_str.as_bytes()).await {
            error!(error = %e, "stdout write failed");
            break;
        }
        if let Err(e) = writer.write_all(b"\n").await {
            error!(error = %e, "stdout newline write failed");
            break;
        }
        if let Err(e) = writer.flush().await {
            error!(error = %e, "stdout flush failed");
            break;
        }
    }
}
