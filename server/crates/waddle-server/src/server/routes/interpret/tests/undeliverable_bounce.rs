//! Undeliverable 1:1 call-IQ bounces at the routing layer
//! (#1444/#1607): RFC 6120 §8.3.1 sanitized echo, targeted
//! token-issuance rollback, the session-terminate result-ack, and the
//! no-bounce rule for result/error IQs.

use super::*;

fn sfu_fixture_for_route_test() -> Arc<dyn waddle_sfu::SfuService> {
    let cfg = waddle_sfu::SfuConfig {
        api_key: waddle_sfu::ApiKey::new("APIxxxxxxxx"),
        api_secret: waddle_sfu::ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length"),
        webhook_secret: waddle_sfu::ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length"),
        ws_url: waddle_sfu::WebsocketUrl::new("wss://livekit.test/".parse().expect("url"))
            .expect("ws url"),
        turn_host: waddle_sfu::TurnHost::new("turn.test"),
        turn_tls_port: 443,
        turn_udp_port: 3478,
        turn_shared_secret: waddle_sfu::TurnSharedSecret::from_text("turn-secret"),
        token_ttl: chrono::Duration::seconds(3600),
        turn_ttl: chrono::Duration::seconds(3600),
    };
    Arc::new(waddle_sfu::LiveKitSfu::new(cfg).expect("LiveKitSfu init in test"))
}

fn jingle_payload_for_route_test(action: &str, sid: &str) -> Element {
    Element::builder("jingle", waddle_xmpp::xep::xep0166::NS_JINGLE)
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), action)
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid)
        .build()
}

fn call_iq_set_for_route_test(id: &str, from: &jid::FullJid, to: &jid::FullJid) -> Iq {
    Iq::Set {
        from: Some(jid::Jid::from(from.clone())),
        to: Some(jid::Jid::from(to.clone())),
        id: id.to_string(),
        payload: jingle_payload_for_route_test("session-info", "offline-sid"),
    }
}

#[tokio::test]
async fn route_to_connection_offline_full_jid_call_iq_returns_service_unavailable() {
    use waddle_xmpp::registry::UserRegistryActor;
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: jid::FullJid = "bob@example.com/phone".parse().expect("bob jid");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Iq(Box::new(call_iq_set_for_route_test(
            "call-offline-1",
            &alice,
            &bob,
        )))),
        call_setup: None,
    }];

    let outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert_eq!(
        outcome.frames.len(),
        1,
        "offline full-JID request IQ should produce one error frame: {:?}",
        outcome.frames
    );
    let element = Element::from_str(&outcome.frames[0]).expect("parseable IQ error");
    let iq = Iq::try_from(element).expect("typed IQ");
    let Iq::Error {
        from,
        to,
        id,
        error,
        payload,
    } = iq
    else {
        panic!("expected IQ error, got {iq:?}");
    };
    assert_eq!(id, "call-offline-1");
    assert_eq!(from, Some(jid::Jid::from(bob)));
    assert_eq!(to, Some(jid::Jid::from(alice)));
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ServiceUnavailable
    );
    // #1444: the RFC 6120 §8.3.1 echo survives for Jingle, but
    // SANITIZED — a credential-free request comes back verbatim.
    let echoed = payload.expect("service-unavailable echoes the sanitized payload");
    assert_eq!(echoed.name(), "jingle");
    assert_eq!(echoed.attr("sid"), Some("offline-sid"));
}

