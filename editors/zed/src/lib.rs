//! Ferridis Zed extension.
//!
//! Pure Rust, compiled to a `wasm32-wasip1` cdylib. Registers
//! `ferridis-mcp-server` as a Zed [context server] — Zed launches the
//! binary on activation and routes MCP traffic to it. From the user's
//! point of view, Ferridis capabilities appear as tools in Zed's
//! assistant alongside any other MCP servers they have configured.
//!
//! # Configuration
//!
//! In Zed settings (`Cmd/Ctrl+,`), set:
//!
//! ```jsonc
//! "context_servers": {
//!   "ferridis-mcp": {
//!     "settings": {
//!       "binary_path": "/absolute/path/to/ferridis-mcp-server",
//!       "adapters_config": "/absolute/path/to/adapters.json"
//!     }
//!   }
//! }
//! ```
//!
//! The extension reads these settings and returns the launch command
//! to Zed.
//!
//! [context server]: https://zed.dev/docs/assistant/context-servers

use zed_extension_api as zed;

struct FerridisExtension;

/// Settings the user provides in Zed for the `ferridis-mcp` context server.
#[derive(Debug, serde::Deserialize)]
struct FerridisSettings {
    /// Absolute path to the `ferridis-mcp-server` binary.
    binary_path: String,
    /// Absolute path to the adapters JSON file (same shape as the VS
    /// Code extension's `ferridis.adapters`).
    adapters_config: String,
    /// Optional override for the wallet file path.
    #[serde(default)]
    wallet_path: Option<String>,
}

impl zed::Extension for FerridisExtension {
    fn new() -> Self {
        Self
    }

    fn context_server_command(
        &mut self,
        context_server_id: &zed::ContextServerId,
        project: &zed::Project,
    ) -> Result<zed::Command, String> {
        let raw = zed::settings::ContextServerSettings::for_project(
            context_server_id.as_ref(),
            project,
        )?;
        let settings_value = raw
            .settings
            .ok_or_else(|| {
                "Ferridis: missing `settings` block. Set `binary_path` and `adapters_config` for `ferridis-mcp` in Zed settings.".to_string()
            })?;
        let settings: FerridisSettings = serde_json::from_value(settings_value)
            .map_err(|e| format!("Ferridis: failed to parse settings: {e}"))?;

        let mut args = vec![
            "--adapters-config".into(),
            settings.adapters_config,
        ];
        if let Some(w) = settings.wallet_path {
            args.push("--wallet".into());
            args.push(w);
        }

        Ok(zed::Command {
            command: settings.binary_path,
            args,
            env: Default::default(),
        })
    }
}

zed::register_extension!(FerridisExtension);
