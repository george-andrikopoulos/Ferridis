//! Domain types for the Google Calendar adapter.
//!
//! Every primitive that crosses a function boundary has its own type.
//! Constructors are the only place validation logic lives — downstream
//! code can rely on every value already being valid.

use secrecy::{ExposeSecret, SecretString};

// ---------------------------------------------------------------------------
// AccessToken
// ---------------------------------------------------------------------------

/// A Google OAuth 2.0 access token.
///
/// Kept behind [`secrecy::SecretString`] so it cannot leak into logs or
/// `Debug` output. Exposed only inside this crate via [`AccessToken::expose`].
pub struct AccessToken(SecretString);

impl AccessToken {
    /// Parse and wrap an access token string.
    ///
    /// Returns [`GoogleCalendarError::EmptyToken`] for an empty string.
    pub fn parse(s: impl Into<String>) -> Result<Self, GoogleCalendarError> {
        let s = s.into();
        if s.is_empty() {
            return Err(GoogleCalendarError::EmptyToken);
        }
        Ok(AccessToken(SecretString::new(s)))
    }

    /// Expose the raw token — only callable inside this crate.
    pub(crate) fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken([REDACTED])")
    }
}

// ---------------------------------------------------------------------------
// RefreshToken
// ---------------------------------------------------------------------------

/// A Google OAuth 2.0 refresh token used to obtain new access tokens.
pub struct RefreshToken(SecretString);

impl RefreshToken {
    /// Parse and wrap a refresh token string.
    pub fn parse(s: impl Into<String>) -> Result<Self, GoogleCalendarError> {
        let s = s.into();
        if s.is_empty() {
            return Err(GoogleCalendarError::EmptyRefreshToken);
        }
        Ok(RefreshToken(SecretString::new(s)))
    }

    pub(crate) fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl std::fmt::Debug for RefreshToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RefreshToken([REDACTED])")
    }
}

// ---------------------------------------------------------------------------
// ClientId
// ---------------------------------------------------------------------------

/// The OAuth 2.0 client ID used for token refresh.
pub struct ClientId(String);

impl ClientId {
    /// Parse a client ID.
    pub fn parse(s: impl Into<String>) -> Result<Self, GoogleCalendarError> {
        let s = s.into();
        if s.is_empty() {
            return Err(GoogleCalendarError::EmptyClientId);
        }
        Ok(ClientId(s))
    }

    /// View the value as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClientId({:?})", self.0)
    }
}

// ---------------------------------------------------------------------------
// ClientSecret
// ---------------------------------------------------------------------------

/// The OAuth 2.0 client secret used for token refresh.
pub struct ClientSecret(SecretString);

impl ClientSecret {
    /// Parse a client secret.
    pub fn parse(s: impl Into<String>) -> Result<Self, GoogleCalendarError> {
        let s = s.into();
        if s.is_empty() {
            return Err(GoogleCalendarError::EmptyClientSecret);
        }
        Ok(ClientSecret(SecretString::new(s)))
    }

    pub(crate) fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl std::fmt::Debug for ClientSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClientSecret([REDACTED])")
    }
}

// ---------------------------------------------------------------------------
// CalendarId
// ---------------------------------------------------------------------------

/// A Google Calendar ID.
///
/// The special value `"primary"` refers to the user's primary calendar.
/// Other calendars use an email-like identifier.
pub struct CalendarId(String);

impl CalendarId {
    /// Parse a calendar ID.
    pub fn parse(s: impl Into<String>) -> Result<Self, GoogleCalendarError> {
        let s = s.into();
        if s.is_empty() {
            return Err(GoogleCalendarError::EmptyCalendarId);
        }
        Ok(CalendarId(s))
    }

    /// View the value as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for CalendarId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CalendarId({:?})", self.0)
    }
}

// ---------------------------------------------------------------------------
// OAuthCredentials
// ---------------------------------------------------------------------------

