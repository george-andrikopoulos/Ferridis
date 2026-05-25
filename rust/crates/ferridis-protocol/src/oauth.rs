//! OAuth 2.1 / OAuth 2.0 + PKCE primitives.
//!
//! This module implements the parts of [RFC 7636] (PKCE) and the
//! authorization-code flow that Ferridis runtimes need. Both are
//! built directly on `reqwest` rather than via a third-party OAuth
//! crate, so the wire is visible and the trust surface is small.
//!
//! # Type-driven discipline
//!
//! - [`PkceVerifier`] and [`PkceChallenge`] are private newtypes. They
//!   can only be constructed via [`PkceVerifier::generate`], which uses
//!   the OS cryptographic RNG. Downstream code can rely on a verifier
//!   being a 43–128-character RFC-7636-compliant string with sufficient
//!   entropy.
//! - [`AuthorizationUrl`] is a newtype over `url::Url` so it cannot be
//!   confused with arbitrary URLs at call sites.
//!
//! # Out of scope
//!
//! - Token introspection ([RFC 7662]) and revocation ([RFC 7009]) are
//!   not implemented in v0.1. Revocation is currently handled by the
//!   wallet dropping the connection.
//!
//! [RFC 7636]: https://datatracker.ietf.org/doc/html/rfc7636
//! [RFC 7662]: https://datatracker.ietf.org/doc/html/rfc7662
//! [RFC 7009]: https://datatracker.ietf.org/doc/html/rfc7009

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use rand::rngs::OsRng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::client::Client;
use crate::error::ProtocolError;

/// Length of a generated PKCE code verifier in bytes, before
/// base64-url-encoding. 32 bytes encodes to ~43 characters, the minimum
/// length [RFC 7636] permits.
///
/// [RFC 7636]: https://datatracker.ietf.org/doc/html/rfc7636
const VERIFIER_BYTES: usize = 32;

/// A PKCE code verifier (RFC 7636 § 4.1).
///
/// The verifier is the secret that the runtime holds across the OAuth
/// round-trip. It is sent to the token endpoint in the exchange step,
/// where the authorization server verifies it matches the challenge
/// originally sent on the authorize step.
#[derive(Debug, Clone)]
pub struct PkceVerifier(String);

impl PkceVerifier {
    /// Generate a fresh verifier using the OS cryptographic RNG.
    pub fn generate() -> Self {
        let mut bytes = [0u8; VERIFIER_BYTES];
        OsRng.fill_bytes(&mut bytes);
        Self(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// The verifier as a string, suitable for the `code_verifier`
    /// parameter on the token endpoint.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Derive the matching S256 challenge.
    pub fn challenge(&self) -> PkceChallenge {
        let mut hasher = Sha256::new();
        hasher.update(self.0.as_bytes());
        let digest = hasher.finalize();
        PkceChallenge(URL_SAFE_NO_PAD.encode(digest))
    }
}

/// A PKCE code challenge (S256-derived from a [`PkceVerifier`]).
///
/// Sent on the authorize step as `code_challenge`, with
/// `code_challenge_method=S256`.
#[derive(Debug, Clone)]
pub struct PkceChallenge(String);

impl PkceChallenge {
    /// The challenge as a string, suitable for the `code_challenge`
    /// parameter on the authorize endpoint.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A URL the user should be redirected to in order to grant consent.
///
/// Newtype over [`Url`] so it cannot be confused with arbitrary URLs in
/// call sites.
#[derive(Debug, Clone)]
pub struct AuthorizationUrl(Url);

impl AuthorizationUrl {
    /// Build an authorization URL with PKCE parameters attached.
    ///
    /// `extra` is appended as additional query parameters and is intended
    /// for service-specific extensions (e.g., Google's `access_type=offline`).
    pub fn build(
        authorize_endpoint: &Url,
        client_id: &str,
        redirect_uri: &Url,
        scopes: &[String],
        state_token: &str,
        challenge: &PkceChallenge,
        extra: &[(&str, &str)],
    ) -> Self {
        let mut u = authorize_endpoint.clone();
        {
            let mut q = u.query_pairs_mut();
            q.append_pair("response_type", "code");
            q.append_pair("client_id", client_id);
            q.append_pair("redirect_uri", redirect_uri.as_str());
            if !scopes.is_empty() {
                q.append_pair("scope", &scopes.join(" "));
            }
            q.append_pair("state", state_token);
            q.append_pair("code_challenge", challenge.as_str());
            q.append_pair("code_challenge_method", "S256");
            for (k, v) in extra {
                q.append_pair(k, v);
            }
        }
        Self(u)
    }

    /// The URL as a [`Url`].
    pub fn as_url(&self) -> &Url {
        &self.0
    }

    /// Render the URL as a string.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// A successful response from the OAuth token endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    /// The issued access token.
    pub access_token: String,
    /// `Bearer`, `MAC`, etc. Almost always `Bearer` in practice.
    #[serde(default)]
    pub token_type: Option<String>,
    /// Lifetime of the access token in seconds.
    #[serde(default)]
    pub expires_in: Option<u64>,
    /// Optional refresh token.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Granted scope, if the server returned one.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Exchange an authorization code for tokens at the token endpoint.
///
/// Uses the PKCE verifier as proof. `client_secret` is optional for public
/// (no-secret) clients — Ferridis runtimes are typically public clients.
pub async fn exchange_code(
    client: &Client,
    token_endpoint: &Url,
    client_id: &str,
    client_secret: Option<&str>,
    redirect_uri: &Url,
    code: &str,
    verifier: &PkceVerifier,
) -> Result<TokenResponse, ProtocolError> {
    let redirect_str = redirect_uri.to_string();
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_str.as_str()),
        ("client_id", client_id),
        ("code_verifier", verifier.as_str()),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret));
    }

