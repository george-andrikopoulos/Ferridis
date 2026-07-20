//! Mapping between Ferridis capabilities/intents and MCP tools.
//!
//! For each registered Ferridis capability, every intent the manifest
//! declares becomes one MCP tool. The tool name is
//! `ferridis_<id_munged>_<intent_munged>` where dots and dashes are
//! replaced with underscores (MCP tool names should be plain identifiers).
//!
//! Per-intent `inputSchema` precedence, highest first:
//!
//! 1. Schemas threaded through [`AdapterEntry::input_schemas`] (set
//!    when the upstream source — typically a federated MCP server —
//!    advertised typed schemas via `tools/list`). These are republished
//!    verbatim so re-exposed tools keep their original parameter types.
//! 2. The hardcoded table in [`schema_for`], covering the v0.1
//!    filesystem reference adapter (`ferridis.fs.v1`).
//! 3. A permissive `{type: "object", additionalProperties: true}`
//!    fallback.
//!
//! v0.2 replaces the hardcoded fs table by deriving schemas from each
//! manifest's OpenAPI document.

use std::collections::HashMap;
use std::sync::Arc;

use ferridis_core::{CapabilityRef, IntentVerb, Manifest};
use serde_json::json;

use crate::protocol::Tool;

/// What the server tracks per registered Ferridis capability.
#[derive(Debug, Clone)]
pub struct AdapterEntry {
    pub capability: CapabilityRef,
    pub manifest: Manifest,
    /// Per-intent `inputSchema` to republish verbatim. Populated for
    /// MCP-backed adapters; `None` for native ones, which fall through
    /// to the hardcoded table.
    pub input_schemas: Option<Arc<HashMap<IntentVerb, serde_json::Value>>>,
}

/// The full Ferridis-→-MCP tool catalogue, indexed by MCP tool name.
#[derive(Debug, Default)]
pub struct ToolCatalogue {
    /// MCP tool name → (capability, intent).
    by_name: HashMap<String, (CapabilityRef, IntentVerb)>,
    /// MCP tool descriptors, in the order they should appear in `tools/list`.
    tools: Vec<Tool>,
}

impl ToolCatalogue {
    /// Build a catalogue from a list of registered adapter entries.
    pub fn from_adapters(adapters: &[AdapterEntry]) -> Self {
        let mut by_name = HashMap::new();
        let mut tools = Vec::new();
        for entry in adapters {
            for intent in entry.manifest.intents() {
                let name = tool_name(entry.manifest.id(), intent);
                let description = describe(&entry.manifest, intent);
                let input_schema = entry
                    .input_schemas
                    .as_ref()
                    .and_then(|m| m.get(intent).cloned())
                    .unwrap_or_else(|| schema_for(entry.manifest.id(), intent));
                tools.push(Tool {
                    name: name.clone(),
                    description,
                    input_schema,
                });
                by_name.insert(name, (entry.capability.clone(), intent.clone()));
            }
        }
        Self { by_name, tools }
    }

    /// Resolve an MCP tool name to its (capability, intent) target.
    pub fn resolve(&self, name: &str) -> Option<&(CapabilityRef, IntentVerb)> {
        self.by_name.get(name)
    }

    /// All tool descriptors for `tools/list`.
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }
}

/// Compute the MCP tool name for a (manifest id, intent) pair.
pub fn tool_name(manifest_id: &str, intent: &IntentVerb) -> String {
    let id = manifest_id.replace(['.', '-'], "_");
    let verb = intent.as_str().replace('-', "_");
    format!("ferridis_{id}_{verb}")
}

fn describe(manifest: &Manifest, intent: &IntentVerb) -> String {
    format!(
        "Ferridis intent `{}` on capability `{}` ({}). {}",
        intent.as_str(),
        manifest.id(),
        manifest.name(),
        manifest.summary().as_str()
    )
}

/// Per-intent input schema for the filesystem reference adapter.
///
/// v0.2 will derive these from each manifest's OpenAPI document
/// instead of carrying a static table.
fn schema_for(manifest_id: &str, intent: &IntentVerb) -> serde_json::Value {
    if manifest_id == "ferridis.fs.v1" {
        return match intent.as_str() {
            "read-file" => json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "description": "Relative path under the adapter's root." }
                }
            }),
            "write-file" => json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                }
            }),
            "list-dir" => json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "description": "Empty string for the root." }
                }
            }),
            "search-files" => json!({
                "type": "object",
                "required": ["path", "name_contains"],
                "properties": {
                    "path": { "type": "string" },
                    "name_contains": { "type": "string" }
                }
            }),
            "move-file" => json!({
                "type": "object",
                "required": ["from", "to"],
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" }
                }
            }),
            _ => generic_object_schema(),
        };
    }
    generic_object_schema()
}

