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
//! responder sends back on a `services` IQ.
//!
//! xmpp-parsers' [`ServiceType`] enum is the subset {Stun, Turn}; the
//! XEP-0215 §3.6.5 `stuns`/`turns` variants do not exist in the
//! typed surface. Because Waddle advertises TLS-protected TURN
//! (terminated at the cluster `tlsroute` for `turn.waddle.social`),
//! the TURN entry MUST go out on the wire with `type='turns'` per
//! the XEP, so we build that `<service/>` element manually via
//! [`minidom::Element::builder`] rather than going through the
//! typed [`Service`] enum.

pub use xmpp_parsers::extdisco::{
    Action, Credentials, Service, ServicesQuery, ServicesResult, Transport, Type as ServiceType,
};

use minidom::Element;

use waddle_sfu::{TurnCredential, TurnHost};

pub const NS_EXT_DISCO: &str = "urn:xmpp:extdisco:2";

/// Wire-form `type` value for TLS-protected TURN per XEP-0215
/// §3.6.5. Modelled as a constant — and not an enum variant — so
/// the (untyped) wire shape is the single source of truth.
pub const TYPE_TURNS: &str = "turns";

/// Build the pair of `<service/>` entries Waddle advertises: one
/// `turns` (TURN-over-TLS on port 443, matching the cluster's
/// `tlsroute` for `turn.waddle.social`) and one plain `stun` on the
/// UDP port. The TURN entry carries the time-limited credential
/// pair; STUN does not need credentials.
pub fn build_services(
    turn_host: &TurnHost,
    turn_tls_port: u16,
    stun_udp_port: u16,
    cred: &TurnCredential,
) -> Vec<Element> {
    vec![
        build_turn_service(turn_host, turn_tls_port, cred),
        build_stun_service_element(turn_host, stun_udp_port),
    ]
}

/// XEP-0215 §3.6.5 TLS-protected TURN entry with HMAC-derived
/// credentials. Returned as a [`minidom::Element`] because the
/// `xmpp_parsers::extdisco::Type` enum lacks a `Turns` variant —
/// emitting the literal `type="turns"` attribute string is the only
/// XEP-conformant wire shape here.
pub fn build_turn_service(
    turn_host: &TurnHost,
    turn_tls_port: u16,
    cred: &TurnCredential,
) -> Element {
    Element::builder("service", NS_EXT_DISCO)
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "add")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), TYPE_TURNS)
        .attr(minidom::rxml::xml_ncname!("transport").to_owned(), "tcp")
        .attr(
            minidom::rxml::xml_ncname!("host").to_owned(),
            turn_host.as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("port").to_owned(),
            turn_tls_port.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("username").to_owned(),
            cred.username.as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("password").to_owned(),
            cred.password.as_str(),
        )
        // XEP-0082 / XEP-0215 §3.6.5: `expires` is a RFC 3339
        // timestamp. Render via chrono directly — same wire shape
        // `xmpp_parsers::date::DateTime` would emit via its
        // `IntoAttributeValue` impl.
        .attr(
            minidom::rxml::xml_ncname!("expires").to_owned(),
            cred.expires_at.fixed_offset().to_rfc3339(),
        )
        .attr(minidom::rxml::xml_ncname!("restricted").to_owned(), "1")
        .build()
}

/// XEP-0215 STUN entry. No credentials, so it's cheap to build and
/// safe to serve to anyone who can already authenticate to the XMPP
/// stream.
///
/// Emitted as a [`minidom::Element`] directly because the typed
/// [`Service`] struct's fields are private in xmpp-parsers 0.22 and
/// the crate exposes no constructor; building the wire shape with
/// [`Element::builder`] is the only conformant path. (The TURN entry
/// already uses the same builder for the same reason — there is no
/// `Type::Turns` variant to construct.)
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
/// `type` attribute pinning the filter that produced the result
/// (XEP-0215 §3.6.1). Built manually rather than via
/// [`ServicesResult`] so the result can hold the manually-built
/// `turns` child — which the typed `ServicesResult.services:
/// Vec<Service>` cannot represent.
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
            ws_url: WebsocketUrl::new("wss://h/".parse().unwrap()).unwrap(),
            turn_host: TurnHost::new("turn.waddle.social"),
            turn_tls_port: 443,
            turn_udp_port: 3478,
            turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
            token_ttl: Duration::seconds(60),
            turn_ttl: Duration::seconds(60),
        };
        let sfu = LiveKitSfu::new(cfg);
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

        assert_eq!(services.len(), 2);

        let turns = &services[0];
        assert_eq!(turns.name(), "service");
        assert_eq!(turns.ns(), NS_EXT_DISCO);
        assert_eq!(
            turns.attr("type"),
            Some(TYPE_TURNS),
            "TLS TURN must be advertised as type='turns' per XEP-0215 §3.6.5"
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
        assert_eq!(turns.attr("restricted"), Some("1"));

        let stun = &services[1];
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
        let turn = build_turn_service(&host, 443, &cred);

        // XEP-0215 §3.6.5: TLS-protected TURN is `type='turns'`,
        // not `type='turn'`. This is the test that fails loudly
        // if someone reverts to the typed `ServiceType::Turn`
        // path (which would emit `type='turn'`).
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
        let turns = children.next().expect("turns child present");
        assert_eq!(turns.attr("type"), Some(TYPE_TURNS));
        let stun = children.next().expect("stun child present");
        assert_eq!(stun.attr("type"), Some("stun"));
        assert!(children.next().is_none());
    }
}
