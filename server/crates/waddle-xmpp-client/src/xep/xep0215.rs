//! XEP-0215: External Service Discovery (client side).
//!
//! The Waddle server advertises its LiveKit-fronted TURN/STUN service over the
//! `urn:xmpp:extdisco:2` namespace. This module is the client counterpart: it
//! builds the `<services/>` IQ-get the client sends to its own server and parses
//! the `<service/>` entries in the result into typed [`ExternalService`] values
//! that the call engine maps to `RTCIceServer`s.
//!
//! Parsing is done by hand over [`minidom::Element`] rather than through
//! `xmpp_parsers::extdisco`: the server emits the TLS-protected TURN entry as
//! `type='turns'` (XEP-0215 §3.6.5), and `xmpp_parsers`' `Type` enum has no
//! `Turns` variant, so the typed parser would reject Waddle's own response.

use minidom::Element;

/// External Service Discovery namespace.
pub const NS_EXT_DISCO: &str = "urn:xmpp:extdisco:2";

/// Service type advertised in a `<service type='…'/>` entry. The XEP-0215
/// §3.6.5 `turns` value (TLS-protected TURN) is a distinct variant — it is the
/// shape Waddle advertises and is absent from `xmpp_parsers`' typed surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalServiceType {
    /// `stun` — STUN server (RFC 5389).
    Stun,
    /// `turn` — plain TURN relay (RFC 5766).
    Turn,
    /// `turns` — TLS-protected TURN relay (XEP-0215 §3.6.5).
    Turns,
}

impl ExternalServiceType {
    /// Map the wire `type` attribute to a typed value. Unknown types (e.g.
    /// `ftp`) yield `None` so the caller can skip entries it cannot use as ICE
    /// servers.
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "stun" => Some(Self::Stun),
            "turn" => Some(Self::Turn),
            "turns" => Some(Self::Turns),
            _ => None,
        }
    }
}

/// Transport of a `<service transport='…'/>` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalServiceTransport {
    Udp,
    Tcp,
}

impl ExternalServiceTransport {
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "udp" => Some(Self::Udp),
            "tcp" => Some(Self::Tcp),
            _ => None,
        }
    }
}

/// A single external service advertised by the server, parsed from one
/// `<service/>` element. Credentials are present only on TURN/TURNS entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalService {
    pub service_type: ExternalServiceType,
    pub host: String,
    pub port: u16,
    pub transport: Option<ExternalServiceTransport>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// XEP-0082 dateTime string (UTC) marking when the credentials expire.
    /// Kept verbatim from the wire — it crosses the WASM boundary unchanged for
    /// the client's refresh logic.
    pub expires: Option<String>,
    /// `restricted='1'` — credentials are required (XEP-0215 §3.6.5).
    pub restricted: bool,
}

/// `jabber:client` namespace for the IQ envelope.
const NS_CLIENT: &str = "jabber:client";

/// Build the `<iq type='get'><services xmlns='urn:xmpp:extdisco:2'/></iq>` a
/// client sends to its own server to learn the advertised external services
/// (XEP-0215 §3.2). An empty `<services/>` requests every service type.
pub fn build_extdisco_services_iq(to: &str, request_id: &str) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), request_id)
        .append(Element::builder("services", NS_EXT_DISCO).build())
        .build()
}

/// Parse the `<service/>` entries from a `services` IQ-result payload into typed
/// values. Entries whose `type` is unknown or which lack the mandatory
/// `host`/`port` attributes are skipped.
pub fn parse_external_services(iq: &Element) -> Vec<ExternalService> {
    let Some(services) = iq.get_child("services", NS_EXT_DISCO) else {
        return Vec::new();
    };
    services
        .children()
        .filter(|child| child.name() == "service" && child.ns() == NS_EXT_DISCO)
        .filter_map(parse_service_element)
        .collect()
}

