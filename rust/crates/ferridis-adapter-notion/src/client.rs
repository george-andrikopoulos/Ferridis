//! Notion API client.
//!
//! Handles Bearer auth, the mandatory `Notion-Version` header, and
//! Notion's JSON error envelope.

use serde_json::Value;

use crate::types::{IntegrationToken, NotionError};

pub(crate) const DEFAULT_API_BASE: &str = "https://api.notion.com/v1";
const NOTION_VERSION: &str = "2022-06-28";

pub(crate) struct NotionClient {
    pub(crate) http: reqwest::Client,
    pub(crate) token: IntegrationToken,
    pub(crate) api_base_url: String,
}

impl NotionClient {
    pub(crate) fn new(token: IntegrationToken) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ferridis-adapter-notion/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("build reqwest client"); // allow:unwrap static config — never fails
        Self { http, token, api_base_url: DEFAULT_API_BASE.to_string() }
    }

    // -----------------------------------------------------------------------
    // API methods
    // -----------------------------------------------------------------------

    /// `POST /search` filtered to databases.
    pub(crate) async fn list_databases(&self) -> Result<Value, NotionError> {
        let url = format!("{}/search", self.api_base_url);
        let body = serde_json::json!({
            "filter": { "value": "database", "property": "object" }
        });
        self.post_json(&url, &body).await
    }

    /// `POST /databases/{database_id}/query`.
    pub(crate) async fn query_database(
        &self,
        database_id: &str,
        body: Value,
    ) -> Result<Value, NotionError> {
        let url = format!("{}/databases/{database_id}/query", self.api_base_url);
        self.post_json(&url, &body).await
    }

    /// `GET /pages/{page_id}`.
    pub(crate) async fn get_page(&self, page_id: &str) -> Result<Value, NotionError> {
        let url = format!("{}/pages/{page_id}", self.api_base_url);
        self.get_json(&url).await
    }

    /// `POST /pages`.
    pub(crate) async fn create_page(&self, body: Value) -> Result<Value, NotionError> {
        let url = format!("{}/pages", self.api_base_url);
        self.post_json(&url, &body).await
    }

    /// `PATCH /pages/{page_id}`.
    pub(crate) async fn update_page(
        &self,
        page_id: &str,
        body: Value,
    ) -> Result<Value, NotionError> {
        let url = format!("{}/pages/{page_id}", self.api_base_url);
        self.patch_json(&url, &body).await
    }

    /// `POST /search` with user-supplied query body.
    pub(crate) async fn search(&self, body: Value) -> Result<Value, NotionError> {
        let url = format!("{}/search", self.api_base_url);
        self.post_json(&url, &body).await
    }

    // -----------------------------------------------------------------------
    // HTTP primitives
    // -----------------------------------------------------------------------

    async fn get_json(&self, url: &str) -> Result<Value, NotionError> {
        let token = self.token.expose().to_owned();
        let resp = self
            .http
            .get(url)
            .bearer_auth(&token)
            .header("Notion-Version", NOTION_VERSION)
            .send()
            .await
            .map_err(NotionError::Transport)?;
        self.handle_response(resp).await
    }

    async fn post_json(&self, url: &str, body: &Value) -> Result<Value, NotionError> {
        let token = self.token.expose().to_owned();
        let resp = self
            .http
            .post(url)
            .bearer_auth(&token)
            .header("Notion-Version", NOTION_VERSION)
            .json(body)
            .send()
            .await
            .map_err(NotionError::Transport)?;
        self.handle_response(resp).await
    }

    async fn patch_json(&self, url: &str, body: &Value) -> Result<Value, NotionError> {
        let token = self.token.expose().to_owned();
        let resp = self
            .http
            .patch(url)
            .bearer_auth(&token)
            .header("Notion-Version", NOTION_VERSION)
            .json(body)
            .send()
            .await
            .map_err(NotionError::Transport)?;
        self.handle_response(resp).await
    }

    async fn handle_response(&self, resp: reqwest::Response) -> Result<Value, NotionError> {
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            return Err(NotionError::RateLimited { retry_after_secs });
        }

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| NotionError::Deserialize(e.to_string()))?;
        let value: Value =
            serde_json::from_str(&text).map_err(|e| NotionError::Deserialize(e.to_string()))?;

        if status.is_success() {
            return Ok(value);
        }

        let code = value
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown") // allow:unwrap using unwrap_or
            .to_owned();
        let message = value
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or(&text) // allow:unwrap using unwrap_or
            .to_owned();
        Err(NotionError::ApiError { status: status.as_u16(), code, message })
    }
}
