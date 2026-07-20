//! OAuth 2.1 (RFC 6749 + RFC 7636 PKCE) authorization-server state
//! machine. Just enough surface to satisfy claude.ai's MCP Custom
//! Connector handshake; deliberately not a general-purpose OAuth
//! server.
//!
//! # Why this exists
//!
//! Modern MCP Custom Connectors don't accept a static `Authorization:
//! Bearer …` header on `/mcp`. They run an OAuth 2.1 authorization-
//! code-with-PKCE flow against the connector URL first and only then
//! call the MCP endpoint with the issued access token. The 404 the
//! maintainer hit on `/authorize` against the bearer-only build was
//! the diagnostic.
//!
//! # Shape
//!
//! Three endpoints, served by [`crate::http_transport`]:
//!
//! - `GET /.well-known/oauth-authorization-server` — RFC 8414 metadata
//!   advertising the issuer + endpoint URLs + supported algorithms +
//!   `S256` PKCE method.
//! - `POST /register` — RFC 7591 Dynamic Client Registration. Returns
//!   a freshly-generated `client_id` for callers that register on the
//!   fly (claude.ai does).
//! - `GET /authorize` — issues a one-shot `code` and 302-redirects to
//!   the caller's `redirect_uri`. **Auto-approves** the request when
//!   the redirect_uri matches the operator-allow-list (claude.ai's
//!   well-known callback `https://claude.ai/api/mcp/auth_callback`
//!   ships in the default list). No HTML approval page in v1 — the
//!   operator already gates the connector via the
//!   `cloudflared` URL + bearer rotation, so a per-request browser
//!   click adds nothing.
//! - `POST /token` — verifies the PKCE challenge, exchanges the code
//!   for an opaque bearer access token.
//!
//! All state is in-memory with TTL eviction on lookup. Restarts wipe
//! tokens; claude.ai re-registers and re-authorises on demand.
//!
//! # Discipline
//!
//! - PKCE `S256` is **mandatory** (RFC 7636 §4.4). `plain` is rejected.
//! - Codes are single-use and expire after [`AUTH_CODE_TTL`].
//! - Access tokens expire after [`ACCESS_TOKEN_TTL`]; refresh tokens
//!   are not issued (out of v1 scope).
//! - All token / code material is generated from `rand::rngs::OsRng`
//!   and base64-url-encoded; nothing predictable on the wire.

// This module is internal to a binary crate, so several useful public
// surface items (e.g. operator-allow-list extension, issuer accessor,
// DCR-recorded client metadata) aren't reachable from main() and trip
// dead-code lints. They're tested in this module and exposed for the
// future test or operator-tooling that wants them; allowing module-
// wide is the minimum-friction way to keep them around without
// hand-annotating each one.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use rand::Rng;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

/// How long an authorization code is valid before the operator must
/// restart the flow. OAuth 2.1 §4.1.2 recommends ≤10 min; claude.ai's
/// flow completes in seconds, so this is plenty.
pub const AUTH_CODE_TTL: Duration = Duration::from_secs(120);

/// How long an issued access token remains valid. 24h matches
/// claude.ai's typical re-auth cadence for MCP Connectors.
pub const ACCESS_TOKEN_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// claude.ai's well-known OAuth callback URL. Auto-approved by
/// default so the operator doesn't need to know it; configurable
/// via [`OAuthServer::with_allowed_redirect_prefix`] for other
/// MCP hosts.
pub const CLAUDE_AI_REDIRECT_URI: &str = "https://claude.ai/api/mcp/auth_callback";

/// All-in-one error type for the OAuth server. Each variant maps to
/// an OAuth 2.0 §5.2 error code by [`Self::oauth_code`] so the HTTP
/// layer can render a spec-conformant JSON body.
#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("invalid_request: {0}")]
    InvalidRequest(String),
    #[error("invalid_client: {0}")]
    InvalidClient(String),
    #[error("invalid_grant: {0}")]
    InvalidGrant(String),
    #[error("unsupported_response_type: {0}")]
    UnsupportedResponseType(String),
    #[error("unsupported_grant_type: {0}")]
    UnsupportedGrantType(String),
    /// PKCE rejection — code_verifier did not match code_challenge.
    /// Folded into `invalid_grant` per RFC 7636 §4.6.
    #[error("invalid_grant (pkce mismatch)")]
    PkceMismatch,
}

