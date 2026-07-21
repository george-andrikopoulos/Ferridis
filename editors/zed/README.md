# Ferridis — Zed extension

Pure Rust, compiled to `wasm32-wasip2` (Zed Preview 1.2+ uses WASI
Preview 2 / Component Model; older Zed versions used `wasm32-wasip1`
and both targets are pre-declared in `rust-toolchain.toml` so the
right one is available). Registers
[`ferridis-mcp-server`](../../rust/crates/ferridis-mcp-server) as a Zed
**context server**, so Ferridis-connected capabilities appear as tools in
Zed's assistant alongside any other MCP servers you have configured.

## Why "context server" and not pure WASM Ferridis client

Zed extensions run in a sandboxed `wasm32-wasip2` environment. `reqwest`
and `tokio` (which `ferridis-client` uses) do not compile to that target
out of the box. This integration therefore reuses the already-built
`ferridis-mcp-server` binary and registers it through Zed's
context-server API. From the extension's point of view, this is pure
Rust — no JavaScript, no manual stdio plumbing in the extension itself.
Zed handles the MCP stdio internally.

A pure-Rust extension that talks Ferridis directly to adapters without
any subprocess (using Zed's `HttpClient`) is queued for a future version.

## What you need

- Zed (any recent version with assistant + context-server support).
- The `ferridis-mcp-server` release binary, built from the workspace:
  ```bash
  cd rust && cargo build --release -p ferridis-mcp-server
  # binary at: rust/target/release/ferridis-mcp-server
  ```
- An adapters config file (same shape as VS Code's `ferridis.adapters`):
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
- A running Ferridis adapter on the configured port (the filesystem
  reference adapter via `cargo run --release -p ferridis-adapter-fs --example serve-fs`).

## Install as a dev extension

Zed compiles the extension for you when you install it as a dev
extension — you do not have to run `cargo build` by hand.

In Zed: `Cmd/Ctrl+Shift+P` → **`zed: install dev extension`** → pick the
`editors/zed/` directory. Zed will compile to `wasm32-wasip2` and install
the resulting WASM into its dev-extension slot.

If you want to verify the build locally before letting Zed do it:

```bash
cd editors/zed
cargo build --target wasm32-wasip2 --release
```

The pinned `rust-toolchain.toml` carries both wasip1 and wasip2 targets,
so the right one is available whichever Zed asks for.

## Configure Zed

Open Zed Settings (`Cmd/Ctrl+,`) and add:

```jsonc
{
  "context_servers": {
    "ferridis-mcp": {
      "settings": {
        "binary_path": "/absolute/path/to/rust/target/release/ferridis-mcp-server",
        "adapters_config": "/absolute/path/to/.ferridis/adapters.json"
      }
    }
  }
}
```

Optional `wallet_path` if you want a non-default wallet location.

## Verify

In the Zed assistant panel, ask:

> "Use Ferridis to read the file hello.txt"

Zed should call the `ferridis_ferridis_fs_v1_read_file` MCP tool and
return the file content. The same Zed assistant session can call other
MCP servers you have configured — Ferridis lives alongside them.

## Reconnecting after a publisher rebuild

After you rebuild `ferridis-mcp-server` (e.g., to pick up a schema
change or a new MCP-backed adapter), **start a fresh agent thread**.
Zed's assistant binds each tool's input schema at thread-start and
does not refresh it when the underlying context-server child process
restarts. Toggling the context server off-and-on in settings won't
help by itself — the same cached schema persists across the
publisher's lifecycle within one thread.

If you're testing a publisher fix that changes a tool's argument types
(e.g., relaxing or tightening an `inputSchema`), the cleanest path is
a brand-new agent thread. Existing threads keep the old schema until
you start a new one.
