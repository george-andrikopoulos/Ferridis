//! Persistence-friendly representations of connections.
//!
//! [`StoredConnection`] is the canonical on-disk form, a tagged enum
//! that can round-trip through any Serde format. The [`as_pending`],
//! [`as_authorized`], [`as_expired`], and [`as_revoked`] methods
//! project a `StoredConnection` back into the typestate
//! [`Connection<S>`](crate::connection::Connection) that callers actually use.
//!
//! The reverse direction — turning a live `Connection<S>` into a
//! `StoredConnection` for persistence — lives on the four
//! [`StoredConnection::from_pending`] / [`from_authorized`] /
//! [`from_expired`] / [`from_revoked`] builders. Together these are
//! the only sanctioned bridge between the typestate world and the
//! persisted world.
//!
//! [`as_pending`]: StoredConnection::as_pending
//! [`as_authorized`]: StoredConnection::as_authorized
//! [`as_expired`]: StoredConnection::as_expired
//! [`as_revoked`]: StoredConnection::as_revoked
//! [`from_authorized`]: StoredConnection::from_authorized
//! [`from_expired`]: StoredConnection::from_expired
//! [`from_revoked`]: StoredConnection::from_revoked

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::capability::CapabilityRef;
use crate::connection::{Authorized, Connection, ConnectionId, Expired, Pending, Revoked};
use crate::tier::Tier;
use crate::token::{AccessToken, RefreshToken};

/// On-disk shape of a connection. Tagged by state so round-trip is possible.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StoredConnection {
    /// Pending — auth not yet completed.
    Pending {
        /// Connection ID.
        id: ConnectionId,
        /// Target capability.
        capability: CapabilityRef,
        /// Tier this connection uses.
        tier: Tier,
        /// Authorization URL.
        auth_url: String,
        /// CSRF state token.
        state_token: String,
    },
    /// Authorized — tokens valid.
    Authorized {
        /// Connection ID.
        id: ConnectionId,
        /// Target capability.
        capability: CapabilityRef,
        /// Tier this connection uses.
        tier: Tier,
        /// Access token.
        access_token: AccessToken,
        /// Refresh token, if any.
        refresh_token: Option<RefreshToken>,
        /// Expiry timestamp.
        expires_at: OffsetDateTime,
    },
    /// Expired — tokens lapsed.
    Expired {
        /// Connection ID.
        id: ConnectionId,
        /// Target capability.
        capability: CapabilityRef,
        /// Tier this connection uses.
        tier: Tier,
        /// Refresh token, if any.
        refresh_token: Option<RefreshToken>,
    },
    /// Revoked — terminal.
    Revoked {
        /// Connection ID.
        id: ConnectionId,
        /// Target capability.
        capability: CapabilityRef,
        /// Tier this connection used.
        tier: Tier,
    },
}

impl StoredConnection {
    /// The ID of the stored connection, regardless of state.
    pub fn id(&self) -> ConnectionId {
        match self {
            StoredConnection::Pending { id, .. }
            | StoredConnection::Authorized { id, .. }
            | StoredConnection::Expired { id, .. }
            | StoredConnection::Revoked { id, .. } => *id,
        }
    }

    /// The capability the stored connection targets.
    pub fn capability(&self) -> &CapabilityRef {
        match self {
            StoredConnection::Pending { capability, .. }
            | StoredConnection::Authorized { capability, .. }
            | StoredConnection::Expired { capability, .. }
            | StoredConnection::Revoked { capability, .. } => capability,
        }
    }

    // ---- Projections: StoredConnection → typestate Connection<S> ----

    /// Project into a [`Connection<Pending>`] if this stored connection
    /// is in the `Pending` state. Returns `None` otherwise.
    pub fn as_pending(&self) -> Option<Connection<Pending>> {
        match self {
            StoredConnection::Pending {
                id,
                capability,
                tier,
                auth_url,
                state_token,
            } => Some(Connection::<Pending>::from_parts(
                *id,
                capability.clone(),
                *tier,
                auth_url.clone(),
                state_token.clone(),
            )),
            _ => None,
        }
    }

    /// Project into a [`Connection<Authorized>`] if this stored
    /// connection is in the `Authorized` state. Returns `None` otherwise.
    pub fn as_authorized(&self) -> Option<Connection<Authorized>> {
        match self {
            StoredConnection::Authorized {
                id,
                capability,
                tier,
                access_token,
                refresh_token,
                expires_at,
            } => Some(Connection::<Authorized>::from_parts(
                *id,
                capability.clone(),
                *tier,
                access_token.clone(),
                refresh_token.clone(),
                *expires_at,
            )),
            _ => None,
        }
    }

    /// Project into a [`Connection<Expired>`] if this stored connection
    /// is in the `Expired` state. Returns `None` otherwise.
    pub fn as_expired(&self) -> Option<Connection<Expired>> {
        match self {
            StoredConnection::Expired {
                id,
                capability,
                tier,
                refresh_token,
            } => Some(Connection::<Expired>::from_parts(
                *id,
                capability.clone(),
                *tier,
                refresh_token.clone(),
            )),
            _ => None,
        }
    }

    /// Project into a [`Connection<Revoked>`] if this stored connection
    /// is in the `Revoked` state. Returns `None` otherwise.
    pub fn as_revoked(&self) -> Option<Connection<Revoked>> {
        match self {
            StoredConnection::Revoked {
                id,
                capability,
                tier,
            } => Some(Connection::<Revoked>::from_parts(
                *id,
                capability.clone(),
                *tier,
            )),
            _ => None,
        }
    }

