//! `McpClient` — the upper-layer connection to one MCP server.
//!
//! Sits on top of a [`McpTransport`] and speaks MCP's
//! `initialize` → `notifications/initialized` → `tools/list` →
//! `tools/call` flow. Each [`McpClient`] is a long-lived handle to one
//! MCP server; [`crate::Client`] stores it in the registry against the
//! synthetic Ferridis capability it represents.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

use crate::error::ClientError;
use crate::mcp::protocol::{
    InitializeResult, McpTool, PROTOCOL_VERSION, ToolsCallResult, ToolsListResult,
};
use crate::mcp::transport::McpTransport;

/// A live MCP server connection.
#[derive(Clone)]
pub struct McpClient {
    transport: Arc<dyn McpTransport>,
    next_id: Arc<AtomicU64>,
    server_name: Arc<String>,
    /// Client name passed in the original `initialize` — replayed on
    /// re-handshake after a session expiry.
    client_name: Arc<String>,
}

impl McpClient {
    /// Build an MCP client over `transport` and perform the handshake.
    /// Returns a connected client and the server's `tools/list` snapshot.
    pub async fn handshake(
        transport: Arc<dyn McpTransport>,
        client_name: &str,
    ) -> Result<(Self, Vec<McpTool>), ClientError> {
        let next_id = Arc::new(AtomicU64::new(1));

        // 1. initialize
        let init_id = next_id.fetch_add(1, Ordering::Relaxed);
        let init_payload = json!({
            "jsonrpc": "2.0",
            "id": init_id,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": client_name,
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }
        });
        let init_resp = transport.request(init_payload, init_id).await?;
        let init_result: InitializeResult = serde_json::from_value(
            init_resp
                .get("result")
                .cloned()
                .ok_or_else(|| ClientError::WalletIo("MCP initialize: no result".into()))?,
        )
        .map_err(ClientError::from)?;
        let server_name = Arc::new(init_result.server_info.name);

        // 2. notifications/initialized
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        transport.notify(notif).await?;

        // 3. tools/list
        let list_id = next_id.fetch_add(1, Ordering::Relaxed);
        let list_payload = json!({
            "jsonrpc": "2.0",
            "id": list_id,
            "method": "tools/list",
            "params": {}
        });
        let list_resp = transport.request(list_payload, list_id).await?;
        let tools_result: ToolsListResult = serde_json::from_value(
            list_resp
                .get("result")
                .cloned()
                .ok_or_else(|| ClientError::WalletIo("MCP tools/list: no result".into()))?,
        )
        .map_err(ClientError::from)?;

        Ok((
            Self {
                transport,
                next_id,
                server_name,
                client_name: Arc::new(client_name.to_string()),
            },
            tools_result.tools,
        ))
    }

    /// Re-run the `initialize` + `notifications/initialized` portion
    /// of the handshake on the current transport without refetching
    /// `tools/list`. Used by [`call_tool`](Self::call_tool) after the
    /// transport reports a session-expired error and has re-handshaked
    /// its underlying connection.
    async fn replay_initialize(&self) -> Result<(), ClientError> {
        let init_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let init_payload = json!({
            "jsonrpc": "2.0",
            "id": init_id,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": self.client_name.as_str(),
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }
        });
        self.transport.request(init_payload, init_id).await?;
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        self.transport.notify(notif).await?;
        Ok(())
    }

    /// The server name reported during `initialize`.
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Invoke `tools/call` and return the parsed result.
    ///
    /// For text content parts, the v0.2 first cut joins them and tries
    /// to parse as JSON. If parsing fails the joined text is returned
    /// as a JSON string. `isError: true` results are surfaced as
    /// [`ClientError::Protocol`]-shaped failures.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        // First attempt. If the upstream MCP server has recycled its
        // sessions (typical: VM restart, container redeploy), the
        // transport surfaces `McpSessionExpired`; we re-handshake the
        // transport, replay `initialize`, and retry exactly once. A
        // second `McpSessionExpired` propagates — at that point the
        // upstream is in a different kind of trouble.
        match self.do_call_tool(tool_name, &arguments).await {
            Err(ClientError::McpSessionExpired(detail)) => {
                tracing::warn!(
                    target: "ferridis_client::mcp",
                    server = %self.server_name,
                    detail = %detail,
                    "MCP session expired; re-handshaking and retrying"
                );
                self.transport.reconnect().await?;
                self.replay_initialize().await?;
                self.do_call_tool(tool_name, &arguments).await
            }
            other => other,
        }
    }

    async fn do_call_tool(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            }
        });
        let resp = self.transport.request(payload, id).await?;

        if let Some(err) = resp.get("error") {
            return Err(ClientError::WalletIo(format!(
                "MCP tools/call error: {err}"
            )));
        }

        let result_val = resp.get("result").cloned().ok_or_else(|| {
            ClientError::WalletIo("MCP tools/call: no result and no error".into())
        })?;
        let result: ToolsCallResult =
            serde_json::from_value(result_val.clone()).map_err(ClientError::from)?;

        let mut text = String::new();
        for part in &result.content {
            if let crate::mcp::protocol::ContentPart::Text { text: t } = part {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
        }

        if result.is_error.unwrap_or(false) {
            return Err(ClientError::WalletIo(format!(
                "MCP tool reported failure: {text}"
            )));
        }

        // Try JSON first, fall back to the raw text as a JSON string.
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => Ok(v),
            Err(_) => Ok(serde_json::Value::String(text)),
        }
    }
}
