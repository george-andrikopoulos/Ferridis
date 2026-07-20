//! Adapter configuration file.
//!
//! Each entry in the JSON array describes one adapter to register with
//! the publisher at startup. Three shapes are accepted, discriminated
//! by which fields are present:
//!
//! **Native** — a Ferridis-native HTTP adapter (the original v0.1
//! shape, unchanged):
//!
//! ```jsonc
//! {
//!   "capability": "ferridis://public.ferridis.io/ferridis/fs@v1",
//!   "manifestUrl": "http://127.0.0.1:7821/manifest.json",
//!   "baseUrl": "http://127.0.0.1:7821/"
//! }
//! ```
//!
//! **MCP-backed (SSE)** — federate another MCP server through Ferridis
//! over the legacy SSE transport:
//!
//! ```jsonc
//! { "mcpSseUrl": "http://192.168.1.190:8765/sse" }
//! ```
//!
//! **MCP-backed (stdio)** — federate another MCP server through
//! Ferridis over stdio (the publisher spawns the child process):
//!
//! ```jsonc
//! {
//!   "mcpStdioCommand": "uvx",
//!   "mcpStdioArgs": ["semble[mcp]", "semble"],
//!   "mcpStdioEnv": { "FOO": "bar" }
//! }
//! ```
//!
//! Configs from before this change continue to parse as `Native`
//! variants without modification.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

/// One adapter configured at server startup.
///
/// Variants are distinguished by their unique discriminator fields
/// (`capability` for Native, `mcpSseUrl` for SSE, `mcpStdioCommand`
/// for stdio). Untagged deserialization picks the first variant whose
/// required fields are present.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AdapterConfig {
    /// A Ferridis-native HTTP adapter.
    Native {
        /// The `ferridis://` capability reference.
        capability: String,
        /// HTTP URL the adapter serves its manifest from.
        #[serde(rename = "manifestUrl")]
        manifest_url: String,
        /// Base URL the adapter's `/intents/:verb` routes live under.
        #[serde(rename = "baseUrl")]
        base_url: String,
    },
    /// An MCP server consumed over SSE and federated through Ferridis.
    McpSse {
        /// The SSE endpoint URL.
        #[serde(rename = "mcpSseUrl")]
        mcp_sse_url: String,
    },
    /// An MCP server consumed over stdio and federated through Ferridis.
    McpStdio {
        /// The executable to spawn.
        #[serde(rename = "mcpStdioCommand")]
        mcp_stdio_command: String,
        /// Arguments to pass to the executable.
        #[serde(rename = "mcpStdioArgs", default)]
        mcp_stdio_args: Vec<String>,
        /// Environment variables to set for the child process.
        #[serde(rename = "mcpStdioEnv", default)]
        mcp_stdio_env: HashMap<String, String>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

/// Read and parse the adapters config file.
pub fn load(path: &Path) -> Result<Vec<AdapterConfig>, ConfigError> {
    let bytes = std::fs::read(path).map_err(|e| ConfigError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    serde_json::from_slice(&bytes).map_err(|e| ConfigError::Parse {
        path: path.display().to_string(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_a_well_formed_native_entry() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("adapters.json");
        let body = r#"[
            {
                "capability": "ferridis://public.ferridis.io/ferridis/fs@v1",
                "manifestUrl": "http://127.0.0.1:7821/manifest.json",
                "baseUrl": "http://127.0.0.1:7821/"
            }
        ]"#;
        std::fs::write(&path, body).unwrap();
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.len(), 1);
        assert!(
            matches!(cfg[0], AdapterConfig::Native { ref capability, .. } if capability == "ferridis://public.ferridis.io/ferridis/fs@v1")
        );
    }

    #[test]
    fn parses_mcp_sse_entry() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("adapters.json");
        let body = r#"[{ "mcpSseUrl": "http://192.168.1.190:8765/sse" }]"#;
        std::fs::write(&path, body).unwrap();
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.len(), 1);
        assert!(matches!(
            cfg[0],
            AdapterConfig::McpSse { ref mcp_sse_url } if mcp_sse_url == "http://192.168.1.190:8765/sse"
        ));
    }

    #[test]
    fn parses_mcp_stdio_entry() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("adapters.json");
        let body = r#"[{
            "mcpStdioCommand": "uvx",
            "mcpStdioArgs": ["semble[mcp]", "semble"]
        }]"#;
        std::fs::write(&path, body).unwrap();
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.len(), 1);
        assert!(matches!(
            cfg[0],
            AdapterConfig::McpStdio { ref mcp_stdio_command, .. } if mcp_stdio_command == "uvx"
        ));
    }

    #[test]
    fn parses_mixed_entries() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("adapters.json");
        let body = r#"[
            {
                "capability": "ferridis://public.ferridis.io/ferridis/fs@v1",
                "manifestUrl": "http://127.0.0.1:7821/manifest.json",
                "baseUrl": "http://127.0.0.1:7821/"
            },
            { "mcpSseUrl": "http://192.168.1.190:8765/sse" }
        ]"#;
        std::fs::write(&path, body).unwrap();
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.len(), 2);
        assert!(matches!(cfg[0], AdapterConfig::Native { .. }));
        assert!(matches!(cfg[1], AdapterConfig::McpSse { .. }));
    }

    #[test]
    fn surfaces_missing_file_as_io_error() {
        let err = load(Path::new("/no/such/path.json")).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }));
    }
}
