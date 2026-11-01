//! Typed wire values for SASL2 as defined by XEP-0388.
//!
//! This module provides protocol parsing and construction only. It does not
//! advertise SASL2 or dispatch authentication in the server runtime.

use std::fmt;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use jid::Jid;
use minidom::Element;
use thiserror::Error;

/// SASL2 namespace from XEP-0388.
pub const NS_SASL2: &str = "urn:xmpp:sasl:2";
/// RFC 6120 SASL namespace used for SASL failure conditions.
pub const NS_SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";

/// Valid unmodeled SASL mechanism name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherSasl2Mechanism(String);

impl OtherSasl2Mechanism {
    /// Construct an extension mechanism after validating its RFC 4422 token.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2Error::InvalidMechanism`] for invalid or built-in names.
    pub fn try_new(value: impl Into<String>) -> Result<Self, Sasl2Error> {
        let value = value.into();
        match Sasl2Mechanism::parse(&value) {
            Some(Sasl2Mechanism::Other(_)) => Ok(Self(value)),
            _ => Err(Sasl2Error::InvalidMechanism),
        }
    }

    /// Borrow the validated extension mechanism token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// SASL mechanism name carried by a SASL2 exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sasl2Mechanism {
    /// PLAIN mechanism (RFC 4616).
    Plain,
    /// OAUTHBEARER mechanism (RFC 7628).
    OAuthBearer,
    /// SCRAM-SHA-256 mechanism (RFC 7677).
    ScramSha256,
    /// A registered or extension mechanism not otherwise modeled here.
    Other(OtherSasl2Mechanism),
}

impl Sasl2Mechanism {
    /// Parse a SASL mechanism name using the RFC 4422 token alphabet.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if value.is_empty()
            || value.len() > 20
            || !value.bytes().all(|byte| {
                byte.is_ascii_uppercase() || byte.is_ascii_digit() || b"-_".contains(&byte)
            })
        {
            return None;
        }
        Some(match value {
            "PLAIN" => Self::Plain,
            "OAUTHBEARER" => Self::OAuthBearer,
            "SCRAM-SHA-256" => Self::ScramSha256,
            other => Self::Other(OtherSasl2Mechanism(other.to_owned())),
        })
    }

    /// Borrow the canonical SASL mechanism name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Plain => "PLAIN",
            Self::OAuthBearer => "OAUTHBEARER",
            Self::ScramSha256 => "SCRAM-SHA-256",
            Self::Other(value) => value.as_str(),
        }
    }
}

impl fmt::Display for Sasl2Mechanism {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors returned when parsing a SASL2 wire value.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum Sasl2Error {
    /// The element name or namespace does not match the expected SASL2 value.
    #[error("invalid SASL2 element")]
    InvalidElement,
    /// The feature did not offer any valid SASL mechanisms.
    #[error("SASL2 authentication feature has no mechanisms")]
    MissingMechanisms,
    /// A required mechanism attribute or supported mechanism was absent.
    #[error("missing or unsupported SASL2 mechanism")]
    InvalidMechanism,
    /// A failure enum cannot be serialized as a concrete condition.
    #[error("cannot serialize unknown SASL2 failure condition")]
    UnknownFailureCondition,
    /// More than one authorization identifier appeared in a success element.
    #[error("duplicate SASL2 authorization identifier")]
    DuplicateAuthorizationIdentifier,
    /// More than one additional-data element appeared in a success element.
    #[error("duplicate SASL2 additional-data element")]
    DuplicateAdditionalData,
    /// A Base64 encoded SASL payload was malformed.
    #[error("invalid Base64 SASL2 payload")]
    InvalidPayload,
    /// The success element omitted its authorization identifier.
    #[error("missing SASL2 authorization identifier")]
    MissingAuthorizationIdentifier,
    /// The authorization identifier was not a valid JID.
    #[error("invalid SASL2 authorization identifier")]
    InvalidAuthorizationIdentifier,
}

/// Base64-encoded SASL data represented as its decoded bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Sasl2Data(Vec<u8>);

impl Sasl2Data {
    /// Construct SASL data from its decoded bytes.
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Decode the element's Base64 text into SASL data.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2Error::InvalidPayload`] if the text is not valid Base64.
    pub fn from_element_text(text: &str) -> Result<Self, Sasl2Error> {
        BASE64_STANDARD
            .decode(text.trim())
            .map(Self)
            .map_err(|_| Sasl2Error::InvalidPayload)
    }

    /// Borrow the decoded SASL bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Encode the SASL bytes for use as element text.
    #[must_use]
    pub fn encoded(&self) -> String {
        BASE64_STANDARD.encode(&self.0)
    }
}

impl fmt::Debug for Sasl2Data {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Sasl2Data(REDACTED)")
    }
}

