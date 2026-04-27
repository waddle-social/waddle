//! XEP-0215: External Service Discovery.

use chrono::{DateTime, Utc};
use minidom::Element;
use std::{fmt, str::FromStr};
use thiserror::Error;
use xmpp_parsers::iq::{Iq, IqType};

/// Namespace for XEP-0215 External Service Discovery.
pub const NS_EXTDISCO: &str = "urn:xmpp:extdisco:2";

/// XEP-0215 service type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalServiceType {
    Stun,
    Turn,
    Other(ExternalServiceTypeName),
}

impl ExternalServiceType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Stun => "stun",
            Self::Turn => "turn",
            Self::Other(value) => value.as_str(),
        }
    }
}

/// Extension service type token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalServiceTypeName(String);

impl ExternalServiceTypeName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ExternalServiceType {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "stun" => Self::Stun,
            "turn" => Self::Turn,
            other => Self::Other(ExternalServiceTypeName::new(other)),
        })
    }
}

/// XEP-0215 transport protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalServiceTransport {
    Udp,
    Tcp,
    Other(ExternalServiceTransportName),
}

impl ExternalServiceTransport {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Udp => "udp",
            Self::Tcp => "tcp",
            Self::Other(value) => value.as_str(),
        }
    }
}

/// Extension service transport token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalServiceTransportName(String);

impl ExternalServiceTransportName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ExternalServiceTransport {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "udp" => Self::Udp,
            "tcp" => Self::Tcp,
            other => Self::Other(ExternalServiceTransportName::new(other)),
        })
    }
}

/// XEP-0215 service push action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalServiceAction {
    Add,
    Delete,
    Modify,
}

impl ExternalServiceAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Delete => "delete",
            Self::Modify => "modify",
        }
    }
}

impl FromStr for ExternalServiceAction {
    type Err = ExtDiscoError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "add" => Ok(Self::Add),
            "delete" | "remove" => Ok(Self::Delete),
            "modify" => Ok(Self::Modify),
            _ => Err(ExtDiscoError::InvalidAttribute("action")),
        }
    }
}

/// One `<service/>` entry.
#[derive(Clone, PartialEq, Eq)]
pub struct ExternalServiceHost(String);

impl ExternalServiceHost {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ExternalServiceHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ExternalServiceHost").field(&self.0).finish()
    }
}

/// TURN/STUN service username.
#[derive(Clone, PartialEq, Eq)]
pub struct ExternalServiceUsername(String);

impl ExternalServiceUsername {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ExternalServiceUsername {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ExternalServiceUsername(<redacted>)")
    }
}

/// TURN/STUN service password.
#[derive(Clone, PartialEq, Eq)]
pub struct ExternalServicePassword(String);

impl ExternalServicePassword {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ExternalServicePassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ExternalServicePassword(<redacted>)")
    }
}

/// Human-readable external service name.
#[derive(Clone, PartialEq, Eq)]
pub struct ExternalServiceName(String);

impl ExternalServiceName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ExternalServiceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ExternalServiceName").field(&self.0).finish()
    }
}

/// One `<service/>` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalService {
    pub service_type: ExternalServiceType,
    pub host: ExternalServiceHost,
    pub port: Option<u16>,
    pub transport: Option<ExternalServiceTransport>,
    pub username: Option<ExternalServiceUsername>,
    pub password: Option<ExternalServicePassword>,
    pub expires: Option<DateTime<Utc>>,
    pub restricted: Option<bool>,
    pub name: Option<ExternalServiceName>,
    pub action: Option<ExternalServiceAction>,
}

