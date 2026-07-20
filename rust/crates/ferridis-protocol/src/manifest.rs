//! Manifest fetching.
//!
//! Manifests are the always-loaded ≈200-token summaries. Fetching one is
//! the first thing a runtime does when establishing a connection to a
//! capability — every other piece of information (schema URL, supported
//! tiers, auth method) lives in the manifest.

use ferridis_core::Manifest;
use url::Url;

use crate::client::Client;
use crate::error::ProtocolError;

/// Fetch and validate a manifest from the given URL.
///
/// The URL is the HTTP(S) endpoint where the manifest JSON lives — for
/// public capabilities this is typically `<capability-url>/manifest.json`.
///
/// The response body is run through [`Manifest::parse`] so that every
/// caller receives an already-validated value.
pub async fn fetch_manifest(client: &Client, url: &Url) -> Result<Manifest, ProtocolError> {
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
    let body = resp.text().await.map_err(|e| ProtocolError::Transport {
        url: url.clone(),
        source: e,
    })?;

    if !status.is_success() {
        return Err(ProtocolError::BadStatus {
            url: url.clone(),
            status: status.as_u16(),
            body: truncate(&body, 256),
        });
    }

    Manifest::parse(&body).map_err(|e| ProtocolError::InvalidManifest {
        url: url.clone(),
        source: e,
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut t = s[..max].to_string();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const VALID_MANIFEST: &str = r#"{
        "ferridis_version": "0.1",
        "id": "test.capability.v1",
        "name": "Test",
        "category": "test",
        "summary": "A test capability.",
        "intents": ["read-file"],
        "schema": { "type": "openapi-3", "url": "https://example.invalid/schema.yaml" },
        "tiers": ["native"],
        "auth": { "type": "none" }
    }"#;

    #[tokio::test]
    async fn fetches_and_validates_a_manifest() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(VALID_MANIFEST, "application/json"),
            )
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/manifest.json", server.uri())).unwrap();
        let client = Client::new();
        let m = fetch_manifest(&client, &url).await.unwrap();
        assert_eq!(m.id(), "test.capability.v1");
    }

    #[tokio::test]
    async fn surfaces_bad_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/manifest.json", server.uri())).unwrap();
        let client = Client::new();
        let err = fetch_manifest(&client, &url).await.unwrap_err();
        assert!(matches!(err, ProtocolError::BadStatus { status: 404, .. }));
    }

    #[tokio::test]
    async fn rejects_invalid_manifest_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("{}", "application/json"))
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/manifest.json", server.uri())).unwrap();
        let client = Client::new();
        let err = fetch_manifest(&client, &url).await.unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidManifest { .. }));
    }
}
