//! Property-based tests for the parser boundaries — the whole-space laws
//! the unit tests only sample: every canonically-shaped input parses and
//! round-trips; every input violating the grammar is rejected, whatever
//! shape it takes.

use ferridis_core::{CapabilityRef, CapabilityVersion, IntentVerb};
use proptest::prelude::*;

// ── Generators ────────────────────────────────────────────────────────────────

/// Canonical intent verbs: dash-separated groups of `[a-z0-9]`.
fn canonical_verb() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z0-9]{1,8}(-[a-z0-9]{1,8}){0,4}")
        .expect("valid generator regex")
}

/// Canonical capability versions: `v` + dot-separated digit groups.
fn canonical_version() -> impl Strategy<Value = String> {
    proptest::string::string_regex("v[0-9]{1,3}(\\.[0-9]{1,3}){0,2}")
        .expect("valid generator regex")
}

/// URL path segments for capability refs (no `/`, no `@`).
fn ref_segment() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z0-9][a-z0-9.-]{0,9}").expect("valid generator regex")
}

proptest! {
    // ── IntentVerb ───────────────────────────────────────────────────────────

    /// Every canonically-shaped verb parses, and the parse is lossless.
    #[test]
    fn canonical_verbs_parse_and_round_trip(s in canonical_verb()) {
        let verb = IntentVerb::parse(&s).expect("canonical verb must parse");
        prop_assert_eq!(verb.as_str(), s.as_str());
        prop_assert_eq!(verb.to_string(), s);
    }

    /// Parsed verbs survive a serde JSON round-trip unchanged — the
    /// `try_from = "String"` boundary is lossless for valid values.
    #[test]
    fn verbs_round_trip_through_serde(s in canonical_verb()) {
        let verb = IntentVerb::parse(&s).expect("canonical verb must parse");
        let json = serde_json::to_string(&verb).expect("serialize");
        let back: IntentVerb = serde_json::from_str(&json).expect("deserialize");
        prop_assert_eq!(verb, back);
    }

    /// Any uppercase character anywhere makes the verb unparseable — and
    /// therefore undeserializable (no bypass around the constructor).
    #[test]
    fn verbs_with_uppercase_are_rejected_everywhere(
        prefix in canonical_verb(),
        upper in "[A-Z]",
        suffix in canonical_verb(),
    ) {
        let bad = format!("{prefix}{upper}{suffix}");
        prop_assert!(IntentVerb::parse(&bad).is_err());
        let json = serde_json::to_string(&bad).expect("serialize raw string");
        prop_assert!(serde_json::from_str::<IntentVerb>(&json).is_err());
    }

    /// Leading, trailing, and doubled dashes are rejected wherever the
    /// dash lands.
    #[test]
    fn verbs_with_malformed_dashes_are_rejected(core in canonical_verb()) {
        let leading = format!("-{core}");
        let trailing = format!("{core}-");
        let doubled = format!("{core}--{core}");
        prop_assert!(IntentVerb::parse(&leading).is_err());
        prop_assert!(IntentVerb::parse(&trailing).is_err());
        prop_assert!(IntentVerb::parse(&doubled).is_err());
    }

    // ── CapabilityVersion ────────────────────────────────────────────────────

    /// Every canonically-shaped version parses and round-trips.
    #[test]
    fn canonical_versions_parse_and_round_trip(s in canonical_version()) {
        let v = CapabilityVersion::parse(&s).expect("canonical version must parse");
        prop_assert_eq!(v.as_str(), s.as_str());
    }

    /// A version not starting with `v`, or with non-digit non-dot tail
    /// characters, is rejected.
    #[test]
    fn malformed_versions_are_rejected(
        tail in canonical_version(),
        junk in "[a-uw-z]",
    ) {
        // Missing the `v` prefix entirely.
        prop_assert!(CapabilityVersion::parse(tail.trim_start_matches('v')).is_err());
        // A letter smuggled into the numeric tail.
        let smuggled = format!("{tail}{junk}");
        prop_assert!(CapabilityVersion::parse(&smuggled).is_err());
    }

    // ── CapabilityRef ────────────────────────────────────────────────────────

    /// A ref built from valid segments parses, exposes each part
    /// verbatim, and `Display → parse` is the identity (including
    /// nested namespaces).
    #[test]
    fn capability_refs_round_trip_through_display(
        registry in ref_segment(),
        ns_segments in proptest::collection::vec(ref_segment(), 1..4),
        id in ref_segment(),
        version in canonical_version(),
    ) {
        let namespace = ns_segments.join("/");
        let raw = format!("ferridis://{registry}/{namespace}/{id}@{version}");
        let parsed = CapabilityRef::parse(&raw).expect("canonical ref must parse");
        prop_assert_eq!(parsed.registry(), registry.as_str());
        prop_assert_eq!(parsed.namespace(), namespace.as_str());
        prop_assert_eq!(parsed.id(), id.as_str());
        prop_assert_eq!(parsed.version().as_str(), version.as_str());

        let reparsed = CapabilityRef::parse(&parsed.to_string()).expect("Display must re-parse");
        prop_assert_eq!(parsed, reparsed);
    }

    /// Refs missing the scheme, the version, or a path segment are
    /// rejected.
    #[test]
    fn structurally_incomplete_refs_are_rejected(
        registry in ref_segment(),
        id in ref_segment(),
        version in canonical_version(),
    ) {
        let wrong_scheme = format!("https://{registry}/ns/{id}@{version}");
        let no_version = format!("ferridis://{registry}/ns/{id}");
        let two_segments = format!("ferridis://{registry}/{id}@{version}");
        prop_assert!(CapabilityRef::parse(&wrong_scheme).is_err());
        prop_assert!(CapabilityRef::parse(&no_version).is_err());
        prop_assert!(CapabilityRef::parse(&two_segments).is_err());
    }
}