impl ExternalService {
    pub fn new(service_type: ExternalServiceType, host: impl Into<String>) -> Self {
        Self {
            service_type,
            host: ExternalServiceHost::new(host),
            port: None,
            transport: None,
            username: None,
            password: None,
            expires: None,
            restricted: None,
            name: None,
            action: None,
        }
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    pub fn with_transport(mut self, transport: ExternalServiceTransport) -> Self {
        self.transport = Some(transport);
        self
    }

    pub fn with_credentials(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
        expires: Option<DateTime<Utc>>,
    ) -> Self {
        self.username = Some(ExternalServiceUsername::new(username));
        self.password = Some(ExternalServicePassword::new(password));
        self.expires = expires;
        self
    }
}

/// XEP-0215 IQ request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtDiscoRequest {
    Services {
        service_type: Option<ExternalServiceType>,
    },
    Credentials {
        service: ExternalService,
    },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ExtDiscoError {
    #[error("not an extdisco request")]
    WrongElement,
    #[error("missing required attribute: {0}")]
    MissingAttribute(&'static str),
    #[error("invalid attribute: {0}")]
    InvalidAttribute(&'static str),
}

/// Check if an IQ carries a XEP-0215 request.
pub fn is_extdisco_iq(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Get(elem)
        if elem.ns() == NS_EXTDISCO && matches!(elem.name(), "services" | "credentials"))
}

/// Parse a XEP-0215 IQ-get request.
pub fn parse_extdisco_request(iq: &Iq) -> Result<ExtDiscoRequest, ExtDiscoError> {
    let elem = match &iq.payload {
        IqType::Get(elem) if elem.ns() == NS_EXTDISCO => elem,
        _ => return Err(ExtDiscoError::WrongElement),
    };

    match elem.name() {
        "services" => Ok(ExtDiscoRequest::Services {
            service_type: elem.attr("type").map(|value| value.parse().unwrap()),
        }),
        "credentials" => {
            let service = elem
                .children()
                .find(|child| child.name() == "service" && child.ns() == NS_EXTDISCO)
                .ok_or(ExtDiscoError::MissingAttribute("service"))?;
            Ok(ExtDiscoRequest::Credentials {
                service: parse_service_element(service)?,
            })
        }
        _ => Err(ExtDiscoError::WrongElement),
    }
}

/// Build a XEP-0215 services result IQ.
pub fn build_services_result(
    original_iq: &Iq,
    service_type: Option<&ExternalServiceType>,
    services: &[ExternalService],
) -> Iq {
    let mut root = Element::builder("services", NS_EXTDISCO).build();
    if let Some(service_type) = service_type {
        root.set_attr("type", service_type.as_str());
    }
    for service in services {
        root.append_child(build_service_element(service));
    }

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(root)),
    }
}

/// Build a XEP-0215 credentials result IQ.
pub fn build_credentials_result(original_iq: &Iq, services: &[ExternalService]) -> Iq {
    let mut root = Element::builder("credentials", NS_EXTDISCO).build();
    for service in services {
        root.append_child(build_service_element(service));
    }

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(root)),
    }
}

/// Build a `<service/>` element.
pub fn build_service_element(service: &ExternalService) -> Element {
    let mut elem = Element::builder("service", NS_EXTDISCO)
        .attr("host", service.host.as_str())
        .attr("type", service.service_type.as_str())
        .build();

    if let Some(port) = service.port {
        elem.set_attr("port", port.to_string());
    }
    if let Some(transport) = &service.transport {
        elem.set_attr("transport", transport.as_str());
    }
    if let Some(username) = &service.username {
        elem.set_attr("username", username.as_str());
    }
    if let Some(password) = &service.password {
        elem.set_attr("password", password.as_str());
    }
    if let Some(expires) = service.expires {
        elem.set_attr("expires", expires.to_rfc3339());
    }
    if let Some(restricted) = service.restricted {
        elem.set_attr("restricted", if restricted { "true" } else { "false" });
    }
    if let Some(name) = &service.name {
        elem.set_attr("name", name.as_str());
    }
    if let Some(action) = service.action {
        elem.set_attr("action", action.as_str());
    }

    elem
}