impl OAuthError {
    /// Map to the RFC 6749 §5.2 error code string.
    pub fn oauth_code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::InvalidClient(_) => "invalid_client",
            Self::InvalidGrant(_) | Self::PkceMismatch => "invalid_grant",
            Self::UnsupportedResponseType(_) => "unsupported_response_type",
            Self::UnsupportedGrantType(_) => "unsupported_grant_type",
        }
    }

    /// Map to the HTTP status code the token endpoint should use
    /// (per RFC 6749 §5.2).
    pub fn http_status(&self) -> u16 {
        match self {
            Self::InvalidClient(_) => 401,
            _ => 400,
        }
    }
}

/// Dynamic Client Registration request (RFC 7591). Most fields are
/// optional and currently ignored — claude.ai only needs an issued
/// `client_id` back.
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    /// Required by claude.ai; we echo it back in the response and
    /// validate it on every authorize call.
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    /// Optional display name; logged at registration time.
    #[serde(default)]
    pub client_name: Option<String>,
    /// Optional token-endpoint auth method. We accept any value
    /// (PKCE makes it moot for public clients).
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
}

/// RFC 7591 §3.2.1 response shape. Only the fields claude.ai
/// actually reads are populated.
#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pub client_id: String,
    pub client_id_issued_at: i64,
    pub redirect_uris: Vec<String>,
    /// Echo back the auth method we accept — always `"none"` for
    /// public PKCE clients.
    pub token_endpoint_auth_method: &'static str,
    /// Indicate the grant types this server supports.
    pub grant_types: Vec<&'static str>,
    pub response_types: Vec<&'static str>,
}

/// `GET /authorize` query parameters, validated.
#[derive(Debug, Deserialize)]
pub struct AuthorizeRequest {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// `POST /token` form parameters, validated.
#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub code: String,
    pub redirect_uri: String,
    pub client_id: String,
    pub code_verifier: String,
}

/// RFC 6749 §5.1 access token response.
#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: u64,
}

/// RFC 8414 authorization-server metadata. Exposes what we actually
/// support; everything else is omitted (RFC 8414 says omitted fields
/// take the spec default).
#[derive(Debug, Serialize)]
pub struct ServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: String,
    pub response_types_supported: Vec<&'static str>,
    pub grant_types_supported: Vec<&'static str>,
    pub code_challenge_methods_supported: Vec<&'static str>,
    pub token_endpoint_auth_methods_supported: Vec<&'static str>,
}

/// One-shot authorization code awaiting redemption.
#[derive(Debug)]
struct AuthCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    issued_at: Instant,
}

/// Live access token issued to a client.
#[derive(Debug)]
struct AccessToken {
    client_id: String,
    expires_at: Instant,
}

/// Registered client (DCR record).
#[derive(Debug)]
struct ClientRecord {
    redirect_uris: Vec<String>,
    name: Option<String>,
    issued_at: i64,
}

/// All state. Construction takes the externally-facing `issuer` URL
/// (whatever claude.ai actually reaches over the tunnel) plus an
/// allow-list of redirect_uri prefixes.
pub struct OAuthServer {
    issuer: Url,
    allowed_redirect_prefixes: Vec<String>,
    state: Mutex<ServerState>,
}

#[derive(Default)]
struct ServerState {
    clients: HashMap<String, ClientRecord>,
    codes: HashMap<String, AuthCode>,
    tokens: HashMap<String, AccessToken>,
}

impl OAuthServer {
    /// New server. `issuer` is the externally-facing base URL (the
    /// one claude.ai will be hitting), e.g.
    /// `https://magical-default-disclaimers-elections.trycloudflare.com/`.
    ///
    /// The default redirect-uri allow-list contains
    /// [`CLAUDE_AI_REDIRECT_URI`]; extend it via
    /// [`Self::with_allowed_redirect_prefix`] for other MCP hosts.
    pub fn new(issuer: Url) -> Self {
        Self {
            issuer,
            allowed_redirect_prefixes: vec![CLAUDE_AI_REDIRECT_URI.to_string()],
            state: Mutex::new(ServerState::default()),
        }
    }

