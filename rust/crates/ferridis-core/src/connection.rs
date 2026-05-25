//! Connection lifecycle as a typestate state machine.
//!
//! [`Connection<S>`] carries its lifecycle state in the type parameter `S`.
//! State transitions consume `self`, so an old state can never be reused
//! after a transition. Methods that only make sense in a particular state
//! are defined only on that state — the compiler refuses to build code
//! that, for example, tries to `expire()` a [`Pending`] connection or
//! `refresh()` a [`Revoked`] one.
//!
//! ```
//! use ferridis_core::{
//!     CapabilityRef, Connection, AccessToken, Pending, Tier,
//! };
//! use time::{Duration, OffsetDateTime};
//!
//! let cap = CapabilityRef::parse("ferridis://public.ferridis.io/test/cap@v1").unwrap();
//! let pending = Connection::<Pending>::new(cap, Tier::Native, "https://auth/", "csrf-1");
//! let authed = pending.complete(
//!     AccessToken::new("token"),
//!     None,
//!     OffsetDateTime::now_utc() + Duration::hours(1),
//! );
//! // authed: Connection<Authorized> — calls and expiry are now permitted.
//! ```

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::capability::CapabilityRef;
use crate::tier::Tier;
use crate::token::{AccessToken, RefreshToken};

/// A unique identifier for a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectionId(Uuid);

impl ConnectionId {
    /// Generate a fresh connection ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Build a connection ID from an existing UUID (for deserialization).
    pub fn from_uuid(u: Uuid) -> Self {
        Self(u)
    }

    /// The underlying UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for ConnectionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Sealed marker trait for connection states.
pub trait ConnectionState: private::Sealed {}

mod private {
    pub trait Sealed {}
}

/// Initial state — connection started, awaiting OAuth completion.
#[derive(Debug, Clone)]
pub struct Pending {
    auth_url: String,
    state_token: String,
}
impl private::Sealed for Pending {}
impl ConnectionState for Pending {}

/// Active state — tokens valid, calls permitted.
#[derive(Debug, Clone)]
pub struct Authorized {
    access_token: AccessToken,
    refresh_token: Option<RefreshToken>,
    expires_at: OffsetDateTime,
}
impl private::Sealed for Authorized {}
impl ConnectionState for Authorized {}

/// Tokens expired. Refresh required to make more calls.
#[derive(Debug, Clone)]
pub struct Expired {
    refresh_token: Option<RefreshToken>,
}
impl private::Sealed for Expired {}
impl ConnectionState for Expired {}

/// Terminal state — user revoked the connection.
#[derive(Debug, Clone)]
pub struct Revoked;
impl private::Sealed for Revoked {}
impl ConnectionState for Revoked {}

/// A connection between a Ferridis runtime and a service.
#[derive(Debug, Clone)]
pub struct Connection<S: ConnectionState> {
    id: ConnectionId,
    capability: CapabilityRef,
    tier: Tier,
    state: S,
}

impl<S: ConnectionState> Connection<S> {
    /// The connection's unique identifier.
    pub fn id(&self) -> ConnectionId {
        self.id
    }

    /// The capability this connection targets.
    pub fn capability(&self) -> &CapabilityRef {
        &self.capability
    }

    /// The execution tier this connection uses.
    pub fn tier(&self) -> Tier {
        self.tier
    }
}

impl Connection<Pending> {
    /// Start a new pending connection.
    pub fn new(
        capability: CapabilityRef,
        tier: Tier,
        auth_url: impl Into<String>,
        state_token: impl Into<String>,
    ) -> Self {
        Self {
            id: ConnectionId::new(),
            capability,
            tier,
            state: Pending {
                auth_url: auth_url.into(),
                state_token: state_token.into(),
            },
        }
    }

    /// Reconstruct a pending connection from previously-persisted parts.
    ///
    /// **Trust boundary.** This bypasses the OAuth round-trip that
    /// [`new`](Self::new) requires, so it must only be called by the
    /// wallet on data it just loaded from disk. Visibility is
    /// `pub(crate)` so the only path in is via [`StoredConnection`]'s
    /// projection methods, which live in the same crate.
    pub(crate) fn from_parts(
        id: ConnectionId,
        capability: CapabilityRef,
        tier: Tier,
        auth_url: String,
        state_token: String,
    ) -> Self {
        Self {
            id,
            capability,
            tier,
            state: Pending {
                auth_url,
                state_token,
            },
        }
    }

    /// The URL to send the user to for authorization.
    pub fn auth_url(&self) -> &str {
        &self.state.auth_url
    }

    /// The CSRF state token that must round-trip through the OAuth flow.
    pub fn state_token(&self) -> &str {
        &self.state.state_token
    }

    /// Complete authorization by attaching the issued tokens.
    pub fn complete(
        self,
        access_token: AccessToken,
        refresh_token: Option<RefreshToken>,
        expires_at: OffsetDateTime,
    ) -> Connection<Authorized> {
        Connection {
            id: self.id,
            capability: self.capability,
            tier: self.tier,
            state: Authorized {
                access_token,
                refresh_token,
                expires_at,
            },
        }
    }
}

impl Connection<Authorized> {
    /// The current access token.
    pub fn access_token(&self) -> &AccessToken {
        &self.state.access_token
    }

    /// When the current access token expires.
    pub fn expires_at(&self) -> OffsetDateTime {
        self.state.expires_at
    }

    /// Whether this connection has a refresh token available.
    pub fn can_refresh(&self) -> bool {
        self.state.refresh_token.is_some()
    }

