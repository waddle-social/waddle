//! XEP-0215: External Service Discovery dedicated suite.
//!
//! Pins the wire shape Waddle advertises on `services` and
//! `credentials` IQs: `type='turn'` (UDP) and `type='turns'` (TLS)
//! entries, exact `type` filtering, and credential requests.

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
    build_services, build_services_result_element, build_stun_service_element, build_turns_service,
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
        ws_url: WebsocketUrl::try_new("wss://livekit.test/".parse().expect("valid url"))
            .expect("valid ws url"),
        turn_host: TurnHost::new("turn.waddle.social"),
        turn_tls_port: 443,
        turn_udp_port: 3478,
        turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
        token_ttl: Duration::seconds(3600),
        turn_ttl: Duration::seconds(3600),
    };
    let sfu = LiveKitSfu::try_new(cfg).expect("LiveKitSfu init in test");
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
        ws_url: WebsocketUrl::try_new("wss://livekit.test/".parse().expect("valid url"))
            .expect("valid ws url"),
        turn_host: TurnHost::new("turn.waddle.social"),
        turn_tls_port: 443,
        turn_udp_port: 3478,
        turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
        token_ttl: Duration::seconds(3600),
        turn_ttl: Duration::seconds(3600),
    };
    Arc::new(LiveKitSfu::try_new(cfg).expect("LiveKitSfu init in test"))
}

fn session_jid() -> jid::FullJid {
    "alice@waddle.social/desktop".parse().expect("valid jid")
}

fn stanza_ctx<'a>(jid: &'a jid::FullJid) -> StanzaContext<'a> {
    StanzaContext {
        participant_registration: None,
        domain: "waddle.social",
        full_jid: jid,
        occupant_session: None,
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
    let host = TurnHost::new("turn.waddle.social");
    let cred = fixture_cred();
    let turn = build_turns_service(&host, 443, &cred);

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
    let turn = build_turns_service(&host, 443, &cred);

    assert_eq!(turn.attr("action"), Some("add"));
    assert_eq!(turn.attr("transport"), Some("tcp"));
    assert_eq!(turn.attr("host"), Some("turn.waddle.social"));
    assert_eq!(turn.attr("port"), Some("443"));
    assert_eq!(turn.attr("username"), Some(cred.username.as_str()));
    assert_eq!(turn.attr("password"), Some(cred.password.as_str()));
    assert!(turn.attr("expires").is_some());
    assert!(turn.attr("restricted").is_none());
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
    assert_eq!(children.len(), 3);
    assert_eq!(children[0].attr("type"), Some("turn"));
    assert_eq!(children[1].attr("type"), Some(TYPE_TURNS));
    assert_eq!(children[2].attr("type"), Some("stun"));
}

#[test]
fn xep0215_services_wrapper_carries_optional_type_filter_attribute() {
    let host = TurnHost::new("turn.waddle.social");
    let cred = fixture_cred();
    let entries = build_services(&host, 443, 3478, &cred);
    let wrapper = build_services_result_element(Some("turn"), entries);
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

fn typed_get(id: &str, name: &str, type_attr: Option<&str>) -> Iq {
    let mut builder = Element::builder(name, NS_EXT_DISCO);
    if let Some(t) = type_attr {
        builder = builder.attr(minidom::rxml::xml_ncname!("type").to_owned(), t);
    }
    Iq::Get {
        from: Some("alice@waddle.social/desktop".parse().expect("valid jid")),
        to: Some("waddle.social".parse().expect("valid jid")),
        id: id.to_string(),
        payload: builder.build(),
    }
}

fn credentials_get(id: &str, services: &[(&str, &str, Option<&str>)]) -> Iq {
    let mut builder = Element::builder("credentials", NS_EXT_DISCO);
    for (host, ty, port) in services {
        let mut svc = Element::builder("service", NS_EXT_DISCO)
            .attr(minidom::rxml::xml_ncname!("host").to_owned(), *host)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), *ty);
        if let Some(port) = port {
            svc = svc.attr(minidom::rxml::xml_ncname!("port").to_owned(), *port);
        }
        builder = builder.append(svc.build());
    }
    Iq::Get {
        from: Some("alice@waddle.social/desktop".parse().expect("valid jid")),
        to: Some("waddle.social".parse().expect("valid jid")),
        id: id.to_string(),
        payload: builder.build(),
    }
}

fn reply_of(iq: &Iq) -> Iq {
    let jid = session_jid();
    let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
    let events = handler.handle(iq, &stanza_ctx(&jid));
    match events.into_iter().next() {
        Some(OutboundEvent::SendStanza(stanza)) => match *stanza {
            Stanza::Iq(reply) => *reply,
            other => panic!("expected IQ reply, got {other:?}"),
        },
        other => panic!("expected SendStanza, got {other:?}"),
    }
}

fn result_children(reply: Iq) -> (Element, Vec<Element>) {
    let Iq::Result {
        payload: Some(payload),
        ..
    } = reply
    else {
        panic!("expected result reply, got {reply:?}")
    };
    let children = payload
        .children()
        .filter(|c| c.is("service", NS_EXT_DISCO))
        .cloned()
        .collect();
    (payload, children)
}

fn error_condition(reply: Iq) -> DefinedCondition {
    let Iq::Error { error, .. } = reply else {
        panic!("expected error reply, got {reply:?}")
    };
    error.defined_condition
}