/// Parse one `<service/>` element. Returns `None` when the entry is unusable as
/// an ICE server: unknown `type`, or missing/invalid `host`/`port`.
fn parse_service_element(service: &Element) -> Option<ExternalService> {
    let service_type = ExternalServiceType::from_wire(service.attr("type")?)?;
    let host = service.attr("host")?.to_string();
    let port = service.attr("port")?.parse::<u16>().ok()?;
    let transport = service
        .attr("transport")
        .and_then(ExternalServiceTransport::from_wire);
    Some(ExternalService {
        service_type,
        host,
        port,
        transport,
        username: service.attr("username").map(str::to_string),
        password: service.attr("password").map(str::to_string),
        expires: service.attr("expires").map(str::to_string),
        restricted: service.attr("restricted") == Some("1"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_stun_service() {
        let xml = "<iq xmlns='jabber:client' type='result' from='waddle.test' id='e1'>\
                     <services xmlns='urn:xmpp:extdisco:2'>\
                       <service action='add' type='stun' transport='udp' \
                                host='turn.waddle.social' port='3478'/>\
                     </services>\
                   </iq>";
        let iq: Element = xml.parse().expect("valid xml");
        let services = parse_external_services(&iq);

        assert_eq!(services.len(), 1);
        let stun = &services[0];
        assert_eq!(stun.service_type, ExternalServiceType::Stun);
        assert_eq!(stun.host, "turn.waddle.social");
        assert_eq!(stun.port, 3478);
        assert_eq!(stun.transport, Some(ExternalServiceTransport::Udp));
        assert!(stun.username.is_none());
        assert!(stun.password.is_none());
        assert!(!stun.restricted);
    }

    /// The conformance-critical case and the reason this module parses by hand:
    /// the server advertises TLS-protected TURN as `type='turns'`, which the
    /// `xmpp_parsers` typed surface cannot represent. This mirrors the exact
    /// wire shape `waddle-xmpp` emits (see its `build_turn_service`).
    #[test]
    fn parses_tls_turn_service_with_credentials() {
        let xml = "<iq xmlns='jabber:client' type='result' from='waddle.test' id='e1'>\
                     <services xmlns='urn:xmpp:extdisco:2'>\
                       <service action='add' type='turns' transport='tcp' \
                                host='turn.waddle.social' port='443' \
                                username='1700000000:alice@waddle.social/desktop' \
                                password='base64hmac==' \
                                expires='2026-06-17T12:00:00Z' restricted='1'/>\
                     </services>\
                   </iq>";
        let iq: Element = xml.parse().expect("valid xml");
        let services = parse_external_services(&iq);

        assert_eq!(services.len(), 1);
        let turns = &services[0];
        assert_eq!(turns.service_type, ExternalServiceType::Turns);
        assert_eq!(turns.transport, Some(ExternalServiceTransport::Tcp));
        assert_eq!(turns.port, 443);
        assert_eq!(
            turns.username.as_deref(),
            Some("1700000000:alice@waddle.social/desktop")
        );
        assert_eq!(turns.password.as_deref(), Some("base64hmac=="));
        assert_eq!(turns.expires.as_deref(), Some("2026-06-17T12:00:00Z"));
        assert!(turns.restricted);
    }

    #[test]
    fn parses_full_server_response_turns_and_stun() {
        let xml = "<iq xmlns='jabber:client' type='result' from='waddle.test' id='e1'>\
                     <services xmlns='urn:xmpp:extdisco:2'>\
                       <service action='add' type='turns' transport='tcp' \
                                host='turn.waddle.social' port='443' \
                                username='u' password='p' restricted='1'/>\
                       <service action='add' type='stun' transport='udp' \
                                host='turn.waddle.social' port='3478'/>\
                     </services>\
                   </iq>";
        let iq: Element = xml.parse().expect("valid xml");
        let services = parse_external_services(&iq);

        assert_eq!(services.len(), 2);
        assert_eq!(services[0].service_type, ExternalServiceType::Turns);
        assert_eq!(services[1].service_type, ExternalServiceType::Stun);
    }

    #[test]
    fn skips_unknown_service_type() {
        let xml = "<iq xmlns='jabber:client' type='result' id='e1'>\
                     <services xmlns='urn:xmpp:extdisco:2'>\
                       <service type='ftp' host='ftp.waddle.social' port='21'/>\
                       <service type='stun' host='turn.waddle.social' port='3478'/>\
                     </services>\
                   </iq>";
        let iq: Element = xml.parse().expect("valid xml");
        let services = parse_external_services(&iq);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].service_type, ExternalServiceType::Stun);
    }

    #[test]
    fn skips_entry_missing_host_or_port() {
        let xml = "<iq xmlns='jabber:client' type='result' id='e1'>\
                     <services xmlns='urn:xmpp:extdisco:2'>\
                       <service type='stun' port='3478'/>\
                       <service type='stun' host='turn.waddle.social'/>\
                       <service type='stun' host='turn.waddle.social' port='not-a-port'/>\
                     </services>\
                   </iq>";
        let iq: Element = xml.parse().expect("valid xml");
        assert!(parse_external_services(&iq).is_empty());
    }

    #[test]
    fn missing_services_payload_yields_empty() {
        let xml = "<iq xmlns='jabber:client' type='result' id='e1'/>";
        let iq: Element = xml.parse().expect("valid xml");
        assert!(parse_external_services(&iq).is_empty());
    }

    #[test]
    fn builds_services_get_iq() {
        let iq = build_extdisco_services_iq("waddle.test", "req-1");
        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("to"), Some("waddle.test"));
        assert_eq!(iq.attr("id"), Some("req-1"));

        let services = iq
            .get_child("services", NS_EXT_DISCO)
            .expect("services payload present");
        // XEP-0215 §3.6.1: an empty <services/> requests every service type.
        assert!(services.attr("type").is_none());
        assert_eq!(services.children().count(), 0);
    }

    /// Round-trip: the query we build, answered with the server's wire shape,
    /// parses back to the advertised services.
    #[test]
    fn query_round_trips_against_server_response() {
        let query = build_extdisco_services_iq("waddle.test", "req-1");
        assert_eq!(
            query.get_child("services", NS_EXT_DISCO).map(Element::name),
            Some("services")
        );

        let response = "<iq xmlns='jabber:client' type='result' from='waddle.test' id='req-1'>\
                          <services xmlns='urn:xmpp:extdisco:2'>\
                            <service action='add' type='turns' transport='tcp' \
                                     host='turn.waddle.social' port='443' \
                                     username='u' password='p' restricted='1'/>\
                            <service action='add' type='stun' transport='udp' \
                                     host='turn.waddle.social' port='3478'/>\
                          </services>\
                        </iq>";
        let response: Element = response.parse().expect("valid xml");
        let services = parse_external_services(&response);
        assert_eq!(services.len(), 2);
    }
}
