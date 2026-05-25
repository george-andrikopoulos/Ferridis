//! Schema-driven request body validation for adapter dispatch (v0.4).
//!
//! The server calls [`validate_body`] before forwarding a request to the
//! adapter's [`Capability::dispatch`] implementation. Adapters opt in by
//! implementing [`Capability::body_schema`]; adapters that return `None`
//! pass validation unconditionally (existing adapters keep compiling).

use serde_json::Value;

/// A single validation failure at a specific location in the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// JSON Pointer path to the failing value (e.g. `"/limit"`).
    pub instance_path: String, // allow:pub-field
    /// Human-readable description of the failure.
    pub detail: String, // allow:pub-field
}

/// The outcome of validating a request body against a JSON Schema.
///
/// Marked `#[must_use]` so callers cannot silently discard the result.
#[must_use]
#[derive(Debug)]
pub enum ValidationOutcome {
    /// Body conforms to the schema.
    Valid,
    /// Body violates the schema; at least one error is present.
    Invalid {
        /// One entry per violated constraint.
        errors: Vec<ValidationError>,
    },
}

/// Validate `body` against a JSON Schema `schema`.
///
/// Returns [`ValidationOutcome::Valid`] when the body conforms.
/// Returns [`ValidationOutcome::Invalid`] when one or more constraints
/// are violated.
///
/// **Lenient on malformed schemas**: if `schema` cannot be compiled by
/// the validator, the function logs a warning and returns `Valid` — the
/// adapter remains the final validator, and a broken schema should not
/// silently reject valid requests. This mirrors the client-side policy
/// in `ferridis-client`.
pub fn validate_body(schema: &Value, body: &Value) -> ValidationOutcome {
    let compiled = match jsonschema::validator_for(schema) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "adapter-sdk: body_schema failed to compile; skipping validation"
            );
            return ValidationOutcome::Valid;
        }
    };

    let errors: Vec<ValidationError> = compiled
        .iter_errors(body)
        .map(|e| ValidationError {
            instance_path: e.instance_path.to_string(),
            detail: e.to_string(),
        })
        .collect();

    if errors.is_empty() {
        ValidationOutcome::Valid
    } else {
        ValidationOutcome::Invalid { errors }
    }
}
