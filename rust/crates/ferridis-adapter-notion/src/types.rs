//! Domain types for the Notion adapter.

use secrecy::{ExposeSecret, SecretString};

// ---------------------------------------------------------------------------
// IntegrationToken
// ---------------------------------------------------------------------------

/// A Notion integration token (`secret_…`).
///
/// Kept behind [`secrecy::SecretString`] so it cannot leak into logs or
/// `Debug` output.
pub struct IntegrationToken(SecretString);

impl IntegrationToken {
    /// Parse and wrap an integration token string.
    ///
    /// Returns [`NotionError::EmptyToken`] for an empty string.
    pub fn parse(s: impl Into<String>) -> Result<Self, NotionError> {
        let s = s.into();
        if s.is_empty() {
            return Err(NotionError::EmptyToken);
        }
        Ok(IntegrationToken(SecretString::new(s)))
    }

    pub(crate) fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl std::fmt::Debug for IntegrationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IntegrationToken([REDACTED])")
    }
}

// ---------------------------------------------------------------------------
// DatabaseId
// ---------------------------------------------------------------------------

/// A Notion database ID.
pub struct DatabaseId(String);

impl DatabaseId {
    /// Parse a database ID.
    pub fn parse(s: impl Into<String>) -> Result<Self, NotionError> {
        let s = s.into();
        if s.is_empty() {
            return Err(NotionError::EmptyDatabaseId);
        }
        Ok(DatabaseId(s))
    }

    /// View the value as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for DatabaseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DatabaseId({:?})", self.0)
    }
}

// ---------------------------------------------------------------------------
// PageId
// ---------------------------------------------------------------------------

/// A Notion page ID.
pub struct PageId(String);

impl PageId {
    /// Parse a page ID.
    pub fn parse(s: impl Into<String>) -> Result<Self, NotionError> {
        let s = s.into();
        if s.is_empty() {
            return Err(NotionError::EmptyPageId);
        }
        Ok(PageId(s))
    }

    /// View the value as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PageId({:?})", self.0)
    }
}

// ---------------------------------------------------------------------------
// NotionError
// ---------------------------------------------------------------------------

/// Errors that can originate inside the Notion adapter.
#[derive(Debug, thiserror::Error)]
pub enum NotionError {
    /// The integration token string was empty.
    #[error("Notion integration token must not be empty")]
    EmptyToken,

    /// The database ID was empty.
    #[error("database ID must not be empty")]
    EmptyDatabaseId,

    /// The page ID was empty.
    #[error("page ID must not be empty")]
    EmptyPageId,

    /// The Notion API returned a non-2xx status.
    #[error("Notion API error {status} ({code}): {message}")]
    ApiError {
        /// HTTP status code.
        status: u16,
        /// Notion machine-readable error code (e.g. `"validation_error"`).
        code: String,
        /// Human-readable error message from Notion.
        message: String,
    },

    /// The Notion API rate-limited the request.
    #[error("rate limited by Notion API — retry-after: {retry_after_secs:?}s")]
    RateLimited {
        /// Value of the `Retry-After` header, if present.
        retry_after_secs: Option<u64>,
    },

    /// An HTTP transport error from `reqwest`.
    #[error("Notion API transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The API response body could not be deserialized.
    #[error("failed to deserialize Notion response: {0}")]
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
        assert!(matches!(IntegrationToken::parse(""), Err(NotionError::EmptyToken)));
    }

    #[test]
    fn token_parse_accepts_valid() {
        let tok = IntegrationToken::parse("secret_test").expect("valid token");
        assert_eq!(tok.expose(), "secret_test");
    }

    #[test]
    fn database_id_rejects_empty() {
        assert!(matches!(DatabaseId::parse(""), Err(NotionError::EmptyDatabaseId)));
    }

    #[test]
    fn page_id_rejects_empty() {
        assert!(matches!(PageId::parse(""), Err(NotionError::EmptyPageId)));
    }
}
