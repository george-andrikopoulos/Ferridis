//! Domain types for the Slack adapter.

use secrecy::{ExposeSecret, SecretString};

// ---------------------------------------------------------------------------
// BotToken
// ---------------------------------------------------------------------------

/// A Slack Bot Token (`xoxb-…`).
///
/// Kept behind [`secrecy::SecretString`] so it cannot leak into logs or
/// `Debug` output.
pub struct BotToken(SecretString);

impl BotToken {
    /// Parse and wrap a bot token string.
    ///
    /// Returns [`SlackError::EmptyToken`] for an empty string.
    pub fn parse(s: impl Into<String>) -> Result<Self, SlackError> {
        let s = s.into();
        if s.is_empty() {
            return Err(SlackError::EmptyToken);
        }
        Ok(BotToken(SecretString::new(s)))
    }

    pub(crate) fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl std::fmt::Debug for BotToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotToken([REDACTED])")
    }
}

// ---------------------------------------------------------------------------
// SlackError
// ---------------------------------------------------------------------------

/// Errors that can originate inside the Slack adapter.
#[derive(Debug, thiserror::Error)]
pub enum SlackError {
    /// The bot token string was empty.
    #[error("Slack bot token must not be empty")]
    EmptyToken,

    /// The Slack Web API returned `"ok": false`.
    ///
    /// The `code` field is the machine-readable error string from the API
    /// (e.g. `"channel_not_found"`, `"not_authed"`).
    #[error("Slack API error: {code}")]
    ApiError {
        /// Machine-readable Slack error code.
        code: String,
    },

    /// The Slack API rate-limited the request.
    #[error("rate limited by Slack API — retry-after: {retry_after_secs:?}s")]
    RateLimited {
        /// Value of the `Retry-After` header, if present.
        retry_after_secs: Option<u64>,
    },

    /// An HTTP transport error from `reqwest`.
    #[error("Slack API transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The API response body could not be deserialized.
    #[error("failed to deserialize Slack response: {0}")]
    Deserialize(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_parse_rejects_empty() {
        assert!(matches!(BotToken::parse(""), Err(SlackError::EmptyToken)));
    }

    #[test]
    fn token_parse_accepts_valid() {
        let tok = BotToken::parse("xoxb-test").expect("valid token");
        assert_eq!(tok.expose(), "xoxb-test");
    }
}
