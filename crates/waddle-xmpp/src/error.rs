//! Error types for the XMPP server.

use thiserror::Error;

use crate::parser::ns;

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

    /// Stanza error (for IQ error responses)
    #[error("Stanza error: {condition}")]
    Stanza {
        /// Error condition
        condition: StanzaErrorCondition,
        /// Error type
        error_type: StanzaErrorType,
        /// Optional text description
        text: Option<String>,
    },
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

    /// Create a stanza error for 'not-authorized'.
    pub fn not_authorized(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::NotAuthorized,
            error_type: StanzaErrorType::Auth,
            text,
        }
    }

    /// Create a stanza error for 'bad-request'.
    pub fn bad_request(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::BadRequest,
            error_type: StanzaErrorType::Modify,
            text,
        }
    }

    /// Create a stanza error for 'item-not-found'.
    pub fn item_not_found(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::ItemNotFound,
            error_type: StanzaErrorType::Cancel,
            text,
        }
    }

    /// Create a stanza error for 'feature-not-implemented'.
    pub fn feature_not_implemented(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::FeatureNotImplemented,
            error_type: StanzaErrorType::Cancel,
            text,
        }
    }

    /// Create a stanza error for 'forbidden'.
    pub fn forbidden(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::Forbidden,
            error_type: StanzaErrorType::Auth,
            text,
        }
    }

    /// Create a stanza error for 'internal-server-error'.
    pub fn internal_server_error(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::InternalServerError,
            error_type: StanzaErrorType::Wait,
            text,
        }
    }

    /// Create a stanza error for 'service-unavailable'.
    pub fn service_unavailable(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::ServiceUnavailable,
            error_type: StanzaErrorType::Cancel,
            text,
        }
    }
}

/// XMPP stanza error conditions (RFC 6120 Section 8.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StanzaErrorCondition {
    /// Bad request (malformed XML, etc.)
    BadRequest,
    /// Conflict (e.g., resource already bound)
    Conflict,
    /// Feature not implemented
    FeatureNotImplemented,
    /// Forbidden (permission denied)
    Forbidden,
    /// Gone (entity no longer available)
    Gone,
    /// Internal server error
    InternalServerError,
    /// Item not found
    ItemNotFound,
    /// JID malformed
    JidMalformed,
    /// Not acceptable
    NotAcceptable,
    /// Not allowed
    NotAllowed,
    /// Not authorized
    NotAuthorized,
    /// Policy violation
    PolicyViolation,
    /// Recipient unavailable
    RecipientUnavailable,
    /// Redirect
    Redirect,
    /// Registration required
    RegistrationRequired,
    /// Remote server not found
    RemoteServerNotFound,
    /// Remote server timeout
    RemoteServerTimeout,
    /// Resource constraint
    ResourceConstraint,
    /// Service unavailable
    ServiceUnavailable,
    /// Subscription required
    SubscriptionRequired,
    /// Undefined condition
    UndefinedCondition,
    /// Unexpected request
    UnexpectedRequest,
}

impl StanzaErrorCondition {
    /// Get the element name for this condition.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BadRequest => "bad-request",
            Self::Conflict => "conflict",
            Self::FeatureNotImplemented => "feature-not-implemented",
            Self::Forbidden => "forbidden",
            Self::Gone => "gone",
            Self::InternalServerError => "internal-server-error",
            Self::ItemNotFound => "item-not-found",
            Self::JidMalformed => "jid-malformed",
            Self::NotAcceptable => "not-acceptable",
            Self::NotAllowed => "not-allowed",
            Self::NotAuthorized => "not-authorized",
            Self::PolicyViolation => "policy-violation",
            Self::RecipientUnavailable => "recipient-unavailable",
            Self::Redirect => "redirect",
            Self::RegistrationRequired => "registration-required",
            Self::RemoteServerNotFound => "remote-server-not-found",
            Self::RemoteServerTimeout => "remote-server-timeout",
            Self::ResourceConstraint => "resource-constraint",
            Self::ServiceUnavailable => "service-unavailable",
            Self::SubscriptionRequired => "subscription-required",
            Self::UndefinedCondition => "undefined-condition",
            Self::UnexpectedRequest => "unexpected-request",
        }
    }
}

impl std::fmt::Display for StanzaErrorCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// XMPP stanza error types (RFC 6120 Section 8.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StanzaErrorType {
    /// Retry after providing credentials
    Auth,
    /// Do not retry (unrecoverable error)
    Cancel,
    /// Retry after changing the data sent
    Modify,
    /// Retry after waiting (temporary error)
    Wait,
}

impl StanzaErrorType {
    /// Get the type attribute value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Cancel => "cancel",
            Self::Modify => "modify",
            Self::Wait => "wait",
        }
    }
}

