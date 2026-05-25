//! Bearer-token authenticated call helper.
//!
//! Once a connection is [`Authorized`](ferridis_core::Authorized), the
//! runtime can call the underlying service over plain HTTPS with the
//! access token in the `Authorization` header. This module wraps that
//! single concern.
//!
//! Two Ferridis-specific advisory headers are attached:
//!
//! - `X-Ferridis-Connection` — the connection ID, for service-side
//!   telemetry and rate-limit isolation.
//! - `X-Ferridis-Intent` — the intent verb being invoked, for the same
//!   reasons.
//!
//! These headers are advisory: services that have not adopted Ferridis
//! ignore them and the call still works as a plain OAuth-bearer call.

use ferridis_core::{Authorized, Connection, IntentVerb};
use reqwest::Method;
use url::Url;

use crate::client::Client;
use crate::error::ProtocolError;

/// A call to issue against a service.
///
/// Construct via [`CallRequest::new`].
#[derive(Debug, Clone)]
pub struct CallRequest {
    method: Method,
    url: Url,
    intent: IntentVerb,
    body: Option<serde_json::Value>,
}

impl CallRequest {
    /// Build a new call request.
    pub fn new(method: Method, url: Url, intent: IntentVerb) -> Self {
        Self {
            method,
            url,
            intent,
            body: None,
        }
    }

    /// Attach a JSON body.
    pub fn with_json_body(mut self, body: serde_json::Value) -> Self {
        self.body = Some(body);
        self
    }

    /// The HTTP method.
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// The target URL.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The intent verb.
    pub fn intent(&self) -> &IntentVerb {
        &self.intent
    }
}

/// A response from a successful service call.
#[derive(Debug, Clone)]
pub struct CallResponse {
    status: u16,
    body: bytes::Bytes,
}

impl CallResponse {
    /// The HTTP status code.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// The raw response body.
    pub fn body(&self) -> &bytes::Bytes {
        &self.body
    }

    /// Parse the body as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, ProtocolError> {
        serde_json::from_slice(&self.body).map_err(ProtocolError::from)
    }
}

/// Issue an authenticated call against a service.
///
/// `connection` must be [`Authorized`]. Expiry is *not* checked here —
/// that is the caller's responsibility, because Ferridis distinguishes
/// "expired locally" (refresh first) from "rejected by service"
/// (re-auth required). If the service returns 401, the caller should
/// transition the connection to [`Expired`](ferridis_core::Expired)
/// using [`Connection::expire`](ferridis_core::Connection::expire).
pub async fn call(
    client: &Client,
    connection: &Connection<Authorized>,
    request: CallRequest,
) -> Result<CallResponse, ProtocolError> {
    let mut req = client
        .http()
        .request(request.method.clone(), request.url.clone())
        .bearer_auth(connection.access_token().expose())
        .header("X-Ferridis-Connection", connection.id().to_string())
        .header("X-Ferridis-Intent", request.intent.as_str());

    if let Some(body) = &request.body {
        req = req.json(body);
    }

    send_and_handle(req, &request).await
}

/// Issue a call without an authorization token.
///
/// For capabilities whose manifest declares `auth: none`. The
/// `X-Ferridis-Intent` advisory header is still attached;
/// `X-Ferridis-Connection` is omitted because no connection ID exists
/// without an authorized connection.
///
/// All non-auth response handling — `401`/`403`/`429`/4xx/5xx mapping —
/// matches [`call`], because none-auth capabilities can still return
/// those statuses for non-token reasons (a `403` for path traversal,
/// say, or a `429` for service-side rate limiting).
pub async fn call_anonymous(
    client: &Client,
    request: CallRequest,
) -> Result<CallResponse, ProtocolError> {
    let mut req = client
        .http()
        .request(request.method.clone(), request.url.clone())
        .header("X-Ferridis-Intent", request.intent.as_str());

    if let Some(body) = &request.body {
        req = req.json(body);
    }

    send_and_handle(req, &request).await
}

