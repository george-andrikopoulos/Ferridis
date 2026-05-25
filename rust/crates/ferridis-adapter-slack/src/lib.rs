//! Slack reference adapter for Ferridis.
//!
//! Canonical messaging example. The operator supplies a Slack Bot Token
//! (`xoxb-…`) and the adapter exposes six intents over the Slack Web API.
//!
//! # Intents
//!
//! | Intent | Slack method |
//! |---|---|
//! | `list-channels` | `conversations.list` |
//! | `post-message` | `chat.postMessage` |
//! | `get-messages` | `conversations.history` |
//! | `send-dm` | `conversations.open` + `chat.postMessage` |
//! | `get-channel-info` | `conversations.info` |
//! | `list-users` | `users.list` |
//!
//! # Auth model
//!
//! The Bot Token is held server-side. The Ferridis manifest advertises
//! `"auth": {"type": "none"}` — local-trust model identical to the
//! GitHub and Google Calendar adapters.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod capability;
mod client;
mod types;

pub use capability::SlackCapability;
pub use types::BotToken;