/// SASL2 feature advertised in stream features.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sasl2Feature {
    /// SASL mechanisms offered by the peer.
    pub mechanisms: Vec<Sasl2Mechanism>,
    /// Optional inline feature set offered by the peer.
    pub inline: Option<Sasl2InlineFeatures>,
}

impl Sasl2Feature {
    /// Parse an XEP-0388 `<authentication/>` stream feature.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2Error::InvalidElement`] for a different element or
    /// namespace. Unknown mechanisms and non-mechanism feature children are
    /// ignored.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2Error> {
        if element.name() != "authentication" || element.ns() != NS_SASL2 {
            return Err(Sasl2Error::InvalidElement);
        }

        let mut mechanisms = Vec::new();
        let mut inline = None;
        for child in element.children() {
            match (child.name(), child.ns().as_str()) {
                ("mechanism", NS_SASL2) => {
                    if let Some(mechanism) = Sasl2Mechanism::parse(child.text().trim()) {
                        if !mechanisms.contains(&mechanism) {
                            mechanisms.push(mechanism);
                        }
                    }
                }
                ("inline", NS_SASL2) => {
                    if inline.is_some() {
                        return Err(Sasl2Error::InvalidElement);
                    }
                    inline = Some(Sasl2InlineFeatures::from_element(child)?);
                }
                _ => {}
            }
        }

        if mechanisms.is_empty() {
            return Err(Sasl2Error::MissingMechanisms);
        }

        Ok(Self { mechanisms, inline })
    }

    /// Build an XEP-0388 `<authentication/>` stream feature.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2Error::MissingMechanisms`] if the feature has no mechanisms.
    pub fn to_element(&self) -> Result<Element, Sasl2Error> {
        if self.mechanisms.is_empty() {
            return Err(Sasl2Error::MissingMechanisms);
        }

        let mut builder = Element::builder("authentication", NS_SASL2);
        for mechanism in &self.mechanisms {
            builder = builder.append(
                Element::builder("mechanism", NS_SASL2)
                    .append(mechanism.as_str())
                    .build(),
            );
        }
        if let Some(inline) = &self.inline {
            builder = builder.append(inline.to_element());
        }
        Ok(builder.build())
    }
}

/// Extension feature elements offered inside SASL2 `<inline/>`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sasl2InlineFeatures {
    /// Direct child feature elements, preserved as typed XML values.
    pub features: Vec<Element>,
}

impl Sasl2InlineFeatures {
    /// Parse an XEP-0388 `<inline/>` feature container.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2Error::InvalidElement`] for a different element or
    /// namespace.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2Error> {
        if element.name() != "inline" || element.ns() != NS_SASL2 {
            return Err(Sasl2Error::InvalidElement);
        }
        Ok(Self {
            features: element.children().cloned().collect(),
        })
    }

    /// Build an XEP-0388 `<inline/>` feature container.
    #[must_use]
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder("inline", NS_SASL2);
        for feature in &self.features {
            builder = builder.append(feature.clone());
        }
        builder.build()
    }
}

/// SASL2 authentication request.
#[derive(Clone, PartialEq, Eq)]
pub struct Sasl2Authenticate {
    /// Mechanism selected for this exchange.
    pub mechanism: Sasl2Mechanism,
    /// Optional decoded initial response.
    pub initial_response: Option<Sasl2Data>,
    // These are direct children, not the <inline/> feature-advertisement
    // container. Keep even unrecognized children so the receiving server can
    // decide whether it supports each requested extension.
    additional_children: Vec<Element>,
}

impl fmt::Debug for Sasl2Authenticate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sasl2Authenticate")
            .field("mechanism", &self.mechanism)
            .field("has_initial_response", &self.initial_response.is_some())
            .field("additional_child_count", &self.additional_children.len())
            .finish()
    }
}

impl Sasl2Authenticate {
    /// Create a request without a user agent or inline feature requests.
    #[must_use]
    pub fn new(mechanism: Sasl2Mechanism, initial_response: Option<Sasl2Data>) -> Self {
        Self {
            mechanism,
            initial_response,
            additional_children: Vec::new(),
        }
    }

    /// Direct children other than the initial response, including the optional
    /// user agent and any requested inline extensions.
    #[must_use]
    pub fn additional_children(&self) -> &[Element] {
        &self.additional_children
    }

    /// Add a direct request child without silently replacing protocol fields.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2Error::InvalidElement`] for a second initial response, an
    /// advertisement-only `<inline/>` container, or a duplicate user agent.
    pub fn push_additional_child(&mut self, child: Element) -> Result<(), Sasl2Error> {
        if child.ns() == NS_SASL2 {
            match child.name() {
                "initial-response" | "inline" => return Err(Sasl2Error::InvalidElement),
                "user-agent"
                    if self
                        .additional_children
                        .iter()
                        .any(|existing| existing.is("user-agent", NS_SASL2)) =>
                {
                    return Err(Sasl2Error::InvalidElement);
                }
                _ => {}
            }
        }
        self.additional_children.push(child);
        Ok(())
    }

