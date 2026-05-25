//! Event publishing scaffolding.
//!
//! In v0.1 the SDK ships only a webhook publisher: events POSTed to a
//! subscriber-declared URL. WebSocket and Server-Sent Events delivery
//! arrive in v0.2; the [`EventPublisher`] trait is shaped so they can
//! be added without changing existing call sites.

use async_trait::async_trait;
use ferridis_core::IntentVerb;
use serde::Serialize;
use thiserror::Error;
use time::OffsetDateTime;
use url::Url;

/// An event emitted by a capability and delivered to subscribers.
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    /// Protocol version of the event envelope.
    pub ferridis_version: String,
    /// The intent verb the event relates to (e.g. `event-fired`).
    pub event: IntentVerb,
    /// When the event occurred.
    pub occurred_at: OffsetDateTime,
    /// Event-specific payload, defined by the capability's AsyncAPI schema.
    pub payload: serde_json::Value,
}

/// Errors that can occur publishing an event.
#[derive(Debug, Error)]
pub enum PublishError {
    /// The transport to the subscriber failed.
    #[error("transport error: {0}")]
    Transport(String),

    /// The subscriber returned a non-success status code.
    #[error("subscriber returned HTTP {0}")]
    BadStatus(u16),
}

/// An event publisher targets a single subscriber endpoint.
#[async_trait]
pub trait EventPublisher: Send + Sync + 'static {
    /// Deliver one event to the subscriber.
    async fn publish(&self, event: &Event) -> Result<(), PublishError>;
}

/// Simple webhook publisher: HTTP POST with the event JSON body.
#[derive(Debug, Clone)]
pub struct WebhookPublisher {
    url: Url,
    http: reqwest::Client,
}

impl WebhookPublisher {
    /// Build a publisher targeting `url`.
    pub fn new(url: Url) -> Self {
        Self {
            url,
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl EventPublisher for WebhookPublisher {
    async fn publish(&self, event: &Event) -> Result<(), PublishError> {
        let resp = self
            .http
            .post(self.url.clone())
            .json(event)
            .send()
            .await
            .map_err(|e| PublishError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(PublishError::BadStatus(resp.status().as_u16()));
        }
        Ok(())
    }
}
