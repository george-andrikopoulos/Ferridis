//! Ferridis adapter that wraps the Claude Code CLI (`claude`) as a
//! stream-kind capability.
//!
//! This adapter lets any Ferridis client — and therefore any
//! MCP-aware host via [`ferridis-mcp-server`] — drive a
//! non-interactive Claude Code session over Ferridis. Each
//! `submit-prompt` or `resume-session` dispatch spawns a `claude -p
//! --output-format stream-json --verbose …` child and streams its
//! line-delimited JSON events back to the caller as ordered chunks.
//!
//! # Why this exists
//!
//! Editor agent panels (Zed's, Cursor's, others) don't yet expose a
//! public extension API for the active assistant session. Until they
//! do, the realistic path to "remote-control a Claude Code session"
//! is to drive the CLI directly. This adapter is the stopgap; the
//! day editor panels grow a control surface, a thinner adapter can
//! replace this one without the rest of the Ferridis stack noticing.
//!
//! # Type-driven discipline
//!
//! Every value that reaches the spawned subprocess passes through a
//! validating constructor first:
//!
//! - [`Prompt`] — non-empty, bounded length.
//! - [`AllowedCwd`] — only constructable against an operator-supplied
//!   [`AllowedRoots`] allow-list. Refuses paths outside it.
//! - [`Model`] — enum, not a free string; the `Custom` variant is
//!   gated behind an operator-configured [`ModelAllowList`].
//! - [`SessionId`] — UUID-validated at the adapter boundary so
//!   `claude` never sees a malformed `-r` argument.
//!
//! Construction is the validation. Downstream code can rely on every
//! `Prompt`, `AllowedCwd`, `Model`, and `SessionId` being safe to
//! propagate as a process argument.
//!
//! [`ferridis-mcp-server`]: https://docs.rs/ferridis-mcp-server

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod capability;
pub mod input;

pub use capability::{CAPABILITY_ID, ClaudeCliCapability, ClaudeCliConfig};
pub use input::{AllowedCwd, AllowedRoots, InputError, Model, ModelAllowList, Prompt, SessionId};
