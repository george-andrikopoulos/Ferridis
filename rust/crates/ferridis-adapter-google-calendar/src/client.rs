//! Google Calendar REST API v3 client.
//!
//! Wraps the six API operations exposed by this adapter, handling Bearer
//! auth headers, 401 → token refresh → retry, rate-limit detection, and
//! error body parsing.

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::types::{AccessToken, GoogleCalendarError, OAuthCredentials};

/// Default Google Calendar API v3 base URL.
pub(crate) const DEFAULT_API_BASE: &str = "https://www.googleapis.com/calendar/v3";

/// Default Google OAuth 2.0 token endpoint.
pub(crate) const DEFAULT_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Thin async wrapper over the Google Calendar REST API v3.
pub(crate) struct GoogleCalendarClient {
    pub(crate) http: reqwest::Client,
    /// Current access token — replaced in-place on successful refresh.
    pub(crate) token: Mutex<AccessToken>,
    /// Optional OAuth 2.0 refresh credentials for automatic token renewal.
    pub(crate) oauth_creds: Option<OAuthCredentials>,
    /// Base URL for the Calendar API (overridable for tests).
    pub(crate) api_base_url: String,
    /// URL for token refresh (overridable for tests).
    pub(crate) token_url: String,
}

impl GoogleCalendarClient {
    pub(crate) fn new(token: AccessToken) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "ferridis-adapter-google-calendar/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .expect("build reqwest client"); // allow:unwrap static config — never fails
        Self {
            http,
            token: Mutex::new(token),
            oauth_creds: None,
            api_base_url: DEFAULT_API_BASE.to_string(),
            token_url: DEFAULT_TOKEN_URL.to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // Public API methods
    // -----------------------------------------------------------------------

    /// `GET /users/me/calendarList`
    pub(crate) async fn list_calendars(&self) -> Result<Value, GoogleCalendarError> {
        let url = format!("{}/users/me/calendarList", self.api_base_url);
        self.get_json_with_refresh(&url).await
    }

    /// `GET /calendars/{calendarId}/events`
    pub(crate) async fn list_events(
        &self,
        calendar_id: &str,
        params: &[(&str, &str)],
    ) -> Result<Value, GoogleCalendarError> {
        let mut url = format!("{}/calendars/{}/events", self.api_base_url, calendar_id);
        let query = params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query);
        }
        self.get_json_with_refresh(&url).await
    }

    /// `GET /calendars/{calendarId}/events/{eventId}`
    pub(crate) async fn get_event(
        &self,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<Value, GoogleCalendarError> {
        let url = format!(
            "{}/calendars/{}/events/{}",
            self.api_base_url, calendar_id, event_id
        );
        self.get_json_with_refresh(&url).await
    }

    /// `POST /calendars/{calendarId}/events`
    pub(crate) async fn create_event(
        &self,
        calendar_id: &str,
        body: &Value,
    ) -> Result<Value, GoogleCalendarError> {
        let url = format!("{}/calendars/{}/events", self.api_base_url, calendar_id);
        self.post_json_with_refresh(&url, body).await
    }

    /// `PUT /calendars/{calendarId}/events/{eventId}`
    pub(crate) async fn update_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        body: &Value,
    ) -> Result<Value, GoogleCalendarError> {
        let url = format!(
            "{}/calendars/{}/events/{}",
            self.api_base_url, calendar_id, event_id
        );
        self.put_json_with_refresh(&url, body).await
    }

    /// `DELETE /calendars/{calendarId}/events/{eventId}`
    pub(crate) async fn delete_event(
        &self,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<(), GoogleCalendarError> {
        let url = format!(
            "{}/calendars/{}/events/{}",
            self.api_base_url, calendar_id, event_id
        );
        self.delete_with_refresh(&url).await
    }

    // -----------------------------------------------------------------------
    // HTTP helpers with 401 → refresh → retry
    // -----------------------------------------------------------------------

    async fn get_json_with_refresh(&self, url: &str) -> Result<Value, GoogleCalendarError> {
        let token = self.token.lock().await.expose().to_owned();
        let resp = self
            .http
            .get(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(GoogleCalendarError::Transport)?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && self.oauth_creds.is_some() {
            self.refresh_access_token().await?;
            let token2 = self.token.lock().await.expose().to_owned();
            let resp2 = self
                .http
                .get(url)
                .bearer_auth(&token2)
                .send()
                .await
                .map_err(GoogleCalendarError::Transport)?;
            return self.handle_response(resp2).await;
        }

        self.handle_response(resp).await
    }

    async fn post_json_with_refresh(
        &self,
        url: &str,
        body: &Value,
    ) -> Result<Value, GoogleCalendarError> {
        let token = self.token.lock().await.expose().to_owned();
        let resp = self
            .http
            .post(url)
            .bearer_auth(&token)
            .json(body)
            .send()
            .await
            .map_err(GoogleCalendarError::Transport)?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && self.oauth_creds.is_some() {
            self.refresh_access_token().await?;
            let token2 = self.token.lock().await.expose().to_owned();
            let resp2 = self
                .http
                .post(url)
                .bearer_auth(&token2)
                .json(body)
                .send()
                .await
                .map_err(GoogleCalendarError::Transport)?;
            return self.handle_response(resp2).await;
        }

        self.handle_response(resp).await
    }

    async fn put_json_with_refresh(
        &self,
        url: &str,
        body: &Value,
    ) -> Result<Value, GoogleCalendarError> {
        let token = self.token.lock().await.expose().to_owned();
        let resp = self
            .http
            .put(url)
            .bearer_auth(&token)
            .json(body)
            .send()
            .await
            .map_err(GoogleCalendarError::Transport)?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && self.oauth_creds.is_some() {
            self.refresh_access_token().await?;
            let token2 = self.token.lock().await.expose().to_owned();
            let resp2 = self
                .http
                .put(url)
                .bearer_auth(&token2)
                .json(body)
                .send()
                .await
                .map_err(GoogleCalendarError::Transport)?;
            return self.handle_response(resp2).await;
        }

        self.handle_response(resp).await
    }

    async fn delete_with_refresh(&self, url: &str) -> Result<(), GoogleCalendarError> {
        let token = self.token.lock().await.expose().to_owned();
        let resp = self
            .http
            .delete(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(GoogleCalendarError::Transport)?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && self.oauth_creds.is_some() {
            self.refresh_access_token().await?;
            let token2 = self.token.lock().await.expose().to_owned();
            let resp2 = self
                .http
                .delete(url)
                .bearer_auth(&token2)
                .send()
                .await
                .map_err(GoogleCalendarError::Transport)?;
            return self.handle_delete_response(resp2).await;
        }

        self.handle_delete_response(resp).await
    }

    // -----------------------------------------------------------------------
    // OAuth 2.0 token refresh
    // -----------------------------------------------------------------------

    async fn refresh_access_token(&self) -> Result<(), GoogleCalendarError> {
        let creds = self
            .oauth_creds
            .as_ref()
            .ok_or_else(|| GoogleCalendarError::TokenRefreshFailed("no credentials".into()))?;

        let refresh_token = creds.refresh_token.expose().to_owned();
        let client_id = creds.client_id.as_str().to_owned();
        let client_secret = creds.client_secret.expose().to_owned();

        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
        ];

        let resp = self
            .http
            .post(&self.token_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| GoogleCalendarError::TokenRefreshFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default(); // allow:unwrap using unwrap_or
            return Err(GoogleCalendarError::TokenRefreshFailed(format!(
                "HTTP {status}: {body}"
            )));
        }

        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
        }

        let body = resp
            .text()
            .await
            .map_err(|e| GoogleCalendarError::TokenRefreshFailed(e.to_string()))?;
        let token_resp: TokenResponse = serde_json::from_str(&body)
            .map_err(|e| GoogleCalendarError::TokenRefreshFailed(e.to_string()))?;

        let new_token = AccessToken::parse(token_resp.access_token)
            .map_err(|e| GoogleCalendarError::TokenRefreshFailed(e.to_string()))?;
        *self.token.lock().await = new_token;

        tracing::info!("Google Calendar OAuth access token refreshed successfully");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Response parsing
    // -----------------------------------------------------------------------

    async fn handle_response(&self, resp: reqwest::Response) -> Result<Value, GoogleCalendarError> {
        let status = resp.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            return Err(GoogleCalendarError::RateLimited { retry_after_secs });
        }

        let text = resp
            .text()
            .await
            .map_err(|e| GoogleCalendarError::Deserialize(e.to_string()))?;

        if !status.is_success() {
            let message = match serde_json::from_str::<Value>(&text) {
                Ok(v) => v
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .map(str::to_owned)
                    .unwrap_or(text), // allow:unwrap using unwrap_or
                Err(_) => text,
            };
            return Err(GoogleCalendarError::ApiError {
                status: status.as_u16(),
                message,
            });
        }

        serde_json::from_str(&text).map_err(|e| GoogleCalendarError::Deserialize(e.to_string()))
    }

    async fn handle_delete_response(
        &self,
        resp: reqwest::Response,
    ) -> Result<(), GoogleCalendarError> {
        let status = resp.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            return Err(GoogleCalendarError::RateLimited { retry_after_secs });
        }

        // 204 No Content is the success response for DELETE.
        if status.is_success() {
            return Ok(());
        }

        let text = resp
            .text()
            .await
            .map_err(|e| GoogleCalendarError::Deserialize(e.to_string()))?;
        let message = match serde_json::from_str::<Value>(&text) {
            Ok(v) => v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(str::to_owned)
                .unwrap_or(text), // allow:unwrap using unwrap_or
            Err(_) => text,
        };
        Err(GoogleCalendarError::ApiError {
            status: status.as_u16(),
            message,
        })
    }
}
