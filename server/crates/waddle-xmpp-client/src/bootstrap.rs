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

pub const NS_BIND: &str = "urn:ietf:params:xml:ns:xmpp-bind";
pub const NS_CLIENT: &str = "jabber:client";
pub const NS_SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";
pub const NS_SESSION: &str = "urn:ietf:params:xml:ns:xmpp-session";
pub const NS_SM: &str = "urn:xmpp:sm:3";
pub const NS_STREAMS: &str = "http://etherx.jabber.org/streams";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMechanism {
    OAuthBearer,
}

impl AuthMechanism {
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
pub enum RequiredStreamFeature {
    Authentication(AuthMechanism),
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
pub struct StreamFeatures {
    pub mechanisms: Vec<AuthMechanism>,
    pub bind: bool,
    pub legacy_session: bool,
    pub stream_management: bool,
    pub resumable: bool,
}

impl StreamFeatures {
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

    pub fn supports(&self, mechanism: AuthMechanism) -> bool {
        self.mechanisms.contains(&mechanism)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticationRequest {
    OAuthBearer(OAuthBearerRequest),
}

impl AuthenticationRequest {
    pub fn from_config(config: &OAuthBearerConfig) -> Self {
        Self::OAuthBearer(OAuthBearerRequest {
            account: config.account.clone(),
            access_token: config.access_token.clone(),
        })
    }

    pub fn to_element(&self) -> Element {
        match self {
            Self::OAuthBearer(request) => request.to_element(),
        }
    }

    pub fn to_transport_message(&self) -> TransportMessage {
        TransportMessage::Element(self.to_element())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OAuthBearerRequest {
    pub account: BareJid,
    pub access_token: AccessToken,
}

impl OAuthBearerRequest {
    pub fn to_element(&self) -> Element {
        Element::builder("auth", NS_SASL)
            .attr("mechanism", AuthMechanism::OAuthBearer.as_str())
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
pub struct SaslFailure {
    pub condition: SaslFailureCondition,
}

impl SaslFailure {
    pub fn from_element(element: &Element) -> ClientResult<Self> {
        if element.name() != "failure" || element.ns() != NS_SASL {
            return Err(ClientError::InvalidSaslFailure);
        }

        let condition = element
            .children()
            .map(|child| SaslFailureCondition::parse(child.name()))
            .next()
            .unwrap_or(SaslFailureCondition::Unknown);

        Ok(Self { condition })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceBindingRequest {
    pub stanza_id: StanzaId,
    pub resource: ClientResource,
}

impl ResourceBindingRequest {
    pub fn new(stanza_id: StanzaId, resource: ClientResource) -> Self {
        Self {
            stanza_id,
            resource,
        }
    }

    pub fn to_element(&self) -> Element {
        Element::builder("iq", NS_CLIENT)
            .attr("id", self.stanza_id.as_str())
            .attr("type", "set")
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

    pub fn to_transport_message(&self) -> TransportMessage {
        TransportMessage::Element(self.to_element())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceBindingResult {
    pub stanza_id: StanzaId,
    pub jid: FullJid,
}

impl ResourceBindingResult {
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
            .and_then(StanzaId::new)?;

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

    pub fn into_session_binding(
        self,
        stream_id: Option<StreamId>,
        resumable: bool,
    ) -> SessionBinding {
        SessionBinding {
            jid: self.jid,
            stream_id,
            resumable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapElement {
    StreamFeatures(StreamFeatures),
    SaslSuccess,
    SaslFailure(SaslFailure),
    ResourceBindingResult(ResourceBindingResult),
}

impl BootstrapElement {
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