    /// Add a redirect-URI prefix to the auto-approve allow-list. A
    /// supplied `redirect_uri` is accepted iff it starts with one of
    /// the listed prefixes. Use sparingly — any URL added here is
    /// implicitly trusted to receive issued codes.
    #[must_use]
    pub fn with_allowed_redirect_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.allowed_redirect_prefixes.push(prefix.into());
        self
    }

    /// The externally-facing base URL.
    pub fn issuer(&self) -> &Url {
        &self.issuer
    }

    /// Build the RFC 8414 metadata document.
    pub fn metadata(&self) -> ServerMetadata {
        let base = self.issuer.as_str().trim_end_matches('/');
        ServerMetadata {
            issuer: base.to_string(),
            authorization_endpoint: format!("{base}/authorize"),
            token_endpoint: format!("{base}/token"),
            registration_endpoint: format!("{base}/register"),
            response_types_supported: vec!["code"],
            grant_types_supported: vec!["authorization_code"],
            code_challenge_methods_supported: vec!["S256"],
            token_endpoint_auth_methods_supported: vec!["none"],
        }
    }

    /// Dynamic Client Registration.
    pub fn register_client(&self, req: RegisterRequest) -> RegisterResponse {
        let client_id = format!("client-{}", random_token(16));
        let issued_at = unix_seconds_now();
        let record = ClientRecord {
            redirect_uris: req.redirect_uris.clone(),
            name: req.client_name.clone(),
            issued_at,
        };
        tracing::info!(
            client_id = %client_id,
            name = ?record.name,
            redirect_uris = ?record.redirect_uris,
            "OAuth: registered new client"
        );
        self.state
            .lock()
            .expect("oauth mutex")
            .clients
            .insert(client_id.clone(), record);
        RegisterResponse {
            client_id,
            client_id_issued_at: issued_at,
            redirect_uris: req.redirect_uris,
            token_endpoint_auth_method: "none",
            grant_types: vec!["authorization_code"],
            response_types: vec!["code"],
        }
    }

    /// Start an authorization-code flow. Returns the issued code on
    /// success — the caller (HTTP transport) is responsible for
    /// 302-redirecting the user-agent to
    /// `<redirect_uri>?code=<code>&state=<state>`.
    pub fn start_authorize(&self, req: AuthorizeRequest) -> Result<String, OAuthError> {
        if req.response_type != "code" {
            return Err(OAuthError::UnsupportedResponseType(req.response_type));
        }
        if req.code_challenge_method != "S256" {
            return Err(OAuthError::InvalidRequest(format!(
                "code_challenge_method must be S256, got {}",
                req.code_challenge_method
            )));
        }
        if req.code_challenge.is_empty() {
            return Err(OAuthError::InvalidRequest("missing code_challenge".into()));
        }
        if !self.redirect_uri_allowed(&req.redirect_uri) {
            return Err(OAuthError::InvalidRequest(format!(
                "redirect_uri `{}` is not on the operator-allow-list",
                req.redirect_uri
            )));
        }

        let code = random_token(32);
        let entry = AuthCode {
            client_id: req.client_id,
            redirect_uri: req.redirect_uri,
            code_challenge: req.code_challenge,
            issued_at: Instant::now(),
        };
        tracing::info!(
            client_id = %entry.client_id,
            redirect_uri = %entry.redirect_uri,
            "OAuth: issued authorization code (auto-approved)"
        );
        self.state
            .lock()
            .expect("oauth mutex")
            .codes
            .insert(code.clone(), entry);
        Ok(code)
    }

    /// Exchange a code for an access token. PKCE-verifies the
    /// supplied `code_verifier` against the stored `code_challenge`.
    pub fn exchange_code(&self, req: TokenRequest) -> Result<TokenResponse, OAuthError> {
        if req.grant_type != "authorization_code" {
            return Err(OAuthError::UnsupportedGrantType(req.grant_type));
        }

        let mut state = self.state.lock().expect("oauth mutex");

        // Atomically remove the code (single-use).
        let entry = state
            .codes
            .remove(&req.code)
            .ok_or_else(|| OAuthError::InvalidGrant("code not found or already redeemed".into()))?;

        if entry.issued_at.elapsed() > AUTH_CODE_TTL {
            return Err(OAuthError::InvalidGrant("code expired".into()));
        }
        if entry.redirect_uri != req.redirect_uri {
            return Err(OAuthError::InvalidGrant(
                "redirect_uri mismatch with authorization request".into(),
            ));
        }
        if entry.client_id != req.client_id {
            return Err(OAuthError::InvalidGrant(
                "client_id mismatch with authorization request".into(),
            ));
        }

        // PKCE verification (RFC 7636 §4.6).
        if !verify_pkce_s256(&req.code_verifier, &entry.code_challenge) {
            return Err(OAuthError::PkceMismatch);
        }

        let access_token = random_token(32);
        let expires_at = Instant::now() + ACCESS_TOKEN_TTL;
        state.tokens.insert(
            access_token.clone(),
            AccessToken {
                client_id: entry.client_id.clone(),
                expires_at,
            },
        );
        tracing::info!(
            client_id = %entry.client_id,
            ttl_secs = ACCESS_TOKEN_TTL.as_secs(),
            "OAuth: issued access token"
        );

        Ok(TokenResponse {
            access_token,
            token_type: "Bearer",
            expires_in: ACCESS_TOKEN_TTL.as_secs(),
        })
    }

    /// Validate a bearer token. Returns the `client_id` it was issued
    /// to on success. Performs lazy eviction of expired tokens.
    pub fn validate_access_token(&self, token: &str) -> Option<String> {
        let mut state = self.state.lock().expect("oauth mutex");
        let now = Instant::now();
        let alive = state
            .tokens
            .get(token)
            .filter(|t| t.expires_at > now)
            .map(|t| t.client_id.clone());
        if alive.is_none() {
            // Either absent or expired — drop expired entries so the
            // map doesn't grow unbounded with stale tokens. Bounded
            // O(n) sweep, fine for the operator-scale we serve.
            state.tokens.retain(|_, t| t.expires_at > now);
        }
        alive
    }

    fn redirect_uri_allowed(&self, redirect_uri: &str) -> bool {
        self.allowed_redirect_prefixes
            .iter()
            .any(|p| redirect_uri.starts_with(p))
    }
}