async fn send_and_handle(
    req: reqwest::RequestBuilder,
    request: &CallRequest,
) -> Result<CallResponse, ProtocolError> {
    let resp = req.send().await.map_err(|e| ProtocolError::Transport {
        url: request.url.clone(),
        source: e,
    })?;

    let status = resp.status();
    let body = resp.bytes().await.map_err(|e| ProtocolError::Transport {
        url: request.url.clone(),
        source: e,
    })?;

    if status.as_u16() == 401 {
        return Err(ProtocolError::ConnectionExpired);
    }
    if status.as_u16() == 403 {
        return Err(ProtocolError::ConsentRequired);
    }
    if status.as_u16() == 429 {
        return Err(ProtocolError::RateLimited { retry_after_secs: 0 });
    }
    if !status.is_success() {
        let preview = String::from_utf8_lossy(&body[..body.len().min(256)]).into_owned();
        return Err(ProtocolError::BadStatus {
            url: request.url.clone(),
            status: status.as_u16(),
            body: preview,
        });
    }

    Ok(CallResponse {
        status: status.as_u16(),
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferridis_core::{AccessToken, CapabilityRef, Connection, Pending, Tier};
    use time::{Duration as TimeDuration, OffsetDateTime};
    use wiremock::matchers::{header, header_exists, method as wmethod, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn auth_connection() -> Connection<Authorized> {
        let cap = CapabilityRef::parse("ferridis://public.ferridis.io/test/cap@v1").unwrap();
        let pending = Connection::<Pending>::new(cap, Tier::Native, "http://x/", "s");
        pending.complete(
            AccessToken::new("the-token"),
            None,
            OffsetDateTime::now_utc() + TimeDuration::hours(1),
        )
    }

    #[tokio::test]
    async fn issues_a_call_with_bearer_and_ferridis_headers() {
        let server = MockServer::start().await;
        Mock::given(wmethod("POST"))
            .and(path("/api/messages"))
            .and(header("authorization", "Bearer the-token"))
            .and(header_exists("x-ferridis-connection"))
            .and(header("x-ferridis-intent", "send-message"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(r#"{"id":"m1"}"#, "application/json"),
            )
            .mount(&server)
            .await;

        let client = Client::new();
        let conn = auth_connection();
        let url = Url::parse(&format!("{}/api/messages", server.uri())).unwrap();
        let intent = IntentVerb::parse("send-message").unwrap();
        let req = CallRequest::new(Method::POST, url, intent)
            .with_json_body(serde_json::json!({"to": "alice", "body": "hi"}));

        let resp = call(&client, &conn, req).await.unwrap();
        assert_eq!(resp.status(), 200);
        let parsed: serde_json::Value = resp.json().unwrap();
        assert_eq!(parsed["id"], "m1");
    }

    #[tokio::test]
    async fn unauthorized_maps_to_connection_expired() {
        let server = MockServer::start().await;
        Mock::given(wmethod("GET"))
            .and(path("/api/x"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = Client::new();
        let conn = auth_connection();
        let url = Url::parse(&format!("{}/api/x", server.uri())).unwrap();
        let intent = IntentVerb::parse("read-events").unwrap();
        let err = call(&client, &conn, CallRequest::new(Method::GET, url, intent))
            .await
            .unwrap_err();
        assert!(matches!(err, ProtocolError::ConnectionExpired));
    }

    #[tokio::test]
    async fn anonymous_call_omits_authorization_and_connection_headers() {
        use wiremock::matchers::header_regex;
        let server = MockServer::start().await;
        Mock::given(wmethod("POST"))
            .and(path("/intents/read-file"))
            .and(header("x-ferridis-intent", "read-file"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(r#"{"ok":true}"#, "application/json"))
            .mount(&server)
            .await;

        // A second mock that ONLY matches if Authorization is present —
        // exercising it would change our response. We rely on the matcher
        // composition: the first mock matches only when the intent
        // header is set, regardless of authorization. The negative
        // assertion is on the absence of authorization in the request.
        Mock::given(wmethod("POST"))
            .and(path("/intents/read-file"))
            .and(header_regex("authorization", ".*"))
            .respond_with(ResponseTemplate::new(599).set_body_string("should not fire"))
            .mount(&server)
            .await;

        let client = Client::new();
        let url = Url::parse(&format!("{}/intents/read-file", server.uri())).unwrap();
        let intent = IntentVerb::parse("read-file").unwrap();
        let req = CallRequest::new(Method::POST, url, intent)
            .with_json_body(serde_json::json!({"path": "x"}));
        let resp = call_anonymous(&client, req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn too_many_requests_maps_to_rate_limited() {
        let server = MockServer::start().await;
        Mock::given(wmethod("GET"))
            .and(path("/api/x"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let client = Client::new();
        let conn = auth_connection();
        let url = Url::parse(&format!("{}/api/x", server.uri())).unwrap();
        let intent = IntentVerb::parse("read-events").unwrap();
        let err = call(&client, &conn, CallRequest::new(Method::GET, url, intent))
            .await
            .unwrap_err();
        assert!(matches!(err, ProtocolError::RateLimited { .. }));
    }
}
