//! Error types for the XMPP server.

use thiserror::Error;

/// XMPP server errors.
#[derive(Debug, Error)]
pub enum XmppError {
    /// IO error (network, file)
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// TLS error
    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),

    /// XML parsing error
    #[error("XML parse error: {0}")]
    XmlParse(String),

    /// Authentication failed
    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    /// Session not found or expired
    #[error("Session not found or expired")]
    SessionNotFound,

    /// Permission denied
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// Resource conflict (e.g., duplicate resource binding)
    #[error("Resource conflict: {0}")]
    ResourceConflict(String),

    /// MUC error
    #[error("MUC error: {0}")]
    Muc(String),

    /// Stream error
    #[error("Stream error: {0}")]
    Stream(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Internal server error
    #[error("Internal error: {0}")]
    Internal(String),
}

impl XmppError {
    /// Create a new XML parse error.
    pub fn xml_parse(msg: impl Into<String>) -> Self {
        Self::XmlParse(msg.into())
    }

    /// Create a new authentication error.
    pub fn auth_failed(msg: impl Into<String>) -> Self {
        Self::AuthFailed(msg.into())
    }

    /// Create a new permission denied error.
    pub fn permission_denied(msg: impl Into<String>) -> Self {
        Self::PermissionDenied(msg.into())
    }

    /// Create a new MUC error.
    pub fn muc(msg: impl Into<String>) -> Self {
        Self::Muc(msg.into())
    }

    /// Create a new stream error.
    pub fn stream(msg: impl Into<String>) -> Self {
        Self::Stream(msg.into())
    }

    /// Create a new configuration error.
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// Create a new internal error.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}
