//! Slack Web API client.
//!
//! Handles Bearer auth, Slack's `{"ok": false}` application-level errors,
//! and 429 rate-limit responses.

use serde_json::Value;
use tokio::sync::Mutex;

use crate::types::{BotToken, SlackError};

pub(crate) const DEFAULT_API_BASE: &str = "https://slack.com/api";

pub(crate) struct SlackClient {
    pub(crate) http: reqwest::Client,
    pub(crate) token: Mutex<BotToken>,
    pub(crate) api_base_url: String,
}

impl SlackClient {
    pub(crate) fn new(token: BotToken) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ferridis-adapter-slack/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("build reqwest client"); // allow:unwrap static config — never fails
        Self {
            http,
            token: Mutex::new(token),
            api_base_url: DEFAULT_API_BASE.to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // Public API methods
    // -----------------------------------------------------------------------

    /// `conversations.list` — list public channels (and private ones the bot is in).
    pub(crate) async fn list_channels(
        &self,
        limit: Option<u64>,
        exclude_archived: bool,
    ) -> Result<Value, SlackError> {
        let mut url = format!("{}/conversations.list", self.api_base_url);
        let limit_str;
        let mut params: Vec<(&str, &str)> = vec![
            ("exclude_archived", if exclude_archived { "true" } else { "false" }),
        ];
        if let Some(n) = limit {
            limit_str = n.to_string();
            params.push(("limit", &limit_str));
        }
        append_query(&mut url, &params);
        self.get_json(&url).await
    }

    /// `chat.postMessage` — post a message to a channel.
    pub(crate) async fn post_message(
        &self,
        channel: &str,
        text: &str,
    ) -> Result<Value, SlackError> {
        let url = format!("{}/chat.postMessage", self.api_base_url);
        let body = serde_json::json!({ "channel": channel, "text": text });
        self.post_json(&url, &body).await
    }

    /// `conversations.history` — fetch recent messages from a channel.
    pub(crate) async fn get_messages(
        &self,
        channel: &str,
        limit: Option<u64>,
    ) -> Result<Value, SlackError> {
        let mut url = format!("{}/conversations.history", self.api_base_url);
        let limit_str;
        let mut params: Vec<(&str, &str)> = vec![("channel", channel)];
        if let Some(n) = limit {
            limit_str = n.to_string();
            params.push(("limit", &limit_str));
        }
        append_query(&mut url, &params);
        self.get_json(&url).await
    }

    /// Open a DM with a user via `conversations.open`, then send the message.
    pub(crate) async fn send_dm(
        &self,
        user_id: &str,
        text: &str,
    ) -> Result<Value, SlackError> {
        // Step 1: open (or retrieve) the DM channel.
        let open_url = format!("{}/conversations.open", self.api_base_url);
        let open_body = serde_json::json!({ "users": user_id });
        let open_resp = self.post_json(&open_url, &open_body).await?;
        let channel_id = open_resp
            .get("channel")
            .and_then(|c| c.get("id"))
            .and_then(|id| id.as_str())
            .ok_or_else(|| SlackError::Deserialize("conversations.open: missing channel.id".into()))?
            .to_owned();

        // Step 2: post the message.
        self.post_message(&channel_id, text).await
    }

    /// `conversations.info` — metadata about a single channel.
    pub(crate) async fn get_channel_info(&self, channel: &str) -> Result<Value, SlackError> {
        let mut url = format!("{}/conversations.info", self.api_base_url);
        append_query(&mut url, &[("channel", channel)]);
        self.get_json(&url).await
    }

    /// `users.list` — list workspace members.
    pub(crate) async fn list_users(&self, limit: Option<u64>) -> Result<Value, SlackError> {
        let mut url = format!("{}/users.list", self.api_base_url);
        let limit_str;
        let mut params: Vec<(&str, &str)> = vec![];
        if let Some(n) = limit {
            limit_str = n.to_string();
            params.push(("limit", &limit_str));
        }
        if !params.is_empty() {
            append_query(&mut url, &params);
        }
        self.get_json(&url).await
    }

    // -----------------------------------------------------------------------
    // HTTP primitives
    // -----------------------------------------------------------------------

    async fn get_json(&self, url: &str) -> Result<Value, SlackError> {
        let token = self.token.lock().await.expose().to_owned();
        let resp = self
            .http
            .get(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(SlackError::Transport)?;
        self.handle_response(resp).await
    }

    async fn post_json(&self, url: &str, body: &Value) -> Result<Value, SlackError> {
        let token = self.token.lock().await.expose().to_owned();
        let resp = self
            .http
            .post(url)
            .bearer_auth(&token)
            .json(body)
            .send()
            .await
            .map_err(SlackError::Transport)?;
        self.handle_response(resp).await
    }

    async fn handle_response(&self, resp: reqwest::Response) -> Result<Value, SlackError> {
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            return Err(SlackError::RateLimited { retry_after_secs });
        }

        let text = resp
            .text()
            .await
            .map_err(|e| SlackError::Deserialize(e.to_string()))?;

        let value: Value = serde_json::from_str(&text)
            .map_err(|e| SlackError::Deserialize(e.to_string()))?;

        // Slack always returns HTTP 200; errors live in the JSON envelope.
        match value.get("ok").and_then(|v| v.as_bool()) {
            Some(true) => Ok(value),
            _ => {
                let code = value
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown_error") // allow:unwrap using unwrap_or
                    .to_owned();
                Err(SlackError::ApiError { code })
            }
        }
    }
}

fn append_query(url: &mut String, params: &[(&str, &str)]) {
    if params.is_empty() {
        return;
    }
    url.push('?');
    let qs = params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    url.push_str(&qs);
}
