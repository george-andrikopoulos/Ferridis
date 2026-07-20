//! End-to-end OAuth 2.1 dance against a real `ferridis-mcp-server
//! --http-bind --oauth-public-base-url` instance, then `tools/call`
//! against a stream-kind capability with the issued access token.
//!
//! This is the claude.ai Custom Connector flow, modelled exactly:
//!
//! 1. GET `/.well-known/oauth-authorization-server` — discovery
//! 2. POST `/register`                              — DCR
//! 3. GET `/authorize?...&code_challenge=...`       — auto-approve, 302 with code
//! 4. POST `/token`                                 — PKCE verify, returns access token
//! 5. POST `/mcp` with `Authorization: Bearer <access_token>`  — works
//! 6. POST `/mcp` with no token / wrong token       — 401
//!
//! Plus two negative paths:
//! - `--http-bind` with no `--bearer-token` and no `--oauth-public-base-url`
//!   refuses to start (load-bearing safety promise).
//! - Code is single-use; second `/token` call with the same code 400s.

use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use ferridis_adapter_sdk::{AdapterServer, Capability, DispatchError, IntentStream, SchemaSource};
use ferridis_core::{IntentVerb, Manifest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::process::{Child, Command};

const MANIFEST: &str = r##"{
    "ferridis_version": "0.3",
    "id": "test.oauthy.v1",
    "name": "oauthy",
    "category": "test",
    "summary": "stream-kind test capability for the OAuth http_transport test.",
    "intents": [
        {"verb": "say-hi", "kind": "stream", "chunk_schema_url": "https://x/chunk.json"}
    ],
    "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
    "tiers": ["native"],
    "auth": { "type": "none" }
}"##;

struct Oauthy {
    manifest: Manifest,
}

#[async_trait]
impl Capability for Oauthy {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn schema(&self) -> SchemaSource {
        SchemaSource::Embedded {
            content_type: "application/yaml".into(),
            body: "openapi: 3.0.0\n".into(),
        }
    }
    async fn dispatch(&self, intent: &IntentVerb, _body: Value) -> Result<Value, DispatchError> {
        Err(DispatchError::UnsupportedIntent(intent.clone()))
    }
    async fn dispatch_stream(
        &self,
        _intent: &IntentVerb,
        _body: Value,
    ) -> Result<IntentStream, DispatchError> {
        let s = async_stream::stream! {
            yield Ok(json!({"type":"system","subtype":"init"}));
            yield Ok(json!({
                "type":"result","subtype":"success","result":"OAUTHY-PONG"
            }));
        };
        Ok(Box::pin(s))
    }
}

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_ferridis-mcp-server"))
}

