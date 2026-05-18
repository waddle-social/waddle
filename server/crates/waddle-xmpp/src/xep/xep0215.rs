//! XEP-0215: External Service Discovery.
//!
//! The server exposes the LiveKit-fronted TURN/STUN service via the
//! `urn:xmpp:extdisco:2` namespace. Credentials are minted from the
//! [`waddle_sfu::SfuService`] and carry an `expires` attribute so
//! clients refresh before they go stale.
//!
//! Thin wrapper over [`xmpp_parsers::extdisco`] — namespace constant
//! and Waddle-specific builder helpers that translate a
//! [`waddle_sfu::TurnCredential`] into the [`Service`] entries the
//! responder sends back on a `services` IQ.
//!
//! xmpp-parsers' [`Type`] enum is the subset {Stun, Turn}; the
//! XEP-0215 §3.6.5 `stuns`/`turns` variants are conveyed implicitly
//! by emitting a [`Type::Turn`] entry on the TLS port with
//! [`Transport::Tcp`]. Pragmatic given Waddle terminates TLS at the
//! `tlsroute` and forwards plain TCP to coturn behind it.

pub use xmpp_parsers::extdisco::{
    Action, Credentials, Restricted, Service, ServicesQuery, ServicesResult, Transport,
    Type as ServiceType,
};

use waddle_sfu::{TurnCredential, TurnHost};

pub const NS_EXT_DISCO: &str = "urn:xmpp:extdisco:2";

/// Build the pair of `<service/>` entries Waddle advertises: one
/// `turn` (TURN-over-TLS on port 443, matching the cluster's
/// `tlsroute` for `turn.waddle.social`) and one plain `stun` on the
/// UDP port. The TURN entry carries the time-limited credential
/// pair; STUN does not need credentials.
pub fn build_services(
    turn_host: &TurnHost,
    turn_tls_port: u16,
    stun_udp_port: u16,
    cred: &TurnCredential,
) -> Vec<Service> {
    vec![
        build_turn_service(turn_host, turn_tls_port, cred),
        build_stun_service(turn_host, stun_udp_port),
    ]
}

/// XEP-0215 TURN entry with HMAC-derived credentials. Split out so
/// the `type='stun'`-filtered request path can skip the TURN mint
/// entirely.
pub fn build_turn_service(
    turn_host: &TurnHost,
    turn_tls_port: u16,
    cred: &TurnCredential,
) -> Service {
    Service {
        action: Action::Add,
        type_: ServiceType::Turn,
        transport: Some(Transport::Tcp),
        host: turn_host.as_str().to_string(),
        port: Some(turn_tls_port),
        username: Some(cred.username.as_str().to_string()),
        password: Some(cred.password.as_str().to_string()),
        expires: Some(xmpp_parsers::date::DateTime(cred.expires_at.fixed_offset())),
        name: None,
        restricted: Restricted::True,
        ext_info: Vec::new(),
    }
}

/// XEP-0215 STUN entry. No credentials, so it's cheap to build and
/// safe to serve to anyone who can already authenticate to the XMPP
/// stream.
pub fn build_stun_service(turn_host: &TurnHost, stun_udp_port: u16) -> Service {
    Service {
        action: Action::Add,
        type_: ServiceType::Stun,
        transport: Some(Transport::Udp),
        host: turn_host.as_str().to_string(),
        port: Some(stun_udp_port),
        username: None,
        password: None,
        expires: None,
        name: None,
        restricted: Restricted::False,
        ext_info: Vec::new(),
    }
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
    fn build_services_emits_turn_and_stun_entries() {
        let host = TurnHost::new("turn.waddle.social");
        let cred = fixture_credential();
        let services = build_services(&host, 443, 3478, &cred);

        assert_eq!(services.len(), 2);

        let turns = &services[0];
        assert_eq!(turns.type_, ServiceType::Turn);
        assert_eq!(turns.transport, Some(Transport::Tcp));
        assert_eq!(turns.host, "turn.waddle.social");
        assert_eq!(turns.port, Some(443));
        assert!(turns.username.is_some());
        assert!(turns.password.is_some());
        assert!(turns.expires.is_some());
        assert_eq!(turns.restricted, Restricted::True);

        let stun = &services[1];
        assert_eq!(stun.type_, ServiceType::Stun);
        assert_eq!(stun.transport, Some(Transport::Udp));
        assert!(stun.username.is_none());
        assert!(stun.password.is_none());
        assert_eq!(stun.restricted, Restricted::False);
    }

    #[test]
    fn turn_entry_round_trips_through_xml() {
        let host = TurnHost::new("turn.waddle.social");
        let cred = fixture_credential();
        let mut services = build_services(&host, 443, 3478, &cred);
        let turn = services.remove(0);

        let elem: minidom::Element = turn.into();
        assert_eq!(elem.name(), "service");
        assert_eq!(elem.ns(), NS_EXT_DISCO);
        let reparsed = Service::try_from(elem).expect("reparses");
        assert_eq!(reparsed.type_, ServiceType::Turn);
        assert_eq!(reparsed.host, "turn.waddle.social");
        assert_eq!(reparsed.port, Some(443));
    }
}
