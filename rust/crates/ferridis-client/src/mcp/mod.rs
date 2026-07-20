//! MCP consumer: lets `ferridis-client` see MCP servers as if they
//! were Ferridis-native capabilities.
//!
//! The flow:
//!
//! 1. The host calls
//!    [`crate::Client::register_mcp_stdio`](crate::Client::register_mcp_stdio)
//!    or
//!    [`crate::Client::register_mcp_sse`](crate::Client::register_mcp_sse)
//!    with a transport descriptor for an MCP server.
//! 2. A [`McpClient`] performs the MCP `initialize` → `tools/list`
//!    handshake. Its `tools/list` is projected into a synthetic
//!    Ferridis [`Manifest`](ferridis_core::Manifest) — one intent
//!    verb per MCP tool, the manifest's id derived from the server's
//!    reported name.
//! 3. The synthetic manifest is registered with the normal
//!    [`crate::Registry`], tagged as MCP-backed. The host's view of
//!    the capability is unchanged: dispatch goes through
//!    [`crate::Client::dispatch`] like any other Ferridis capability.
//! 4. At dispatch time, [`crate::Client::dispatch`] checks the
//!    capability's backend and routes through the live [`McpClient`]
//!    rather than the HTTP path.
//!
//! This is the second half of the Ferridis-↔-MCP interoperability
//! shim. The first half (`ferridis-mcp-server`, the publisher
//! direction) shipped in v0.1.

pub mod client;
pub mod protocol;
pub mod transport;

pub use client::McpClient;
pub use protocol::McpTool;
pub use transport::{McpTransport, SseTransport, StdioTransport};

use std::collections::HashMap;

use ferridis_core::{CapabilityRef, IntentVerb, Manifest};

/// Result of projecting an MCP `tools/list` into a Ferridis manifest.
///
/// - `intent_to_mcp` lets dispatch translate a Ferridis intent verb
///   back to the original MCP tool name when invoking `tools/call`.
/// - `intent_to_input_schema` preserves the upstream MCP server's
///   declared `inputSchema` per tool, so a publisher (or any caller
///   re-exposing the projected capability) can republish the original
///   typed schema instead of falling back to permissive defaults.
///   Missing entries mean the upstream server did not declare a schema
///   for that tool.
#[derive(Debug, Clone)]
pub struct ProjectedManifest {
    /// The synthesised Ferridis manifest.
    pub manifest: Manifest,
    /// The CapabilityRef the manifest is registered under.
    pub capability: CapabilityRef,
    /// Mapping from Ferridis intent verb → MCP tool name.
    pub intent_to_mcp: HashMap<IntentVerb, String>,
    /// Mapping from Ferridis intent verb → verbatim MCP `inputSchema`.
    pub intent_to_input_schema: HashMap<IntentVerb, serde_json::Value>,
}

