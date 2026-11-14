//! Typed stream-level and SASL failure conditions (RFC 6120 §4.9.3, §6.5).

use std::collections::BTreeMap;
use std::fmt;

use minidom::Element;
use xmpp_parsers::sasl::{DefinedCondition as SaslDefinedCondition, Failure as SaslFailure};
use xmpp_parsers::stream_error::{DefinedCondition, StreamError};

/// A stream error condition this codec sends or recognises.
///
/// Outbound stream errors carry only the defined condition: no descriptive
/// text, no application-specific element and nothing reflected from peer
/// input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamErrorCondition {
    BadFormat,
    BadNamespacePrefix,
    Conflict,
    ConnectionTimeout,
    HostGone,
    HostUnknown,
    ImproperAddressing,
    InternalServerError,
    InvalidFrom,
    InvalidNamespace,
    InvalidXml,
    NotAuthorized,
    NotWellFormed,
    PolicyViolation,
    RemoteConnectionFailed,
    Reset,
    ResourceConstraint,
    RestrictedXml,
    SeeOtherHost,
    SystemShutdown,
    UndefinedCondition,
    UnsupportedEncoding,
    UnsupportedFeature,
    UnsupportedStanzaType,
    UnsupportedVersion,
}

impl StreamErrorCondition {
    /// The RFC 6120 element name of this condition.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadFormat => "bad-format",
            Self::BadNamespacePrefix => "bad-namespace-prefix",
            Self::Conflict => "conflict",
            Self::ConnectionTimeout => "connection-timeout",
            Self::HostGone => "host-gone",
            Self::HostUnknown => "host-unknown",
            Self::ImproperAddressing => "improper-addressing",
            Self::InternalServerError => "internal-server-error",
            Self::InvalidFrom => "invalid-from",
            Self::InvalidNamespace => "invalid-namespace",
            Self::InvalidXml => "invalid-xml",
            Self::NotAuthorized => "not-authorized",
            Self::NotWellFormed => "not-well-formed",
            Self::PolicyViolation => "policy-violation",
            Self::RemoteConnectionFailed => "remote-connection-failed",
            Self::Reset => "reset",
            Self::ResourceConstraint => "resource-constraint",
            Self::RestrictedXml => "restricted-xml",
            Self::SeeOtherHost => "see-other-host",
            Self::SystemShutdown => "system-shutdown",
            Self::UndefinedCondition => "undefined-condition",
            Self::UnsupportedEncoding => "unsupported-encoding",
            Self::UnsupportedFeature => "unsupported-feature",
            Self::UnsupportedStanzaType => "unsupported-stanza-type",
            Self::UnsupportedVersion => "unsupported-version",
        }
    }

    /// Build the `<stream:error/>` element for this condition.
    ///
    /// `see-other-host` is never sent by this codec: redirects are not part
    /// of the federation profile, so it is encoded as `undefined-condition`.
    pub fn to_element(self) -> Element {
        let condition = match self {
            Self::BadFormat => DefinedCondition::BadFormat,
            Self::BadNamespacePrefix => DefinedCondition::BadNamespacePrefix,
            Self::Conflict => DefinedCondition::Conflict,
            Self::ConnectionTimeout => DefinedCondition::ConnectionTimeout,
            Self::HostGone => DefinedCondition::HostGone,
            Self::HostUnknown => DefinedCondition::HostUnknown,
            Self::ImproperAddressing => DefinedCondition::ImproperAddressing,
            Self::InternalServerError => DefinedCondition::InternalServerError,
            Self::InvalidFrom => DefinedCondition::InvalidFrom,
            Self::InvalidNamespace => DefinedCondition::InvalidNamespace,
            Self::InvalidXml => DefinedCondition::InvalidXml,
            Self::NotAuthorized => DefinedCondition::NotAuthorized,
            Self::NotWellFormed => DefinedCondition::NotWellFormed,
            Self::PolicyViolation => DefinedCondition::PolicyViolation,
            Self::RemoteConnectionFailed => DefinedCondition::RemoteConnectionFailed,
            Self::Reset => DefinedCondition::Reset,
            Self::ResourceConstraint => DefinedCondition::ResourceConstraint,
            Self::RestrictedXml => DefinedCondition::RestrictedXml,
            Self::SeeOtherHost | Self::UndefinedCondition => DefinedCondition::UndefinedCondition,
            Self::SystemShutdown => DefinedCondition::SystemShutdown,
            Self::UnsupportedEncoding => DefinedCondition::UnsupportedEncoding,
            Self::UnsupportedFeature => DefinedCondition::UnsupportedFeature,
            Self::UnsupportedStanzaType => DefinedCondition::UnsupportedStanzaType,
            Self::UnsupportedVersion => DefinedCondition::UnsupportedVersion,
        };
        Element::from(StreamError {
            condition,
            texts: BTreeMap::new(),
            application_specific: Vec::new(),
        })
    }

    /// Classify a received `<stream:error/>` element.
    ///
    /// RFC 6120 §4.9.2 requires an unknown or missing condition to be
    /// treated as `undefined-condition`.
    pub fn from_peer_element(element: Element) -> Self {
        match StreamError::try_from(element) {
            Ok(error) => Self::from_defined(&error.condition),
            Err(_) => Self::UndefinedCondition,
        }
    }

    fn from_defined(condition: &DefinedCondition) -> Self {
        match condition {
            DefinedCondition::BadFormat => Self::BadFormat,
            DefinedCondition::BadNamespacePrefix => Self::BadNamespacePrefix,
            DefinedCondition::Conflict => Self::Conflict,
            DefinedCondition::ConnectionTimeout => Self::ConnectionTimeout,
            DefinedCondition::HostGone => Self::HostGone,
            DefinedCondition::HostUnknown => Self::HostUnknown,
            DefinedCondition::ImproperAddressing => Self::ImproperAddressing,
            DefinedCondition::InternalServerError => Self::InternalServerError,
            DefinedCondition::InvalidFrom => Self::InvalidFrom,
            DefinedCondition::InvalidNamespace => Self::InvalidNamespace,
            DefinedCondition::InvalidXml => Self::InvalidXml,
            DefinedCondition::NotAuthorized => Self::NotAuthorized,
            DefinedCondition::NotWellFormed => Self::NotWellFormed,
            DefinedCondition::PolicyViolation => Self::PolicyViolation,
            DefinedCondition::RemoteConnectionFailed => Self::RemoteConnectionFailed,
            DefinedCondition::Reset => Self::Reset,
            DefinedCondition::ResourceConstraint => Self::ResourceConstraint,
            DefinedCondition::RestrictedXml => Self::RestrictedXml,
            DefinedCondition::SeeOtherHost(_) => Self::SeeOtherHost,
            DefinedCondition::SystemShutdown => Self::SystemShutdown,
            DefinedCondition::UndefinedCondition => Self::UndefinedCondition,
            DefinedCondition::UnsupportedEncoding => Self::UnsupportedEncoding,
            DefinedCondition::UnsupportedFeature => Self::UnsupportedFeature,
            DefinedCondition::UnsupportedStanzaType => Self::UnsupportedStanzaType,
            DefinedCondition::UnsupportedVersion => Self::UnsupportedVersion,
        }
    }
}