impl std::fmt::Display for StanzaErrorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Generate an IQ error response.
///
/// Creates an error IQ stanza with the appropriate error element.
pub fn generate_iq_error(
    id: &str,
    to: Option<&str>,
    from: Option<&str>,
    condition: StanzaErrorCondition,
    error_type: StanzaErrorType,
    text: Option<&str>,
) -> String {
    let mut iq = format!("<iq type='error' id='{}'", id);

    if let Some(to) = to {
        iq.push_str(&format!(" to='{}'", to));
    }

    if let Some(from) = from {
        iq.push_str(&format!(" from='{}'", from));
    }

    iq.push_str(&format!(
        "><error type='{}'><{} xmlns='{}'/>{}</error></iq>",
        error_type.as_str(),
        condition.as_str(),
        ns::STANZAS,
        text.map(|t| format!("<text xmlns='{}' xml:lang='en'>{}</text>", ns::STANZAS, t))
            .unwrap_or_default()
    ));

    iq
}

/// Generate a stream error and close tag.
///
/// Stream errors are fatal and must be followed by closing the stream.
pub fn generate_stream_error(condition: &str, text: Option<&str>) -> String {
    let mut error = format!(
        "<stream:error><{} xmlns='urn:ietf:params:xml:ns:xmpp-streams'/>",
        condition
    );

    if let Some(t) = text {
        error.push_str(&format!(
            "<text xmlns='urn:ietf:params:xml:ns:xmpp-streams' xml:lang='en'>{}</text>",
            t
        ));
    }

    error.push_str("</stream:error></stream:stream>");
    error
}

/// Common stream error conditions.
pub mod stream_errors {
    /// Stream error: bad format
    pub const BAD_FORMAT: &str = "bad-format";
    /// Stream error: bad namespace prefix
    pub const BAD_NAMESPACE_PREFIX: &str = "bad-namespace-prefix";
    /// Stream error: conflict (resource already connected)
    pub const CONFLICT: &str = "conflict";
    /// Stream error: connection timeout
    pub const CONNECTION_TIMEOUT: &str = "connection-timeout";
    /// Stream error: host gone
    pub const HOST_GONE: &str = "host-gone";
    /// Stream error: host unknown
    pub const HOST_UNKNOWN: &str = "host-unknown";
    /// Stream error: improper addressing
    pub const IMPROPER_ADDRESSING: &str = "improper-addressing";
    /// Stream error: internal server error
    pub const INTERNAL_SERVER_ERROR: &str = "internal-server-error";
    /// Stream error: invalid from
    pub const INVALID_FROM: &str = "invalid-from";
    /// Stream error: invalid namespace
    pub const INVALID_NAMESPACE: &str = "invalid-namespace";
    /// Stream error: invalid XML
    pub const INVALID_XML: &str = "invalid-xml";
    /// Stream error: not authorized
    pub const NOT_AUTHORIZED: &str = "not-authorized";
    /// Stream error: not well-formed
    pub const NOT_WELL_FORMED: &str = "not-well-formed";
    /// Stream error: policy violation
    pub const POLICY_VIOLATION: &str = "policy-violation";
    /// Stream error: remote connection failed
    pub const REMOTE_CONNECTION_FAILED: &str = "remote-connection-failed";
    /// Stream error: reset
    pub const RESET: &str = "reset";
    /// Stream error: resource constraint
    pub const RESOURCE_CONSTRAINT: &str = "resource-constraint";
    /// Stream error: restricted XML
    pub const RESTRICTED_XML: &str = "restricted-xml";
    /// Stream error: see other host
    pub const SEE_OTHER_HOST: &str = "see-other-host";
    /// Stream error: system shutdown
    pub const SYSTEM_SHUTDOWN: &str = "system-shutdown";
    /// Stream error: undefined condition
    pub const UNDEFINED_CONDITION: &str = "undefined-condition";
    /// Stream error: unsupported encoding
    pub const UNSUPPORTED_ENCODING: &str = "unsupported-encoding";
    /// Stream error: unsupported feature
    pub const UNSUPPORTED_FEATURE: &str = "unsupported-feature";
    /// Stream error: unsupported stanza type
    pub const UNSUPPORTED_STANZA_TYPE: &str = "unsupported-stanza-type";
    /// Stream error: unsupported version
    pub const UNSUPPORTED_VERSION: &str = "unsupported-version";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iq_error_generation() {
        let error = generate_iq_error(
            "test-id",
            Some("user@example.com"),
            Some("server.example.com"),
            StanzaErrorCondition::NotAuthorized,
            StanzaErrorType::Auth,
            Some("You must authenticate first"),
        );

        assert!(error.contains("type='error'"));
        assert!(error.contains("id='test-id'"));
        assert!(error.contains("to='user@example.com'"));
        assert!(error.contains("from='server.example.com'"));
        assert!(error.contains("<not-authorized"));
        assert!(error.contains("You must authenticate first"));
    }

    #[test]
    fn test_stream_error_generation() {
        let error = generate_stream_error(
            stream_errors::NOT_AUTHORIZED,
            Some("Invalid credentials"),
        );

        assert!(error.contains("<stream:error>"));
        assert!(error.contains("<not-authorized"));
        assert!(error.contains("Invalid credentials"));
        assert!(error.contains("</stream:stream>"));
    }

    #[test]
    fn test_stanza_error_conditions() {
        assert_eq!(StanzaErrorCondition::BadRequest.as_str(), "bad-request");
        assert_eq!(StanzaErrorCondition::NotAuthorized.as_str(), "not-authorized");
        assert_eq!(StanzaErrorCondition::ItemNotFound.as_str(), "item-not-found");
    }
}