async fn spin_adapter() -> SocketAddr {
    let cap = Oauthy {
        manifest: Manifest::parse(MANIFEST).expect("manifest parses"),
    };
    let server = AdapterServer::new(cap);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = server.into_router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

fn ephemeral_port() -> u16 {
    let l = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

fn write_adapters_config(dir: &TempDir, adapter_addr: SocketAddr) -> std::path::PathBuf {
    let path = dir.path().join("adapters.json");
    let body = json!([{
        "capability": "ferridis://personal.test/oauthy/v1@v1",
        "manifestUrl": format!("http://{adapter_addr}/manifest.json"),
        "baseUrl":     format!("http://{adapter_addr}/"),
    }]);
    std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap()).unwrap();
    path
}

async fn spawn_publisher_oauth(
    cfg_path: &std::path::Path,
    bind: SocketAddr,
    issuer: &str,
) -> Child {
    Command::new(binary_path())
        .arg("--adapters-config")
        .arg(cfg_path)
        .arg("--http-bind")
        .arg(bind.to_string())
        .arg("--oauth-public-base-url")
        .arg(issuer)
        .env("FERRIDIS_WALLET_MEMORY", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn ferridis-mcp-server --http-bind --oauth-public-base-url")
}

async fn wait_until_listening(bind: SocketAddr) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if tokio::net::TcpStream::connect(bind).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("publisher never bound {bind}");
}

/// PKCE S256 pair. The verifier is a 64-byte base64url-encoded
/// random buffer (well within RFC 7636's 43..=128 range); the
/// challenge is the base64url-no-pad SHA-256 of the verifier bytes.
fn pkce_pair() -> (String, String) {
    use rand::Rng;
    let mut buf = [0u8; 48];
    rand::rngs::OsRng.fill(&mut buf[..]);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

#[tokio::test]
async fn oauth_full_dance_and_mcp_call() {
    let work = TempDir::new().unwrap();
    let adapter_addr = spin_adapter().await;
    let cfg_path = write_adapters_config(&work, adapter_addr);

    let port = ephemeral_port();
    let bind: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let issuer = format!("http://127.0.0.1:{port}/");
    let mut child = spawn_publisher_oauth(&cfg_path, bind, &issuer).await;
    wait_until_listening(bind).await;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // ---- 1. discovery ----
    let md: Value = client
        .get(format!(
            "http://{bind}/.well-known/oauth-authorization-server"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(md["issuer"], issuer.trim_end_matches('/'));
    assert!(
        md["code_challenge_methods_supported"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "S256"),
        "S256 must be advertised"
    );
    let authorize_endpoint = md["authorization_endpoint"].as_str().unwrap().to_string();
    let token_endpoint = md["token_endpoint"].as_str().unwrap().to_string();
    let registration_endpoint = md["registration_endpoint"].as_str().unwrap().to_string();

    // ---- 2. dynamic client registration ----
    let reg: Value = client
        .post(&registration_endpoint)
        .json(&json!({
            "redirect_uris": ["https://claude.ai/api/mcp/auth_callback"],
            "client_name": "claude.ai test",
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let client_id = reg["client_id"].as_str().unwrap().to_string();
    assert!(client_id.starts_with("client-"));

    // ---- 3. authorize → 302 with ?code= & ?state= ----
    let (verifier, challenge) = pkce_pair();
    let state_param = "opaque-state-xyz";
    let auth_resp = client
        .get(&authorize_endpoint)
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state_param),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        auth_resp.status(),
        reqwest::StatusCode::SEE_OTHER,
        "authorize must 303-redirect on auto-approve, got: {:?}",
        auth_resp.status()
    );
    let location = auth_resp
        .headers()
        .get("location")
        .expect("Location header")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        location.starts_with("https://claude.ai/api/mcp/auth_callback?"),
        "redirect must point at claude.ai callback, got: {location}"
    );
    let parsed = url::Url::parse(&location).unwrap();
    let mut q = std::collections::HashMap::new();
    for (k, v) in parsed.query_pairs() {
        q.insert(k.into_owned(), v.into_owned());
    }
    let code = q.get("code").expect("code in redirect").clone();
    assert_eq!(q.get("state").map(String::as_str), Some(state_param));

    // ---- 4. token exchange (PKCE verified) ----
    let tok: Value = client
        .post(&token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("client_id", client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let access = tok["access_token"].as_str().unwrap().to_string();
    assert_eq!(tok["token_type"], "Bearer");

    // ---- 5. /mcp with the issued OAuth token ----
    // initialize
    let init: Value = client
        .post(format!("http://{bind}/mcp"))
        .header("authorization", format!("Bearer {access}"))
        .header("content-type", "application/json")
        .json(&json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "ferridis");

    // tools/list
    let listed: Value = client
        .post(format!("http://{bind}/mcp"))
        .header("authorization", format!("Bearer {access}"))
        .header("content-type", "application/json")
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"ferridis_test_oauthy_v1_say_hi"),
        "stream-kind intent missing: {names:?}"
    );

    // tools/call (stream-kind → collected)
    let called: Value = client
        .post(format!("http://{bind}/mcp"))
        .header("authorization", format!("Bearer {access}"))
        .header("content-type", "application/json")
        .json(&json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"ferridis_test_oauthy_v1_say_hi","arguments":{}}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = called["result"]["content"][0]["text"]
        .as_str()
        .expect("content text");
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["result"], "OAUTHY-PONG");

    // ---- 6. wrong token → 401 on /mcp ----
    let resp = client
        .post(format!("http://{bind}/mcp"))
        .header("authorization", "Bearer wrong-token")
        .header("content-type", "application/json")
        .json(&json!({"jsonrpc":"2.0","id":4,"method":"ping"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // ---- 7. code is single-use ----
    let resp = client
        .post(&token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("client_id", client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let err: Value = resp.json().await.unwrap();
    assert_eq!(err["error"], "invalid_grant");

    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
}

#[tokio::test]
async fn pkce_mismatch_on_token_exchange_400s() {
    let work = TempDir::new().unwrap();
    let adapter_addr = spin_adapter().await;
    let cfg_path = write_adapters_config(&work, adapter_addr);

    let port = ephemeral_port();
    let bind: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let issuer = format!("http://127.0.0.1:{port}/");
    let mut child = spawn_publisher_oauth(&cfg_path, bind, &issuer).await;
    wait_until_listening(bind).await;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let reg: Value = client
        .post(format!("http://{bind}/register"))
        .json(&json!({
            "redirect_uris": ["https://claude.ai/api/mcp/auth_callback"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let client_id = reg["client_id"].as_str().unwrap().to_string();

    let (_correct_verifier, challenge) = pkce_pair();
    let auth = client
        .get(format!("http://{bind}/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .unwrap();
    let location = auth
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let parsed = url::Url::parse(&location).unwrap();
    let code = parsed
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .unwrap();

    // Pose with a wrong (but still well-formed-length) verifier.
    let (wrong_verifier, _) = pkce_pair();
    let resp = client
        .post(format!("http://{bind}/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("client_id", client_id.as_str()),
            ("code_verifier", wrong_verifier.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let err: Value = resp.json().await.unwrap();
    assert_eq!(err["error"], "invalid_grant");

    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
}

#[tokio::test]
async fn unallowed_redirect_uri_is_rejected_with_plain_text() {
    let work = TempDir::new().unwrap();
    let adapter_addr = spin_adapter().await;
    let cfg_path = write_adapters_config(&work, adapter_addr);

    let port = ephemeral_port();
    let bind: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let issuer = format!("http://127.0.0.1:{port}/");
    let mut child = spawn_publisher_oauth(&cfg_path, bind, &issuer).await;
    wait_until_listening(bind).await;

    let (_, challenge) = pkce_pair();
    let resp = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(format!("http://{bind}/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", "evil"),
            ("redirect_uri", "https://evil.example/cb"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let ct = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    assert!(ct.starts_with("text/plain"), "got content-type: {ct}");
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("invalid_request") || body.contains("redirect_uri"),
        "unexpected body: {body}"
    );

    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
}

#[tokio::test]
async fn mcp_endpoint_also_served_at_root_path() {
    // claude.ai's Custom Connector flow POSTs MCP requests to the
    // connector URL itself (no `/mcp` suffix), so the root must
    // accept MCP messages the same as `/mcp`. Regression guard for
    // the silent-404-after-token-issuance failure the maintainer
    // hit during the first claude.ai wiring attempt.
    let work = TempDir::new().unwrap();
    let adapter_addr = spin_adapter().await;
    let cfg_path = write_adapters_config(&work, adapter_addr);

    let port = ephemeral_port();
    let bind: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let issuer = format!("http://127.0.0.1:{port}/");
    let mut child = spawn_publisher_oauth(&cfg_path, bind, &issuer).await;
    wait_until_listening(bind).await;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // Get a live access token via the dance.
    let reg: Value = client
        .post(format!("http://{bind}/register"))
        .json(&json!({"redirect_uris": ["https://claude.ai/api/mcp/auth_callback"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let client_id = reg["client_id"].as_str().unwrap().to_string();

    let (verifier, challenge) = pkce_pair();
    let auth = client
        .get(format!("http://{bind}/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .unwrap();
    let loc = auth
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let code = url::Url::parse(&loc)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .unwrap();

    let tok: Value = client
        .post(format!("http://{bind}/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("client_id", client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let access = tok["access_token"].as_str().unwrap().to_string();

    // Compare both endpoints with the same JSON-RPC payload.
    let body = json!({
        "jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}
    });

    for path in ["/", "/mcp"] {
        let resp = client
            .post(format!("http://{bind}{path}"))
            .header("authorization", format!("Bearer {access}"))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            200,
            "POST {path} must be served as MCP, got {:?}",
            resp.status()
        );
        let r: Value = resp.json().await.unwrap();
        assert_eq!(
            r["result"]["serverInfo"]["name"], "ferridis",
            "POST {path} did not return an MCP initialize result"
        );
    }

    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
}

#[tokio::test]
async fn unmapped_route_returns_404_and_logs() {
    let work = TempDir::new().unwrap();
    let adapter_addr = spin_adapter().await;
    let cfg_path = write_adapters_config(&work, adapter_addr);

    let port = ephemeral_port();
    let bind: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let issuer = format!("http://127.0.0.1:{port}/");
    let mut child = spawn_publisher_oauth(&cfg_path, bind, &issuer).await;
    wait_until_listening(bind).await;

    let resp = reqwest::Client::new()
        .get(format!("http://{bind}/this/does/not/exist"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "not_found");
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .contains("/this/does/not/exist"),
        "404 body must echo the path so journalctl-vs-curl correlation is trivial"
    );

    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
}

#[tokio::test]
async fn publisher_refuses_http_bind_without_any_auth() {
    // Neither --bearer-token nor --oauth-public-base-url: must exit 2.
    let work = TempDir::new().unwrap();
    let adapter_addr = spin_adapter().await;
    let cfg_path = write_adapters_config(&work, adapter_addr);

    let port = ephemeral_port();
    let bind: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let output = Command::new(binary_path())
        .arg("--adapters-config")
        .arg(&cfg_path)
        .arg("--http-bind")
        .arg(bind.to_string())
        .env("FERRIDIS_WALLET_MEMORY", "1")
        .env_remove("FERRIDIS_MCP_BEARER")
        .env_remove("FERRIDIS_OAUTH_PUBLIC_BASE_URL")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn ferridis-mcp-server");

    assert!(
        !output.status.success(),
        "publisher must refuse --http-bind with no auth path"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("auth") || stderr.contains("bearer") || stderr.contains("oauth"),
        "stderr should mention auth requirement; got: {stderr}"
    );
}