    /// Parse an XEP-0388 `<authenticate/>` request.
    ///
    /// # Errors
    ///
    /// Returns a typed SASL2 error for an unexpected element, missing or
    /// unsupported mechanism, or malformed Base64 initial response.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2Error> {
        if element.name() != "authenticate" || element.ns() != NS_SASL2 {
            return Err(Sasl2Error::InvalidElement);
        }
        let mechanism = element
            .attr("mechanism")
            .and_then(Sasl2Mechanism::parse)
            .ok_or(Sasl2Error::InvalidMechanism)?;
        if !element.text().trim().is_empty() {
            return Err(Sasl2Error::InvalidElement);
        }
        let mut request = Self::new(mechanism, None);
        for child in element.children() {
            if child.is("initial-response", NS_SASL2) {
                if request.initial_response.is_some() || child.children().next().is_some() {
                    return Err(Sasl2Error::InvalidElement);
                }
                request.initial_response = Some(Sasl2Data::from_element_text(&child.text())?);
            } else {
                request.push_additional_child(child.clone())?;
            }
        }
        Ok(request)
    }

    /// Build an XEP-0388 `<authenticate/>` request.
    #[must_use]
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder("authenticate", NS_SASL2).attr(
            minidom::rxml::xml_ncname!("mechanism").to_owned(),
            self.mechanism.as_str(),
        );
        if let Some(response) = &self.initial_response {
            builder = builder.append(
                Element::builder("initial-response", NS_SASL2)
                    .append(response.encoded())
                    .build(),
            );
        }
        for child in &self.additional_children {
            builder = builder.append(child.clone());
        }
        builder.build()
    }
}

/// SASL2 challenge sent by the peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sasl2Challenge {
    /// Decoded challenge payload.
    pub data: Sasl2Data,
}

impl Sasl2Challenge {
    /// Parse an XEP-0388 `<challenge/>` element.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unexpected element or malformed Base64.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2Error> {
        if element.name() != "challenge" || element.ns() != NS_SASL2 {
            return Err(Sasl2Error::InvalidElement);
        }
        Ok(Self {
            data: Sasl2Data::from_element_text(&element.text())?,
        })
    }

    /// Build an XEP-0388 `<challenge/>` element.
    #[must_use]
    pub fn to_element(&self) -> Element {
        Element::builder("challenge", NS_SASL2)
            .append(self.data.encoded())
            .build()
    }
}

/// SASL2 response sent by the client to a challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sasl2Response {
    /// Decoded response payload.
    pub data: Sasl2Data,
}

impl Sasl2Response {
    /// Parse an XEP-0388 `<response/>` element.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unexpected element, nested child, or
    /// malformed Base64 payload.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2Error> {
        if !element.is("response", NS_SASL2) || element.children().next().is_some() {
            return Err(Sasl2Error::InvalidElement);
        }
        Ok(Self {
            data: Sasl2Data::from_element_text(&element.text())?,
        })
    }

    /// Build an XEP-0388 `<response/>` element.
    #[must_use]
    pub fn to_element(&self) -> Element {
        Element::builder("response", NS_SASL2)
            .append(self.data.encoded())
            .build()
    }
}

/// SASL2 success response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sasl2Success {
    /// Optional mechanism-specific success data.
    pub additional_data: Option<Sasl2Data>,
    /// Identity authorized by the peer for the authenticated session.
    pub authorization_identifier: Jid,
    /// Direct child results returned for SASL2 inline requests.
    pub inline_results: Vec<Element>,
}

