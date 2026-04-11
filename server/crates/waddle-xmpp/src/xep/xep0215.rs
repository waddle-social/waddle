//! XEP-0215: External Service Discovery
//!
//! Discover external services (TURN, STUN) for NAT traversal and
//! other infrastructure needs. Used by WebRTC clients to find
//! relay servers for voice/video calls.
//!
//! ## XML Format
//!
//! Query:
//! ```xml
//! <iq type='get' to='example.com' id='ext-1'>
//!   <services xmlns='urn:xmpp:extdisco:2'/>
//! </iq>
//! ```
//!
//! Response:
//! ```xml
//! <iq type='result' from='example.com' id='ext-1'>
//!   <services xmlns='urn:xmpp:extdisco:2'>
//!     <service type='stun' host='stun.example.com' port='3478'
//!              transport='udp'/>
//!     <service type='turn' host='turn.example.com' port='3478'
//!              transport='udp' username='user' password='pass'
//!              restricted='true'/>
//!   </services>
//! </iq>
//! ```

use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

/// Namespace for XEP-0215 External Service Discovery.
pub const NS_EXTDISCO: &str = "urn:xmpp:extdisco:2";

/// Service type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ServiceType {
    /// STUN server for NAT traversal.
    Stun,
    /// TURN relay server.
    Turn,
    /// Other service type.
    Other(String),
}

impl ServiceType {
    /// Parse from attribute.
    pub fn from_str_attr(s: &str) -> Self {
        match s {
            "stun" => Self::Stun,
            "turn" => Self::Turn,
            other => Self::Other(other.to_owned()),
        }
    }

    /// Convert to attribute string.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Stun => "stun",
            Self::Turn => "turn",
            Self::Other(s) => s,
        }
    }
}

impl std::fmt::Display for ServiceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Transport protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transport {
    /// UDP transport.
    Udp,
    /// TCP transport.
    Tcp,
}

impl Transport {
    /// Parse from attribute.
    pub fn from_str_attr(s: &str) -> Option<Self> {
        match s {
            "udp" => Some(Self::Udp),
            "tcp" => Some(Self::Tcp),
            _ => None,
        }
    }

    /// Convert to attribute string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Udp => "udp",
            Self::Tcp => "tcp",
        }
    }
}

/// An external service entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalService {
    /// Service type (stun, turn, etc.).
    pub service_type: ServiceType,
    /// Hostname or IP address.
    pub host: String,
    /// Port number.
    pub port: Option<u16>,
    /// Transport protocol.
    pub transport: Option<Transport>,
    /// Username for authentication.
    pub username: Option<String>,
    /// Password for authentication.
    pub password: Option<String>,
    /// Whether credentials are required.
    pub restricted: bool,
}

impl ExternalService {
    /// Create a STUN service.
    pub fn stun(host: impl Into<String>, port: u16) -> Self {
        Self {
            service_type: ServiceType::Stun,
            host: host.into(),
            port: Some(port),
            transport: Some(Transport::Udp),
            username: None,
            password: None,
            restricted: false,
        }
    }

    /// Create a TURN service.
    pub fn turn(host: impl Into<String>, port: u16) -> Self {
        Self {
            service_type: ServiceType::Turn,
            host: host.into(),
            port: Some(port),
            transport: Some(Transport::Udp),
            username: None,
            password: None,
            restricted: false,
        }
    }

    /// Set credentials.
    pub fn with_credentials(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
        self.restricted = true;
        self
    }

