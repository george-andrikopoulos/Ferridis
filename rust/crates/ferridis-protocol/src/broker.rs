//! The connection broker.
//!
//! The broker is the wire-layer object that owns the OAuth round-trip
//! state. It is the only component that holds the [`PkceVerifier`]
//! between the authorize step and the token-exchange step. Keeping
//! the verifier inside the broker (rather than serialising it into
//! [`ferridis_core::Connection<Pending>`]) means the core types stay
//! pure and free of crypto material.
//!
//! # Typical flow
//!
//! ```ignore
//! let broker = Broker::new(client, oauth_config);
//! // 1. Start a new connection for a given capability and manifest.
//! let pending: Connection<Pending> = broker.start_connect(&manifest).await?;
//! // 2. Send the user to `pending.auth_url()` in a browser.
//! // 3. When the OAuth callback arrives with `state` + `code`, hand them back.
//! let authorized: Connection<Authorized> = broker.complete(pending, "auth-code").await?;
//! ```

use std::collections::HashMap;
use std::sync::Mutex;
use time::{Duration as TimeDuration, OffsetDateTime};
use url::Url;
use uuid::Uuid;

use ferridis_core::{
    AccessToken, AuthMethod, Authorized, Connection, Manifest, Pending, RefreshToken,
};

use crate::client::Client;
use crate::error::ProtocolError;
use crate::oauth::{AuthorizationUrl, PkceVerifier, exchange_code};

/// Configuration the broker needs to drive an OAuth flow.
///
/// In a production runtime these come from the wallet's
/// per-service registration. For v0.1 they are passed in directly.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// The authorization endpoint for the service.
    pub authorize_endpoint: Url,
    /// The token endpoint for the service.
    pub token_endpoint: Url,
    /// The runtime's registered client ID.
    pub client_id: String,
    /// The runtime's client secret, if any. Public clients use `None`.
    pub client_secret: Option<String>,
    /// The redirect URI the runtime listens on for the callback.
    pub redirect_uri: Url,
    /// Service-specific extra parameters appended to the authorize URL
    /// (e.g., `[("access_type", "offline")]` for Google).
    pub extra_authorize_params: Vec<(String, String)>,
}

/// State retained between the authorize step and the token exchange.
///
/// One entry per in-flight authorization, keyed by the CSRF state token.
struct PendingState {
    verifier: PkceVerifier,
    redirect_uri: Url,
    token_endpoint: Url,
    client_id: String,
    client_secret: Option<String>,
}

/// The connection broker.
///
/// Holds in-memory state for in-flight OAuth flows. One broker per
/// runtime is typical; sharing across threads is supported (an internal
/// [`Mutex`] guards the in-flight map).
pub struct Broker {
    client: Client,
    config: OAuthConfig,
    pending: Mutex<HashMap<String, PendingState>>,
}

