//! XEP-0215: External Service Discovery dedicated suite.
//!
//! Pins the wire shape Waddle advertises on a `services` IQ — most
//! importantly that TLS-protected TURN goes out as `type='turns'`
//! per §3.6.5, not the typed-surface `type='turn'` that xmpp-parsers
//! defaults to. The handler builds the `<services/>` wrapper
//! manually for exactly this reason; the tests below are the safety
//! net against an accidental reversion to `ServicesResult` + typed
//! `Vec<Service>`.

use chrono::Duration;
use minidom::Element;
use waddle_sfu::{
    ApiKey, ApiSecret, Identity, LiveKitSfu, SfuConfig, SfuService, TurnCredential, TurnHost,
    TurnSharedSecret, WebsocketUrl,
};
use waddle_xmpp::xep::xep0215::{
    build_services, build_services_result_element, build_stun_service_element, build_turn_service,
    NS_EXT_DISCO, TYPE_TURNS,
};

fn fixture_cred() -> TurnCredential {
    let cfg = SfuConfig {
        api_key: ApiKey::new("APIxxxxxxxx"),
        api_secret: ApiSecret::from_text("api-secret-meets-min-length-32!!")
            .expect("test secret meets min length"),
        webhook_secret: ApiSecret::from_text("api-secret-meets-min-length-32!!")
            .expect("test secret meets min length"),
        ws_url: WebsocketUrl::new("wss://livekit.test/".parse().expect("valid url"))
            .expect("valid ws url"),
        turn_host: TurnHost::new("turn.waddle.social"),
        turn_tls_port: 443,
        turn_udp_port: 3478,
        turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
        token_ttl: Duration::seconds(3600),
        turn_ttl: Duration::seconds(3600),
    };
    let sfu = LiveKitSfu::new(cfg);
    let jid: jid::FullJid = "alice@waddle.social/desktop".parse().expect("valid jid");
    let identity = Identity::from_jid(jid);
    sfu.issue_turn_credentials(&identity)
        .expect("credential mint")
}

#[test]
fn xep0215_tls_turn_is_advertised_as_turns() {
    // The single most important wire-shape assertion: §3.6.5 says
    // TLS-protected TURN uses `type='turns'`. xmpp-parsers' typed
    // `ServiceType` only carries `Stun`/`Turn`, so the
    // `build_turn_service` helper hand-builds the element to emit
    // the literal `turns` value.
    let host = TurnHost::new("turn.waddle.social");
    let cred = fixture_cred();
    let turn = build_turn_service(&host, 443, &cred);

    assert_eq!(turn.name(), "service");
    assert_eq!(turn.ns(), NS_EXT_DISCO);
    assert_eq!(turn.attr("type"), Some(TYPE_TURNS));
    assert_ne!(
        turn.attr("type"),
        Some("turn"),
        "TLS-protected TURN must not regress to the plain `turn` wire value"
    );
}

#[test]
fn xep0215_turns_entry_carries_required_attributes() {
    let host = TurnHost::new("turn.waddle.social");
    let cred = fixture_cred();
    let turn = build_turn_service(&host, 443, &cred);

    assert_eq!(turn.attr("action"), Some("add"));
    assert_eq!(turn.attr("transport"), Some("tcp"));
    assert_eq!(turn.attr("host"), Some("turn.waddle.social"));
    assert_eq!(turn.attr("port"), Some("443"));
    assert_eq!(turn.attr("username"), Some(cred.username.as_str()));
    assert_eq!(turn.attr("password"), Some(cred.password.as_str()));
    assert!(turn.attr("expires").is_some());
    assert_eq!(turn.attr("restricted"), Some("1"));
}

#[test]
fn xep0215_stun_entry_uses_typed_surface() {
    // STUN does have a typed `Type::Stun` variant, so the helper
    // goes through the xmpp-parsers `Service` -> `Element`
    // conversion. Wire shape stays `type='stun'`.
    let host = TurnHost::new("turn.waddle.social");
    let stun = build_stun_service_element(&host, 3478);
    assert_eq!(stun.attr("type"), Some("stun"));
    assert_eq!(stun.attr("transport"), Some("udp"));
    assert!(stun.attr("username").is_none());
    assert!(stun.attr("password").is_none());
}

#[test]
fn xep0215_services_wrapper_preserves_turns_child() {
    let host = TurnHost::new("turn.waddle.social");
    let cred = fixture_cred();
    let entries = build_services(&host, 443, 3478, &cred);
    let wrapper = build_services_result_element(None, entries);

    assert_eq!(wrapper.name(), "services");
    assert_eq!(wrapper.ns(), NS_EXT_DISCO);
    let children: Vec<&Element> = wrapper
        .children()
        .filter(|c| c.is("service", NS_EXT_DISCO))
        .collect();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].attr("type"), Some(TYPE_TURNS));
    assert_eq!(children[1].attr("type"), Some("stun"));
}

#[test]
fn xep0215_services_wrapper_carries_optional_type_filter_attribute() {
    let host = TurnHost::new("turn.waddle.social");
    let cred = fixture_cred();
    let entries = build_services(&host, 443, 3478, &cred);
    let wrapper = build_services_result_element(Some("turn"), entries);
    // The filter on the request is the typed `turn` value; the
    // response wrapper echoes it. Only the inner `<service/>`
    // child uses `turns` for the TLS-protected service.
    assert_eq!(wrapper.attr("type"), Some("turn"));
}
