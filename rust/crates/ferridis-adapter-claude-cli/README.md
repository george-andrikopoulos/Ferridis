# ferridis-adapter-claude-cli

Wraps the Claude Code CLI (`claude`) as a Ferridis stream-kind capability so any Ferridis client — and therefore any MCP-aware host via `ferridis-mcp-server` — can drive a non-interactive Claude Code session over the wire. The stopgap for "remote-control Claude Code from outside the editor" until editor agent panels (Zed, Cursor, others) expose a public extension API.

## Intents

| Verb | Kind | What it does |
|---|---|---|
| `submit-prompt` | `stream` | Start a fresh session. `prompt` required; `cwd` / `model` / `session_id` optional. |
| `resume-session` | `stream` | Continue an existing session by id. `session_id` + `prompt` required. |

Both stream the CLI's `--output-format stream-json --verbose` events as ordered SSE chunks — `event: chunk` per JSON line, `event: end` at termination.

## Spinning the adapter

```sh
cd rust
cargo run --release --example serve -p ferridis-adapter-claude-cli -- \
  --bind 127.0.0.1:7823 \
  --allowed-cwd "$HOME/your/project" \
  --default-cwd "$HOME/your/project" \
  --default-model haiku
```

Flags: `--bind` (required); `--allowed-cwd <path>` (repeatable; every operator-allowed root); `--default-cwd`, `--default-model`; `--model-alias` (extend the model allow-list); `--claude-binary` (pin a specific `claude` install).

## Direct test (curl over SSE)

```sh
curl -N -X POST http://127.0.0.1:7823/intents/submit-prompt \
  -H 'accept: text/event-stream' -H 'content-type: application/json' \
  -d '{"prompt":"Reply with exactly PONG.","model":"haiku"}'
```

The final `event: chunk` carries `"type":"result"` with `"result":"PONG"` and the `session_id`. Pass that `session_id` to `/intents/resume-session` to continue.

## Wiring into `ferridis-mcp-server` (the recursive cases)

Two paths, depending on whether the MCP host runs **on the same machine** as the adapter (local editor agent panel) or **remotely** (claude.ai, mobile Claude).

### A. Local host — stdio transport

For Claude Code in Zed, Cursor, MCP-supporting Copilot, anything that spawns MCP servers as child processes on your laptop.

### 1. Adapters config

Drop this at `~/.config/ferridis/adapters.claude-cli.json` (or wherever your own MCP setup keeps adapter configs):

```jsonc
[
  {
    "capability": "ferridis://personal.local/claude/cli@v1",
    "manifestUrl": "http://127.0.0.1:7823/manifest.json",
    "baseUrl":     "http://127.0.0.1:7823/"
  }
]
```

### 2. Add the MCP server to your client config

For Claude Code (`~/.claude.json`), add to the `mcpServers` block:

```jsonc
"ferridis-claude-cli": {
  "command": "/absolute/path/to/rust/target/release/ferridis-mcp-server",
  "args": [
    "--adapters-config",
    "/home/<you>/.config/ferridis/adapters.claude-cli.json"
  ],
  "env": {
    "FERRIDIS_KEYCHAIN_NAMESPACE": "ferridis"
  }
}
```

For Zed (`settings.json` under `context_servers`), the shape is the equivalent context-server block — see `editors/zed/` for the existing `ferridis-mcp` example.

### 3. What you'll see

`tools/list` from your host will include:

- `ferridis_personal_claude_cli_v1_submit_prompt`
- `ferridis_personal_claude_cli_v1_resume_session`

