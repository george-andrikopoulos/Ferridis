//! MCP JSON-RPC envelope types, as seen from the client side.
//!
//! This is a minimal subset of MCP `2024-11-05` — exactly the methods
//! and shapes [`McpClient`](super::client::McpClient) sends and
//! receives. Mirror of `ferridis-mcp-server`'s protocol module from
//! the opposite perspective.
//!
//! Field-level docs are omitted because these types are mechanical
//! mirrors of the MCP wire spec; the spec is authoritative.

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};

/// Protocol version this client negotiates.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Outbound JSON-RPC request from client to server.
#[derive(Debug, Serialize)]
pub struct Request {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'static str,
    pub params: serde_json::Value,
}

/// Outbound JSON-RPC notification (no `id`, no response).
#[derive(Debug, Serialize)]
pub struct Notification {
    pub jsonrpc: &'static str,
    pub method: &'static str,
    pub params: serde_json::Value,
}

/// Inbound JSON-RPC response or notification from server to client.
#[derive(Debug, Deserialize)]
pub struct Inbound {
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<InboundError>,
}

#[derive(Debug, Deserialize)]
pub struct InboundError {
    pub code: i32,
    pub message: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub data: Option<serde_json::Value>,
}

// --- Method-specific shapes used by the consumer ---

#[derive(Debug, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    #[allow(dead_code)]
    pub protocol_version: String,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
    #[serde(default)]
    #[allow(dead_code)]
    pub capabilities: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ToolsListResult {
    pub tools: Vec<McpTool>,
}

/// One MCP tool descriptor as returned by `tools/list`.
#[derive(Debug, Clone, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// The MCP server's `inputSchema` for this tool, verbatim. Threaded
    /// through `ProjectedManifest` to the publisher so re-exposed MCP
    /// tools keep their original typed schema (string vs integer vs
    /// object), instead of collapsing to `additionalProperties: true`.
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Option<serde_json::Value>,
}

/// Result envelope for `tools/call`. Content is a vector of parts; v0.2
/// recognises `text` parts and stitches them into a single JSON value
/// where possible.
#[derive(Debug, Deserialize)]
pub struct ToolsCallResult {
    pub content: Vec<ContentPart>,
    #[serde(rename = "isError", default)]
    pub is_error: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ContentPart {
    Text { text: String },
    #[serde(other)]
    Unknown,
}
