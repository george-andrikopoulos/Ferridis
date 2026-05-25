//! Intent verbs — the controlled vocabulary used by the routing layer.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::FerridisError;

/// An intent verb in canonical form.
///
/// Verbs are ASCII, lowercase, dash-separated, optionally with digits
/// (e.g. `send-message`, `read-events`, `find-time`, `query-v2`). The
/// vocabulary itself is governed by the mesh registry; this type
/// enforces only the syntactic rules.
///
/// **Digits accepted (v0.2)** to support versioned identifiers in
/// projected names from external systems — for example, an MCP tool
/// `ferridis_fs_v1_read_file` projects to the Ferridis intent verb
/// `ferridis-fs-v1-read-file`. The previous rule (letters and dashes
/// only) made that projection impossible.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IntentVerb(String);

impl TryFrom<String> for IntentVerb {
    type Error = FerridisError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<IntentVerb> for String {
    fn from(v: IntentVerb) -> Self {
        v.0
    }
}

impl IntentVerb {
    /// Parse and validate an intent verb.
    pub fn parse(s: &str) -> Result<Self, FerridisError> {
        if s.is_empty() {
            return Err(FerridisError::MissingField("intent verb"));
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(FerridisError::Parse(format!(
                "intent verb must be lowercase ASCII letters, digits, or dashes, got: {s}"
            )));
        }
        if s.starts_with('-') || s.ends_with('-') || s.contains("--") {
            return Err(FerridisError::Parse(format!(
                "intent verb must not have leading, trailing, or double dashes: {s}"
            )));
        }
        Ok(Self(s.to_string()))
    }

    /// The canonical string form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IntentVerb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for IntentVerb {
    type Err = FerridisError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_verbs() {
        assert!(IntentVerb::parse("send-message").is_ok());
        assert!(IntentVerb::parse("read-events").is_ok());
        assert!(IntentVerb::parse("find-time").is_ok());
    }

    #[test]
    fn accepts_verbs_with_digits() {
        assert!(IntentVerb::parse("query-v2").is_ok());
        assert!(IntentVerb::parse("ferridis-fs-v1-read-file").is_ok());
        assert!(IntentVerb::parse("v3").is_ok());
    }

    #[test]
    fn rejects_uppercase() {
        assert!(IntentVerb::parse("Send-Message").is_err());
    }

    #[test]
    fn rejects_underscores() {
        assert!(IntentVerb::parse("send_message").is_err());
    }

    #[test]
    fn rejects_leading_dash() {
        assert!(IntentVerb::parse("-send").is_err());
    }

    #[test]
    fn rejects_trailing_dash() {
        assert!(IntentVerb::parse("send-").is_err());
    }

    #[test]
    fn rejects_double_dash() {
        assert!(IntentVerb::parse("send--message").is_err());
    }

    #[test]
    fn deserialize_runs_through_validator() {
        // Valid verb round-trips.
        let ok: IntentVerb = serde_json::from_str("\"send-message\"").unwrap();
        assert_eq!(ok.as_str(), "send-message");
        assert_eq!(serde_json::to_string(&ok).unwrap(), "\"send-message\"");

        // Pre-fix: serde(transparent) accepted these silently; post-fix
        // they fail with a validating-deserialize error.
        let cases = [
            r#""""#,
            r#""Send-Message""#,
            r#""send_message""#,
            r#""-send""#,
            r#""send-""#,
            r#""send--message""#,
        ];
        for case in cases {
            assert!(
                serde_json::from_str::<IntentVerb>(case).is_err(),
                "JSON `{case}` must not deserialize into IntentVerb"
            );
        }
    }
}
