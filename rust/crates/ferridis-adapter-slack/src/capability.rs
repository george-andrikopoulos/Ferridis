//! [`SlackCapability`] — Ferridis [`Capability`] implementation for Slack.

use async_trait::async_trait;
use ferridis_adapter_sdk::{Capability, DispatchError, SchemaSource};
use ferridis_core::{IntentVerb, Manifest};
use serde_json::Value;

use crate::client::SlackClient;
use crate::types::{BotToken, SlackError};

const DEFAULT_MANIFEST_JSON: &str = r#"{
    "ferridis_version": "0.1",
    "id": "ferridis.slack.v1",
    "name": "Ferridis Slack",
    "category": "messaging",
    "summary": "Browse channels, post messages, and manage Slack conversations.",
    "intents": [
        "list-channels",
        "post-message",
        "get-messages",
        "send-dm",
        "get-channel-info",
        "list-users"
    ],
    "schema": {
        "type": "openapi-3",
        "url": "https://ferridis.io/schemas/slack.v1.yaml"
    },
    "tiers": ["native"],
    "auth": { "type": "none" }
}"#;

const EMBEDDED_SCHEMA: &str = r#"openapi: 3.0.3
info:
  title: Ferridis Slack
  version: 0.1.0
paths:
  /intents/list-channels:
    post:
      summary: List channels the bot has access to.
      requestBody:
        required: false
        content:
          application/json:
            schema:
              type: object
              properties:
                limit:
                  type: integer
                exclude_archived:
                  type: boolean
      responses:
        "200":
          description: Slack conversations.list response.

  /intents/post-message:
    post:
      summary: Post a message to a channel.
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [channel, text]
              properties:
                channel:
                  type: string
                  description: Channel ID or name.
                text:
                  type: string
      responses:
        "200":
          description: Slack chat.postMessage response.

  /intents/get-messages:
    post:
      summary: Fetch recent messages from a channel.
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [channel]
              properties:
                channel:
                  type: string
                limit:
                  type: integer
      responses:
        "200":
          description: Slack conversations.history response.

  /intents/send-dm:
    post:
      summary: Send a direct message to a user.
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [user_id, text]
              properties:
                user_id:
                  type: string
                  description: Slack user ID (e.g. U01234ABCDE).
                text:
                  type: string
      responses:
        "200":
          description: Slack chat.postMessage response for the DM.

  /intents/get-channel-info:
    post:
      summary: Get metadata about a channel.
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [channel]
              properties:
                channel:
                  type: string
      responses:
        "200":
          description: Slack conversations.info response.

  /intents/list-users:
    post:
      summary: List workspace members.
      requestBody:
        required: false
        content:
          application/json:
            schema:
              type: object
              properties:
                limit:
                  type: integer
      responses:
        "200":
          description: Slack users.list response.
"#;

// ---------------------------------------------------------------------------
// SlackCapability
// ---------------------------------------------------------------------------

/// Holds a [`SlackClient`] configured with the operator-supplied Bot Token.
pub struct SlackCapability {
    manifest: Manifest,
    client: SlackClient,
}

impl SlackCapability {
    /// Build a capability from a bot token.
    pub fn new(token: BotToken) -> Result<Self, DispatchError> {
        let manifest = Manifest::parse(DEFAULT_MANIFEST_JSON)
            .map_err(|e| DispatchError::Internal(format!("default manifest failed to parse: {e}")))?;
        Ok(Self { manifest, client: SlackClient::new(token) })
    }

    /// Override the Slack API base URL (for tests).
    pub fn with_api_base_url(mut self, url: impl Into<String>) -> Self {
        self.client.api_base_url = url.into();
        self
    }
}

// ---------------------------------------------------------------------------
// Capability impl
// ---------------------------------------------------------------------------

#[async_trait]
impl Capability for SlackCapability {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn schema(&self) -> SchemaSource {
        SchemaSource::Embedded {
            content_type: "application/yaml".into(),
            body: EMBEDDED_SCHEMA.into(),
        }
    }

    async fn dispatch(
        &self,
        intent: &IntentVerb,
        body: Value,
    ) -> Result<Value, DispatchError> {
        match intent.as_str() {
            "list-channels" => {
                let limit = body.get("limit").and_then(|v| v.as_u64());
                let exclude_archived =
                    body.get("exclude_archived").and_then(|v| v.as_bool()).unwrap_or(true); // allow:unwrap using unwrap_or
                self.client.list_channels(limit, exclude_archived).await.map_err(to_dispatch)
            }

            "post-message" => {
                let channel = str_field(&body, "channel")?;
                let text = str_field(&body, "text")?;
                self.client.post_message(channel, text).await.map_err(to_dispatch)
            }

            "get-messages" => {
                let channel = str_field(&body, "channel")?;
                let limit = body.get("limit").and_then(|v| v.as_u64());
                self.client.get_messages(channel, limit).await.map_err(to_dispatch)
            }

            "send-dm" => {
                let user_id = str_field(&body, "user_id")?;
                let text = str_field(&body, "text")?;
                self.client.send_dm(user_id, text).await.map_err(to_dispatch)
            }

            "get-channel-info" => {
                let channel = str_field(&body, "channel")?;
                self.client.get_channel_info(channel).await.map_err(to_dispatch)
            }

            "list-users" => {
                let limit = body.get("limit").and_then(|v| v.as_u64());
                self.client.list_users(limit).await.map_err(to_dispatch)
            }

            _ => Err(DispatchError::UnsupportedIntent(intent.clone())), // allow:clone — SDK variant takes owned IntentVerb; small string
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn str_field<'a>(body: &'a Value, key: &str) -> Result<&'a str, DispatchError> {
    body.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| DispatchError::InvalidRequest(format!("missing or non-string `{key}`")))
}

fn to_dispatch(e: SlackError) -> DispatchError {
    match e {
        SlackError::ApiError { code } if code == "not_authed" || code == "invalid_auth" => {
            DispatchError::Forbidden(format!("Slack auth error: {code}"))
        }
        SlackError::ApiError { code } if code == "channel_not_found" || code == "user_not_found" => {
            DispatchError::NotFound(code)
        }
        other => DispatchError::Internal(other.to_string()),
    }
}
