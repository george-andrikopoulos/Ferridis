# Ferridis features

A glance at what the protocol does, what's built, and what's coming. Status is current as of the most recent commit. For commit-by-commit history of the protocol layer, see [TODO.md](./TODO.md).

## Status legend

- ✅ **Built** — implemented and (where applicable) tested.
- 🚧 **In progress** — being worked on now.
- 📋 **Planned** — designed, queued, not started.
- 💡 **Future** — known to matter, not yet designed.

---

## Layer 1 — Connections

| Status | Feature | Where |
|---|---|---|
| ✅ | Connection lifecycle as typestate (`Pending → Authorized → Expired → Revoked`) | `rust/crates/ferridis-core/src/connection.rs` |
| ✅ | Token wrappers with redacted Debug output | `rust/crates/ferridis-core/src/token.rs` |
| ✅ | Capability reference URL parser (`ferridis://`) | `rust/crates/ferridis-core/src/capability.rs` |
| ✅ | Storage-friendly tagged enum for wallet round-trip | `rust/crates/ferridis-core/src/storage.rs` |
| ✅ | OAuth 2.0+PKCE primitives (verifier/challenge/S256, authorization URL builder, token exchange) | `rust/crates/ferridis-protocol/src/oauth.rs` |
| ✅ | Connection broker (`start_connect` → `Pending`, `complete` → `Authorized`) | `rust/crates/ferridis-protocol/src/broker.rs` |
| ✅ | Bearer-token authenticated call helper (`X-Ferridis-Connection`, `X-Ferridis-Intent` headers) | `rust/crates/ferridis-protocol/src/call.rs` |
| ✅ | Anonymous call helper (for `auth: none` capabilities) | `rust/crates/ferridis-protocol/src/call.rs` |
| ✅ | **OS keychain wallet via `keyring`** — Linux Secret Service / macOS Keychain / Windows Credential Manager. Refuses to run if backend unreachable; no plaintext fallback. Per-connection entries plus reserved `__ferridis_wallet_index__` for enumeration. | `rust/crates/ferridis-client/src/wallet.rs`, `wallet_store.rs` |
| ✅ | In-memory wallet (`Wallet::ephemeral`) for tests and embedded use | `rust/crates/ferridis-client/src/wallet_store.rs` |

## Layer 2 — Capability mesh

