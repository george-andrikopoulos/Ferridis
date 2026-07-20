//! Lazy schema fetching.
//!
//! The full OpenAPI 3 or AsyncAPI schema is loaded on demand, only when
//! a capability has been selected for a call. The manifest carries a URL
//! at [`Manifest::schema_url`](ferridis_core::Manifest::schema_url) — this
//! module fetches the bytes at that URL.
//!
//! Schema bytes are returned opaque. Parsing is deferred to the caller
//! because some callers will validate (e.g., the SDK) while others will
//! pass-through to a model (e.g., the client). Keeping the bytes opaque
//! at this layer avoids forcing a single parser choice on the workspace.

use bytes::Bytes;
use url::Url;

use crate::client::Client;
use crate::error::ProtocolError;

/// Raw schema bytes returned from [`fetch_schema`].
///
/// Typically OpenAPI 3 (YAML or JSON) or AsyncAPI (YAML or JSON). The
/// `content_type` header from the response is preserved so callers can
/// choose the right parser.
#[derive(Debug, Clone)]
pub struct SchemaBytes {
    content_type: Option<String>,
    body: Bytes,
}

impl SchemaBytes {
    /// Build a [`SchemaBytes`] from raw bytes and an optional content type.
    pub fn new(content_type: Option<String>, body: Bytes) -> Self {
        Self { content_type, body }
    }

    /// The `Content-Type` header from the response, if any.
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// The raw response body.
    pub fn body(&self) -> &Bytes {
        &self.body
    }

    /// Borrow the body as a UTF-8 string slice, if it is valid UTF-8.
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.body).ok()
    }
}

/// Fetch the raw schema bytes from the given URL.
pub async fn fetch_schema(client: &Client, url: &Url) -> Result<SchemaBytes, ProtocolError> {
    let resp =
        client
            .http()
            .get(url.clone())
            .send()
            .await
            .map_err(|e| ProtocolError::Transport {
                url: url.clone(),
                source: e,
            })?;

    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let body = resp.bytes().await.map_err(|e| ProtocolError::Transport {
        url: url.clone(),
        source: e,
    })?;

    if !status.is_success() {
        let preview = String::from_utf8_lossy(&body[..body.len().min(256)]).into_owned();
        return Err(ProtocolError::BadStatus {
            url: url.clone(),
            status: status.as_u16(),
            body: preview,
        });
    }

    Ok(SchemaBytes::new(content_type, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn fetches_schema_bytes_with_content_type() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/schema.yaml"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw("openapi: 3.0.0\n", "application/yaml"),
            )
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/schema.yaml", server.uri())).unwrap();
        let client = Client::new();
        let s = fetch_schema(&client, &url).await.unwrap();
        assert_eq!(s.content_type(), Some("application/yaml"));
        assert!(s.as_str().unwrap().contains("openapi"));
    }
}