/// Parse a `<service/>` element.
pub fn parse_service_element(elem: &Element) -> Result<ExternalService, ExtDiscoError> {
    if elem.name() != "service" || elem.ns() != NS_EXTDISCO {
        return Err(ExtDiscoError::WrongElement);
    }

    let service_type = elem
        .attr("type")
        .filter(|value| !value.is_empty())
        .ok_or(ExtDiscoError::MissingAttribute("type"))?
        .parse()
        .unwrap();
    let host = elem
        .attr("host")
        .filter(|value| !value.is_empty())
        .ok_or(ExtDiscoError::MissingAttribute("host"))?
        .to_owned();

    let mut service = ExternalService::new(service_type, host);
    service.port = elem
        .attr("port")
        .map(|value| {
            value
                .parse::<u16>()
                .map_err(|_| ExtDiscoError::InvalidAttribute("port"))
        })
        .transpose()?;
    service.transport = elem.attr("transport").map(|value| value.parse().unwrap());
    service.username = elem.attr("username").map(ExternalServiceUsername::new);
    service.password = elem.attr("password").map(ExternalServicePassword::new);
    service.expires = elem
        .attr("expires")
        .map(|value| {
            chrono::DateTime::parse_from_rfc3339(value)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|_| ExtDiscoError::InvalidAttribute("expires"))
        })
        .transpose()?;
    service.restricted = elem
        .attr("restricted")
        .map(|value| match value {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => Err(ExtDiscoError::InvalidAttribute("restricted")),
        })
        .transpose()?;
    service.name = elem.attr("name").map(ExternalServiceName::new);
    service.action = elem.attr("action").map(str::parse).transpose()?;

    Ok(service)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_services_request_filter() {
        let iq = Iq {
            from: None,
            to: None,
            id: "x1".to_string(),
            payload: IqType::Get(
                Element::builder("services", NS_EXTDISCO)
                    .attr("type", "turn")
                    .build(),
            ),
        };
        assert!(is_extdisco_iq(&iq));
        assert_eq!(
            parse_extdisco_request(&iq),
            Ok(ExtDiscoRequest::Services {
                service_type: Some(ExternalServiceType::Turn)
            })
        );
    }

    #[test]
    fn builds_and_parses_service() {
        let service = ExternalService::new(ExternalServiceType::Turn, "turn.example")
            .with_port(3478)
            .with_transport(ExternalServiceTransport::Udp)
            .with_credentials("u", "p", None);
        let elem = build_service_element(&service);
        assert_eq!(elem.attr("host"), Some("turn.example"));
        assert_eq!(elem.attr("type"), Some("turn"));
        assert_eq!(parse_service_element(&elem), Ok(service.clone()));
    }

    #[test]
    fn serializes_schema_delete_action_name() {
        let mut service = ExternalService::new(ExternalServiceType::Turn, "turn.example");
        service.action = Some(ExternalServiceAction::Delete);
        let elem = build_service_element(&service);
        assert_eq!(elem.attr("action"), Some("delete"));
        assert_eq!(parse_service_element(&elem), Ok(service.clone()));

        let mut legacy_elem = elem.clone();
        legacy_elem.set_attr("action", "remove");
        assert_eq!(parse_service_element(&legacy_elem), Ok(service));
    }

    #[test]
    fn credentials_request_requires_service_identity() {
        let iq = Iq {
            from: None,
            to: None,
            id: "x1".to_string(),
            payload: IqType::Get(
                Element::builder("credentials", NS_EXTDISCO)
                    .append(
                        Element::builder("service", NS_EXTDISCO)
                            .attr("type", "turn")
                            .attr("host", "turn.example")
                            .build(),
                    )
                    .build(),
            ),
        };
        match parse_extdisco_request(&iq).expect("parsed") {
            ExtDiscoRequest::Credentials { service } => {
                assert_eq!(service.service_type, ExternalServiceType::Turn);
                assert_eq!(service.host.as_str(), "turn.example");
            }
            _ => panic!("expected credentials request"),
        }
    }
}