Calling either one returns a single MCP `tools/call` response whose JSON content carries `result` (the final `result.result` text from the streamed session — typically the assistant's last completion) and `chunks` (the full stream-json array, in order). Progressive forwarding waits for MCP `notifications/...`-as-chunks to land in the spec.

### B. Remote host — HTTP transport (claude.ai Custom Connectors / mobile Claude)

This is the path that makes the adapter callable from claude.ai (web) and the Anthropic mobile app — you ask mobile Claude *"call the Claude-CLI tool and tell it to ..."* and the request lands on your laptop.

Requires a **Pro / Max / Team / Enterprise** account (custom Connectors aren't available on free), and an HTTPS tunnel because claude.ai's runtime can't reach `http://127.0.0.1`.

#### Auth model

claude.ai's Custom Connector for MCP uses **OAuth 2.1 with PKCE (S256) and Dynamic Client Registration** (RFC 7591). A static `Authorization: Bearer` header is *not* what the UI's auth flows; the publisher serves a full OAuth authorization-server surface for that path. The publisher also accepts a static bearer (for direct `curl` testing or non-OAuth callers), but you'll need OAuth on for claude.ai.

The publisher's OAuth shape:
- `GET /.well-known/oauth-authorization-server` — RFC 8414 discovery
- `POST /register` — RFC 7591 DCR (returns a fresh `client_id`)
- `GET /authorize` — issues a one-shot `code` and 302-redirects back to claude.ai's callback. **Auto-approves** for redirect URIs on the operator-allow-list (claude.ai's `https://claude.ai/api/mcp/auth_callback` ships in the default list) — no in-browser approval page in v1
- `POST /token` — verifies PKCE, returns an opaque bearer access token
- `POST /mcp` — accepts either an OAuth-issued token or the static bearer, whichever is configured

State is in-memory; tokens persist for 24h. Restart wipes them and claude.ai re-handshakes on its next call.

#### 1. Start the publisher with OAuth enabled

```sh
# Optional static bearer for direct curl / non-OAuth callers.
TOKEN="$(openssl rand -hex 32)"

ferridis-mcp-server \
  --adapters-config /home/<you>/.config/ferridis/adapters.claude-cli.json \
  --http-bind 127.0.0.1:7824 \
  --bearer-token "$TOKEN" \
  --oauth-public-base-url https://<your-tunnel-host>/
```

The `--oauth-public-base-url` is the **externally-facing** base URL claude.ai will be reaching through the tunnel — it's what the publisher advertises as `issuer` in its OAuth metadata and what claude.ai uses to derive `/authorize`, `/token`, `/register`. **It must match the tunnel URL exactly** or claude.ai's discovery will go to the wrong host.

Equivalent env vars: `FERRIDIS_MCP_BEARER`, `FERRIDIS_OAUTH_PUBLIC_BASE_URL`. The publisher refuses to start with `--http-bind` and *no* auth path (neither bearer nor OAuth) — an unauthenticated MCP endpoint would let anyone on the internet spend money on your API key.

#### 2. Tunnel it (one of):

```sh
# Cloudflare Tunnel — no port-forward, no public IP, free. For a
# random URL: `cloudflared tunnel --url http://127.0.0.1:7824`.
# For a stable hostname you can configure --oauth-public-base-url
# against, use a named tunnel under a domain you own.
cloudflared tunnel --url http://127.0.0.1:7824
```

```sh
# Tailscale Funnel — if you already use Tailscale; exposes the local
# port over Tailscale's edge as <machine>.<tailnet>.ts.net (stable
# across restarts, friendlier for the issuer URL).
tailscale serve http://127.0.0.1:7824
tailscale funnel 443 on
```

```sh
# ngrok — easiest for a one-off test (random URL per session unless
# you have a reserved domain).
ngrok http 7824
```

Whichever you pick, capture the resulting `https://...` URL and pass it as `--oauth-public-base-url`. **The connector URL you give claude.ai is exactly that same base URL** — claude.ai derives `/.well-known/oauth-authorization-server`, `/mcp`, etc. from it; you don't append `/mcp` yourself.

#### 3. Register the Connector in claude.ai

Settings → **Connectors** → *Add custom connector*

| Field | Value |
|---|---|
| Name | `ferridis-claude-cli` (or anything you'll recognise) |
| MCP server URL | `https://<your-tunnel>/` (the base URL — no `/mcp` suffix) |
| Authentication | **OAuth** (claude.ai handles DCR + PKCE itself) |

Save. claude.ai will:
1. Fetch `/.well-known/oauth-authorization-server` — discovery.
2. POST `/register` — get a `client_id`.
3. Pop a browser window to `/authorize?...` — auto-approved by the publisher (operator-grade trust).
4. POST `/token` with the PKCE verifier — get an access token.
5. POST `/mcp` with `Authorization: Bearer <access_token>` from then on.

#### 4. Use it from mobile Claude

In any conversation on the Claude mobile app (or claude.ai web), the connector's tools are available the same way Web Search or Drive tools are. Reference them in plain English — e.g.

> Using the *ferridis-claude-cli* connector, call **submit-prompt** with prompt `"Write a haiku about typestate programming in Rust."` and model `haiku`. Show me the result.

Mobile Claude calls `tools/call`, your publisher dispatches into `dispatch_streaming`, the Claude-CLI adapter spawns `claude` on your laptop, the result comes back through the tunnel, mobile Claude displays it. `resume-session` works the same way — feed it the `session_id` mobile Claude saw on the previous call.

#### 5. Operational notes

- **Rotate the bearer** by restarting the publisher with a new `FERRIDIS_MCP_BEARER` and updating the connector entry. There's no other secret on the wire (for the bearer path).
- **Rotate OAuth state** by restarting the publisher — all issued access tokens are in-memory and wiped on restart. claude.ai re-handshakes automatically on its next call.
- **Operator-allow-list** for non-claude.ai redirect URIs is hard-coded to claude.ai's well-known callback today. Adding hosts requires a code change (`OAuthServer::with_allowed_redirect_prefix`); operator-config-file path is queued.
- **Cost ceiling**: every `submit-prompt` / `resume-session` call costs real money on your Anthropic API key. Consider setting `--default-model haiku` and (when the operator-budget feature lands) a per-call budget cap.
- **Single concurrent caller** is fine — the publisher holds no per-session state. Multiple callers race only over the wallet/keychain, which is serialised.
- **CORS**: not needed. Connector requests come from claude.ai's *server*, not browser-side JavaScript.

### Always-on via systemd (recommended)

For a setup that survives logout / reboot, install one unit per process.

**Install the binaries** into `~/.local/bin/` (the same convention the fs-adapter unit uses):

```sh
cargo build --release --bin ferridis-mcp-server -p ferridis-mcp-server
cargo build --release --example serve -p ferridis-adapter-claude-cli
install -Dm755 rust/target/release/ferridis-mcp-server       ~/.local/bin/ferridis-mcp-server
install -Dm755 rust/target/release/examples/serve            ~/.local/bin/ferridis-adapter-claude-cli
```

**Mint and store secrets** in a `0600` env file (kept out of the unit body so they're never world-readable in `systemctl cat`):

```sh
mkdir -p ~/.config/ferridis
umask 077 && cat > ~/.config/ferridis/mcp-http.env <<EOF
# Static bearer for direct curl / non-OAuth callers. Optional when
# FERRIDIS_OAUTH_PUBLIC_BASE_URL is also set; required otherwise.
FERRIDIS_MCP_BEARER=$(openssl rand -hex 32)

# OAuth 2.1 issuer URL — the externally-facing base URL claude.ai
# reaches via the tunnel. Comment out / leave unset for bearer-only.
# Must match the tunnel URL exactly (see the OAuth section above).
# FERRIDIS_OAUTH_PUBLIC_BASE_URL=https://your-named-tunnel.example/
EOF
chmod 600 ~/.config/ferridis/mcp-http.env
```

For the **claude.ai Connector path**, uncomment and fill in `FERRIDIS_OAUTH_PUBLIC_BASE_URL` with your tunnel URL. For **bearer-only** (direct `curl` / non-OAuth), leave it commented and use the `FERRIDIS_MCP_BEARER` value when you call `/mcp`.

**Unit 1 — adapter** (`~/.config/systemd/user/ferridis-claude-cli-adapter.service`):

```ini
[Unit]
Description=Ferridis Claude Code CLI adapter (HTTP on 127.0.0.1:7823)

[Service]
Type=simple
# systemd splits ExecStart on whitespace; quote any path with spaces.
ExecStart=%h/.local/bin/ferridis-adapter-claude-cli --bind 127.0.0.1:7823 --allowed-cwd "%h/path/to/your/project" --default-cwd "%h/path/to/your/project" --default-model haiku
Restart=on-failure
RestartSec=3s
# `claude` reads ~/.claude/ and writes per-session jsonl files,
# so this unit cannot use ProtectHome.
Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin
NoNewPrivileges=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
LockPersonality=true
RestrictRealtime=true
RestrictSUIDSGID=true
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=default.target
```

**Unit 2 — publisher** (`~/.config/systemd/user/ferridis-mcp-http.service`). Reads the bearer from the env file and `ExecStartPre`-blocks until the adapter is actually accepting connections:

```ini
[Unit]
Description=Ferridis MCP publisher in HTTP mode (POST /mcp on 127.0.0.1:7824)
After=ferridis-claude-cli-adapter.service
Wants=ferridis-claude-cli-adapter.service

[Service]
Type=simple
ExecStartPre=/usr/bin/curl --silent --fail --max-time 30 --retry 60 --retry-delay 1 --retry-connrefused --output /dev/null http://127.0.0.1:7823/manifest.json
ExecStart=%h/.local/bin/ferridis-mcp-server --adapters-config %h/.config/ferridis/adapters.claude-cli.json --http-bind 127.0.0.1:7824
EnvironmentFile=%h/.config/ferridis/mcp-http.env
# claude-cli adapter is auth:none → no real wallet entries to maintain;
# memory wallet keeps this unit hermetic from the OS keychain.
Environment=FERRIDIS_WALLET_MEMORY=1
Restart=on-failure
RestartSec=3s
TimeoutStartSec=45s
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
LockPersonality=true
RestrictRealtime=true
RestrictSUIDSGID=true
MemoryDenyWriteExecute=true
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=default.target
```

**Activate**:

```sh
systemctl --user daemon-reload
systemctl --user enable --now ferridis-claude-cli-adapter ferridis-mcp-http
```

`systemctl --user status ferridis-mcp-http` should show `active (running)` and `ExecStartPre` cleanly returned. Logs land in `journalctl --user -u ferridis-mcp-http` and `journalctl --user -u ferridis-claude-cli-adapter`.

**Linger**: by default user units stop on logout. If you want them up while you're not logged in (typical for a tunnelled connector), enable lingering once: `loginctl enable-linger $(whoami)`.

**Rotate the bearer**: write a new token to `~/.config/ferridis/mcp-http.env`, then `systemctl --user restart ferridis-mcp-http`. The adapter doesn't carry the secret so it doesn't need restarting.

**Stop everything** before changing the unit files or rotating: `systemctl --user disable --now ferridis-mcp-http ferridis-claude-cli-adapter`.

## Security stance

The adapter does **not** trust client-supplied paths or model names. Validation happens at the dispatch boundary through typestate newtypes:

- `Prompt` — non-empty, bounded length.
- `AllowedCwd` — only constructable against an operator-supplied `AllowedRoots` allow-list. Empty allow-list (the default) rejects every cwd; the operator opts in by listing roots at startup.
- `Model` enum — gated by `ModelAllowList` (default opens `sonnet` / `opus` / `haiku`; arbitrary custom aliases require `--model-alias <name>` at startup).
- `SessionId` — UUID-validated so `claude -r <id>` never sees garbage.

The child process is launched with `kill_on_drop` so a stuck `claude` can't outlive a dropped consumer stream. Non-zero child exits surface as a final `adapter-event` chunk before the SDK emits `end`, so the SSE termination story is clean for both success and failure paths.

## Why this exists

Editor agent panels (Zed's, Cursor's, others) don't yet expose a public extension API for the active assistant session, so wrapping the CLI is the realistic path to "remote-control Claude Code today." When editor panels grow a control surface, a thinner adapter replaces this one with the rest of the Ferridis stack noticing nothing.
