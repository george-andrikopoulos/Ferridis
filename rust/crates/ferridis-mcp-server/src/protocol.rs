//! MCP protocol envelope and message types.
//!
//! MCP is JSON-RPC 2.0 over a transport (stdio, in v0.1). The envelope
//! shape is identical to plain JSON-RPC; the methods and result shapes
//! are MCP-specific.
//!
//! Reference: <https://modelcontextprotocol.io/specification/2024-11-05>

use serde::{Deserialize, Serialize};

/// The MCP protocol version this server speaks.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Server identity reported in the `initialize` response.
pub const SERVER_NAME: &str = "ferridis";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Inbound JSON-RPC envelope. Requests carry an `id`; notifications do not.
#[derive(Debug, Deserialize)]
pub struct Inbound {
    /// Must equal `"2.0"`. Read by [`Inbound::is_valid`].
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl Inbound {
    /// Whether this message is a notification (no `id`).
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// Whether the JSON-RPC version field is the required `"2.0"`.
    pub fn is_valid(&self) -> bool {
        self.jsonrpc == "2.0"
    }
}

/// Outbound JSON-RPC response envelope.
#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Response {
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: serde_json::Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// Standard JSON-RPC error codes (MCP reuses these directly).
///
/// `INVALID_REQUEST` and `INTERNAL_ERROR` are not yet emitted by the
/// current handlers — they are exposed for completeness so future
/// handlers can map to them without re-deriving constants.
#[allow(dead_code)]
pub mod codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
}

// --- MCP method-specific result shapes ---

/// `initialize` response.
#[derive(Debug, Serialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Debug, Serialize)]
pub struct ServerCapabilities {
    /// Server publishes tools that can be invoked. We don't support
    /// `listChanged` notifications in v0.1; the tool set is fixed at
    /// startup based on the adapters config.
    pub tools: ToolsCapability,
}

#[derive(Debug, Serialize, Default)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub name: &'static str,
    pub version: &'static str,
}

/// `tools/list` response.
#[derive(Debug, Serialize)]
pub struct ToolsListResult {
    pub tools: Vec<Tool>,
}

/// A single tool descriptor (MCP shape).
#[derive(Debug, Serialize, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

/// `tools/call` request params.
#[derive(Debug, Deserialize)]
pub struct ToolsCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// `tools/call` response. Content is a vector of typed parts; v0.1
/// always returns a single `text` part containing the dispatch result
/// serialized as JSON.
#[derive(Debug, Serialize)]
pub struct ToolsCallResult {
    pub content: Vec<ContentPart>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ContentPart {
    Text { text: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_request_carries_id() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}"#;
        let msg: Inbound = serde_json::from_str(raw).unwrap();
        assert!(!msg.is_notification());
        assert_eq!(msg.method, "ping");
    }

    #[test]
    fn inbound_notification_has_no_id() {
        let raw = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        let msg: Inbound = serde_json::from_str(raw).unwrap();
        assert!(msg.is_notification());
    }

    #[test]
    fn response_serializes_with_only_result_or_error() {
        let ok = Response::success(serde_json::json!(1), serde_json::json!({"x": 1}));
        let s = serde_json::to_string(&ok).unwrap();
        assert!(s.contains("\"result\""));
        assert!(!s.contains("\"error\""));

        let err = Response::error(serde_json::json!(1), codes::METHOD_NOT_FOUND, "unknown");
        let s = serde_json::to_string(&err).unwrap();
        assert!(s.contains("\"error\""));
        assert!(!s.contains("\"result\""));
    }
}
