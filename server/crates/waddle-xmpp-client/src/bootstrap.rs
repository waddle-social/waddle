//! XMPP stream-feature, SASL, and resource-binding parsing and construction.

use std::fmt;
use std::str::FromStr;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use jid::{BareJid, FullJid};
use minidom::Element;

use crate::config::{AccessToken, ClientResource, OAuthBearerConfig};
use crate::error::{ClientError, ClientResult};
use crate::request::StanzaId;
use crate::state::{SessionBinding, StreamId};
use crate::transport::TransportMessage;

/// Protocol token `urn:ietf:params:xml:ns:xmpp-bind`.
pub const NS_BIND: &str = "urn:ietf:params:xml:ns:xmpp-bind";
/// Protocol token `jabber:client`.
pub const NS_CLIENT: &str = "jabber:client";
/// Protocol token `urn:ietf:params:xml:ns:xmpp-sasl`.
pub const NS_SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";
/// Protocol token `urn:ietf:params:xml:ns:xmpp-session`.
pub const NS_SESSION: &str = "urn:ietf:params:xml:ns:xmpp-session";
/// Protocol token `urn:xmpp:sm:3`.
pub const NS_SM: &str = "urn:xmpp:sm:3";
/// Protocol token `http://etherx.jabber.org/streams`.
pub const NS_STREAMS: &str = "http://etherx.jabber.org/streams";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// SASL authentication mechanism supported by the client.
#[non_exhaustive]
pub enum AuthMechanism {
    /// Authenticate using the SASL OAUTHBEARER mechanism.
    OAuthBearer,
}

impl AuthMechanism {
    /// Borrow the stored text or canonical protocol token without allocating.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OAuthBearer => "OAUTHBEARER",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "OAUTHBEARER" => Some(Self::OAuthBearer),
            _ => None,
        }
    }
}

