# Ferridis — VS Code extension

Registers `ferridis-mcp-server` with VS Code's native MCP runtime so that
Ferridis-connected capabilities — native adapters and any wrapped MCP servers
— appear as tools in Copilot Chat. Coexists with any other MCP servers you
have configured; it does not replace existing MCP plumbing.

## What you need

- VS Code 1.99+.
- A built `ferridis-mcp-server` binary. From the workspace root:
  ```bash
  cd rust && cargo build --release -p ferridis-mcp-server
  # binary at: rust/target/release/ferridis-mcp-server
  ```
- An adapters config file listing the capabilities to expose. Each entry
  can be a native Ferridis adapter, an MCP SSE server, or an MCP stdio
  server. Defaults to `~/.ferridis/adapters.json` when the setting is empty:
  ```bash
  cat > ~/.ferridis/adapters.json <<'EOF'
  [
    {
      "capability": "ferridis://public.ferridis.io/ferridis/fs@v1",
      "manifestUrl": "http://127.0.0.1:7821/manifest.json",
      "baseUrl": "http://127.0.0.1:7821/"
    }
  ]
  EOF
  ```

- At least one running Ferridis adapter, e.g. the filesystem reference adapter:

  ```bash
  cargo run --release -p ferridis-adapter-fs --example serve
  ```

## Configure

Open VS Code Settings (JSON) and set:

```jsonc
{
  "ferridis.serverPath": "/absolute/path/to/rust/target/release/ferridis-mcp-server",
  "ferridis.adaptersConfig": "/absolute/path/to/.ferridis/adapters.json"
}
```

Optional:

- `ferridis.keychainNamespace` — OS keychain service name used for credential
  storage (default: `"ferridis"`).

## Commands

| Command | What it does |
|---|---|
| `Ferridis: Configure` | Open the extension settings. |
| `Ferridis: Show Logs` | Open the Ferridis output channel. |

## Building

```bash
cd editors/vscode
npm install
npm run build           # compile TypeScript via esbuild
npm run package         # produce ferridis.vsix
code --install-extension ferridis.vsix
```

For an iterative dev loop, open `editors/vscode/` in VS Code and hit
`F5` to launch an Extension Development Host with the extension live.

## Reconnecting after a server rebuild

After rebuilding `ferridis-mcp-server` (e.g. to pick up a new adapter or a
schema change), reload the VS Code window (`Developer: Reload Window`) so the
extension restarts the server process and Copilot rediscovers the updated tool
definitions. Copilot pins tool schemas at chat-session start — a window reload
is the cleanest way to get a fresh snapshot.
