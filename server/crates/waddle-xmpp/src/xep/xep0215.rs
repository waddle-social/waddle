//! XEP-0215: External Service Discovery.
//!
//! The server exposes the LiveKit-fronted TURN/STUN service via the
//! `urn:xmpp:extdisco:2` namespace. Credentials are minted from the
//! [`waddle_sfu::SfuService`] and carry an `expires` attribute so
//! clients refresh before they go stale.
//!
//! Thin wrapper over [`xmpp_parsers::extdisco`] — namespace constant
//! plus Waddle-specific builder helpers that translate a
//! [`waddle_sfu::TurnCredential`] into the `<service/>` entries the
//! responder sends back on a `services` or `credentials` IQ.
//!
//! The `type` attribute is "the service type as registered with the
//! Registrar" (XEP-0215, Service element attributes). Waddle uses
//! `stun`, `turn` (TURN over UDP) and `turns` (TURN over TLS,
//! terminated at the cluster `tlsroute` for `turn.waddle.social`).
//! xmpp-parsers' [`ServiceType`] only models `stun` and `turn`, so
//! every `<service/>` entry is built manually via
//! [`minidom::Element::builder`] and the request filter is modelled
//! by [`ServiceKind`].

pub use xmpp_parsers::extdisco::{
    Action, Credentials, Service, ServicesQuery, ServicesResult, Transport, Type as ServiceType,
};

use minidom::Element;

use waddle_sfu::{TurnCredential, TurnHost};

pub const NS_EXT_DISCO: &str = "urn:xmpp:extdisco:2";

/// Wire-form `type` value for TLS-protected TURN.
pub const TYPE_TURNS: &str = "turns";

/// A service type Waddle understands, as carried in the `type`
/// attribute of `<services/>` filters, `<service/>` entries and
/// `<credentials/>` requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Stun,
    Stuns,
    Turn,
    Turns,
}

impl ServiceKind {
    /// Parse a wire `type` value; `None` for an unknown type.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "stun" => Some(Self::Stun),
            "stuns" => Some(Self::Stuns),
            "turn" => Some(Self::Turn),
            TYPE_TURNS => Some(Self::Turns),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stun => "stun",
            Self::Stuns => "stuns",
            Self::Turn => "turn",
            Self::Turns => TYPE_TURNS,
        }
    }
}

/// Build the `<service/>` entries Waddle advertises: `turn` (UDP) and
/// `turns` (TLS on port 443, matching the cluster's `tlsroute` for
/// `turn.waddle.social`) carrying the time-limited credential pair,
/// plus a plain `stun` entry on the UDP port.
pub fn build_services(
    turn_host: &TurnHost,
    turn_tls_port: u16,
    turn_udp_port: u16,
    cred: &TurnCredential,
) -> Vec<Element> {
    vec![
        build_turn_service(turn_host, turn_udp_port, cred),
        build_turns_service(turn_host, turn_tls_port, cred),
        build_stun_service_element(turn_host, turn_udp_port),
    ]
}

/// TURN-over-UDP entry (`type='turn'`) with HMAC-derived credentials.
pub fn build_turn_service(
    turn_host: &TurnHost,
    turn_udp_port: u16,
    cred: &TurnCredential,
) -> Element {
    turn_entry(
        ServiceKind::Turn,
        "udp",
        turn_host,
        turn_udp_port,
        cred,
        true,
    )
}

/// TURN-over-TLS entry (`type='turns'`) with HMAC-derived credentials.
pub fn build_turns_service(
    turn_host: &TurnHost,
    turn_tls_port: u16,
    cred: &TurnCredential,
) -> Element {
    turn_entry(
        ServiceKind::Turns,
        "tcp",
        turn_host,
        turn_tls_port,
        cred,
        true,
    )
}

/// `<service/>` child of a `<credentials/>` result: identifies the
/// service by host, type and port and carries the minted credential.
pub fn build_credentials_service(
    kind: ServiceKind,
    turn_host: &TurnHost,
    port: u16,
    cred: &TurnCredential,
) -> Element {
    turn_entry(kind, "", turn_host, port, cred, false)
}

fn turn_entry(
    kind: ServiceKind,
    transport: &str,
    turn_host: &TurnHost,
    port: u16,
    cred: &TurnCredential,
    services_entry: bool,
) -> Element {
    let mut builder = Element::builder("service", NS_EXT_DISCO);
    if services_entry {
        builder = builder
            .attr(minidom::rxml::xml_ncname!("action").to_owned(), "add")
            .attr(
                minidom::rxml::xml_ncname!("transport").to_owned(),
                transport,
            );
    }
    builder
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), kind.as_str())
        .attr(
            minidom::rxml::xml_ncname!("host").to_owned(),
            turn_host.as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("port").to_owned(),
            port.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("username").to_owned(),
            cred.username.as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("password").to_owned(),
            cred.password.as_str(),
        )
        // XEP-0082: `expires` is an RFC 3339 timestamp.
        .attr(
            minidom::rxml::xml_ncname!("expires").to_owned(),
            cred.expires_at.fixed_offset().to_rfc3339(),
        )
        .build()
}

/// XEP-0215 STUN entry. No credentials, so it's cheap to build and
/// safe to serve to anyone who can already authenticate to the XMPP
/// stream.
pub fn build_stun_service_element(turn_host: &TurnHost, stun_udp_port: u16) -> Element {
    Element::builder("service", NS_EXT_DISCO)
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "add")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "stun")
        .attr(minidom::rxml::xml_ncname!("transport").to_owned(), "udp")
        .attr(
            minidom::rxml::xml_ncname!("host").to_owned(),
            turn_host.as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("port").to_owned(),
            stun_udp_port.to_string(),
        )
        .build()
}

