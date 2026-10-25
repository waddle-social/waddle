//! Shared request-validation errors for typed XMPP primitives.

use thiserror::Error;

/// Result of an operation using the shared XMPP validation error type.
pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// Validation failures returned by shared XMPP parsers and builders.
#[non_exhaustive]
pub enum CoreError {
    #[error("bad request")]
    /// Bad request.
    BadRequest(Option<String>),

    #[error("not acceptable")]
    /// Not acceptable.
    NotAcceptable(Option<String>),

    #[error("waddle-xmpp-core scaffold is not implemented yet")]
    /// Waddle-xmpp-core scaffold is not implemented yet.
    NotImplemented,
}

impl CoreError {
    /// Create a malformed-request error with optional explanatory text.
    #[must_use]
    pub fn bad_request(text: Option<String>) -> Self {
        Self::BadRequest(text)
    }

    /// Create an unacceptable-request error with optional explanatory text.
    #[must_use]
    pub fn not_acceptable(text: Option<String>) -> Self {
        Self::NotAcceptable(text)
    }
}
