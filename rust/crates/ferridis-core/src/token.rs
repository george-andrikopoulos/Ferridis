//! Wrappers for OAuth tokens that prevent accidental disclosure.
//!
//! [`AccessToken`] and [`RefreshToken`] wrap their secret material in
//! [`secrecy::SecretString`] so it cannot be accidentally logged or
//! printed. The [`std::fmt::Debug`] implementation deliberately redacts
//! the inner value.

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An OAuth access token.
#[derive(Clone)]
pub struct AccessToken(SecretString);

impl AccessToken {
    /// Wrap a raw token string.
    pub fn new(s: impl Into<String>) -> Self {
        Self(SecretString::new(s.into()))
    }

    /// Expose the inner string. Use only when sending the token over the wire.
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken(***)")
    }
}

// Manual Serde impls: `secrecy::SecretString` deliberately refuses
// blanket `Serialize`/`Deserialize` so callers must opt in. Serialization
// is required for persistence in the encrypted connection wallet.
impl Serialize for AccessToken {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        self.0.expose_secret().serialize(ser)
    }
}

impl<'de> Deserialize<'de> for AccessToken {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        String::deserialize(de).map(Self::new)
    }
}

/// An OAuth refresh token.
#[derive(Clone)]
pub struct RefreshToken(SecretString);

impl RefreshToken {
    /// Wrap a raw token string.
    pub fn new(s: impl Into<String>) -> Self {
        Self(SecretString::new(s.into()))
    }

    /// Expose the inner string. Use only when sending the token over the wire.
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl std::fmt::Debug for RefreshToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RefreshToken(***)")
    }
}

impl Serialize for RefreshToken {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        self.0.expose_secret().serialize(ser)
    }
}

impl<'de> Deserialize<'de> for RefreshToken {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        String::deserialize(de).map(Self::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "sk-live-EXTREMELY-SECRET-VALUE-12345";

    #[test]
    fn access_token_debug_never_leaks_the_secret() {
        let token = AccessToken::new(SECRET);
        let rendered = format!("{token:?}");
        assert!(!rendered.contains(SECRET));
        assert_eq!(rendered, "AccessToken(***)");
    }

    #[test]
    fn refresh_token_debug_never_leaks_the_secret() {
        let token = RefreshToken::new(SECRET);
        let rendered = format!("{token:?}");
        assert!(!rendered.contains(SECRET));
        assert_eq!(rendered, "RefreshToken(***)");
    }

    #[test]
    fn access_token_serde_round_trip_preserves_secret_but_debug_stays_redacted() {
        let json = serde_json::to_string(&AccessToken::new(SECRET)).expect("serialize token");
        assert_eq!(json, format!("\"{SECRET}\""));
        let back: AccessToken = serde_json::from_str(&json).expect("deserialize token");
        assert_eq!(back.expose(), SECRET);
        assert!(!format!("{back:?}").contains(SECRET));
    }
}