/// Constant-time-ish PKCE S256 check. We compute the SHA-256 of the
/// verifier ourselves, base64-url-encode (no padding), then compare
/// the resulting string to the stored challenge. The comparison is
/// byte-wise XOR to avoid leaking which byte differs first.
fn verify_pkce_s256(verifier: &str, challenge: &str) -> bool {
    if verifier.len() < 43 || verifier.len() > 128 {
        // RFC 7636 §4.1: code_verifier MUST be 43..=128 chars.
        return false;
    }
    let digest = Sha256::digest(verifier.as_bytes());
    let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    let a = computed.as_bytes();
    let b = challenge.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn random_token(n_bytes: usize) -> String {
    let mut buf = vec![0u8; n_bytes];
    OsRng.fill(&mut buf[..]);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn unix_seconds_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> OAuthServer {
        OAuthServer::new(Url::parse("https://example.invalid/").unwrap())
    }

    fn issue_pkce() -> (String, String) {
        // Verifier = 64-byte random base64url; challenge = base64url(SHA256(verifier)).
        let verifier = random_token(48); // 48 bytes → 64 base64url chars
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        (verifier, challenge)
    }

    #[test]
    fn metadata_advertises_all_required_endpoints() {
        let md = server().metadata();
        assert_eq!(md.issuer, "https://example.invalid");
        assert!(md.authorization_endpoint.ends_with("/authorize"));
        assert!(md.token_endpoint.ends_with("/token"));
        assert!(md.registration_endpoint.ends_with("/register"));
        assert!(md.code_challenge_methods_supported.contains(&"S256"));
        assert!(md.grant_types_supported.contains(&"authorization_code"));
        assert!(md.response_types_supported.contains(&"code"));
    }

    #[test]
    fn register_issues_client_id_and_records_metadata() {
        let s = server();
        let resp = s.register_client(RegisterRequest {
            redirect_uris: vec![CLAUDE_AI_REDIRECT_URI.into()],
            client_name: Some("claude.ai".into()),
            token_endpoint_auth_method: Some("none".into()),
        });
        assert!(resp.client_id.starts_with("client-"));
        assert_eq!(resp.token_endpoint_auth_method, "none");
        assert!(resp.grant_types.contains(&"authorization_code"));
    }

    #[test]
    fn authorize_then_token_round_trip_succeeds() {
        let s = server();
        let (verifier, challenge) = issue_pkce();
        let code = s
            .start_authorize(AuthorizeRequest {
                response_type: "code".into(),
                client_id: "client-xyz".into(),
                redirect_uri: CLAUDE_AI_REDIRECT_URI.into(),
                code_challenge: challenge,
                code_challenge_method: "S256".into(),
                state: Some("opaque-state".into()),
                scope: None,
            })
            .expect("authorize");
        let tok = s
            .exchange_code(TokenRequest {
                grant_type: "authorization_code".into(),
                code: code.clone(),
                redirect_uri: CLAUDE_AI_REDIRECT_URI.into(),
                client_id: "client-xyz".into(),
                code_verifier: verifier,
            })
            .expect("token");
        assert_eq!(tok.token_type, "Bearer");
        assert!(!tok.access_token.is_empty());
        assert_eq!(
            s.validate_access_token(&tok.access_token).as_deref(),
            Some("client-xyz")
        );
    }

    #[test]
    fn code_is_single_use() {
        let s = server();
        let (verifier, challenge) = issue_pkce();
        let code = s
            .start_authorize(AuthorizeRequest {
                response_type: "code".into(),
                client_id: "c".into(),
                redirect_uri: CLAUDE_AI_REDIRECT_URI.into(),
                code_challenge: challenge,
                code_challenge_method: "S256".into(),
                state: None,
                scope: None,
            })
            .unwrap();
        // First exchange succeeds.
        s.exchange_code(TokenRequest {
            grant_type: "authorization_code".into(),
            code: code.clone(),
            redirect_uri: CLAUDE_AI_REDIRECT_URI.into(),
            client_id: "c".into(),
            code_verifier: verifier.clone(),
        })
        .expect("first exchange");
        // Second exchange must fail — code consumed.
        let err = s
            .exchange_code(TokenRequest {
                grant_type: "authorization_code".into(),
                code,
                redirect_uri: CLAUDE_AI_REDIRECT_URI.into(),
                client_id: "c".into(),
                code_verifier: verifier,
            })
            .unwrap_err();
        assert!(matches!(err, OAuthError::InvalidGrant(_)));
    }

    #[test]
    fn pkce_mismatch_rejects_token_exchange() {
        let s = server();
        let (_correct_verifier, challenge) = issue_pkce();
        let code = s
            .start_authorize(AuthorizeRequest {
                response_type: "code".into(),
                client_id: "c".into(),
                redirect_uri: CLAUDE_AI_REDIRECT_URI.into(),
                code_challenge: challenge,
                code_challenge_method: "S256".into(),
                state: None,
                scope: None,
            })
            .unwrap();
        // Wrong verifier (also valid length).
        let wrong = random_token(48);
        let err = s
            .exchange_code(TokenRequest {
                grant_type: "authorization_code".into(),
                code,
                redirect_uri: CLAUDE_AI_REDIRECT_URI.into(),
                client_id: "c".into(),
                code_verifier: wrong,
            })
            .unwrap_err();
        assert!(matches!(err, OAuthError::PkceMismatch));
    }

    #[test]
    fn redirect_uri_outside_allow_list_is_rejected() {
        let s = server();
        let (_, challenge) = issue_pkce();
        let err = s
            .start_authorize(AuthorizeRequest {
                response_type: "code".into(),
                client_id: "c".into(),
                redirect_uri: "https://evil.example/cb".into(),
                code_challenge: challenge,
                code_challenge_method: "S256".into(),
                state: None,
                scope: None,
            })
            .unwrap_err();
        assert!(matches!(err, OAuthError::InvalidRequest(_)));
    }

    #[test]
    fn rejects_plain_pkce_method() {
        let s = server();
        let err = s
            .start_authorize(AuthorizeRequest {
                response_type: "code".into(),
                client_id: "c".into(),
                redirect_uri: CLAUDE_AI_REDIRECT_URI.into(),
                code_challenge: "anything".into(),
                code_challenge_method: "plain".into(),
                state: None,
                scope: None,
            })
            .unwrap_err();
        assert!(matches!(err, OAuthError::InvalidRequest(_)));
    }

    #[test]
    fn rejects_non_code_response_type() {
        let s = server();
        let (_, challenge) = issue_pkce();
        let err = s
            .start_authorize(AuthorizeRequest {
                response_type: "token".into(),
                client_id: "c".into(),
                redirect_uri: CLAUDE_AI_REDIRECT_URI.into(),
                code_challenge: challenge,
                code_challenge_method: "S256".into(),
                state: None,
                scope: None,
            })
            .unwrap_err();
        assert!(matches!(err, OAuthError::UnsupportedResponseType(_)));
    }

    #[test]
    fn invalid_pkce_verifier_length_rejected() {
        let challenge = "anything";
        assert!(!verify_pkce_s256("short", challenge), "<43 chars must fail");
        assert!(
            !verify_pkce_s256(&"a".repeat(129), challenge),
            ">128 chars must fail"
        );
    }

    #[test]
    fn validate_returns_none_for_unknown_token() {
        let s = server();
        assert!(s.validate_access_token("never-issued").is_none());
    }

    #[test]
    fn allowed_redirect_prefix_can_be_extended() {
        let s = server().with_allowed_redirect_prefix("https://my-other-host.example/cb");
        let (_, challenge) = issue_pkce();
        s.start_authorize(AuthorizeRequest {
            response_type: "code".into(),
            client_id: "c".into(),
            redirect_uri: "https://my-other-host.example/cb/whatever".into(),
            code_challenge: challenge,
            code_challenge_method: "S256".into(),
            state: None,
            scope: None,
        })
        .expect("extended host must be accepted");
    }
}