/// Build a synthetic [`Manifest`] from an MCP server's
/// `(server_name, tools)` and a host-supplied id namespace.
///
/// - `server_name` is the value the MCP server returned in
///   `initialize.serverInfo.name`. It is incorporated into the manifest
///   id.
/// - `id_namespace` is the registry namespace under which the
///   capability lives (e.g. `"mcp"`).
/// - `version` is the version suffix (e.g. `"v1"`).
///
/// Returns the manifest, the matching [`CapabilityRef`], and the
/// intent-verb → MCP-tool-name lookup map.
pub fn project_tools_to_manifest(
    server_name: &str,
    id_namespace: &str,
    version: &str,
    tools: &[McpTool],
) -> Result<ProjectedManifest, crate::ClientError> {
    let server_slug = slugify(server_name);
    let id = format!("{id_namespace}.{server_slug}.{version}");

    let mut intent_strs = Vec::with_capacity(tools.len());
    let mut intent_to_mcp: HashMap<IntentVerb, String> = HashMap::new();
    let mut intent_to_input_schema: HashMap<IntentVerb, serde_json::Value> = HashMap::new();
    for tool in tools {
        let verb_str = mcp_tool_name_to_intent_verb(&tool.name);
        let verb = IntentVerb::parse(&verb_str).map_err(crate::ClientError::from)?;
        if let Some(existing) = intent_to_mcp.get(&verb) {
            return Err(crate::ClientError::McpToolNameCollision {
                first: existing.clone(),
                second: tool.name.clone(),
                verb,
            });
        }
        intent_strs.push(verb_str);
        if let Some(schema) = &tool.input_schema {
            intent_to_input_schema.insert(verb.clone(), schema.clone());
        }
        intent_to_mcp.insert(verb, tool.name.clone());
    }

    // The synthetic manifest's schema_url is a non-fetchable URI in
    // the `mcp+tools-list:` scheme. It is never dereferenced; it exists
    // because v0.1 `Manifest` requires a parseable URL there. v0.2
    // hardening may make schema_url optional.
    let manifest_json = serde_json::json!({
        "ferridis_version": "0.1",
        "id": id,
        "name": format!("MCP — {server_name}"),
        "category": "external-mcp",
        "summary": format!(
            "Synthetic Ferridis capability projected from MCP server `{server_name}` ({} tools).",
            tools.len()
        ),
        "intents": intent_strs,
        "schema": {
            "type": "mcp-tools-list",
            "url": format!("mcp+tools-list:{id}")
        },
        "tiers": ["native"],
        "auth": { "type": "none" }
    });
    let manifest_str = serde_json::to_string(&manifest_json).map_err(crate::ClientError::from)?;
    let manifest = Manifest::parse(&manifest_str)?;

    let capability_ref = CapabilityRef::parse(&format!(
        "ferridis://wallet/{id_namespace}/{server_slug}@{version}"
    ))?;

    Ok(ProjectedManifest {
        manifest,
        capability: capability_ref,
        intent_to_mcp,
        intent_to_input_schema,
    })
}

/// Convert an MCP tool name to a Ferridis intent verb.
///
/// MCP's tool naming rules are permissive: `[a-zA-Z0-9_.-]+` in
/// practice, with both `_` and `.` showing up in real servers
/// (`ha_get_state`, `services.list-units`, `system.boot-performance`).
/// Ferridis intent verbs are stricter — `[a-z0-9-]+`, no leading or
/// trailing dash, no consecutive dashes.
///
/// This function normalizes any printable input to a valid
/// [`IntentVerb`](ferridis_core::IntentVerb) by:
/// - lowercasing ASCII letters,
/// - keeping digits,
/// - replacing every other character (including `_`, `.`, whitespace,
///   and punctuation) with a single dash,
/// - collapsing consecutive dashes, and
/// - trimming leading and trailing dashes.
///
/// Round-trip dispatch is preserved via the `intent_to_mcp` map on
/// [`ProjectedManifest`]: the original MCP tool name is what we pass
/// back to the server's `tools/call`.
///
/// Returns an empty string for input that contains no alphanumeric
/// characters; callers must surface that as an [`IntentVerb`] parse
/// error rather than registering the tool.
fn mcp_tool_name_to_intent_verb(name: &str) -> String {
    slugify(name)
}

