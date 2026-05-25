//! Notion reference adapter for Ferridis.
//!
//! Canonical knowledge-base adapter. The operator supplies a Notion
//! integration token. The adapter exposes six intents covering databases,
//! pages, and workspace search.
//!
//! # Intents
//!
//! | Intent | Notion API |
//! |---|---|
//! | `list-databases` | `POST /search` (filter: database) |
//! | `query-database` | `POST /databases/{id}/query` |
//! | `get-page` | `GET /pages/{id}` |
//! | `create-page` | `POST /pages` |
//! | `update-page` | `PATCH /pages/{id}` |
//! | `search` | `POST /search` |
//!
//! # Auth model
//!
//! The integration token is held server-side. The Ferridis manifest
//! advertises `"auth": {"type": "none"}` — local-trust model identical
//! to the GitHub, Google Calendar, and Slack adapters.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod capability;
mod client;
mod types;

pub use capability::NotionCapability;
pub use types::{DatabaseId, IntegrationToken, PageId};
