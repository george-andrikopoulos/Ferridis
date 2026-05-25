//! Error types for the Ferridis core crate.

use thiserror::Error;

/// Errors that can occur in the Ferridis core types.
#[derive(Debug, Error)]
pub enum FerridisError {
    /// A value failed to parse or validate.
    #[error("parse error: {0}")]
    Parse(String),

    /// A required field was missing or empty.
    #[error("missing required field: {0}")]
    MissingField(&'static str),

    /// A field exceeded its size or value bound.
    #[error("field out of bounds: {field}: {detail}")]
    OutOfBounds {
        /// The field that was out of bounds.
        field: &'static str,
        /// A description of how it was out of bounds.
        detail: String,
    },

    /// Manifest validation failed.
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    /// JSON deserialization failed.
    #[error("JSON error: {0}")]
    Json(String),
}