#[tokio::test]
async fn route_to_connection_offline_session_initiate_bounce_carries_no_credentials_and_revokes_token(
) {
    use waddle_sfu::{MediaCapabilities, SfuService};
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::xep::xep_waddle_livekit_transport::{
        NS_WADDLE_LIVEKIT_TRANSPORT, TRANSPORT_NAME,
    };

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: jid::FullJid = "bob@example.com/phone".parse().expect("bob jid");
    let sfu: Arc<dyn SfuService> = sfu_fixture_for_route_test();

    // Mirror what the Jingle handler did before routing: mint the
    // callee's join token and register both participants. An EARLIER
    // issuance for the same pair (an independent, successful
    // negotiation) must survive the bounce untouched.
    let call_id =
        waddle_sfu::CallId::new("alice@example.com::offline-sid".to_string()).expect("call id");
    let bob_identity = waddle_sfu::Identity::from_jid(bob.clone());
    let earlier_token = sfu
        .issue_join_token(
            &call_id,
            &bob_identity,
            MediaCapabilities::direct_call_peer(),
        )
        .expect("mint earlier");
    let token = sfu
        .issue_join_token(
            &call_id,
            &bob_identity,
            MediaCapabilities::direct_call_peer(),
        )
        .expect("mint");
    sfu.register_call_participant(&call_id, &waddle_sfu::Identity::from_jid(alice.clone()));
    sfu.register_call_participant(&call_id, &bob_identity);

    // The forwarded session-initiate carrying the minted credential.
    let jingle = Element::builder("jingle", waddle_xmpp::xep::xep0166::NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-initiate",
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), "offline-sid")
        .append(
            Element::builder("content", waddle_xmpp::xep::xep0166::NS_JINGLE)
                .attr(
                    minidom::rxml::xml_ncname!("creator").to_owned(),
                    "initiator",
                )
                .attr(minidom::rxml::xml_ncname!("name").to_owned(), "0")
                .append(
                    // The REAL issued wire shape (url/room/identity
                    // attrs + <token/> child), exactly as the Jingle
                    // handler injects it.
                    waddle_xmpp::xep::xep_waddle_livekit_transport::WaddleLiveKitTransport::from_join_token(token.clone())
                        .to_element(),
                )
                .build(),
        )
        .build();
    let initiate = Iq::Set {
        from: Some(jid::Jid::from(alice.clone())),
        to: Some(jid::Jid::from(bob.clone())),
        id: "call-offline-2".to_string(),
        payload: jingle,
    };
    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Iq(Box::new(initiate))),
        call_setup: None,
    }];

    let mut deps = Deps::registry_with_user_registry(&registry, &user_registry);
    deps.sfu = Some(sfu.as_ref());
    let outcome = interpret(events, &deps).await;

    // The bounce echoes a SANITIZED payload: the sender's own request
    // minus the server-injected LiveKit transport — no credential
    // material anywhere in the frame.
    assert_eq!(
        outcome.frames.len(),
        1,
        "one error frame: {:?}",
        outcome.frames
    );
    let bounced = Iq::try_from(Element::from_str(&outcome.frames[0]).expect("parseable IQ"))
        .expect("typed IQ");
    let Iq::Error { payload, .. } = bounced else {
        panic!("expected IQ error, got {bounced:?}");
    };
    let echoed = payload.expect("sanitized echo present");
    assert_eq!(echoed.name(), "jingle");
    let has_livekit_transport = echoed
        .children()
        .flat_map(|content| content.children())
        .any(|elem| elem.is(TRANSPORT_NAME, NS_WADDLE_LIVEKIT_TRANSPORT));
    assert!(
        !has_livekit_transport,
        "sanitized echo must not carry the LiveKit transport: {echoed:?}"
    );
    assert!(
        !outcome.frames[0].contains(token.jwt.as_str()),
        "bounced error must not contain the minted JWT"
    );

    // Targeted compensation (#1607 review): exactly the issuance the
    // bounced stanza carried is revoked — the pair's earlier token and
    // its registration (a possibly live session from an independent
    // negotiation) survive.
    assert!(
        sfu.is_revoked(&token.jti),
        "undelivered invite must revoke the freshly minted JTI"
    );
    assert!(
        !sfu.is_revoked(&earlier_token.jti),
        "the pair's earlier issuance must survive the bounce"
    );
    assert!(
        sfu.has_call_participant(&call_id, &bob_identity),
        "the bounce must not unregister the participant"
    );
}

#[tokio::test]
async fn route_to_connection_offline_full_jid_session_terminate_is_acked() {
    // #1130 + #1131 interaction: a session-terminate forwarded to a peer
    // whose resource is already gone is a *successful* hangup — the caller
    // must get an empty <iq type='result'/> ack, never <service-unavailable/>.
    use waddle_xmpp::registry::UserRegistryActor;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: jid::FullJid = "bob@example.com/phone".parse().expect("bob jid");
    let terminate = Iq::Set {
        from: Some(jid::Jid::from(alice.clone())),
        to: Some(jid::Jid::from(bob.clone())),
        id: "term-offline-1".to_string(),
        payload: jingle_payload_for_route_test("session-terminate", "offline-sid"),
    };
    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Iq(Box::new(terminate))),
        call_setup: None,
    }];

    let outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert_eq!(
        outcome.frames.len(),
        1,
        "an undeliverable session-terminate should be acked, not dropped: {:?}",
        outcome.frames
    );
    let iq = Iq::try_from(Element::from_str(&outcome.frames[0]).expect("parseable IQ"))
        .expect("typed IQ");
    let Iq::Result {
        from,
        to,
        id,
        payload,
    } = iq
    else {
        panic!("expected empty IQ result ack, got {iq:?}");
    };
    assert_eq!(id, "term-offline-1");
    assert_eq!(from, Some(jid::Jid::from(bob)));
    assert_eq!(to, Some(jid::Jid::from(alice)));
    assert!(payload.is_none(), "terminate ack carries no payload");
}