/// Lowercase + non-alphanumeric → dashes, no leading/trailing dashes,
/// no double dashes. Result is empty if the input is empty.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_dash = true; // suppresses leading dashes
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_name_to_intent_verb_underscores_become_dashes() {
        assert_eq!(mcp_tool_name_to_intent_verb("ha_get_state"), "ha-get-state");
        assert_eq!(mcp_tool_name_to_intent_verb("read-file"), "read-file");
        assert_eq!(mcp_tool_name_to_intent_verb("getState"), "getstate");
    }

    #[test]
    fn mcp_name_to_intent_verb_dots_become_dashes() {
        // Real linux-health-mcp tool names that motivated this support.
        assert_eq!(
            mcp_tool_name_to_intent_verb("services.list-units"),
            "services-list-units"
        );
        assert_eq!(
            mcp_tool_name_to_intent_verb("system.boot-performance"),
            "system-boot-performance"
        );
        assert_eq!(
            mcp_tool_name_to_intent_verb("journal.query"),
            "journal-query"
        );
    }

    #[test]
    fn mcp_name_to_intent_verb_collapses_and_trims() {
        assert_eq!(mcp_tool_name_to_intent_verb("._foo._bar._"), "foo-bar");
        assert_eq!(mcp_tool_name_to_intent_verb("a..b"), "a-b");
        assert_eq!(mcp_tool_name_to_intent_verb(""), "");
    }

    #[test]
    fn projects_a_dot_named_tool_with_intent_to_mcp_roundtrip() {
        let tools = vec![McpTool {
            name: "services.list-units".into(),
            description: None,
            input_schema: None,
        }];
        let projected = project_tools_to_manifest("linux-health", "mcp", "v1", &tools).unwrap();
        let verb = IntentVerb::parse("services-list-units").unwrap();
        assert_eq!(
            projected.intent_to_mcp.get(&verb).map(String::as_str),
            Some("services.list-units"),
            "dispatch must be able to recover the original MCP tool name"
        );
    }

    #[test]
    fn projecting_collides_when_two_mcp_names_normalize_to_same_verb() {
        // `services.list-units` and `services_list_units` both normalize
        // to `services-list-units`. Registration must fail loudly
        // rather than silently overwrite.
        let tools = vec![
            McpTool {
                name: "services.list-units".into(),
                description: None,
                input_schema: None,
            },
            McpTool {
                name: "services_list_units".into(),
                description: None,
                input_schema: None,
            },
        ];
        let err = project_tools_to_manifest("x", "mcp", "v1", &tools).unwrap_err();
        match err {
            crate::ClientError::McpToolNameCollision {
                first,
                second,
                verb,
            } => {
                assert_eq!(first, "services.list-units");
                assert_eq!(second, "services_list_units");
                assert_eq!(verb.as_str(), "services-list-units");
            }
            other => panic!("expected McpToolNameCollision, got {other:?}"),
        }
    }

    #[test]
    fn slugifies_server_names() {
        assert_eq!(slugify("home-assistant"), "home-assistant");
        assert_eq!(slugify("Home Assistant 2026!"), "home-assistant-2026");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn projects_a_minimal_tools_list_to_a_valid_manifest() {
        let tools = vec![McpTool {
            name: "ha_get_state".into(),
            description: Some("Get a state".into()),
            input_schema: None,
        }];
        let projected = project_tools_to_manifest("ha", "mcp", "v1", &tools).unwrap();
        assert_eq!(projected.manifest.id(), "mcp.ha.v1");
        assert_eq!(projected.manifest.intents().len(), 1);
        let verb = IntentVerb::parse("ha-get-state").unwrap();
        assert_eq!(
            projected.intent_to_mcp.get(&verb),
            Some(&"ha_get_state".to_string())
        );
        assert!(
            projected.intent_to_input_schema.is_empty(),
            "no inputSchema declared upstream means no entry threaded through"
        );
    }

    #[test]
    fn projects_preserve_upstream_inputschema_verbatim() {
        // The real linux-health-mcp `journal.query` schema: integer
        // `limit` with bounds, string `unit`/`priority`/`since`. This is
        // exactly the shape that was getting flattened to
        // `{additionalProperties: true}` before this change.
        let upstream_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "priority": { "type": "string" },
                "unit":     { "type": "string" },
                "since":    { "type": "string" },
                "limit":    { "type": "integer", "minimum": 1, "maximum": 1000 }
            },
            "additionalProperties": false
        });
        let tools = vec![McpTool {
            name: "journal.query".into(),
            description: None,
            input_schema: Some(upstream_schema.clone()),
        }];
        let projected = project_tools_to_manifest("linux-health", "mcp", "v1", &tools).unwrap();
        let verb = IntentVerb::parse("journal-query").unwrap();
        assert_eq!(
            projected.intent_to_input_schema.get(&verb),
            Some(&upstream_schema),
            "the projected manifest must carry the upstream schema bit-for-bit"
        );
    }
}