    /// Set transport.
    pub fn with_transport(mut self, transport: Transport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Generate a URI for this service (e.g., `stun:host:port` or `turn:host:port`).
    pub fn to_uri(&self) -> String {
        let proto = self.service_type.as_str();
        match self.port {
            Some(port) => format!("{proto}:{host}:{port}", host = self.host),
            None => format!("{proto}:{host}", host = self.host),
        }
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an IQ is an external service discovery request.
pub fn is_extdisco_request(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Get(elem)
        if elem.name() == "services" && elem.ns() == NS_EXTDISCO)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse external services from an IQ response.
pub fn parse_services_response(iq: &Iq) -> Vec<ExternalService> {
    let elem = match &iq.payload {
        IqType::Result(Some(elem)) if elem.name() == "services" && elem.ns() == NS_EXTDISCO => elem,
        _ => return Vec::new(),
    };

    elem.children()
        .filter(|c| c.name() == "service" && c.ns() == NS_EXTDISCO)
        .filter_map(|c| {
            let service_type = ServiceType::from_str_attr(c.attr("type").unwrap_or(""));
            let host = c.attr("host").filter(|h| !h.is_empty())?.to_owned();
            let port = c.attr("port").and_then(|p| p.parse().ok());
            let transport = c.attr("transport").and_then(Transport::from_str_attr);
            let username = c
                .attr("username")
                .filter(|u| !u.is_empty())
                .map(|u| u.to_owned());
            let password = c
                .attr("password")
                .filter(|p| !p.is_empty())
                .map(|p| p.to_owned());
            let restricted = c.attr("restricted") == Some("true");

            Some(ExternalService {
                service_type,
                host,
                port,
                transport,
                username,
                password,
                restricted,
            })
        })
        .collect()
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a services response IQ.
pub fn build_services_response(original_iq: &Iq, services: &[ExternalService]) -> Iq {
    let mut services_elem = Element::builder("services", NS_EXTDISCO).build();

    for svc in services {
        let mut service = Element::builder("service", NS_EXTDISCO)
            .attr("type", svc.service_type.as_str())
            .attr("host", svc.host.as_str())
            .build();

        if let Some(port) = svc.port {
            service.set_attr("port", port.to_string());
        }
        if let Some(transport) = svc.transport {
            service.set_attr("transport", transport.as_str());
        }
        if let Some(ref username) = svc.username {
            service.set_attr("username", username);
        }
        if let Some(ref password) = svc.password {
            service.set_attr("password", password);
        }
        if svc.restricted {
            service.set_attr("restricted", "true");
        }

        services_elem.append_child(service);
    }

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(services_elem)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request() -> Iq {
        let elem = Element::builder("services", NS_EXTDISCO).build();
        Iq {
            from: Some("user@example.com".parse().expect("valid")),
            to: Some("example.com".parse().expect("valid")),
            id: "ext-1".to_owned(),
            payload: IqType::Get(elem),
        }
    }

    #[test]
    fn test_is_extdisco_request() {
        assert!(is_extdisco_request(&make_request()));
    }

    #[test]
    fn test_is_extdisco_request_false() {
        let elem = Element::builder("query", "jabber:iq:roster").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "x".to_owned(),
            payload: IqType::Get(elem),
        };
        assert!(!is_extdisco_request(&iq));
    }

    #[test]
    fn test_build_and_parse_response() {
        let request = make_request();
        let services = vec![
            ExternalService::stun("stun.example.com", 3478),
            ExternalService::turn("turn.example.com", 3478).with_credentials("user", "pass"),
        ];

        let response = build_services_response(&request, &services);
        let parsed = parse_services_response(&response);

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].service_type, ServiceType::Stun);
        assert_eq!(parsed[0].host, "stun.example.com");
        assert_eq!(parsed[0].port, Some(3478));
        assert!(!parsed[0].restricted);

        assert_eq!(parsed[1].service_type, ServiceType::Turn);
        assert_eq!(parsed[1].username.as_deref(), Some("user"));
        assert_eq!(parsed[1].password.as_deref(), Some("pass"));
        assert!(parsed[1].restricted);
    }

    #[test]
    fn test_parse_empty_response() {
        let iq = Iq {
            from: None,
            to: None,
            id: "x".to_owned(),
            payload: IqType::Result(None),
        };
        assert!(parse_services_response(&iq).is_empty());
    }

    #[test]
    fn test_service_to_uri() {
        let stun = ExternalService::stun("stun.example.com", 3478);
        assert_eq!(stun.to_uri(), "stun:stun.example.com:3478");

        let turn = ExternalService::turn("turn.example.com", 443).with_transport(Transport::Tcp);
        assert_eq!(turn.to_uri(), "turn:turn.example.com:443");
    }

    #[test]
    fn test_service_type_roundtrip() {
        assert_eq!(ServiceType::from_str_attr("stun"), ServiceType::Stun);
        assert_eq!(ServiceType::from_str_attr("turn"), ServiceType::Turn);
        assert_eq!(
            ServiceType::from_str_attr("sip"),
            ServiceType::Other("sip".into())
        );
    }

    #[test]
    fn test_service_type_display() {
        assert_eq!(ServiceType::Stun.to_string(), "stun");
        assert_eq!(ServiceType::Turn.to_string(), "turn");
    }

    #[test]
    fn test_transport() {
        assert_eq!(Transport::from_str_attr("udp"), Some(Transport::Udp));
        assert_eq!(Transport::from_str_attr("tcp"), Some(Transport::Tcp));
        assert_eq!(Transport::from_str_attr("ws"), None);
        assert_eq!(Transport::Udp.as_str(), "udp");
    }
}