impl Sasl2Success {
    /// Parse an XEP-0388 `<success/>` response.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unexpected element, duplicate or malformed
    /// additional data, a missing identity, or an invalid JID.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2Error> {
        if element.name() != "success" || element.ns() != NS_SASL2 {
            return Err(Sasl2Error::InvalidElement);
        }
        let mut identity = None;
        let mut additional_data = None;
        let mut inline_results = Vec::new();
        for child in element.children() {
            match (child.name(), child.ns().as_str()) {
                ("authorization-identifier", NS_SASL2) => {
                    if identity.is_some() {
                        return Err(Sasl2Error::DuplicateAuthorizationIdentifier);
                    }
                    let parsed = child
                        .text()
                        .trim()
                        .parse()
                        .map_err(|_| Sasl2Error::InvalidAuthorizationIdentifier)?;
                    identity = Some(parsed);
                }
                ("additional-data", NS_SASL2) => {
                    if additional_data.is_some() {
                        return Err(Sasl2Error::DuplicateAdditionalData);
                    }
                    additional_data = Some(Sasl2Data::from_element_text(&child.text())?);
                }
                _ => inline_results.push(child.clone()),
            }
        }
        let authorization_identifier =
            identity.ok_or(Sasl2Error::MissingAuthorizationIdentifier)?;
        Ok(Self {
            additional_data,
            authorization_identifier,
            inline_results,
        })
    }

    /// Build an XEP-0388 `<success/>` response.
    #[must_use]
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder("success", NS_SASL2);
        if let Some(additional_data) = &self.additional_data {
            builder = builder.append(
                Element::builder("additional-data", NS_SASL2)
                    .append(additional_data.encoded())
                    .build(),
            );
        }
        builder = builder.append(
            Element::builder("authorization-identifier", NS_SASL2)
                .append(self.authorization_identifier.to_string())
                .build(),
        );
        for result in &self.inline_results {
            builder = builder.append(result.clone());
        }
        builder.build()
    }
}

/// Recognized SASL failure condition in SASL1 or SASL2 namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Sasl2FailureCondition {
    /// Authentication exchange was aborted.
    Aborted,
    /// Account is disabled.
    AccountDisabled,
    /// Credentials expired.
    CredentialsExpired,
    /// Encryption is required.
    EncryptionRequired,
    /// Character encoding is incorrect.
    IncorrectEncoding,
    /// Authorization identity is invalid.
    InvalidAuthzid,
    /// Mechanism is invalid or unsupported.
    InvalidMechanism,
    /// Request is malformed.
    MalformedRequest,
    /// Mechanism is too weak.
    MechanismTooWeak,
    /// Authorization was denied.
    NotAuthorized,
    /// Authentication temporarily failed.
    TemporaryAuthFailure,
    /// Failure condition is unknown to this implementation.
    Unknown,
}

impl Sasl2FailureCondition {
    fn parse(value: &str) -> Self {
        match value {
            "aborted" => Self::Aborted,
            "account-disabled" => Self::AccountDisabled,
            "credentials-expired" => Self::CredentialsExpired,
            "encryption-required" => Self::EncryptionRequired,
            "incorrect-encoding" => Self::IncorrectEncoding,
            "invalid-authzid" => Self::InvalidAuthzid,
            "invalid-mechanism" => Self::InvalidMechanism,
            "malformed-request" => Self::MalformedRequest,
            "mechanism-too-weak" => Self::MechanismTooWeak,
            "not-authorized" => Self::NotAuthorized,
            "temporary-auth-failure" => Self::TemporaryAuthFailure,
            _ => Self::Unknown,
        }
    }

    fn as_str(self) -> Option<&'static str> {
        match self {
            Self::Aborted => Some("aborted"),
            Self::AccountDisabled => Some("account-disabled"),
            Self::CredentialsExpired => Some("credentials-expired"),
            Self::EncryptionRequired => Some("encryption-required"),
            Self::IncorrectEncoding => Some("incorrect-encoding"),
            Self::InvalidAuthzid => Some("invalid-authzid"),
            Self::InvalidMechanism => Some("invalid-mechanism"),
            Self::MalformedRequest => Some("malformed-request"),
            Self::MechanismTooWeak => Some("mechanism-too-weak"),
            Self::NotAuthorized => Some("not-authorized"),
            Self::TemporaryAuthFailure => Some("temporary-auth-failure"),
            Self::Unknown => None,
        }
    }
}

/// SASL2 failure response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sasl2Failure {
    /// Failure condition reported by the peer.
    pub condition: Sasl2FailureCondition,
}

impl Sasl2Failure {
    /// Parse an XEP-0388 `<failure/>` response.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2Error::InvalidElement`] for a different element or
    /// namespace. An omitted or unrecognized condition is `Unknown`.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2Error> {
        if element.name() != "failure" || element.ns() != NS_SASL2 {
            return Err(Sasl2Error::InvalidElement);
        }
        let condition = element
            .children()
            .find(|child| child.name() != "text" && child.ns() == NS_SASL)
            .map(|child| Sasl2FailureCondition::parse(child.name()))
            .unwrap_or(Sasl2FailureCondition::Unknown);
        Ok(Self { condition })
    }

    /// Build an XEP-0388 `<failure/>` response.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2Error::UnknownFailureCondition`] if this value was
    /// parsed with an unrecognized condition.
    pub fn to_element(&self) -> Result<Element, Sasl2Error> {
        let condition = self
            .condition
            .as_str()
            .ok_or(Sasl2Error::UnknownFailureCondition)?;
        Ok(Element::builder("failure", NS_SASL2)
            .append(Element::builder(condition, NS_SASL).build())
            .build())
    }
}