    /// The refresh token, if any. Crate-private so the only consumer
    /// is the storage module, when projecting an `Authorized` connection
    /// into its persisted [`StoredConnection`] form. Userland code that
    /// needs the refresh token must first transition the connection
    /// into [`Expired`] via [`expire`](Self::expire).
    pub(crate) fn refresh_token(&self) -> Option<&RefreshToken> {
        self.state.refresh_token.as_ref()
    }

    /// Reconstruct an authorized connection from previously-persisted parts.
    /// See [`Connection<Pending>::from_parts`] for the trust-boundary note.
    pub(crate) fn from_parts(
        id: ConnectionId,
        capability: CapabilityRef,
        tier: Tier,
        access_token: AccessToken,
        refresh_token: Option<RefreshToken>,
        expires_at: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            capability,
            tier,
            state: Authorized {
                access_token,
                refresh_token,
                expires_at,
            },
        }
    }

    /// Mark this connection as expired, retaining the refresh token if any.
    pub fn expire(self) -> Connection<Expired> {
        Connection {
            id: self.id,
            capability: self.capability,
            tier: self.tier,
            state: Expired {
                refresh_token: self.state.refresh_token,
            },
        }
    }

    /// Revoke this connection. Terminal — the connection cannot be reused.
    pub fn revoke(self) -> Connection<Revoked> {
        Connection {
            id: self.id,
            capability: self.capability,
            tier: self.tier,
            state: Revoked,
        }
    }
}

impl Connection<Expired> {
    /// Whether this connection has a refresh token that can be used to revive it.
    pub fn can_refresh(&self) -> bool {
        self.state.refresh_token.is_some()
    }

    /// The refresh token, if any.
    pub fn refresh_token(&self) -> Option<&RefreshToken> {
        self.state.refresh_token.as_ref()
    }

    /// Reconstruct an expired connection from previously-persisted parts.
    /// See [`Connection<Pending>::from_parts`] for the trust-boundary note.
    pub(crate) fn from_parts(
        id: ConnectionId,
        capability: CapabilityRef,
        tier: Tier,
        refresh_token: Option<RefreshToken>,
    ) -> Self {
        Self {
            id,
            capability,
            tier,
            state: Expired { refresh_token },
        }
    }

    /// Refresh this connection using new tokens.
    pub fn refresh(
        self,
        new_access: AccessToken,
        new_refresh: Option<RefreshToken>,
        new_expires_at: OffsetDateTime,
    ) -> Connection<Authorized> {
        Connection {
            id: self.id,
            capability: self.capability,
            tier: self.tier,
            state: Authorized {
                access_token: new_access,
                refresh_token: new_refresh.or(self.state.refresh_token),
                expires_at: new_expires_at,
            },
        }
    }

    /// Revoke an expired connection. Terminal.
    pub fn revoke(self) -> Connection<Revoked> {
        Connection {
            id: self.id,
            capability: self.capability,
            tier: self.tier,
            state: Revoked,
        }
    }
}

// `Connection<Revoked>` exposes only the inherited methods on `Connection<S>`
// (`id`, `capability`, `tier`) and has no transition methods.
// Revocation is terminal, and the type system enforces that.

impl Connection<Revoked> {
    /// Reconstruct a revoked connection from previously-persisted parts.
    /// See [`Connection<Pending>::from_parts`] for the trust-boundary note.
    pub(crate) fn from_parts(id: ConnectionId, capability: CapabilityRef, tier: Tier) -> Self {
        Self {
            id,
            capability,
            tier,
            state: Revoked,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::CapabilityRef;
    use crate::tier::Tier;
    use time::Duration;

    fn cap() -> CapabilityRef {
        CapabilityRef::parse("ferridis://public.ferridis.io/test/cap@v1").unwrap()
    }

    #[test]
    fn pending_to_authorized() {
        let pending = Connection::<Pending>::new(cap(), Tier::Native, "https://auth/", "csrf-1");
        assert_eq!(pending.auth_url(), "https://auth/");
        assert_eq!(pending.state_token(), "csrf-1");

        let now = OffsetDateTime::now_utc();
        let authed = pending.complete(
            AccessToken::new("token"),
            Some(RefreshToken::new("refresh")),
            now + Duration::hours(1),
        );
        assert!(authed.can_refresh());
        assert_eq!(authed.tier(), Tier::Native);
    }

    #[test]
    fn authorized_to_expired_to_authorized() {
        let pending = Connection::<Pending>::new(cap(), Tier::Native, "u", "s");
        let authed = pending.complete(
            AccessToken::new("a"),
            Some(RefreshToken::new("r")),
            OffsetDateTime::now_utc() + Duration::hours(1),
        );
        let expired = authed.expire();
        assert!(expired.can_refresh());

        let _re = expired.refresh(
            AccessToken::new("a2"),
            None,
            OffsetDateTime::now_utc() + Duration::hours(1),
        );
    }

    #[test]
    fn revoked_is_terminal() {
        let pending = Connection::<Pending>::new(cap(), Tier::Native, "u", "s");
        let authed = pending.complete(
            AccessToken::new("a"),
            None,
            OffsetDateTime::now_utc() + Duration::hours(1),
        );
        let revoked = authed.revoke();
        // No transition methods exist on `Connection<Revoked>`.
        assert_eq!(revoked.tier(), Tier::Native);
    }

    #[test]
    fn id_is_preserved_across_transitions() {
        let pending = Connection::<Pending>::new(cap(), Tier::Native, "u", "s");
        let id = pending.id();
        let authed = pending.complete(
            AccessToken::new("a"),
            None,
            OffsetDateTime::now_utc() + Duration::hours(1),
        );
        assert_eq!(authed.id(), id);
        let expired = authed.expire();
        assert_eq!(expired.id(), id);
    }
}
