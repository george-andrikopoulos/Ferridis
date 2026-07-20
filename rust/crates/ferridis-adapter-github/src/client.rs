//! Thin async wrapper around the GitHub REST API v3.
//!
//! The adapter calls this client for every intent; the client holds the PAT
//! and injects it on every request. All response bodies are returned as
//! [`serde_json::Value`] so the capability layer can shape them for callers
//! without the client needing to know the intent context.

use base64::Engine as _;
use serde_json::Value;

use crate::types::{GitHubError, GitHubToken, RepoName, RepoOwner};

/// GitHub REST API base URL — production.
const GITHUB_API: &str = "https://api.github.com";

/// The `Accept` header value GitHub requires for v3 JSON responses.
const ACCEPT_GITHUB_JSON: &str = "application/vnd.github+json";

/// The `X-GitHub-Api-Version` header value (pinned).
const GITHUB_API_VERSION: &str = "2022-11-28";

// ---------------------------------------------------------------------------
// GitHubClient
// ---------------------------------------------------------------------------

/// Async client for the GitHub REST API.
///
/// The `base_url` is configurable so tests can point at a local stub server,
/// and so operators running GitHub Enterprise Server can override the endpoint.
pub(crate) struct GitHubClient {
    http: reqwest::Client,
    token: GitHubToken,
    base_url: String,
}

impl GitHubClient {
    /// Construct a client targeting the public GitHub API.
    pub(crate) fn new(token: GitHubToken) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "ferridis-adapter-github/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .expect("reqwest::Client construction with default TLS cannot fail"); // allow:expect
        Self {
            http,
            token,
            base_url: GITHUB_API.to_owned(),
        }
    }

    /// Override the API base URL.
    ///
    /// Used in tests to point at a wiremock stub and by operators targeting a
    /// GitHub Enterprise Server instance (`https://HOSTNAME/api/v3`).
    pub fn with_api_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    // -----------------------------------------------------------------------
    // Intent-level methods
    // -----------------------------------------------------------------------

    /// List repositories for `owner` (user or organisation).
    ///
    /// `kind` controls the filter (`all`, `public`, `private`, `forks`,
    /// `sources`, `member`). `per_page` is capped at 100 by the GitHub API.
    pub(crate) async fn list_repos(
        &self,
        owner: &RepoOwner,
        kind: Option<&str>,
        per_page: u8,
    ) -> Result<Value, GitHubError> {
        let per_page = per_page.min(100);
        let kind = kind.unwrap_or("all");
        let path = format!(
            "/users/{}/repos?type={}&per_page={}",
            owner.as_str(),
            kind,
            per_page
        );
        self.get(&path).await
    }

    /// Fetch a file's contents from a repository.
    ///
    /// Returns the decoded UTF-8 text alongside path metadata.
    /// `git_ref` defaults to the repository's default branch when `None`.
    pub(crate) async fn get_file(
        &self,
        owner: &RepoOwner,
        repo: &RepoName,
        path: &str,
        git_ref: Option<&str>,
    ) -> Result<Value, GitHubError> {
        let query = git_ref.map(|r| format!("?ref={}", r)).unwrap_or_default();
        let api_path = format!(
            "/repos/{}/{}/contents/{}{}",
            owner.as_str(),
            repo.as_str(),
            path.trim_start_matches('/'),
            query
        );
        let raw: Value = self.get(&api_path).await?;

        // Decode base64 content (GitHub strips newlines inserted for readability).
        let encoded = raw
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .replace('\n', "");

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| GitHubError::InvalidBase64(e.to_string()))?;

        let text = String::from_utf8(bytes).map_err(|e| GitHubError::NotUtf8(e.to_string()))?;

        Ok(serde_json::json!({
            "path":    raw.get("path").and_then(|v| v.as_str()).unwrap_or(path),
            "sha":     raw.get("sha").and_then(|v| v.as_str()).unwrap_or(""),
            "size":    raw.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
            "content": text,
        }))
    }

    /// List issues for a repository.
    ///
    /// `state` is `open`, `closed`, or `all`; defaults to `open`.
    pub(crate) async fn list_issues(
        &self,
        owner: &RepoOwner,
        repo: &RepoName,
        state: Option<&str>,
    ) -> Result<Value, GitHubError> {
        let state = state.unwrap_or("open");
        let path = format!(
            "/repos/{}/{}/issues?state={}&per_page=30",
            owner.as_str(),
            repo.as_str(),
            state
        );
        self.get(&path).await
    }

    /// Open a new issue in a repository.
    pub(crate) async fn create_issue(
        &self,
        owner: &RepoOwner,
        repo: &RepoName,
        title: &str,
        body: Option<&str>,
        labels: &[String],
    ) -> Result<Value, GitHubError> {
        let path = format!("/repos/{}/{}/issues", owner.as_str(), repo.as_str());
        let payload = serde_json::json!({
            "title":  title,
            "body":   body.unwrap_or(""),
            "labels": labels,
        });
        self.post(&path, &payload).await
    }

    /// List pull requests for a repository.
    ///
    /// `state` is `open`, `closed`, or `all`; defaults to `open`.
    pub(crate) async fn list_pull_requests(
        &self,
        owner: &RepoOwner,
        repo: &RepoName,
        state: Option<&str>,
    ) -> Result<Value, GitHubError> {
        let state = state.unwrap_or("open");
        let path = format!(
            "/repos/{}/{}/pulls?state={}&per_page=30",
            owner.as_str(),
            repo.as_str(),
            state
        );
        self.get(&path).await
    }

    /// Search code on GitHub.
    ///
    /// `query` uses GitHub's search syntax. `per_page` is capped at 100.
    pub(crate) async fn search_code(
        &self,
        query: &str,
        per_page: u8,
    ) -> Result<Value, GitHubError> {
        let per_page = per_page.min(100);
        let encoded = urlencoded(query);
        let path = format!("/search/code?q={}&per_page={}", encoded, per_page);
        self.get(&path).await
    }

    // -----------------------------------------------------------------------
    // HTTP helpers
    // -----------------------------------------------------------------------

    async fn get(&self, path: &str) -> Result<Value, GitHubError> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .http
            .get(&url)
            .bearer_auth(self.token.expose())
            .header("Accept", ACCEPT_GITHUB_JSON)
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .send()
            .await?;
        self.handle_response(response).await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, GitHubError> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .http
            .post(&url)
            .bearer_auth(self.token.expose())
            .header("Accept", ACCEPT_GITHUB_JSON)
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .json(body)
            .send()
            .await?;
        self.handle_response(response).await
    }

    async fn handle_response(&self, response: reqwest::Response) -> Result<Value, GitHubError> {
        let status = response.status();

        if status.as_u16() == 429 {
            let retry = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            return Err(GitHubError::RateLimited {
                retry_after_secs: retry,
            });
        }

        let text = response.text().await?;

        if !status.is_success() {
            // Extract the GitHub `message` field when available; fall back to
            // the raw body. Both paths consume `text` so no allocation needed.
            let message = match serde_json::from_str::<Value>(&text) {
                Ok(v) => v
                    .get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_owned)
                    .unwrap_or(text),
                Err(_) => text,
            };
            return Err(GitHubError::ApiError {
                status: status.as_u16(),
                message,
            });
        }

        serde_json::from_str(&text).map_err(|e| GitHubError::Deserialize(e.to_string()))
    }
}

/// Percent-encode a search query for GitHub's code-search endpoint.
///
/// Covers the common patterns (spaces, quoted strings) without pulling in a
/// full URL-encoding crate.
fn urlencoded(s: &str) -> String {
    s.replace(' ', "+").replace('"', "%22")
}