    // ---- Builders: typestate Connection<S> → StoredConnection ----

    /// Build a [`StoredConnection`] from a live [`Connection<Pending>`].
    pub fn from_pending(c: &Connection<Pending>) -> Self {
        Self::Pending {
            id: c.id(),
            capability: c.capability().clone(),
            tier: c.tier(),
            auth_url: c.auth_url().to_string(),
            state_token: c.state_token().to_string(),
        }
    }

    /// Build a [`StoredConnection`] from a live [`Connection<Authorized>`].
    pub fn from_authorized(c: &Connection<Authorized>) -> Self {
        Self::Authorized {
            id: c.id(),
            capability: c.capability().clone(),
            tier: c.tier(),
            access_token: c.access_token().clone(),
            refresh_token: c.refresh_token().cloned(),
            expires_at: c.expires_at(),
        }
    }

    /// Build a [`StoredConnection`] from a live [`Connection<Expired>`].
    pub fn from_expired(c: &Connection<Expired>) -> Self {
        Self::Expired {
            id: c.id(),
            capability: c.capability().clone(),
            tier: c.tier(),
            refresh_token: c.refresh_token().cloned(),
        }
    }

    /// Build a [`StoredConnection`] from a live [`Connection<Revoked>`].
    pub fn from_revoked(c: &Connection<Revoked>) -> Self {
        Self::Revoked {
            id: c.id(),
            capability: c.capability().clone(),
            tier: c.tier(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::{AccessToken, RefreshToken};
    use time::{Duration, OffsetDateTime};

    fn cap() -> CapabilityRef {
        CapabilityRef::parse("ferridis://public.ferridis.io/test/cap@v1").unwrap()
    }

    #[test]
    fn pending_round_trips_through_stored_form() {
        let pending = Connection::<Pending>::new(cap(), Tier::Native, "https://auth/", "csrf-1");
        let id = pending.id();

        let stored = StoredConnection::from_pending(&pending);
        let json = serde_json::to_string(&stored).unwrap();
        let restored: StoredConnection = serde_json::from_str(&json).unwrap();

        let recovered = restored
            .as_pending()
            .expect("Pending stored form must project back to Connection<Pending>");
        assert_eq!(recovered.id(), id);
        assert_eq!(recovered.tier(), Tier::Native);
        assert_eq!(recovered.auth_url(), "https://auth/");
        assert_eq!(recovered.state_token(), "csrf-1");
    }

    #[test]
    fn authorized_round_trips_with_tokens_preserved() {
        let pending = Connection::<Pending>::new(cap(), Tier::Browser, "u", "s");
        let expires_at = OffsetDateTime::now_utc() + Duration::hours(1);
        let authed = pending.complete(
            AccessToken::new("the-access"),
            Some(RefreshToken::new("the-refresh")),
            expires_at,
        );
        let id = authed.id();

        let stored = StoredConnection::from_authorized(&authed);
        let json = serde_json::to_string(&stored).unwrap();
        let restored: StoredConnection = serde_json::from_str(&json).unwrap();

        let recovered = restored
            .as_authorized()
            .expect("Authorized stored form must project back to Connection<Authorized>");
        assert_eq!(recovered.id(), id);
        assert_eq!(recovered.tier(), Tier::Browser);
        assert_eq!(recovered.access_token().expose(), "the-access");
        assert!(recovered.can_refresh());
    }

    #[test]
    fn expired_round_trips_with_refresh_token() {
        let pending = Connection::<Pending>::new(cap(), Tier::Native, "u", "s");
        let authed = pending.complete(
            AccessToken::new("a"),
            Some(RefreshToken::new("r")),
            OffsetDateTime::now_utc() + Duration::hours(1),
        );
        let expired = authed.expire();
        let id = expired.id();

        let stored = StoredConnection::from_expired(&expired);
        let json = serde_json::to_string(&stored).unwrap();
        let restored: StoredConnection = serde_json::from_str(&json).unwrap();

        let recovered = restored.as_expired().expect("must project back");
        assert_eq!(recovered.id(), id);
        assert!(recovered.can_refresh());
    }

    #[test]
    fn revoked_round_trips() {
        let pending = Connection::<Pending>::new(cap(), Tier::Native, "u", "s");
        let authed = pending.complete(
            AccessToken::new("a"),
            None,
            OffsetDateTime::now_utc() + Duration::hours(1),
        );
        let revoked = authed.revoke();
        let id = revoked.id();

        let stored = StoredConnection::from_revoked(&revoked);
        let json = serde_json::to_string(&stored).unwrap();
        let restored: StoredConnection = serde_json::from_str(&json).unwrap();

        let recovered = restored.as_revoked().expect("must project back");
        assert_eq!(recovered.id(), id);
        assert_eq!(recovered.tier(), Tier::Native);
    }

    #[test]
    fn projections_return_none_for_wrong_state() {
        let pending = Connection::<Pending>::new(cap(), Tier::Native, "u", "s");
        let stored = StoredConnection::from_pending(&pending);

        assert!(stored.as_authorized().is_none());
        assert!(stored.as_expired().is_none());
        assert!(stored.as_revoked().is_none());
        assert!(stored.as_pending().is_some());
    }
}