fn generic_object_schema() -> serde_json::Value {
    json!({ "type": "object", "additionalProperties": true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_munges_dots_and_dashes() {
        let intent = IntentVerb::parse("read-file").unwrap();
        assert_eq!(
            tool_name("ferridis.fs.v1", &intent),
            "ferridis_ferridis_fs_v1_read_file"
        );
    }

    #[test]
    fn catalogue_indexes_each_intent() {
        let manifest_json = r#"{
            "ferridis_version": "0.1",
            "id": "ferridis.fs.v1",
            "name": "FS",
            "category": "files",
            "summary": "Filesystem.",
            "intents": ["read-file", "write-file"],
            "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
            "tiers": ["native"],
            "auth": { "type": "none" }
        }"#;
        let manifest = Manifest::parse(manifest_json).unwrap();
        let cap = CapabilityRef::parse("ferridis://public.ferridis.io/ferridis/fs@v1").unwrap();
        let cat = ToolCatalogue::from_adapters(&[AdapterEntry {
            capability: cap.clone(),
            manifest,
            input_schemas: None,
        }]);
        assert_eq!(cat.tools().len(), 2);
        let intent = IntentVerb::parse("read-file").unwrap();
        let target = cat.resolve(&tool_name("ferridis.fs.v1", &intent)).unwrap();
        assert_eq!(target.0, cap);
        assert_eq!(target.1.as_str(), "read-file");
    }

    /// MCP-backed adapter: when the upstream's `inputSchema` is
    /// threaded through, the catalogue must republish it verbatim
    /// instead of the permissive fallback. This is the regression
    /// guard for the `limit=5` integer-stringification bug.
    #[test]
    fn catalogue_prefers_threaded_input_schemas_over_fallback() {
        let manifest_json = r#"{
            "ferridis_version": "0.1",
            "id": "mcp.linux-health-mcp.v1",
            "name": "MCP — linux-health-mcp",
            "category": "external-mcp",
            "summary": "Synthetic Ferridis capability projected from MCP server `linux-health-mcp` (1 tools).",
            "intents": ["journal-query"],
            "schema": { "type": "mcp-tools-list", "url": "mcp+tools-list:mcp.linux-health-mcp.v1" },
            "tiers": ["native"],
            "auth": { "type": "none" }
        }"#;
        let manifest = Manifest::parse(manifest_json).unwrap();
        let cap = CapabilityRef::parse("ferridis://wallet/mcp/linux-health-mcp@v1").unwrap();
        let verb = IntentVerb::parse("journal-query").unwrap();
        let upstream_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "minimum": 1, "maximum": 1000 }
            },
            "additionalProperties": false
        });
        let mut schemas: HashMap<IntentVerb, serde_json::Value> = HashMap::new();
        schemas.insert(verb.clone(), upstream_schema.clone());
        let cat = ToolCatalogue::from_adapters(&[AdapterEntry {
            capability: cap,
            manifest,
            input_schemas: Some(Arc::new(schemas)),
        }]);
        let tool = cat
            .tools()
            .iter()
            .find(|t| t.name == tool_name("mcp.linux-health-mcp.v1", &verb))
            .expect("journal-query tool must be present");
        assert_eq!(
            tool.input_schema, upstream_schema,
            "publisher must republish the upstream MCP inputSchema verbatim"
        );
    }

    /// Native fs adapter without threaded schemas: the existing
    /// hardcoded table for `ferridis.fs.v1` still wins.
    #[test]
    fn catalogue_falls_back_to_hardcoded_fs_schema() {
        let manifest_json = r#"{
            "ferridis_version": "0.1",
            "id": "ferridis.fs.v1",
            "name": "FS",
            "category": "files",
            "summary": "Filesystem.",
            "intents": ["read-file"],
            "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
            "tiers": ["native"],
            "auth": { "type": "none" }
        }"#;
        let manifest = Manifest::parse(manifest_json).unwrap();
        let cap = CapabilityRef::parse("ferridis://public.ferridis.io/ferridis/fs@v1").unwrap();
        let cat = ToolCatalogue::from_adapters(&[AdapterEntry {
            capability: cap,
            manifest,
            input_schemas: None,
        }]);
        let intent = IntentVerb::parse("read-file").unwrap();
        let tool = cat
            .tools()
            .iter()
            .find(|t| t.name == tool_name("ferridis.fs.v1", &intent))
            .unwrap();
        // The hardcoded fs read-file schema requires a `path` field.
        let required = tool
            .input_schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .expect("read-file schema must declare required fields");
        assert!(required.iter().any(|v| v == "path"));
    }

    /// MCP-backed adapter with no threaded schema and no hardcoded
    /// entry: fall through to the permissive object schema. (This is
    /// the pre-fix behaviour; the test pins it so future changes don't
    /// silently regress.)
    #[test]
    fn catalogue_falls_back_to_permissive_when_no_schema_is_known() {
        let manifest_json = r#"{
            "ferridis_version": "0.1",
            "id": "mcp.unknown-server.v1",
            "name": "MCP — unknown",
            "category": "external-mcp",
            "summary": "Synthetic.",
            "intents": ["do-something"],
            "schema": { "type": "mcp-tools-list", "url": "mcp+tools-list:mcp.unknown-server.v1" },
            "tiers": ["native"],
            "auth": { "type": "none" }
        }"#;
        let manifest = Manifest::parse(manifest_json).unwrap();
        let cap = CapabilityRef::parse("ferridis://wallet/mcp/unknown-server@v1").unwrap();
        let cat = ToolCatalogue::from_adapters(&[AdapterEntry {
            capability: cap,
            manifest,
            input_schemas: None,
        }]);
        let intent = IntentVerb::parse("do-something").unwrap();
        let tool = cat
            .tools()
            .iter()
            .find(|t| t.name == tool_name("mcp.unknown-server.v1", &intent))
            .unwrap();
        assert_eq!(
            tool.input_schema.get("additionalProperties"),
            Some(&serde_json::Value::Bool(true)),
            "with neither a threaded nor hardcoded schema, fall through to the permissive default"
        );
    }
}