| Status | Feature | Where |
|---|---|---|
| ✅ | Manifest parser (parse-don't-validate, all rules enforced) | `rust/crates/ferridis-core/src/manifest.rs` |
| ✅ | Intent verb validator (canonical form enforced) | `rust/crates/ferridis-core/src/intent.rs` |
| ✅ | Tier set (non-empty by construction, precedence-aware) | `rust/crates/ferridis-core/src/tier.rs` |
| ✅ | Manifest fetching over HTTP (validated via core's parser on receipt) | `rust/crates/ferridis-protocol/src/manifest.rs` |
| ✅ | In-memory manifest registry with TTL freshness check | `rust/crates/ferridis-client/src/registry.rs` |
| ✅ | **Three-tier registry traversal** (`Personal → Org → Public`) with tier-aware candidate ordering and per-tier grouping | `rust/crates/ferridis-client/src/registry.rs` |
| ✅ | **Public mesh registry** with per-capability TTL cache and `endpoint_url` field on manifests | `rust/crates/ferridis-protocol/src/mesh.rs` |
| ✅ | **Federated mesh** — priority-ordered chain of mesh clients, fall-through on 404 | `rust/crates/ferridis-protocol/src/mesh.rs` (`FederatedMesh`) |
| ✅ | **Signed mesh-index** — `{mesh_root}/index.json` + cosign bundle, verified through the full Sigstore trust chain, expiry-enforced | `rust/crates/ferridis-protocol/src/mesh.rs` (`MeshIndex`) |
| ✅ | **`Client::register_from_federated_mesh` + `register_all_from_mesh`** — auto-populate Public tier from signed index | `rust/crates/ferridis-client/src/client.rs` |
| ✅ | **Manifest signing — full Sigstore trust chain + CRL revocation.** Signature-vs-cert + cert validity window + Rekor inclusion proof + Fulcio cert chain via `rustls-webpki` + CRL check against signing cert's CDP extension. `TrustRoot::from_sigstore_tuf(cache_dir)` auto-fetches Fulcio CAs + Rekor keys from Sigstore's Public Good TUF repo. `RevocationMode::BestEffort \| Required \| Skip` configurable via `TrustRoot::with_revocation_mode`. `verify_signed_manifest_with_revocation` is the new top-level verification entry point. | `rust/crates/ferridis-protocol/src/signing.rs` |
| ✅ | **mDNS service scanning** — `MdnsScanner` browses `_mcp._tcp.local` + `_ferridis._tcp.local` via `mdns-sd 0.13`; `Client::discover_mdns()` auto-registers resolved services and returns a drop-to-stop `DiscoveryHandle` | `rust/crates/ferridis-protocol/src/mdns.rs` |
| ✅ | **Discovery broker** (`ferridis-discovery-broker`, port 7825) — `POST /discovery/register` (ephemeral TTL-300s or persistent), `GET /discovery/services` (with `"persistent": bool`), `GET /discovery/events` (SSE push), `DELETE /discovery/services/:name` (204/404). `Lifetime` enum (`Ephemeral { expires_at }` \| `Pinned`, TDP Pattern 5). Pinned entries survive TTL and restart; state file `~/.local/share/ferridis/discovery.json` stores only pinned entries. `Client::discover_broker(url)` syncs snapshot + subscribes to SSE stream. Systemd user unit ships. | `rust/crates/ferridis-discovery-broker/` |
| ✅ | **Adapter self-registration (v0.6)** — `ferridis-adapter-sdk::broker`: `BrokerRegistration` RAII handle (heartbeat 240 s, abort-on-drop), `RegistrationPersistence` enum (Ephemeral/Pinned), `ServiceKind` enum (Mcp/Ferridis). `ferridis-adapter-fs` and `ferridis-adapter-claude-cli` serve examples wired with `--discovery-broker`, `--service-name`, `--pinned`. | `rust/crates/ferridis-adapter-sdk/src/broker.rs` |
| ✅ | **`ferridis-stdio-bridge` (v0.6)** — binary crate bridging any stdio MCP server to HTTP SSE (session-per-connection, `start_kill` on drop). `--command`, `--arg`, `--bind 127.0.0.1:7826`, `--discovery-broker`, `--service-name`, `--pinned`. TDP: `Command`, `SessionId`, `SpawnConfig::spawn() -> SpawnedChild`. | `rust/crates/ferridis-stdio-bridge/` |

## Layer 3 — Web-native specs

| Status | Feature | Where |
|---|---|---|
| ✅ | Schema URL parsing into `url::Url` | `rust/crates/ferridis-core/src/manifest.rs` |
| ✅ | Lazy schema fetching (raw bytes + content type, opaque to parsers) | `rust/crates/ferridis-protocol/src/schema.rs` |
| ✅ | Adapter-side schema serving (embedded or redirect) | `rust/crates/ferridis-adapter-sdk/src/server.rs` |
| ✅ | **Schema-driven request validation at the dispatch boundary** — `jsonschema 0.30` validates outbound bodies against `CapabilityRecord.input_schemas[intent]`; typed `InvalidArgs { capability, intent, details }`; lenient on malformed schemas (logs WARN, lets adapter remain final validator) | `rust/crates/ferridis-client/src/client.rs` |
| ✅ | **MCP `inputSchema` verbatim through the projection** — ferridis-client preserves MCP server schemas end-to-end so integer/typed args round-trip cleanly through the publisher | `rust/crates/ferridis-client/src/mcp/`, `rust/crates/ferridis-mcp-server/` |
| ✅ | **Adapter-SDK request validation** — `validate_body` + `ValidationOutcome` + `Capability::body_schema` hook + server gate (mirror of the client-side check, second line of defence) | `rust/crates/ferridis-adapter-sdk/src/server.rs` |
| 💡 | AsyncAPI schema fetching for event channels | future |

## Layer 4 — Bidirectional channels

| Status | Feature | Where |
|---|---|---|
| ✅ | Event types + `EventPublisher` trait | `rust/crates/ferridis-adapter-sdk/src/events.rs` |
| ✅ | Webhook event delivery | `rust/crates/ferridis-adapter-sdk/src/events.rs` |
| ✅ | **SSE event subscriptions (server → client)** — `ferridis-protocol::events::subscribe(url)` returns `Stream<Item = Result<ServerEvent, _>>`; standard SSE wire format with multi-line `data:`, chunk-boundary handling, `\r\n` tolerance | `rust/crates/ferridis-protocol/src/events.rs` |
| ✅ | **Manifest-declared event channels** — `event_channels: [{name, chunk_schema_url?}]` on the manifest; `Client::subscribe` rejects undeclared channels with typed `EventChannelNotDeclared` | `rust/crates/ferridis-core/src/manifest.rs`, `rust/crates/ferridis-client/src/client.rs` |
| ✅ | **WebSocket bidi transport** — `connect_ws(url, bearer) -> WsConnection` splits into independent `WsSender`/`WsReceiver`. Typed `ClientMessage` enum upstream (`Subscribe`/`Unsubscribe`/`Ack`/`Control`); reused `ServerEvent` downstream | `rust/crates/ferridis-protocol/src/ws.rs` |
| ✅ | **Streaming responses (request kind: `"stream"`)** — server emits ordered SSE chunks for streamed intents; `Client::dispatch_streaming` returns `Pin<Box<dyn Stream<Item = Result<Value, _>>>>`; client-side kind checks (`IntentRequiresStreaming`, `IntentNotStreaming`); MCP-backed one-shot bridge wraps `tools/call` response as a stream-of-one | `rust/crates/ferridis-client/src/client.rs`, `rust/crates/ferridis-adapter-sdk/src/server.rs` |
| ✅ | **`Client::subscribe_ws`** — convenience on top of the WS primitive; `ClientError::WrongTransport` when the channel's declared transport is SSE | `rust/crates/ferridis-client/src/client.rs` |
| ✅ | **Adapter-SDK WebSocket handler trait** — `WsHandler` with `WsConnId` + `WsMessage` newtypes | `rust/crates/ferridis-adapter-sdk/src/server.rs` |
| ✅ | **SSE auto-reconnect with `Last-Event-ID` cursor** — `ReconnectCursor` + `subscribe_with_cursor` | `rust/crates/ferridis-protocol/src/events.rs` |

## Layer 5 — Intent layer

| Status | Feature | Where |
|---|---|---|
| ✅ | Intent → manifest matching, tier-aware (`Personal → Org → Public`) | `rust/crates/ferridis-client/src/registry.rs` |
| ✅ | Auth-method-aware dispatch (`None` anonymous / `Oauth2` bearer / `ApiKey` typed-unsupported) | `rust/crates/ferridis-client/src/client.rs` |
| ✅ | **Intent vocabulary v0 — drafted** (35 verbs across 8 categories: messaging, calendar, files, search, payments, identity, scheduling, common). Proposed for AAIF governance. | [`vocabulary.md`](./vocabulary.md) |
| ✅ | **`IntentKind::Stream` plumbing through manifests + adapter SDK + client** (see Layer 4) | `rust/crates/ferridis-core/src/manifest.rs` |
| ✅ | **Confidence scoring + tie-breaking** — `ConfidenceScore` + `ScoredCandidate`; `Registry::candidates_for_intent_scored` (Personal=100, Org=66, Public=33, lexicographic tie-breaking) | `rust/crates/ferridis-client/src/registry.rs` |
| 💡 | Cross-capability intent transactions | future |

## Layer 6 — Browser / computer-use fallback

| Status | Feature | Where |
|---|---|---|
| ✅ | Tier-used logging on every dispatch (`tracing::info!` with capability/intent/tier/auth/connection_id) | `rust/crates/ferridis-client/src/client.rs` |
| 📋 | Per-service tier override (user-facing policy) | `ferridis-client` |
| 💡 | Browser session driver | future |
| 💡 | Computer-use vision driver | future |

---

## IDE integrations

| Status | Feature | Where |
|---|---|---|
| ✅ | **MCP-publisher shim — `ferridis-mcp-server`**. Works for any MCP-aware client (Claude Code, Zed, Cursor, MCP-supporting Copilot, claude.ai web, mobile Claude). Partial-failure-tolerant adapter registration with per-adapter 10s timeout. Stream-kind intents (e.g. the Claude-CLI adapter) are collected via `dispatch_streaming` and the final `result` text is returned alongside the full chunk array. **Two transports**: default stdio (in-editor MCP hosts) and `--http-bind` (MCP Streamable HTTP). **HTTP auth, two paths**: static `--bearer-token` for `curl`/non-OAuth callers, and `--oauth-public-base-url` to spin a full **OAuth 2.1 + PKCE (S256) + Dynamic Client Registration** authorization server alongside `/mcp` — the shape claude.ai's Custom Connectors actually require. Either or both. End-to-end-verified: claude.ai-shaped DCR → authorize → token → `/mcp` tools/call against real `claude`. | `rust/crates/ferridis-mcp-server/` |
| ✅ | **MCP-consumer shim — `ferridis-client::mcp`**. Lets `ferridis-client` consume MCP servers as `Tier::Native` capabilities; stdio and SSE transports. **SSE session-loss auto-recovery** — on `POST → 404`, transport re-handshakes the session, replays `initialize`, retries the original call once. Live-validated against a real MCP SSE endpoint. | `rust/crates/ferridis-client/src/mcp/` |
| ✅ | **VS Code Ferridis integration** — TypeScript extension registers `ferridis-mcp-server` with VS Code's native MCP runtime; Ferridis-connected capabilities appear as tools in Copilot Chat | `editors/vscode/` |
| ✅ | Zed Ferridis integration (Rust → WASM extension that registers `ferridis-mcp-server` as a Zed context server) | `editors/zed/` |
| 📋 | Zed pure-WASM Ferridis client (no subprocess) | `editors/zed/` follow-up |

The MCP-publisher shim is the headline integration: drop the binary path into your client's MCP server config (e.g. `~/.claude.json`'s `mcpServers`) alongside any other MCP server you already use, and Ferridis-connected capabilities appear as MCP tools in the same session. Verified end-to-end in Claude Code in VS Code with multiple MCP servers running side by side.

The VS Code extension takes the native MCP path: it registers `ferridis-mcp-server` directly in VS Code, so Ferridis tools appear in Copilot Chat alongside any other MCP servers already configured.

## Reference adapters

| Status | Adapter | Notes |
|---|---|---|
| ✅ | Filesystem (`ferridis-adapter-fs`) | First end-to-end demo. Five intents: `read-file`, `write-file`, `list-dir`, `search-files`, `move-file`. Path-traversal protection via the `Root` / `RelPath` typestate. 7 integration tests pass against a real HTTP server. systemd user unit ships for auto-start. |
| ✅ | **Claude Code CLI (`ferridis-adapter-claude-cli`)** | Drives the `claude` CLI as a stream-kind Ferridis capability. Two intents: `submit-prompt` (fresh session) and `resume-session` (continue by id), both streaming the CLI's `--output-format stream-json --verbose` events as ordered chunks over SSE. Typestate-validated inputs (`Prompt`, `AllowedCwd` against operator-supplied allow-list, `Model` enum gated by `ModelAllowList`, UUID-validated `SessionId`). **Stopgap for remote-controlling Claude Code while editor agent panels (Zed, Cursor, others) lack a public extension API for their assistant surface.** Verified end-to-end against a real `claude` install: prompt → PONG result + session_id, then resume-session continues the same conversation with cache-read evidence. 7 unit + 3 hermetic e2e tests against a stub script + 1 `#[ignore]`'d live test. |
| ✅ | **Google Calendar (`ferridis-adapter-google-calendar`)** | 6 intents: `list-calendars`, `list-events`, `get-event`, `create-event`, `update-event`, `delete-event`. OAuth 2.0 access token held server-side; optional refresh credentials (refresh token + client ID + client secret) for automatic token renewal on 401. `--api-base-url` override for proxies. Default port 7828. 7 unit + 9 e2e tests via wiremock. |
| ✅ | **Slack (`ferridis-adapter-slack`)** | 6 intents: `list-channels`, `post-message`, `get-messages`, `send-dm`, `get-channel-info`, `list-users`. Long-lived bot token (xoxb-) held server-side (local-trust model). `send-dm` is a two-step operation: `conversations.open` → `chat.postMessage`. Slack's always-200 error model handled transparently in the client layer. Default port 7829. 2 unit + 9 e2e tests via wiremock. |
| ✅ | **GitHub (`ferridis-adapter-github`)** | 6 intents: `list-repos`, `get-file`, `list-issues`, `create-issue`, `list-pull-requests`, `search-code`. PAT held server-side (local-trust model). `--api-base-url` for GitHub Enterprise. Broker self-registration. 7 unit + 8 e2e tests via wiremock. Default port 7827. |
| ✅ | **Notion (`ferridis-adapter-notion`)** | 6 intents: `list-databases`, `query-database`, `get-page`, `create-page`, `update-page`, `search`. Integration token held server-side (local-trust model). Mandatory `Notion-Version: 2022-06-28` header on every request. `query-database` and `update-page` strip routing fields before forwarding to the API. Default port 7830. 4 unit + 9 e2e tests via wiremock. |

---

## Hardening — landed in v0.2 / v0.3 / v0.4 / v0.6

These are the gaps that v0.1 deliberately deferred, now closed:

- ✅ Tightened `serde(transparent)` newtypes (`IntentVerb`, `Category`, `Summary`, `CapabilityVersion`) to use `try_from = "String"` so direct deserialization runs validation. Catches malformed JSON like `"Send_Message"` at the deserialize boundary.
- ✅ Schema-driven request validation at the client dispatch boundary (`jsonschema 0.30`, `InvalidArgs` error).
- ✅ Manifest signing format and verification (Sigstore — see Layer 2).
- ✅ Streaming response schema language (see Layer 4).
- ✅ HTTP-client pooling hardening: `pool_idle_timeout(15s)`, `pool_max_idle_per_host(2)`, `connect_timeout(5s)` to prevent the stale-keepalive wedge symptom seen during adapter restarts.
- ✅ **CRL-based certificate revocation (v0.6).** `RevocationMode` enum (BestEffort/Required/Skip), `CrlData` opaque newtype, `extract_cdp_url`, `fetch_crl`, `check_cert_not_revoked`, `verify_signed_manifest_with_revocation`. Sigstore signing now does revocation by default.

## Still queued

- 📋 CT log validation (CRL revocation shipped v0.6; CT log check is the remaining gap).
- 💡 Local-capability trust model.