impl fmt::Display for AuthMechanism {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Bootstrap feature required before advancing the session.
#[non_exhaustive]
pub enum RequiredStreamFeature {
    /// The peer must advertise the selected SASL mechanism.
    Authentication(AuthMechanism),
    /// The server supports binding an account resource.
    ResourceBinding,
}

impl fmt::Display for RequiredStreamFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authentication(mechanism) => {
                write!(f, "authentication mechanism `{mechanism}`")
            }
            Self::ResourceBinding => f.write_str("resource binding"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// RFC 6120 SASL failure condition returned by the authentication peer.
#[non_exhaustive]
pub enum SaslFailureCondition {
    /// The peer aborted authentication before it completed.
    Aborted,
    /// The account is disabled and cannot authenticate.
    AccountDisabled,
    /// The supplied authentication credentials have expired.
    CredentialsExpired,
    /// The peer requires transport encryption before continuing.
    EncryptionRequired,
    /// The peer used an invalid character encoding.
    IncorrectEncoding,
    /// The requested authorization identity is invalid.
    InvalidAuthzid,
    /// The selected SASL mechanism is not supported.
    InvalidMechanism,
    /// The SASL request does not conform to the mechanism's syntax.
    MalformedRequest,
    /// The SASL mechanism does not meet the peer's security requirements.
    MechanismTooWeak,
    /// The peer rejected the authorization attempt.
    NotAuthorized,
    /// Authentication failed for a transient reason and may succeed later.
    TemporaryAuthFailure,
    /// The received token is not recognized by this client.
    Unknown,
}

impl SaslFailureCondition {
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
}

impl fmt::Display for SaslFailureCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Aborted => f.write_str("aborted"),
            Self::AccountDisabled => f.write_str("account-disabled"),
            Self::CredentialsExpired => f.write_str("credentials-expired"),
            Self::EncryptionRequired => f.write_str("encryption-required"),
            Self::IncorrectEncoding => f.write_str("incorrect-encoding"),
            Self::InvalidAuthzid => f.write_str("invalid-authzid"),
            Self::InvalidMechanism => f.write_str("invalid-mechanism"),
            Self::MalformedRequest => f.write_str("malformed-request"),
            Self::MechanismTooWeak => f.write_str("mechanism-too-weak"),
            Self::NotAuthorized => f.write_str("not-authorized"),
            Self::TemporaryAuthFailure => f.write_str("temporary-auth-failure"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Recognized SASL, resource-binding, session, and stream-management features advertised by the peer.
pub struct StreamFeatures {
    /// SASL mechanisms advertised by the server.
    pub mechanisms: Vec<AuthMechanism>,
    /// Whether resource binding was advertised by the server.
    pub bind: bool,
    /// Whether the server advertises legacy session establishment.
    pub legacy_session: bool,
    /// Whether the server advertised XEP-0198 support.
    pub stream_management: bool,
    /// Whether the session may be resumed after transport loss.
    pub resumable: bool,
}

impl StreamFeatures {
    /// Parse the supported protocol fields from an XML element.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::InvalidStreamFeatures` unless the root is a features element in the stream namespace. Unrecognized advertised mechanisms and features are ignored.
    pub fn from_element(element: &Element) -> ClientResult<Self> {
        if element.name() != "features" || !is_streams_ns(&element.ns()) {
            return Err(ClientError::InvalidStreamFeatures);
        }

        let mut mechanisms = Vec::new();
        let mut bind = false;
        let mut legacy_session = false;
        let mut stream_management = false;
        let mut resumable = false;

        for child in element.children() {
            let ns = child.ns();
            match child.name() {
                "mechanisms" if ns == NS_SASL => {
                    for mechanism in child.children() {
                        if mechanism.name() != "mechanism" || mechanism.ns() != NS_SASL {
                            continue;
                        }

                        let text = mechanism.text();
                        if let Some(parsed) = AuthMechanism::parse(text.trim()) {
                            if !mechanisms.contains(&parsed) {
                                mechanisms.push(parsed);
                            }
                        }
                    }
                }
                "bind" if ns == NS_BIND => bind = true,
                "session" if ns == NS_SESSION => legacy_session = true,
                "sm" if ns == NS_SM => {
                    stream_management = true;
                    resumable = matches!(child.attr("resume"), Some("true") | Some("1"));
                }
                _ => {}
            }
        }

        Ok(Self {
            mechanisms,
            bind,
            legacy_session,
            stream_management,
            resumable,
        })
    }

    /// Whether the peer advertised the requested authentication mechanism.
    #[must_use]
    pub fn supports(&self, mechanism: AuthMechanism) -> bool {
        self.mechanisms.contains(&mechanism)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Typed authentication exchange to be written to the stream.
#[non_exhaustive]
pub enum AuthenticationRequest {
    /// A SASL OAUTHBEARER initial response ready for transmission.
    OAuthBearer(OAuthBearerRequest),
}

impl AuthenticationRequest {
    /// Construct this bootstrap value from the validated client configuration.
    #[must_use]
    pub fn from_config(config: &OAuthBearerConfig) -> Self {
        Self::OAuthBearer(OAuthBearerRequest {
            account: config.account.clone(),
            access_token: config.access_token.clone(),
        })
    }

    /// Serialize this value as a typed XML element.
    #[must_use]
    pub fn to_element(&self) -> Element {
        match self {
            Self::OAuthBearer(request) => request.to_element(),
        }
    }

    /// Wrap this bootstrap XML in a transport message for transmission.
    #[must_use]
    pub fn to_transport_message(&self) -> TransportMessage {
        TransportMessage::Element(self.to_element())
    }
}

#[derive(Clone, PartialEq, Eq)]
/// SASL OAUTHBEARER initial response assembled from the account and bearer credential.
pub struct OAuthBearerRequest {
    /// Bare JID of the account being authenticated.
    pub account: BareJid,
    /// Bearer credential used during OAUTHBEARER authentication.
    pub access_token: AccessToken,
}

impl OAuthBearerRequest {
    /// Serialize this value as a typed XML element.
    #[must_use]
    pub fn to_element(&self) -> Element {
        Element::builder("auth", NS_SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                AuthMechanism::OAuthBearer.as_str(),
            )
            .append(self.encoded_initial_response())
            .build()
    }

    fn encoded_initial_response(&self) -> String {
        let mut payload = format!("n,a={},", self.account).into_bytes();
        payload.push(0x01);
        payload.extend_from_slice(b"auth=Bearer ");
        payload.extend_from_slice(self.access_token.as_str().as_bytes());
        payload.push(0x01);
        payload.push(0x01);
        BASE64_STANDARD.encode(payload)
    }
}

impl fmt::Debug for OAuthBearerRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthBearerRequest")
            .field("account", &self.account)
            .field("access_token", &self.access_token)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// SASL failure condition parsed from the peer's authentication response.
pub struct SaslFailure {
    /// XMPP error condition identifying the protocol failure.
    pub condition: SaslFailureCondition,
}

impl SaslFailure {
    /// Parse the supported protocol fields from an XML element.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::InvalidSaslFailure` unless the root is a failure element in the SASL namespace. A missing or unrecognized condition becomes `SaslFailureCondition::Unknown`.
    pub fn from_element(element: &Element) -> ClientResult<Self> {
        if element.name() != "failure" || element.ns() != NS_SASL {
            return Err(ClientError::InvalidSaslFailure);
        }

        let condition = element
            .children()
            .find(|child| child.name() != "text")
            .map(|child| SaslFailureCondition::parse(child.name()))
            .unwrap_or(SaslFailureCondition::Unknown);

        Ok(Self { condition })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// IQ correlation identifier and requested resource for XMPP resource binding.
pub struct ResourceBindingRequest {
    /// Identifier used for stanza correlation.
    pub stanza_id: StanzaId,
    /// Requested XMPP resource name for account binding.
    pub resource: ClientResource,
}

impl ResourceBindingRequest {
    /// Create a bind IQ request with the supplied stanza identifier and resource.
    #[must_use]
    pub fn new(stanza_id: StanzaId, resource: ClientResource) -> Self {
        Self {
            stanza_id,
            resource,
        }
    }

    /// Serialize this value as a typed XML element.
    #[must_use]
    pub fn to_element(&self) -> Element {
        Element::builder("iq", NS_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                self.stanza_id.as_str(),
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .append(
                Element::builder("bind", NS_BIND)
                    .append(
                        Element::builder("resource", NS_BIND)
                            .append(self.resource.as_str())
                            .build(),
                    )
                    .build(),
            )
            .build()
    }

    /// Wrap this bootstrap XML in a transport message for transmission.
    #[must_use]
    pub fn to_transport_message(&self) -> TransportMessage {
        TransportMessage::Element(self.to_element())
    }
}

/// Whether the bound session may be resumed with XEP-0198.
///
/// Intentionally exhaustive: the two variants replace a bare resumption boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamResumption {
    /// The peer may resume this stream.
    Resumable,
    /// The stream cannot be resumed.
    NotResumable,
}

impl StreamResumption {
    /// Whether this session may be resumed.
    #[must_use]
    pub const fn is_resumable(self) -> bool {
        matches!(self, Self::Resumable)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Full account JID returned by successful resource binding.
pub struct ResourceBindingResult {
    /// Identifier used for stanza correlation.
    pub stanza_id: StanzaId,
    /// XMPP address identifying the referenced entity.
    pub jid: FullJid,
}

impl ResourceBindingResult {
    /// Parse the supported protocol fields from an XML element.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::InvalidBindResponse` for a non-result client IQ, missing bind/JID elements, or an invalid full JID. Missing IQ identifiers are invalid bind responses; blank identifiers return `EmptyStanzaId`.
    pub fn from_element(element: &Element) -> ClientResult<Self> {
        if element.name() != "iq" || !is_client_stanza_ns(&element.ns()) {
            return Err(ClientError::InvalidBindResponse);
        }

        if element.attr("type") != Some("result") {
            return Err(ClientError::InvalidBindResponse);
        }

        let stanza_id = element
            .attr("id")
            .ok_or(ClientError::InvalidBindResponse)
            .and_then(StanzaId::try_new)?;

        let bind = element
            .children()
            .find(|child| child.name() == "bind" && child.ns() == NS_BIND)
            .ok_or(ClientError::InvalidBindResponse)?;
        let jid = bind
            .children()
            .find(|child| child.name() == "jid" && child.ns() == NS_BIND)
            .ok_or(ClientError::InvalidBindResponse)?
            .text();

        let jid = FullJid::from_str(jid.trim()).map_err(|_| ClientError::InvalidBindResponse)?;

        Ok(Self { stanza_id, jid })
    }

    /// Consume the resource-binding result and attach the stream identity and resumption policy.
    #[must_use]
    pub fn into_session_binding(
        self,
        stream_id: Option<StreamId>,
        resumption: StreamResumption,
    ) -> SessionBinding {
        SessionBinding {
            jid: self.jid,
            stream_id,
            resumable: resumption.is_resumable(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Recognized stream-bootstrap response parsed before application stanzas are dispatched.
#[non_exhaustive]
pub enum BootstrapElement {
    /// Features advertised by the peer during stream negotiation.
    StreamFeatures(StreamFeatures),
    /// The SASL exchange completed successfully.
    SaslSuccess,
    /// Authentication rejection reported by the peer.
    SaslFailure(SaslFailure),
    /// Successful resource binding reported by the peer.
    ResourceBindingResult(ResourceBindingResult),
}

impl BootstrapElement {
    /// Parse a supported wire value, rejecting unrecognized or malformed input.
    ///
    /// # Errors
    ///
    /// Returns a typed client error inside `Some` for recognized bootstrap elements with invalid content; unrelated elements return `None`.
    #[must_use]
    pub fn parse(element: &Element) -> Option<ClientResult<Self>> {
        let ns = element.ns();
        match element.name() {
            "features" if is_streams_ns(&ns) => {
                Some(StreamFeatures::from_element(element).map(Self::StreamFeatures))
            }
            "success" if ns == NS_SASL => Some(Ok(Self::SaslSuccess)),
            "failure" if ns == NS_SASL => {
                Some(SaslFailure::from_element(element).map(Self::SaslFailure))
            }
            "iq" => {
                let bind = element
                    .children()
                    .any(|child| child.name() == "bind" && child.ns() == NS_BIND);
                if bind {
                    Some(
                        ResourceBindingResult::from_element(element)
                            .map(Self::ResourceBindingResult),
                    )
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

fn is_client_stanza_ns(ns: &str) -> bool {
    ns.is_empty() || ns == NS_CLIENT
}

fn is_streams_ns(ns: &str) -> bool {
    ns.is_empty() || ns == NS_STREAMS
}