/// Build the `<services xmlns='urn:xmpp:extdisco:2'/>` wrapper
/// element from a list of `<service/>` children, with an optional
/// `type` attribute echoing the filter that produced the result.
pub fn build_services_result_element(type_filter: Option<&str>, services: Vec<Element>) -> Element {
    let mut builder = Element::builder("services", NS_EXT_DISCO);
    if let Some(t) = type_filter {
        builder = builder.attr(minidom::rxml::xml_ncname!("type").to_owned(), t);
    }
    for service in services {
        builder = builder.append(service);
    }
    builder.build()
}

/// Build the `<credentials xmlns='urn:xmpp:extdisco:2'/>` result.
pub fn build_credentials_result_element(services: Vec<Element>) -> Element {
    let mut builder = Element::builder("credentials", NS_EXT_DISCO);
    for service in services {
        builder = builder.append(service);
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn fixture_credential() -> TurnCredential {
        use waddle_sfu::{
            ApiKey, ApiSecret, LiveKitSfu, SfuConfig, SfuService, TurnSharedSecret, WebsocketUrl,
        };
        let cfg = SfuConfig {
            api_key: ApiKey::new("k"),
            api_secret: ApiSecret::from_text("api-secret-meets-min-length-32!!")
                .expect("test secret meets min length"),
            webhook_secret: ApiSecret::from_text("api-secret-meets-min-length-32!!")
                .expect("test secret meets min length"),
            ws_url: WebsocketUrl::try_new("wss://h/".parse().unwrap()).unwrap(),
            turn_host: TurnHost::new("turn.waddle.social"),
            turn_tls_port: 443,
            turn_udp_port: 3478,
            turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
            token_ttl: Duration::seconds(60),
            turn_ttl: Duration::seconds(60),
        };
        let sfu = LiveKitSfu::try_new(cfg).expect("LiveKitSfu init in test");
        let jid: jid::FullJid = "alice@waddle.social/desktop".parse().unwrap();
        let identity = waddle_sfu::Identity::from_jid(jid);
        sfu.issue_turn_credentials(&identity).unwrap()
    }

    #[test]
    fn ns_matches_xmpp_parsers() {
        assert_eq!(NS_EXT_DISCO, xmpp_parsers::ns::EXT_DISCO);
    }

    #[test]
    fn build_services_emits_turns_and_stun_entries() {
        let host = TurnHost::new("turn.waddle.social");
        let cred = fixture_credential();
        let services = build_services(&host, 443, 3478, &cred);

        assert_eq!(services.len(), 3);

        let turn = &services[0];
        assert_eq!(turn.attr("type"), Some("turn"));
        assert_eq!(turn.attr("transport"), Some("udp"));
        assert_eq!(turn.attr("port"), Some("3478"));
        assert!(turn.attr("username").is_some());

        let turns = &services[1];
        assert_eq!(turns.name(), "service");
        assert_eq!(turns.ns(), NS_EXT_DISCO);
        assert_eq!(
            turns.attr("type"),
            Some(TYPE_TURNS),
            "TLS TURN must be advertised as type='turns'"
        );
        assert_ne!(
            turns.attr("type"),
            Some("turn"),
            "TLS-protected TURN must not be conflated with plain UDP TURN"
        );
        assert_eq!(turns.attr("transport"), Some("tcp"));
        assert_eq!(turns.attr("host"), Some("turn.waddle.social"));
        assert_eq!(turns.attr("port"), Some("443"));
        assert!(turns.attr("username").is_some());
        assert!(turns.attr("password").is_some());
        assert!(turns.attr("expires").is_some());
        assert!(turns.attr("restricted").is_none());

        let stun = &services[2];
        assert_eq!(stun.name(), "service");
        assert_eq!(stun.attr("type"), Some("stun"));
        assert_eq!(stun.attr("transport"), Some("udp"));
        assert!(stun.attr("username").is_none());
        assert!(stun.attr("password").is_none());
    }

    #[test]
    fn build_turn_service_pins_turns_wire_shape() {
        let host = TurnHost::new("turn.waddle.social");
        let cred = fixture_credential();
        let turn = build_turns_service(&host, 443, &cred);

        // TLS-protected TURN is `type='turns'`, not `type='turn'`.
        assert_eq!(turn.attr("type"), Some(TYPE_TURNS));
        assert_ne!(turn.attr("type"), Some("turn"));
    }

    #[test]
    fn services_result_wrapper_carries_turns_child_verbatim() {
        let host = TurnHost::new("turn.waddle.social");
        let cred = fixture_credential();
        let services = build_services(&host, 443, 3478, &cred);
        let wrapper = build_services_result_element(None, services);

        assert_eq!(wrapper.name(), "services");
        assert_eq!(wrapper.ns(), NS_EXT_DISCO);
        let mut children = wrapper.children().filter(|c| c.is("service", NS_EXT_DISCO));
        let turn = children.next().expect("turn child present");
        assert_eq!(turn.attr("type"), Some("turn"));
        let turns = children.next().expect("turns child present");
        assert_eq!(turns.attr("type"), Some(TYPE_TURNS));
        let stun = children.next().expect("stun child present");
        assert_eq!(stun.attr("type"), Some("stun"));
        assert!(children.next().is_none());
    }
}
