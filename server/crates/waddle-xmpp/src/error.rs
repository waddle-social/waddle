//! Error types for the XMPP server.

use crate::xep::xep0050::AdHocCommandCondition;
use thiserror::Error;
use waddle_xmpp_core::CoreError;

use crate::ownership::Entity;

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
    fn test_stream_error_generation() {
        let error =
            generate_stream_error(stream_errors::NOT_AUTHORIZED, Some("Invalid credentials"));

        assert!(error.contains("<stream:error>"));
        assert!(error.contains("<not-authorized"));
        assert!(error.contains("Invalid credentials"));
        assert!(error.contains("</stream:stream>"));
    }

    #[test]
    fn test_stanza_error_conditions() {
        assert_eq!(StanzaErrorCondition::BadRequest.as_str(), "bad-request");
        assert_eq!(
            StanzaErrorCondition::NotAuthorized.as_str(),
            "not-authorized"
        );
        assert_eq!(
            StanzaErrorCondition::ItemNotFound.as_str(),
            "item-not-found"
        );
    }
}

mod implementation;
pub use implementation::generate_stream_error;
pub use implementation::StanzaErrorCondition;
pub use implementation::StanzaErrorType;
pub use implementation::XmppError;