#[test]
fn xep0215_type_turn_filter_returns_only_turn_entries() {
    let (wrapper, entries) = result_children(reply_of(&typed_get("t1", "services", Some("turn"))));
    assert_eq!(wrapper.attr("type"), Some("turn"));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].attr("type"), Some("turn"));
    assert_eq!(entries[0].attr("transport"), Some("udp"));
    assert_eq!(entries[0].attr("port"), Some("3478"));
    assert!(entries[0].attr("username").is_some());
    assert!(entries[0].attr("password").is_some());
}

#[test]
fn xep0215_type_turns_filter_returns_only_turns_entries() {
    let (wrapper, entries) = result_children(reply_of(&typed_get("t2", "services", Some("turns"))));
    assert_eq!(wrapper.attr("type"), Some("turns"));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].attr("type"), Some(TYPE_TURNS));
    assert_eq!(entries[0].attr("transport"), Some("tcp"));
    assert_eq!(entries[0].attr("port"), Some("443"));
}

#[test]
fn xep0215_type_stun_filter_returns_only_stun_entries() {
    let (_, entries) = result_children(reply_of(&typed_get("t3", "services", Some("stun"))));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].attr("type"), Some("stun"));
}

#[test]
fn xep0215_type_stuns_filter_is_accepted_and_empty() {
    let (wrapper, entries) = result_children(reply_of(&typed_get("t4", "services", Some("stuns"))));
    assert_eq!(wrapper.attr("type"), Some("stuns"));
    assert!(entries.is_empty());
}

#[test]
fn xep0215_unfiltered_services_returns_every_type() {
    let (_, entries) = result_children(reply_of(&typed_get("t5", "services", None)));
    let types: Vec<_> = entries.iter().filter_map(|e| e.attr("type")).collect();
    assert_eq!(types, ["turn", "turns", "stun"]);
}

#[test]
fn xep0215_unknown_type_filter_is_bad_request() {
    let cond = error_condition(reply_of(&typed_get("t6", "services", Some("ftp"))));
    assert_eq!(cond, DefinedCondition::BadRequest);
}

#[test]
fn xep0215_credentials_request_returns_credentials_for_configured_turn() {
    let reply = reply_of(&credentials_get(
        "c1",
        &[("turn.waddle.social", "turn", None)],
    ));
    let (payload, entries) = result_children(reply);
    assert_eq!(payload.name(), "credentials");
    assert_eq!(payload.ns(), NS_EXT_DISCO);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].attr("host"), Some("turn.waddle.social"));
    assert_eq!(entries[0].attr("type"), Some("turn"));
    assert_eq!(entries[0].attr("port"), Some("3478"));
    assert!(entries[0].attr("username").is_some());
    assert!(entries[0].attr("password").is_some());
}

#[test]
fn xep0215_credentials_request_port_selects_the_matching_service() {
    let reply = reply_of(&credentials_get(
        "c2",
        &[("turn.waddle.social", "turns", Some("443"))],
    ));
    let (_, entries) = result_children(reply);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].attr("type"), Some(TYPE_TURNS));
    assert_eq!(entries[0].attr("port"), Some("443"));

    let cond = error_condition(reply_of(&credentials_get(
        "c3",
        &[("turn.waddle.social", "turns", Some("9999"))],
    )));
    assert_eq!(cond, DefinedCondition::ItemNotFound);
}

#[test]
fn xep0215_credentials_request_for_unknown_service_is_item_not_found() {
    for services in [
        &[("other.example", "turn", None)][..],
        &[("turn.waddle.social", "stun", None)][..],
        &[("turn.waddle.social", "ftp", None)][..],
    ] {
        let cond = error_condition(reply_of(&credentials_get("c4", services)));
        assert_eq!(cond, DefinedCondition::ItemNotFound);
    }
}

#[test]
fn xep0215_credentials_request_requires_host_and_type() {
    let mut iq = credentials_get("c5", &[("turn.waddle.social", "turn", None)]);
    if let Iq::Get { payload, .. } = &mut iq {
        let svc = Element::builder("service", NS_EXT_DISCO)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "turn")
            .build();
        *payload = Element::builder("credentials", NS_EXT_DISCO)
            .append(svc)
            .build();
    }
    assert_eq!(error_condition(reply_of(&iq)), DefinedCondition::BadRequest);

    let empty = credentials_get("c6", &[]);
    assert_eq!(
        error_condition(reply_of(&empty)),
        DefinedCondition::BadRequest
    );
}

#[test]
fn xep0215_credentials_request_shares_the_turn_rate_limit() {
    let jid = session_jid();
    let handler = ExtDiscoHandler::with_turn_credential_rate_limit(
        fixture_sfu(),
        443,
        3478,
        Arc::new(TurnCredentialRateLimit::new(1, StdDuration::from_secs(30))),
    );
    let iq = credentials_get("c7", &[("turn.waddle.social", "turn", None)]);
    let first = handler.handle(&iq, &stanza_ctx(&jid));
    assert!(matches!(
        &first[0],
        OutboundEvent::SendStanza(s) if matches!(s.as_ref(), Stanza::Iq(r) if matches!(&**r, Iq::Result { .. }))
    ));
    let second = handler.handle(&iq, &stanza_ctx(&jid));
    let OutboundEvent::SendStanza(stanza) = &second[0] else {
        panic!("expected SendStanza")
    };
    let Stanza::Iq(reply) = stanza.as_ref() else {
        panic!("expected IQ")
    };
    let Iq::Error { error, .. } = &**reply else {
        panic!("expected error")
    };
    assert_eq!(error.defined_condition, DefinedCondition::PolicyViolation);
}
