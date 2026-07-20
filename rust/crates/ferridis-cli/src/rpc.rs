//! JSON-RPC 2.0 envelope and method dispatch.
//!
//! The protocol is line-delimited: one JSON object per line, both
//! directions. Each request matches the [JSON-RPC 2.0 specification]:
//!
//! ```text
//! { "jsonrpc": "2.0", "id": <num|string|null>, "method": <string>, "params": <object|array> }
//! ```
//!
//! Responses carry the same `id` and contain either `result` or
//! `error`, never both.
//!
//! [JSON-RPC 2.0 specification]: https://www.jsonrpc.org/specification

use std::sync::Arc;

use ferridis_client::{Client, ClientError};
use ferridis_core::{CapabilityRef, IntentVerb, StoredConnection};
use serde::{Deserialize, Serialize};
use url::Url;

/// A JSON-RPC 2.0 request envelope.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    /// Protocol version. Must be `"2.0"`.
    pub jsonrpc: String,
    /// Request ID. Echoed in the response. JSON-RPC permits number,
    /// string, or null; we accept any value and echo it.
    #[serde(default)]
    pub id: serde_json::Value,
    /// Method name.
    pub method: String,
    /// Method-specific parameters.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// A JSON-RPC 2.0 response envelope.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    /// Protocol version, always `"2.0"`.
    pub jsonrpc: &'static str,
    /// Echoed request ID.
    pub id: serde_json::Value,
    /// Success payload, mutually exclusive with `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Failure payload, mutually exclusive with `result`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// A JSON-RPC 2.0 error.
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    /// Error code. Standard JSON-RPC codes plus implementation range -32000..-32099.
    pub code: i32,
    /// Human-readable message.
    pub message: String,
    /// Optional structured detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    /// Build a success response.
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Build an error response.
    pub fn error(id: serde_json::Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// Standard JSON-RPC error codes.
pub mod codes {
    /// Invalid JSON received.
    pub const PARSE_ERROR: i32 = -32700;
    /// The JSON sent is not a valid request object.
    pub const INVALID_REQUEST: i32 = -32600;
    /// The method does not exist.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Invalid method parameters.
    pub const INVALID_PARAMS: i32 = -32602;
    /// Generic internal error.
    pub const INTERNAL_ERROR: i32 = -32603;
    /// Capability not registered with the client. Ferridis-specific.
    pub const CAPABILITY_NOT_REGISTERED: i32 = -32001;
    /// Intent not supported by the manifest. Ferridis-specific.
    pub const INTENT_NOT_SUPPORTED: i32 = -32002;
    /// No authorized connection in the wallet for the capability. Ferridis-specific.
    pub const NO_AUTHORIZED_CONNECTION: i32 = -32003;
    /// Auth method not yet supported by ferridis-client. Ferridis-specific.
    pub const UNSUPPORTED_AUTH: i32 = -32004;
    /// Wire-layer / network failure. Ferridis-specific.
    pub const PROTOCOL: i32 = -32010;
}

/// Map a [`ClientError`] to a JSON-RPC error code.
pub fn code_for(err: &ClientError) -> i32 {
    match err {
        ClientError::CapabilityNotRegistered(_) => codes::CAPABILITY_NOT_REGISTERED,
        ClientError::IntentNotSupported { .. } => codes::INTENT_NOT_SUPPORTED,
        ClientError::NoAuthorizedConnection(_) => codes::NO_AUTHORIZED_CONNECTION,
        ClientError::UnsupportedAuthMethod(_) => codes::UNSUPPORTED_AUTH,
        ClientError::Protocol(_) => codes::PROTOCOL,
        _ => codes::INTERNAL_ERROR,
    }
}

// ---- Per-method parameter and result types ----

#[derive(Debug, Deserialize)]
struct RegisterParams {
    capability: String,
    manifest_url: String,
    base_url: String,
}

#[derive(Debug, Serialize)]
struct RegisterResult {
    capability: String,
    id: String,
    name: String,
    category: String,
    summary: String,
    intents: Vec<String>,
    tiers: Vec<String>,
    auth: String,
}

#[derive(Debug, Deserialize)]
struct CandidatesParams {
    intent: String,
}

#[derive(Debug, Deserialize)]
struct DispatchParams {
    capability: String,
    intent: String,
    #[serde(default)]
    body: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct CapabilityInfo {
    capability: String,
    id: String,
    name: String,
    intents: Vec<String>,
    tiers: Vec<String>,
    auth: String,
    base_url: String,
    fetched_at: String,
}

/// Dispatch one parsed [`JsonRpcRequest`] against the shared [`Client`].
pub async fn dispatch(client: &Arc<Client>, request: JsonRpcRequest) -> JsonRpcResponse {
    if request.jsonrpc != "2.0" {
        return JsonRpcResponse::error(
            request.id,
            codes::INVALID_REQUEST,
            "jsonrpc must be \"2.0\"",
        );
    }

    let id = request.id.clone();
    let result: Result<serde_json::Value, JsonRpcError> = match request.method.as_str() {
        "register" => method_register(client, request.params).await,
        "candidates_for_intent" => method_candidates(client, request.params).await,
        "dispatch" => method_dispatch(client, request.params).await,
        "insert_connection" => method_insert_connection(client, request.params).await,
        "list_capabilities" => method_list_capabilities(client).await,
        other => Err(JsonRpcError {
            code: codes::METHOD_NOT_FOUND,
            message: format!("unknown method: {other}"),
            data: None,
        }),
    };

    match result {
        Ok(v) => JsonRpcResponse::success(id, v),
        Err(e) => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(e),
        },
    }
}

fn invalid_params(detail: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code: codes::INVALID_PARAMS,
        message: detail.into(),
        data: None,
    }
}