    let resp = client
        .http()
        .post(token_endpoint.clone())
        .form(&form)
        .send()
        .await
        .map_err(|e| ProtocolError::Transport {
            url: token_endpoint.clone(),
            source: e,
        })?;

    let status = resp.status();
    let body = resp.text().await.map_err(|e| ProtocolError::Transport {
        url: token_endpoint.clone(),
        source: e,
    })?;

    if !status.is_success() {
        return Err(ProtocolError::OAuth(format!(
            "token endpoint returned {status}: {}",
            preview(&body, 256)
        )));
    }

    serde_json::from_str(&body)
        .map_err(|e| ProtocolError::OAuth(format!("malformed token response: {e}")))
}

fn preview(s: &str, max: usize) -> String {
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
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn verifier_round_trips_to_challenge() {
        let v = PkceVerifier::generate();
        let c = v.challenge();
        // S256 of a 32-byte verifier base64url-encodes to 43 chars (256 bits / 6).
        assert_eq!(c.as_str().len(), 43);
    }

    #[test]
    fn different_verifiers_produce_different_challenges() {
        let a = PkceVerifier::generate();
        let b = PkceVerifier::generate();
        assert_ne!(a.as_str(), b.as_str());
        assert_ne!(a.challenge().as_str(), b.challenge().as_str());
    }

    #[test]
    fn authorization_url_carries_pkce_parameters() {
        let endpoint = Url::parse("https://auth.example/authorize").unwrap();
        let redirect = Url::parse("https://app.example/cb").unwrap();
        let verifier = PkceVerifier::generate();
        let url = AuthorizationUrl::build(
            &endpoint,
            "client-123",
            &redirect,
            &["read".into(), "write".into()],
            "csrf-xyz",
            &verifier.challenge(),
            &[],
        );
        let s = url.as_str();
        assert!(s.contains("response_type=code"));
        assert!(s.contains("client_id=client-123"));
        assert!(s.contains("code_challenge_method=S256"));
        assert!(s.contains("state=csrf-xyz"));
        assert!(s.contains("scope=read+write"));
    }

    #[tokio::test]
    async fn exchanges_code_for_tokens() {
        let server = MockServer::start().await;
        let body = r#"{
            "access_token": "atk",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "rtk"
        }"#;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let token_endpoint = Url::parse(&format!("{}/token", server.uri())).unwrap();
        let redirect = Url::parse("https://app.example/cb").unwrap();
        let verifier = PkceVerifier::generate();
        let client = Client::new();
        let resp = exchange_code(
            &client,
            &token_endpoint,
            "client-123",
            None,
            &redirect,
            "auth-code-from-callback",
            &verifier,
        )
        .await
        .unwrap();
        assert_eq!(resp.access_token, "atk");
        assert_eq!(resp.refresh_token.as_deref(), Some("rtk"));
        assert_eq!(resp.expires_in, Some(3600));
    }

    #[tokio::test]
    async fn surfaces_token_endpoint_failures() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_grant"}"#))
            .mount(&server)
            .await;

        let token_endpoint = Url::parse(&format!("{}/token", server.uri())).unwrap();
        let redirect = Url::parse("https://app.example/cb").unwrap();
        let verifier = PkceVerifier::generate();
        let client = Client::new();
        let err = exchange_code(
            &client,
            &token_endpoint,
            "client-123",
            None,
            &redirect,
            "code",
            &verifier,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ProtocolError::OAuth(_)));
    }
}