impl fmt::Display for StreamErrorCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A SASL failure condition this codec sends or recognises (RFC 6120 §6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SaslFailureCondition {
    Aborted,
    AccountDisabled,
    CredentialsExpired,
    EncryptionRequired,
    IncorrectEncoding,
    InvalidAuthzid,
    InvalidMechanism,
    MalformedRequest,
    MechanismTooWeak,
    NotAuthorized,
    TemporaryAuthFailure,
}

impl SaslFailureCondition {
    /// The RFC 6120 element name of this condition.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Aborted => "aborted",
            Self::AccountDisabled => "account-disabled",
            Self::CredentialsExpired => "credentials-expired",
            Self::EncryptionRequired => "encryption-required",
            Self::IncorrectEncoding => "incorrect-encoding",
            Self::InvalidAuthzid => "invalid-authzid",
            Self::InvalidMechanism => "invalid-mechanism",
            Self::MalformedRequest => "malformed-request",
            Self::MechanismTooWeak => "mechanism-too-weak",
            Self::NotAuthorized => "not-authorized",
            Self::TemporaryAuthFailure => "temporary-auth-failure",
        }
    }

    /// Build the SASL `<failure/>` element for this condition, without text.
    pub fn to_element(self) -> Element {
        let defined_condition = match self {
            Self::Aborted => SaslDefinedCondition::Aborted,
            Self::AccountDisabled => SaslDefinedCondition::AccountDisabled,
            Self::CredentialsExpired => SaslDefinedCondition::CredentialsExpired,
            Self::EncryptionRequired => SaslDefinedCondition::EncryptionRequired,
            Self::IncorrectEncoding => SaslDefinedCondition::IncorrectEncoding,
            Self::InvalidAuthzid => SaslDefinedCondition::InvalidAuthzid,
            Self::InvalidMechanism => SaslDefinedCondition::InvalidMechanism,
            Self::MalformedRequest => SaslDefinedCondition::MalformedRequest,
            Self::MechanismTooWeak => SaslDefinedCondition::MechanismTooWeak,
            Self::NotAuthorized => SaslDefinedCondition::NotAuthorized,
            Self::TemporaryAuthFailure => SaslDefinedCondition::TemporaryAuthFailure,
        };
        Element::from(SaslFailure {
            defined_condition,
            texts: BTreeMap::new(),
        })
    }

    /// Classify a received SASL `<failure/>` element.
    ///
    /// RFC 6120 §6.5 requires an unknown condition to be treated as
    /// `not-authorized`.
    pub fn from_peer_element(element: Element) -> Self {
        let Ok(failure) = SaslFailure::try_from(element) else {
            return Self::NotAuthorized;
        };
        match failure.defined_condition {
            SaslDefinedCondition::Aborted => Self::Aborted,
            SaslDefinedCondition::AccountDisabled => Self::AccountDisabled,
            SaslDefinedCondition::CredentialsExpired => Self::CredentialsExpired,
            SaslDefinedCondition::EncryptionRequired => Self::EncryptionRequired,
            SaslDefinedCondition::IncorrectEncoding => Self::IncorrectEncoding,
            SaslDefinedCondition::InvalidAuthzid => Self::InvalidAuthzid,
            SaslDefinedCondition::InvalidMechanism => Self::InvalidMechanism,
            SaslDefinedCondition::MalformedRequest => Self::MalformedRequest,
            SaslDefinedCondition::MechanismTooWeak => Self::MechanismTooWeak,
            SaslDefinedCondition::NotAuthorized => Self::NotAuthorized,
            SaslDefinedCondition::TemporaryAuthFailure => Self::TemporaryAuthFailure,
        }
    }
}

impl fmt::Display for SaslFailureCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