fn err_from_client(err: ClientError) -> JsonRpcError {
    JsonRpcError {
        code: code_for(&err),
        message: err.to_string(),
        data: None,
    }
}

async fn method_register(
    client: &Client,
    params: serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let p: RegisterParams = serde_json::from_value(params).map_err(|e| {
        invalid_params(format!(
            "expected {{capability, manifest_url, base_url}}: {e}"
        ))
    })?;
    let capability = CapabilityRef::parse(&p.capability)
        .map_err(|e| invalid_params(format!("capability: {e}")))?;
    let manifest_url =
        Url::parse(&p.manifest_url).map_err(|e| invalid_params(format!("manifest_url: {e}")))?;
    let base_url = Url::parse(&p.base_url).map_err(|e| invalid_params(format!("base_url: {e}")))?;
    let manifest = client
        .register(capability.clone(), manifest_url, base_url)
        .await
        .map_err(err_from_client)?;
    let result = RegisterResult {
        capability: capability.to_string(),
        id: manifest.id().to_string(),
        name: manifest.name().to_string(),
        category: manifest.category().as_str().to_string(),
        summary: manifest.summary().as_str().to_string(),
        intents: manifest
            .intents()
            .iter()
            .map(|i| i.as_str().to_string())
            .collect(),
        tiers: tier_strings(&manifest),
        auth: auth_label(manifest.auth()),
    };
    Ok(serde_json::to_value(result).expect("RegisterResult serializes"))
}

async fn method_candidates(
    client: &Client,
    params: serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let p: CandidatesParams = serde_json::from_value(params)
        .map_err(|e| invalid_params(format!("expected {{intent}}: {e}")))?;
    let intent =
        IntentVerb::parse(&p.intent).map_err(|e| invalid_params(format!("intent: {e}")))?;
    let candidates = client.candidates_for_intent(&intent).await;
    let out: Vec<String> = candidates.into_iter().map(|c| c.to_string()).collect();
    Ok(serde_json::to_value(out).expect("Vec<String> serializes"))
}

async fn method_dispatch(
    client: &Client,
    params: serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let p: DispatchParams = serde_json::from_value(params)
        .map_err(|e| invalid_params(format!("expected {{capability, intent, body}}: {e}")))?;
    let capability = CapabilityRef::parse(&p.capability)
        .map_err(|e| invalid_params(format!("capability: {e}")))?;
    let intent =
        IntentVerb::parse(&p.intent).map_err(|e| invalid_params(format!("intent: {e}")))?;
    client
        .dispatch(&capability, intent, p.body)
        .await
        .map_err(err_from_client)
}

