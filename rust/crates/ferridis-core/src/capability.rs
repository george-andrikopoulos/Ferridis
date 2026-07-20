//! Capability references — parsed `ferridis://` URLs.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::FerridisError;

/// A capability version, embedded in the capability URL after `@`.
///
/// Versions are validated to start with `v` followed by digits, optionally
/// with `.` separators (e.g. `v1`, `v3`, `v2.1`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CapabilityVersion(String);

impl TryFrom<String> for CapabilityVersion {
    type Error = FerridisError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<CapabilityVersion> for String {
    fn from(v: CapabilityVersion) -> Self {
        v.0
    }
}

impl CapabilityVersion {
    /// Parse a version string. Validates the format.
    pub fn parse(s: &str) -> Result<Self, FerridisError> {
        if !s.starts_with('v') {
            return Err(FerridisError::Parse(format!(
                "capability version must start with 'v', got: {s}"
            )));
        }
        let rest = &s[1..];
        if rest.is_empty() {
            return Err(FerridisError::Parse(
                "capability version is empty after 'v'".into(),
            ));
        }
        if !rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return Err(FerridisError::Parse(format!(
                "capability version must be digits and dots after 'v', got: {s}"
            )));
        }
        Ok(Self(s.to_string()))
    }

    /// The version string as written (e.g. `"v3"`).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CapabilityVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CapabilityVersion {
    type Err = FerridisError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// A reference to a capability, parsed from a `ferridis://` URL.
///
/// Format: `ferridis://<registry>/<namespace>/<id>@<version>`
///
/// Example: `ferridis://public.ferridis.io/google/calendar@v3`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CapabilityRef {
    registry: String,
    namespace: String,
    id: String,
    version: CapabilityVersion,
}

impl CapabilityRef {
    /// Parse a capability reference URL.
    pub fn parse(url: &str) -> Result<Self, FerridisError> {
        let prefix = "ferridis://";
        let rest = url.strip_prefix(prefix).ok_or_else(|| {
            FerridisError::Parse(format!("expected ferridis:// scheme, got: {url}"))
        })?;

        let (path_part, version_str) = rest.rsplit_once('@').ok_or_else(|| {
            FerridisError::Parse(format!("missing @version in capability ref: {url}"))
        })?;
        let version = CapabilityVersion::parse(version_str)?;

        let segments: Vec<&str> = path_part.split('/').collect();
        if segments.len() < 3 {
            return Err(FerridisError::Parse(format!(
                "capability ref needs registry/namespace/id segments, got: {url}"
            )));
        }
        let registry = segments[0].to_string();
        let namespace = segments[1..segments.len() - 1].join("/");
        let id = segments[segments.len() - 1].to_string();

        if registry.is_empty() || namespace.is_empty() || id.is_empty() {
            return Err(FerridisError::Parse(format!(
                "capability ref segments must be non-empty: {url}"
            )));
        }

        Ok(Self {
            registry,
            namespace,
            id,
            version,
        })
    }

    /// The registry host (e.g. `public.ferridis.io`).
    pub fn registry(&self) -> &str {
        &self.registry
    }

    /// The namespace within the registry (e.g. `google`).
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The capability ID within its namespace (e.g. `calendar`).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The capability version (e.g. `v3`).
    pub fn version(&self) -> &CapabilityVersion {
        &self.version
    }
}

impl fmt::Display for CapabilityRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ferridis://{}/{}/{}@{}",
            self.registry, self.namespace, self.id, self.version
        )
    }
}

impl FromStr for CapabilityRef {
    type Err = FerridisError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for CapabilityRef {
    type Error = FerridisError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<CapabilityRef> for String {
    fn from(c: CapabilityRef) -> Self {
        c.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_capability_ref() {
        let r = CapabilityRef::parse("ferridis://public.ferridis.io/google/calendar@v3").unwrap();
        assert_eq!(r.registry(), "public.ferridis.io");
        assert_eq!(r.namespace(), "google");
        assert_eq!(r.id(), "calendar");
        assert_eq!(r.version().as_str(), "v3");
    }

    #[test]
    fn parses_nested_namespace() {
        let r = CapabilityRef::parse("ferridis://wallet/local/home-server/lights@v1").unwrap();
        assert_eq!(r.registry(), "wallet");
        assert_eq!(r.namespace(), "local/home-server");
        assert_eq!(r.id(), "lights");
    }

    #[test]
    fn rejects_missing_scheme() {
        assert!(CapabilityRef::parse("public.ferridis.io/google/calendar@v3").is_err());
    }

    #[test]
    fn rejects_missing_version() {
        assert!(CapabilityRef::parse("ferridis://public.ferridis.io/google/calendar").is_err());
    }

    #[test]
    fn rejects_invalid_version() {
        assert!(CapabilityRef::parse("ferridis://public.ferridis.io/google/calendar@3").is_err());
        assert!(
            CapabilityRef::parse("ferridis://public.ferridis.io/google/calendar@vfoo").is_err()
        );
    }

    #[test]
    fn round_trips_through_display() {
        let s = "ferridis://public.ferridis.io/google/calendar@v3";
        let r = CapabilityRef::parse(s).unwrap();
        assert_eq!(r.to_string(), s);
    }

    #[test]
    fn capability_version_deserialize_runs_through_validator() {
        // Valid versions round-trip.
        let ok: CapabilityVersion = serde_json::from_str("\"v3\"").unwrap();
        assert_eq!(ok.as_str(), "v3");
        let ok2: CapabilityVersion = serde_json::from_str("\"v2.1\"").unwrap();
        assert_eq!(ok2.as_str(), "v2.1");

        // Pre-fix: serde(transparent) accepted these silently.
        for bad in [r#""""#, r#""3""#, r#""v""#, r#""vfoo""#, r#""v3-beta""#] {
            assert!(
                serde_json::from_str::<CapabilityVersion>(bad).is_err(),
                "JSON `{bad}` must not deserialize into CapabilityVersion"
            );
        }
    }
}