/// Bundled OAuth 2.0 refresh credentials for automatic token renewal.
///
/// When supplied to [`GoogleCalendarCapability::with_oauth_credentials`],
/// the adapter refreshes the access token automatically on 401 responses.
pub struct OAuthCredentials {
    pub(crate) refresh_token: RefreshToken,
    pub(crate) client_id: ClientId,
    pub(crate) client_secret: ClientSecret,
}

impl OAuthCredentials {
    /// Bundle the three required OAuth 2.0 refresh credentials.
    pub fn new(
        refresh_token: RefreshToken,
        client_id: ClientId,
        client_secret: ClientSecret,
    ) -> Self {
        Self {
            refresh_token,
            client_id,
            client_secret,
        }
    }
}

impl std::fmt::Debug for OAuthCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthCredentials")
            .field("client_id", &self.client_id)
            .field("refresh_token", &self.refresh_token)
            .field("client_secret", &"[REDACTED]")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// GoogleCalendarError
// ---------------------------------------------------------------------------

/// Errors that can originate inside the Google Calendar adapter.
#[derive(Debug, thiserror::Error)]
pub enum GoogleCalendarError {
    /// The access token string was empty.
    #[error("Google Calendar access token must not be empty")]
    EmptyToken,

    /// The refresh token string was empty.
    #[error("OAuth 2.0 refresh token must not be empty")]
    EmptyRefreshToken,

    /// The OAuth 2.0 client ID was empty.
    #[error("OAuth 2.0 client ID must not be empty")]
    EmptyClientId,

    /// The OAuth 2.0 client secret was empty.
    #[error("OAuth 2.0 client secret must not be empty")]
    EmptyClientSecret,

    /// The calendar ID was empty.
    #[error("calendar ID must not be empty")]
    EmptyCalendarId,

    /// The event ID was empty.
    #[error("event ID must not be empty")]
    EmptyEventId,

    /// The access token expired and no refresh credentials were supplied.
    #[error("access token expired — supply OAuth refresh credentials for automatic renewal")]
    TokenExpired,

    /// The token refresh request failed.
    #[error("OAuth token refresh failed: {0}")]
    TokenRefreshFailed(String),

    /// The Google Calendar API returned a non-2xx status.
    #[error("Google Calendar API error {status}: {message}")]
    ApiError {
        /// HTTP status code.
        status: u16,
        /// The `message` field from the error body, or the raw body.
        message: String,
    },

    /// The Google Calendar API rate-limited the request.
    #[error("rate limited by Google Calendar API — retry-after: {retry_after_secs:?}s")]
    RateLimited {
        /// Value of the `Retry-After` header, if present.
        retry_after_secs: Option<u64>,
    },

    /// An HTTP transport error from `reqwest`.
    #[error("Google Calendar API transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The API response body could not be deserialized.
    #[error("failed to deserialize Google Calendar response: {0}")]
    Deserialize(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_token_rejects_empty() {
        assert!(matches!(
            AccessToken::parse(""),
            Err(GoogleCalendarError::EmptyToken)
        ));
    }

    #[test]
    fn access_token_accepts_valid() {
        let tok = AccessToken::parse("ya29.test").expect("valid token");
        assert_eq!(tok.expose(), "ya29.test");
    }

    #[test]
    fn refresh_token_rejects_empty() {
        assert!(matches!(
            RefreshToken::parse(""),
            Err(GoogleCalendarError::EmptyRefreshToken)
        ));
    }

    #[test]
    fn client_id_rejects_empty() {
        assert!(matches!(
            ClientId::parse(""),
            Err(GoogleCalendarError::EmptyClientId)
        ));
    }

    #[test]
    fn client_secret_rejects_empty() {
        assert!(matches!(
            ClientSecret::parse(""),
            Err(GoogleCalendarError::EmptyClientSecret)
        ));
    }

    #[test]
    fn calendar_id_primary_is_valid() {
        let id = CalendarId::parse("primary").expect("primary is valid");
        assert_eq!(id.as_str(), "primary");
    }

    #[test]
    fn calendar_id_rejects_empty() {
        assert!(matches!(
            CalendarId::parse(""),
            Err(GoogleCalendarError::EmptyCalendarId)
        ));
    }
}