impl Broker {
    /// Build a new broker.
    pub fn new(client: Client, config: OAuthConfig) -> Self {
        Self {
            client,
            config,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Begin a connection: generate PKCE material, store it under a fresh
    /// CSRF state token, and return a [`Connection<Pending>`] whose
    /// `auth_url` is ready to open in the user's browser.
    ///
    /// The manifest must declare `oauth2` auth. Other auth methods will
    /// be supported in subsequent versions; today they return
    /// [`ProtocolError::OAuth`].
    pub fn start_connect(&self, manifest: &Manifest) -> Result<Connection<Pending>, ProtocolError> {
        let scopes = match manifest.auth() {
            AuthMethod::Oauth2 { scopes } => scopes.clone(),
            AuthMethod::None | AuthMethod::ApiKey { .. } => {
                return Err(ProtocolError::OAuth(
                    "broker only supports oauth2 auth in v0.1".into(),
                ));
            }
        };

        let verifier = PkceVerifier::generate();
        let challenge = verifier.challenge();
        let state_token = Uuid::new_v4().simple().to_string();

        let extra: Vec<(&str, &str)> = self
            .config
            .extra_authorize_params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let auth_url = AuthorizationUrl::build(
            &self.config.authorize_endpoint,
            &self.config.client_id,
            &self.config.redirect_uri,
            &scopes,
            &state_token,
            &challenge,
            &extra,
        );

        // Build the capability reference from the manifest URL convention.
        // The manifest's `id` field is documented as the dotted ID; we
        // assemble a `CapabilityRef`-shaped URL from the registry host
        // implied by the schema URL host. Adapters that publish manifests
        // with explicit `capability_url` may surface it here in future.
        let capability_ref = capability_ref_from_manifest(manifest)?;

        let mut map = self.pending.lock().expect("broker pending mutex poisoned");
        map.insert(
            state_token.clone(),
            PendingState {
                verifier,
                redirect_uri: self.config.redirect_uri.clone(),
                token_endpoint: self.config.token_endpoint.clone(),
                client_id: self.config.client_id.clone(),
                client_secret: self.config.client_secret.clone(),
            },
        );

        Ok(Connection::<Pending>::new(
            capability_ref,
            manifest.tiers().preferred(),
            auth_url.as_str(),
            state_token,
        ))
    }

    /// Complete authorization: take the [`Pending`] connection and the
    /// authorization code received on the OAuth callback, exchange the
    /// code for tokens, and return [`Connection<Authorized>`].
    ///
    /// The CSRF `state` is read from the pending connection and must
    /// match the one this broker handed out on [`start_connect`]; if it
    /// does not, [`ProtocolError::UnknownState`] is returned and the
    /// pending connection is dropped.
    pub async fn complete(
        &self,
        pending: Connection<Pending>,
        code: &str,
    ) -> Result<Connection<Authorized>, ProtocolError> {
        let state_token = pending.state_token().to_string();
        let state = {
            let mut map = self.pending.lock().expect("broker pending mutex poisoned");
            map.remove(&state_token)
                .ok_or(ProtocolError::UnknownState)?
        };

        let tokens = exchange_code(
            &self.client,
            &state.token_endpoint,
            &state.client_id,
            state.client_secret.as_deref(),
            &state.redirect_uri,
            code,
            &state.verifier,
        )
        .await?;

        let expires_at = tokens
            .expires_in
            .map(|s| OffsetDateTime::now_utc() + TimeDuration::seconds(s as i64))
            .unwrap_or_else(|| OffsetDateTime::now_utc() + TimeDuration::hours(1));

        let access = AccessToken::new(tokens.access_token);
        let refresh = tokens.refresh_token.map(RefreshToken::new);
        Ok(pending.complete(access, refresh, expires_at))
    }

    /// Whether the broker still holds state for the given CSRF token.
    /// Exposed for diagnostics and tests.
    pub fn has_pending(&self, state_token: &str) -> bool {
        self.pending
            .lock()
            .expect("broker pending mutex poisoned")
            .contains_key(state_token)
    }
}

fn capability_ref_from_manifest(
    manifest: &Manifest,
) -> Result<ferridis_core::CapabilityRef, ProtocolError> {
    // Convention used in v0.1: the manifest's schema URL host is the
    // service host; the registry, namespace, and id are derived from the
    // manifest's dotted ID (e.g. `google.calendar.v3` →
    // `ferridis://public.ferridis.io/google/calendar@v3`). Public mesh
    // tooling will issue authoritative refs in later versions; for now
    // we accept the best-effort mapping and surface a clean error if
    // the manifest ID is not in the expected dotted form.
    let id = manifest.id();
    let parts: Vec<&str> = id.split('.').collect();
    if parts.len() < 3 {
        return Err(ProtocolError::InvalidUrl(format!(
            "manifest id `{id}` is not in `<ns>.<cap>.v<n>` form"
        )));
    }
    let namespace = parts[0];
    let cap = parts[1..parts.len() - 1].join(".");
    let version = parts[parts.len() - 1];
    let raw = format!("ferridis://public.ferridis.io/{namespace}/{cap}@{version}");
    ferridis_core::CapabilityRef::parse(&raw)
        .map_err(|e| ProtocolError::InvalidUrl(format!("{raw}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferridis_core::Tier;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const VALID_MANIFEST: &str = r#"{
        "ferridis_version": "0.1",
        "id": "test.example.v1",
        "name": "Test",
        "category": "test",
        "summary": "A test capability.",
        "intents": ["read-file"],
        "schema": { "type": "openapi-3", "url": "https://example.invalid/s.yaml" },
        "tiers": ["native"],
        "auth": { "type": "oauth2", "scopes": ["read"] }
    }"#;

    fn cfg(server_uri: &str) -> OAuthConfig {
        OAuthConfig {
            authorize_endpoint: Url::parse(&format!("{server_uri}/authorize")).unwrap(),
            token_endpoint: Url::parse(&format!("{server_uri}/token")).unwrap(),
            client_id: "client-123".into(),
            client_secret: None,
            redirect_uri: Url::parse("http://127.0.0.1:7777/cb").unwrap(),
            extra_authorize_params: vec![],
        }
    }

    #[tokio::test]
    async fn end_to_end_pending_to_authorized() {
        let server = MockServer::start().await;
        let token_body = r#"{
            "access_token": "atk",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "rtk"
        }"#;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(token_body, "application/json"))
            .mount(&server)
            .await;

        let client = Client::new();
        let broker = Broker::new(client, cfg(&server.uri()));
        let manifest = Manifest::parse(VALID_MANIFEST).unwrap();

        let pending = broker.start_connect(&manifest).unwrap();
        assert!(broker.has_pending(pending.state_token()));
        let auth = broker.complete(pending, "auth-code").await.unwrap();

        assert_eq!(auth.tier(), Tier::Native);
        assert!(auth.can_refresh());
        assert_eq!(auth.access_token().expose(), "atk");
    }

    #[tokio::test]
    async fn unknown_state_is_rejected() {
        let server = MockServer::start().await;
        let client = Client::new();
        let broker = Broker::new(client, cfg(&server.uri()));
        let manifest = Manifest::parse(VALID_MANIFEST).unwrap();
        let pending = broker.start_connect(&manifest).unwrap();

        // Drop the broker's record by completing once.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(r#"{"access_token":"a"}"#, "application/json"),
            )
            .mount(&server)
            .await;

        let _ = broker
            .complete(pending.clone(), "code")
            .await
            .expect("first complete should succeed");

        // Second attempt with the same pending value: state no longer in map.
        let err = broker.complete(pending, "code").await.unwrap_err();
        assert!(matches!(err, ProtocolError::UnknownState));
    }
}
