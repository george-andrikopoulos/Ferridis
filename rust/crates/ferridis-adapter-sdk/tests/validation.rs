//! RED tests for Task 2 — adapter-SDK request validation (v0.4).

use ferridis_adapter_sdk::validation::{ValidationOutcome, validate_body};
use serde_json::json;

#[test]
fn valid_body_returns_valid() {
    let schema = json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"],
        "additionalProperties": false
    });
    let outcome = validate_body(&schema, &json!({"path": "/tmp/foo"}));
    assert!(matches!(outcome, ValidationOutcome::Valid));
}

#[test]
fn wrong_type_returns_invalid_with_errors() {
    let schema = json!({
        "type": "object",
        "properties": { "limit": { "type": "integer" } },
        "required": ["limit"]
    });
    let outcome = validate_body(&schema, &json!({"limit": "not-a-number"}));
    match outcome {
        ValidationOutcome::Invalid { errors } => {
            assert!(!errors.is_empty(), "expected at least one validation error");
        }
        ValidationOutcome::Valid => panic!("expected Invalid, got Valid"),
    }
}

#[test]
fn missing_required_field_returns_invalid() {
    let schema = json!({
        "type": "object",
        "required": ["path"]
    });
    let outcome = validate_body(&schema, &json!({}));
    assert!(matches!(outcome, ValidationOutcome::Invalid { .. }));
}

#[test]
fn malformed_schema_is_lenient() {
    // A schema that can't compile should not crash the adapter —
    // it should allow the body through (adapter remains the final validator).
    let bad_schema = json!({"$ref": "#/definitions/DoesNotExist"});
    let outcome = validate_body(&bad_schema, &json!({"anything": true}));
    assert!(matches!(outcome, ValidationOutcome::Valid));
}
