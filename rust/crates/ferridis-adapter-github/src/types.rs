//! Domain types for the GitHub adapter.
//!
//! Every primitive that crosses a function boundary has its own type.
//! Constructors are the only place validation logic lives — downstream
//! code can rely on every value already being valid.

use secrecy::{ExposeSecret, SecretString};

// ---------------------------------------------------------------------------
// GitHubToken
// ---------------------------------------------------------------------------

/// A GitHub personal access token (classic or fine-grained PAT).
///
/// The inner value is kept behind [`secrecy::SecretString`] so it cannot
/// leak into logs or `Debug` output. The only path to the raw bytes is
/// [`GitHubToken::expose`], which is intentionally `pub(crate)`.
pub struct GitHubToken(SecretString);

impl GitHubToken {
    /// Parse and wrap a token string.
    ///
    /// Returns [`GitHubError::EmptyToken`] for an empty string.
    pub fn parse(s: impl Into<String>) -> Result<Self, GitHubError> {
        let s = s.into();
        if s.is_empty() {
            return Err(GitHubError::EmptyToken);
        }
        Ok(GitHubToken(SecretString::new(s)))
    }

    /// Expose the raw token bytes — only callable inside this crate.
    pub(crate) fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl std::fmt::Debug for GitHubToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitHubToken([REDACTED])")
    }
}

// ---------------------------------------------------------------------------
// RepoOwner
// ---------------------------------------------------------------------------

/// A GitHub owner — a user login or organisation name.
///
/// GitHub imposes: non-empty, max 39 characters, no leading or trailing
/// hyphens, no consecutive hyphens. We enforce the length bound; further
/// character validation is left to the GitHub API (it returns a typed error
/// on bad logins, which the client surfaces as [`GitHubError::ApiError`]).
pub struct RepoOwner(String);

impl RepoOwner {
    /// Parse an owner name.
    pub fn parse(s: impl Into<String>) -> Result<Self, GitHubError> {
        let s = s.into();
        if s.is_empty() {
            return Err(GitHubError::EmptyOwner);
        }
        if s.len() > 39 {
            return Err(GitHubError::InvalidOwner(s));
        }
        Ok(RepoOwner(s))
    }

    /// View the owner as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RepoOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RepoOwner({:?})", self.0)
    }
}

// ---------------------------------------------------------------------------
// RepoName
// ---------------------------------------------------------------------------

/// A GitHub repository name.
///
/// Non-empty; further character validation is left to the GitHub API.
pub struct RepoName(String);

impl RepoName {
    /// Parse a repository name.
    pub fn parse(s: impl Into<String>) -> Result<Self, GitHubError> {
        let s = s.into();
        if s.is_empty() {
            return Err(GitHubError::EmptyRepoName);
        }
        Ok(RepoName(s))
    }

    /// View the name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RepoName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RepoName({:?})", self.0)
    }
}

// ---------------------------------------------------------------------------
// GitHubError
// ---------------------------------------------------------------------------

/// Errors that can originate inside the GitHub adapter.
#[derive(Debug, thiserror::Error)]
pub enum GitHubError {
    /// The provided token string was empty.
    #[error("GitHub token must not be empty")]
    EmptyToken,

    /// The owner string was empty.
    #[error("GitHub owner must not be empty")]
    EmptyOwner,

    /// The owner string exceeded GitHub's 39-character login limit.
    #[error("invalid GitHub owner `{0}` (max 39 characters)")]
    InvalidOwner(String),

    /// The repository name string was empty.
    #[error("GitHub repository name must not be empty")]
    EmptyRepoName,

    /// The GitHub API returned a non-2xx status.
    #[error("GitHub API error {status}: {message}")]
    ApiError {
        /// HTTP status code.
        status: u16,
        /// The `message` field from the GitHub error body, or the raw body.
        message: String,
    },

    /// The GitHub API rate-limited the request.
    #[error("rate limited by GitHub API — retry-after: {retry_after_secs:?}s")]
    RateLimited {
        /// Value of the `Retry-After` header, if present.
        retry_after_secs: Option<u64>,
    },

    /// An HTTP transport error from `reqwest`.
    #[error("GitHub API transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The GitHub API response body could not be deserialized.
    #[error("failed to deserialize GitHub response: {0}")]
    Deserialize(String),

    /// The file content returned by GitHub was not valid base64.
    #[error("file content is not valid base64: {0}")]
    InvalidBase64(String),

    /// The decoded file bytes were not valid UTF-8.
    #[error("file content is not valid UTF-8: {0}")]
    NotUtf8(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_parse_rejects_empty() {
        assert!(matches!(
            GitHubToken::parse(""),
            Err(GitHubError::EmptyToken)
        ));
    }

    #[test]
    fn token_parse_accepts_valid() {
        let tok = GitHubToken::parse("ghp_abc123").expect("non-empty token");
        assert_eq!(tok.expose(), "ghp_abc123");
    }

    #[test]
    fn repo_owner_parse_rejects_empty() {
        assert!(matches!(RepoOwner::parse(""), Err(GitHubError::EmptyOwner)));
    }

    #[test]
    fn repo_owner_parse_rejects_too_long() {
        let long = "a".repeat(40);
        assert!(matches!(
            RepoOwner::parse(long),
            Err(GitHubError::InvalidOwner(_))
        ));
    }

    #[test]
    fn repo_owner_parse_accepts_valid() {
        let owner = RepoOwner::parse("octocat").expect("valid owner");
        assert_eq!(owner.as_str(), "octocat");
    }

    #[test]
    fn repo_name_parse_rejects_empty() {
        assert!(matches!(
            RepoName::parse(""),
            Err(GitHubError::EmptyRepoName)
        ));
    }

    #[test]
    fn repo_name_parse_accepts_valid() {
        let name = RepoName::parse("hello-world").expect("valid name");
        assert_eq!(name.as_str(), "hello-world");
    }
}
