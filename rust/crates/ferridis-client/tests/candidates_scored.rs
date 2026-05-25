//! RED tests for Task 6 — confidence scoring in candidates_for_intent (v0.4).

use ferridis_client::registry::{CapabilityRecord, ConfidenceScore, ScoredCandidate};
use ferridis_client::{Registry, RegistryTier};
use ferridis_core::{CapabilityRef, Manifest};
use url::Url;

const MANIFEST_A: &str = r#"{
    "ferridis_version": "0.1",
    "id": "test.a.v1",
    "name": "A",
    "category": "files",
    "summary": "Test capability A.",
    "intents": ["read"],
    "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
    "tiers": ["native"],
    "auth": { "type": "none" }
}"#;

const MANIFEST_B: &str = r#"{
    "ferridis_version": "0.1",
    "id": "test.b.v1",
    "name": "B",
    "category": "files",
    "summary": "Test capability B.",
    "intents": ["read"],
    "schema": { "type": "openapi-3", "url": "https://x/s.yaml" },
    "tiers": ["native"],
    "auth": { "type": "none" }
}"#;

fn make_record(json: &str, cap: &str, base: &str, tier: RegistryTier) -> CapabilityRecord {
    let m = Manifest::parse(json).unwrap(); // allow:unwrap — test setup
    let cap = CapabilityRef::parse(cap).unwrap(); // allow:unwrap — test setup
    let base = Url::parse(base).unwrap(); // allow:unwrap — test setup
    CapabilityRecord::new(cap, m, base).with_tier(tier)
}

/// Personal-tier candidate scores higher than public-tier.
#[test]
fn personal_scores_higher_than_public() {
    let mut r = Registry::new();
    r.insert(make_record(
        MANIFEST_A,
        "ferridis://personal.local/test/a@v1",
        "http://a.local/",
        RegistryTier::Personal,
    ));
    r.insert(make_record(
        MANIFEST_B,
        "ferridis://public.mesh/test/b@v1",
        "http://b.mesh/",
        RegistryTier::Public,
    ));

    let intent = "read".parse().unwrap(); // allow:unwrap — test setup
    let scored = r.candidates_for_intent_scored(&intent);
    assert_eq!(scored.len(), 2);
    assert!(
        scored[0].score().as_u8() >= scored[1].score().as_u8(),
        "higher-scored candidate must come first"
    );
    let personal_cap: CapabilityRef = "ferridis://personal.local/test/a@v1".parse().unwrap(); // allow:unwrap — test setup
    assert_eq!(scored[0].capability(), &personal_cap);
}

/// ConfidenceScore tier constants are in 0..=100 and ordered correctly.
#[test]
fn confidence_score_ordering() {
    assert!(ConfidenceScore::personal().as_u8() <= 100);
    assert!(ConfidenceScore::org().as_u8() <= 100);
    assert!(ConfidenceScore::public().as_u8() <= 100);
    assert!(ConfidenceScore::personal().as_u8() > ConfidenceScore::org().as_u8());
    assert!(ConfidenceScore::org().as_u8() > ConfidenceScore::public().as_u8());
}

/// ScoredCandidate exposes capability ref and score accessors.
#[test]
fn scored_candidate_accessors() {
    let cap: CapabilityRef = "ferridis://personal.local/test/x@v1".parse().unwrap(); // allow:unwrap — test setup
    let sc = ScoredCandidate::new(cap.clone(), ConfidenceScore::personal()); // clone: ScoredCandidate takes owned value
    assert_eq!(sc.capability(), &cap);
    assert_eq!(sc.score(), ConfidenceScore::personal());
}
