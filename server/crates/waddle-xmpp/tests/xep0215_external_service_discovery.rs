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
use std::sync::Arc;
use std::time::Duration as StdDuration;
use waddle_sfu::{
    ApiKey, ApiSecret, Identity, LiveKitSfu, SfuConfig, SfuService, TurnCredential, TurnHost,
    TurnSharedSecret, WebsocketUrl,
};
use waddle_xmpp::protocol::event::{OutboundEvent, StanzaContext};
use waddle_xmpp::protocol::handlers::extdisco::ExtDiscoHandler;
use waddle_xmpp::protocol::handlers::session_initiate_rate_limit::TurnCredentialRateLimit;
use waddle_xmpp::protocol::IqHandler;
use waddle_xmpp::xep::xep0215::{
    build_services, build_services_result_element, build_stun_service_element, build_turn_service,
    NS_EXT_DISCO, TYPE_TURNS,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

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
    let sfu = LiveKitSfu::new(cfg).expect("LiveKitSfu init in test");
    let jid: jid::FullJid = "alice@waddle.social/desktop".parse().expect("valid jid");
    let identity = Identity::from_jid(jid);
    sfu.issue_turn_credentials(&identity)
        .expect("credential mint")
}

fn fixture_sfu() -> Arc<dyn SfuService> {
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
    Arc::new(LiveKitSfu::new(cfg).expect("LiveKitSfu init in test"))
}

fn session_jid() -> jid::FullJid {
    "alice@waddle.social/desktop".parse().expect("valid jid")
}

fn stanza_ctx<'a>(jid: &'a jid::FullJid) -> StanzaContext<'a> {
    StanzaContext {
        domain: "waddle.social",
        full_jid: jid,
        media_capabilities: None,
    }
}

fn services_get_iq(id: &str) -> Iq {
    Iq::Get {
        from: Some("alice@waddle.social/desktop".parse().expect("valid jid")),
        to: Some("waddle.social".parse().expect("valid jid")),
        id: id.to_string(),
        payload: Element::builder("services", NS_EXT_DISCO).build(),
    }
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

#[test]
fn xep0215_turn_credential_budget_exhaustion_returns_policy_violation() {
    let jid = session_jid();
    let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);

    for attempt in 0..10 {
        let events = handler.handle(
            &services_get_iq(&format!("within-budget-{attempt}")),
            &stanza_ctx(&jid),
        );
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    assert!(
                        matches!(&**reply, Iq::Result { .. }),
                        "attempt {attempt} should stay within the default budget"
                    );
                }
                other => panic!("expected IQ reply, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    let events = handler.handle(&services_get_iq("over-budget"), &stanza_ctx(&jid));
    match &events[0] {
        OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
            Stanza::Iq(reply) => {
                let Iq::Error { error, .. } = &**reply else {
                    panic!("expected typed error reply")
                };
                assert_eq!(error.type_, ErrorType::Cancel);
                assert_eq!(error.defined_condition, DefinedCondition::PolicyViolation);
            }
            other => panic!("expected IQ reply, got {other:?}"),
        },
        other => panic!("expected SendStanza, got {other:?}"),
    }
}

#[test]
fn xep0215_turn_credential_budget_recovers_after_window() {
    let jid = session_jid();
    let handler = ExtDiscoHandler::with_turn_credential_rate_limit(
        fixture_sfu(),
        443,
        3478,
        Arc::new(TurnCredentialRateLimit::new(
            1,
            StdDuration::from_millis(20),
        )),
    );

    let first = handler.handle(&services_get_iq("first"), &stanza_ctx(&jid));
    match &first[0] {
        OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
            Stanza::Iq(reply) => {
                assert!(matches!(&**reply, Iq::Result { .. }));
            }
            other => panic!("expected IQ reply, got {other:?}"),
        },
        other => panic!("expected SendStanza, got {other:?}"),
    }

    let rejected = handler.handle(&services_get_iq("rejected"), &stanza_ctx(&jid));
    match &rejected[0] {
        OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
            Stanza::Iq(reply) => {
                let Iq::Error { error, .. } = &**reply else {
                    panic!("expected typed error reply")
                };
                assert_eq!(error.defined_condition, DefinedCondition::PolicyViolation);
            }
            other => panic!("expected IQ reply, got {other:?}"),
        },
        other => panic!("expected SendStanza, got {other:?}"),
    }

    std::thread::sleep(StdDuration::from_millis(50));

    let recovered = handler.handle(&services_get_iq("recovered"), &stanza_ctx(&jid));
    match &recovered[0] {
        OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
            Stanza::Iq(reply) => {
                assert!(
                    matches!(&**reply, Iq::Result { .. }),
                    "request after the limiter window should succeed again"
                );
            }
            other => panic!("expected IQ reply, got {other:?}"),
        },
        other => panic!("expected SendStanza, got {other:?}"),
    }
}
