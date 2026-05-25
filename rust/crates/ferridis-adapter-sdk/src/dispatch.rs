//! Dispatch-time error type for adapter implementations.

use ferridis_core::IntentVerb;
use thiserror::Error;

/// Errors an adapter's [`Capability::dispatch`](crate::Capability::dispatch)
/// implementation can return.
#[derive(Debug, Error)]
pub enum DispatchError {
    /// The intent verb is not implemented by this capability.
    #[error("intent `{0}` is not supported by this capability")]
    UnsupportedIntent(IntentVerb),

    /// The request body failed validation against the schema or the
    /// capability's own constraints.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// The capability could not be authorised to perform this operation
    /// (separate from token validity — this is the underlying service's
    /// authorization layer).
    #[error("operation not permitted: {0}")]
    Forbidden(String),

    /// The target resource was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// An internal error in the adapter itself (filesystem I/O, etc.).
    #[error("internal adapter error: {0}")]
    Internal(String),
}

impl DispatchError {
    /// Map this error to an HTTP status code.
    pub fn status_code(&self) -> u16 {
        match self {
            DispatchError::UnsupportedIntent(_) => 404,
            DispatchError::InvalidRequest(_) => 400,
            DispatchError::Forbidden(_) => 403,
            DispatchError::NotFound(_) => 404,
            DispatchError::Internal(_) => 500,
        }
    }
}

impl From<serde_json::Error> for DispatchError {
    fn from(e: serde_json::Error) -> Self {
        // Serialization failures are adapter-internal: by construction
        // the values we produce should always serialize cleanly. A
        // failure here is a bug, not a bad request.
        DispatchError::Internal(format!("serde_json: {e}"))
    }
}