#[tokio::test]
async fn route_to_connection_offline_full_jid_call_iq_result_error_do_not_bounce() {
    use waddle_xmpp::registry::UserRegistryActor;
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: jid::FullJid = "bob@example.com/phone".parse().expect("bob jid");
    let result_iq = Iq::Result {
        from: Some(jid::Jid::from(alice.clone())),
        to: Some(jid::Jid::from(bob.clone())),
        id: "call-result-1".to_string(),
        payload: None,
    };
    let error_iq = Iq::Error {
        from: Some(jid::Jid::from(alice)),
        to: Some(jid::Jid::from(bob.clone())),
        id: "call-error-1".to_string(),
        error: StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::NotAllowed,
            "en",
            "already failed",
        ),
        payload: None,
    };
    let events = vec![
        OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(bob.clone()),
            stanza: Box::new(Stanza::Iq(Box::new(result_iq))),
            call_setup: None,
        },
        OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(bob),
            stanza: Box::new(Stanza::Iq(Box::new(error_iq))),
            call_setup: None,
        },
    ];

    let outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert!(
        outcome.frames.is_empty(),
        "IQ result/error stanzas must not receive synthesized service-unavailable bounces: {:?}",
        outcome.frames
    );
}

// -----------------------------------------------------------------
// #1488 — route-disposition call-setup accounting. The Jingle handler
// counts `attempted` only and attaches a `PendingCallSetupRoute`
// ticket to the routing effect; the interpreter closes the attempt
// from the actual delivery outcome. Asserted through the in-memory
// reader seam, never instrument internals.
// -----------------------------------------------------------------

fn session_initiate_iq_for_route_test(id: &str, from: &jid::FullJid, to: &jid::FullJid) -> Iq {
    Iq::Set {
        from: Some(jid::Jid::from(from.clone())),
        to: Some(jid::Jid::from(to.clone())),
        id: id.to_string(),
        payload: jingle_payload_for_route_test("session-initiate", "disposition-sid"),
    }
}

#[tokio::test]
async fn unroutable_session_initiate_counts_peer_unavailable_not_ok() {
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::telemetry::call::PendingCallSetupRoute;

    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: jid::FullJid = "bob@example.com/phone".parse().expect("bob jid");
    // Bob has no live channel and no detached session: confirmed
    // offline, so the caller gets the undeliverable bounce.
    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Iq(Box::new(session_initiate_iq_for_route_test(
            "call-unroutable-1",
            &alice,
            &bob,
        )))),
        call_setup: Some(PendingCallSetupRoute),
    }];

    let outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert_eq!(
        outcome.frames.len(),
        1,
        "unroutable invite must still bounce to the caller: {:?}",
        outcome.frames
    );
    assert_eq!(
        metrics.counter_sum(
            "waddle.call.setup.failed",
            &[("reason", "peer_unavailable")]
        ),
        Some(1)
    );
    assert_eq!(
        metrics
            .counter_sum("waddle.call.setup.ok", &[])
            .unwrap_or(0),
        0
    );
}

#[tokio::test]
async fn delivered_session_initiate_counts_ok() {
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::telemetry::call::PendingCallSetupRoute;

    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: jid::FullJid = "bob@example.com/phone".parse().expect("bob jid");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    register_into_both_tiers(&registry, &user_registry, &bob, tx).await;

    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Iq(Box::new(session_initiate_iq_for_route_test(
            "call-delivered-1",
            &alice,
            &bob,
        )))),
        call_setup: Some(PendingCallSetupRoute),
    }];

    let outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert!(
        outcome.frames.is_empty(),
        "a delivered invite produces no caller-facing bounce: {:?}",
        outcome.frames
    );
    assert!(
        rx.try_recv().is_ok(),
        "the invite must have landed on bob's live channel"
    );
    assert_eq!(metrics.counter_sum("waddle.call.setup.ok", &[]), Some(1));
    assert_eq!(
        metrics
            .counter_sum(
                "waddle.call.setup.failed",
                &[("reason", "peer_unavailable")]
            )
            .unwrap_or(0),
        0
    );
}

#[tokio::test]
async fn detached_queued_session_initiate_counts_ok() {
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    use waddle_xmpp::telemetry::call::PendingCallSetupRoute;

    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: jid::FullJid = "bob@example.com/phone".parse().expect("bob jid");

    // Bob's resource is detached but XEP-0198-resumable: the invite is
    // queued for replay, which is a deliberate `ok` — the peer picks
    // it up on resume, so the alert must not read it as a failure.
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    sm.store_session(DetachedSession {
        stream_id: "bob-phone-stream".to_string(),
        user_id: "bob".to_string(),
        jid: bob.clone(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    })
    .await
    .expect("store detached session");

    let mut deps = Deps::registry_with_user_registry(&registry, &user_registry);
    deps.sm_session_registry = Some(&sm);

    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Iq(Box::new(session_initiate_iq_for_route_test(
            "call-detached-1",
            &alice,
            &bob,
        )))),
        call_setup: Some(PendingCallSetupRoute),
    }];

    let outcome = interpret(events, &deps).await;

    assert!(
        outcome.frames.is_empty(),
        "a replay-queued invite produces no caller-facing bounce: {:?}",
        outcome.frames
    );
    assert_eq!(metrics.counter_sum("waddle.call.setup.ok", &[]), Some(1));
    assert_eq!(
        metrics
            .counter_sum(
                "waddle.call.setup.failed",
                &[("reason", "peer_unavailable")]
            )
            .unwrap_or(0),
        0
    );
}
