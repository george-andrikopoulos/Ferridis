//! Google Calendar reference adapter for Ferridis.
//!
//! This is the canonical example of an OAuth 2.0 adapter. The operator
//! supplies a Google OAuth 2.0 access token (and optionally refresh
//! credentials for auto-renewal). The adapter exposes six intents that
//! cover the core Google Calendar v3 API surface.
//!
//! # Intents
//!
//! | Intent | Description |
//! |---|---|
//! | `list-calendars` | List all calendars the authenticated user has access to |
//! | `list-events` | List events from a calendar |
//! | `get-event` | Fetch a single event by ID |
//! | `create-event` | Create a new event |
//! | `update-event` | Update an existing event |
//! | `delete-event` | Delete an event |
//!
//! # Auth model
//!
//! The OAuth token is held server-side by the adapter process. The
//! Ferridis manifest advertises `"auth": {"type": "none"}` — clients
//! trust the adapter to authenticate on their behalf (local-trust model).
//!
//! If refresh credentials are supplied via
//! [`GoogleCalendarCapability::with_oauth_credentials`], the adapter
//! automatically refreshes the access token on 401 responses.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod capability;
mod client;
mod types;

pub use capability::GoogleCalendarCapability;
pub use types::{AccessToken, CalendarId, ClientId, ClientSecret, OAuthCredentials, RefreshToken};