async fn method_insert_connection(
    client: &Client,
    params: serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let stored: StoredConnection = serde_json::from_value(params)
        .map_err(|e| invalid_params(format!("expected StoredConnection shape: {e}")))?;
    client
        .insert_connection(stored)
        .await
        .map_err(err_from_client)?;
    Ok(serde_json::Value::Null)
}

async fn method_list_capabilities(client: &Client) -> Result<serde_json::Value, JsonRpcError> {
    let registry = client.registry().lock().await;
    let out: Vec<CapabilityInfo> = registry
        .iter()
        .map(|rec| {
            let m = rec.manifest();
            CapabilityInfo {
                capability: rec.capability().to_string(),
                id: m.id().to_string(),
                name: m.name().to_string(),
                intents: m.intents().iter().map(|i| i.as_str().to_string()).collect(),
                tiers: tier_strings(m),
                auth: auth_label(m.auth()),
                base_url: rec
                    .base_url()
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| "<mcp-backed>".into()),
                fetched_at: rec.fetched_at().to_string(),
            }
        })
        .collect();
    Ok(serde_json::to_value(out).expect("Vec<CapabilityInfo> serializes"))
}

fn tier_strings(m: &ferridis_core::Manifest) -> Vec<String> {
    m.tiers()
        .iter()
        .map(|t| match t {
            ferridis_core::Tier::Native => "native".into(),
            ferridis_core::Tier::Browser => "browser".into(),
            ferridis_core::Tier::Vision => "vision".into(),
        })
        .collect()
}

fn auth_label(a: &ferridis_core::AuthMethod) -> String {
    match a {
        ferridis_core::AuthMethod::None => "none".into(),
        ferridis_core::AuthMethod::Oauth2 { .. } => "oauth2".into(),
        ferridis_core::AuthMethod::ApiKey { .. } => "api_key".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_jsonrpc_version() {
        let _ = JsonRpcResponse::error(serde_json::json!(1), codes::INVALID_REQUEST, "test");
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let client = Arc::new(Client::ephemeral());
        let resp = dispatch(
            &client,
            JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: serde_json::json!(7),
                method: "no_such_method".into(),
                params: serde_json::Value::Null,
            },
        )
        .await;
        assert_eq!(resp.id, serde_json::json!(7));
        let err = resp.error.expect("must error");
        assert_eq!(err.code, codes::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn invalid_jsonrpc_version_is_rejected() {
        let client = Arc::new(Client::ephemeral());
        let resp = dispatch(
            &client,
            JsonRpcRequest {
                jsonrpc: "1.0".into(),
                id: serde_json::json!(1),
                method: "list_capabilities".into(),
                params: serde_json::Value::Null,
            },
        )
        .await;
        let err = resp.error.expect("must error");
        assert_eq!(err.code, codes::INVALID_REQUEST);
    }

    #[tokio::test]
    async fn list_capabilities_returns_empty_for_fresh_client() {
        let client = Arc::new(Client::ephemeral());
        let resp = dispatch(
            &client,
            JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: serde_json::json!(1),
                method: "list_capabilities".into(),
                params: serde_json::Value::Null,
            },
        )
        .await;
        let result = resp.result.expect("must succeed");
        assert_eq!(result.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn dispatch_against_unregistered_capability_errors() {
        let client = Arc::new(Client::ephemeral());
        let resp = dispatch(
            &client,
            JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: serde_json::json!(2),
                method: "dispatch".into(),
                params: serde_json::json!({
                    "capability": "ferridis://public.ferridis.io/test/cap@v1",
                    "intent": "read-file",
                    "body": {"path": "x"}
                }),
            },
        )
        .await;
        let err = resp.error.expect("must error");
        assert_eq!(err.code, codes::CAPABILITY_NOT_REGISTERED);
    }
}
