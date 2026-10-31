//! RFC 6121 §8.5.3.1 local full-JID replies use authenticated peer routing.

use std::time::Duration;

use jid::{FullJid, Jid};
use minidom::Element;
use tokio::sync::mpsc;
use waddle_xmpp::disco::{build_disco_info_response, Feature, Identity};
use waddle_xmpp::protocol::handlers::errors::NS_BLOCKING_ERRORS;
use waddle_xmpp::protocol::ConnectionPhase;
use waddle_xmpp::registry::OutboundStanza;
use waddle_xmpp::xep::xep0115::{compute_caps_hash, Caps, CapsCacheKey};
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::{Iq, IqPayload};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::super::state::WsConnState;
use super::super::WebSocketState;
use super::iq_destination_support::{bound_connection, dispatch, origin, request, DOMAIN, ORIGIN};
use super::{create_test_websocket_state, register_test_connection};
use crate::db::blocking::DatabaseBlockingStorage;
use crate::server::caps_resolution::build_caps_disco_info_query;

const PEER: &str = "bob@example.com/phone";

fn replies(target: Option<&str>, from: Option<&str>) -> Vec<Iq> {
    let payload = Element::builder("ping", NS_PING).build();
    let error = StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::FeatureNotImplemented,
        "en",
        "Peer cannot handle this request.",
    );
    [
        IqPayload::Result(None),
        IqPayload::Result(Some(payload.clone())),
        IqPayload::Error(error),
    ]
    .into_iter()
    .map(|data| {
        let mut reply = request(data, target, from, "peer-reply<&");
        if let Iq::Error { payload: echo, .. } = &mut reply {
            *echo = Some(payload.clone());
        }
        reply
    })
    .collect()
}

async fn receive(rx: &mut mpsc::Receiver<OutboundStanza>) -> Iq {
    let outbound = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("peer reply promptly")
        .expect("peer channel open");
    let Stanza::Iq(iq) = outbound.stanza else {
        panic!("expected IQ reply")
    };
    *iq
}

async fn assert_request_error(
    state: &WebSocketState,
    conn: &mut WsConnState,
    expected: DefinedCondition,
) {
    let ping = request(
        IqPayload::Get(Element::builder("ping", NS_PING).build()),
        Some(PEER),
        None,
        "request-still-bounces",
    );
    let frames = dispatch(state, conn, ping).await;
    let [Iq::Error { error, .. }] = frames.as_slice() else {
        panic!("requests retain their normal error response")
    };
    assert_eq!(error.defined_condition, expected);
}

#[tokio::test]
async fn local_full_jid_replies_preserve_contents_and_bind_the_sender() {
    let state = create_test_websocket_state().await;
    let (mut conn, _origin_rx) = bound_connection(&state).await;
    for target in [PEER, "alice@example.com/phone", ORIGIN] {
        let target: FullJid = target.parse().expect("target");
        let (tx, mut rx) = mpsc::channel(16);
        let owner = register_test_connection(&state, &target, tx).await;
        if target == origin() {
            conn.registry_owner = Some(owner);
        }
        for from in [None, Some(ORIGIN), Some("forged@remote.test/resource")] {
            for mut reply in replies(Some(target.as_str()), from) {
                assert!(dispatch(&state, &mut conn, reply.clone()).await.is_empty());
                *reply.from_mut() = Some(Jid::from(origin()));
                assert_eq!(receive(&mut rx).await, reply);
            }
        }
        assert!(rx.try_recv().is_err(), "no duplicate delivery");
    }
}

#[tokio::test]
async fn own_account_block_rules_do_not_block_full_jid_requests_or_replies() {
    let state = create_test_websocket_state().await;
    let (mut conn, _origin_rx) = bound_connection(&state).await;
    let storage = DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone());
    storage
        .add_blocks(
            &origin().to_bare(),
            &[
                Jid::from(origin().to_bare()),
                DOMAIN.parse().expect("domain block"),
            ],
        )
        .await
        .expect("own-account block rules");
    for target in [ORIGIN, "alice@example.com/phone"] {
        let target: FullJid = target.parse().expect("same-account resource");
        let (tx, mut rx) = mpsc::channel(8);
        let owner = register_test_connection(&state, &target, tx).await;
        if target == origin() {
            conn.registry_owner = Some(owner);
        }
        let mut stanzas = replies(Some(target.as_str()), None);
        for payload in [IqPayload::Get, IqPayload::Set] {
            stanzas.push(request(
                payload(Element::builder("ping", NS_PING).build()),
                Some(target.as_str()),
                None,
                "own-request",
            ));
        }
        for mut stanza in stanzas {
            assert!(dispatch(&state, &mut conn, stanza.clone()).await.is_empty());
            *stanza.from_mut() = Some(Jid::from(origin()));
            assert_eq!(receive(&mut rx).await, stanza);
        }
    }
}

#[tokio::test]
async fn unavailable_and_closed_resources_never_bounce_a_reply() {
    let state = create_test_websocket_state().await;
    let (mut conn, mut origin_rx) = bound_connection(&state).await;
    for closed in [false, true] {
        if closed {
            let (tx, rx) = mpsc::channel(4);
            register_test_connection(&state, &PEER.parse().expect("peer"), tx).await;
            drop(rx);
        }
        for reply in replies(Some(PEER), None) {
            assert!(dispatch(&state, &mut conn, reply).await.is_empty());
        }
    }
    assert!(origin_rx.try_recv().is_err(), "no routed error loop");
}

#[tokio::test]
async fn blocked_replies_are_silent_and_do_not_reach_the_recipient() {
    let state = create_test_websocket_state().await;
    let (mut conn, mut origin_rx) = bound_connection(&state).await;
    let peer: FullJid = PEER.parse().expect("peer");
    let (tx, mut rx) = mpsc::channel(4);
    register_test_connection(&state, &peer, tx).await;
    let storage = DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone());
    storage
        .add_blocks(&peer.to_bare(), &[Jid::from(origin().to_bare())])
        .await
        .expect("block sender");
    for reply in replies(Some(PEER), Some("forged@remote.test/resource")) {
        assert!(dispatch(&state, &mut conn, reply).await.is_empty());
    }
    assert_request_error(&state, &mut conn, DefinedCondition::ServiceUnavailable).await;
    assert!(rx.try_recv().is_err(), "blocked reply not delivered");
    assert!(origin_rx.try_recv().is_err(), "blocked reply not bounced");
}

#[tokio::test]
async fn outgoing_blocks_match_bare_full_and_domain_targets_before_routing() {
    for (blocked, denied) in [
        ("bob@example.com", true),
        (PEER, true),
        (DOMAIN, true),
        ("bob@example.com/other-resource", false),
    ] {
        let state = create_test_websocket_state().await;
        let (mut conn, mut origin_rx) = bound_connection(&state).await;
        let (tx, mut rx) = mpsc::channel(8);
        register_test_connection(&state, &PEER.parse().expect("peer"), tx).await;
        let storage = DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone());
        storage
            .add_blocks(
                &origin().to_bare(),
                &[blocked.parse().expect("blocked target")],
            )
            .await
            .expect("sender blocklist");
        conn.sm_state
            .enable("outgoing-block-sm".into(), false, None);
        let mut count = 0;
        for mut reply in replies(Some(PEER), Some("forged@remote.test/resource")) {
            assert!(dispatch(&state, &mut conn, reply.clone()).await.is_empty());
            if !denied {
                *reply.from_mut() = Some(Jid::from(origin()));
                assert_eq!(receive(&mut rx).await, reply);
            }
            count += 1;
        }
        for constructor in [IqPayload::Get, IqPayload::Set] {
            let payload = Element::builder("ping", NS_PING).build();
            let mut iq = request(
                constructor(payload.clone()),
                Some(PEER),
                Some("forged@remote.test/resource"),
                "outgoing-blocked-request",
            );
            let frames = dispatch(&state, &mut conn, iq.clone()).await;
            if denied {
                let [Iq::Error {
                    from,
                    to,
                    id,
                    payload: echo,
                    error,
                }] = frames.as_slice()
                else {
                    panic!("blocked outgoing request needs a bounded error: {frames:?}")
                };
                assert_eq!(from.as_ref(), iq.to());
                assert_eq!(to.as_ref(), Some(&Jid::from(origin())));
                assert_eq!(id, iq.id());
                assert_eq!(echo.as_ref(), Some(&payload));
                let mut expected = StanzaError::new(
                    ErrorType::Cancel,
                    DefinedCondition::NotAcceptable,
                    "en",
                    "Recipient is on your blocklist.",
                );
                expected.other = Some(Element::builder("blocked", NS_BLOCKING_ERRORS).build());
                expected.by = Some(DOMAIN.parse().expect("home domain"));
                assert_eq!(error, &expected);
            } else {
                assert!(frames.is_empty());
                *iq.from_mut() = Some(Jid::from(origin()));
                assert_eq!(receive(&mut rx).await, iq);
            }
            count += 1;
        }
        assert_eq!(conn.sm_state.inbound_count(), count);
        assert_eq!(conn.sm_inbound_completion.pending_count(), 0);
        assert!(!conn.sm_inbound_completion.has_unhandled_hole());
        assert!(
            rx.try_recv().is_err(),
            "blocked stanza must not reach the peer"
        );
        assert!(origin_rx.try_recv().is_err(), "no routed error loops");
    }
}

#[tokio::test]
async fn blocklist_storage_failure_drops_replies_without_an_error_loop() {
    let state = create_test_websocket_state().await;
    let (mut conn, mut origin_rx) = bound_connection(&state).await;
    let (tx, mut rx) = mpsc::channel(4);
    register_test_connection(&state, &PEER.parse().expect("peer"), tx).await;
    state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("database")
        .execute("DROP TABLE blocking_list", crate::db_params![])
        .await
        .expect("break blocking lookup");
    for reply in replies(Some(PEER), None) {
        assert!(dispatch(&state, &mut conn, reply).await.is_empty());
    }
    assert_request_error(&state, &mut conn, DefinedCondition::InternalServerError).await;
    assert!(rx.try_recv().is_err(), "fail closed");
    assert!(origin_rx.try_recv().is_err(), "no routed error loop");
}

#[tokio::test]
async fn non_peer_destinations_and_unbound_senders_do_not_forward_replies() {
    let state = create_test_websocket_state().await;
    let (mut conn, mut origin_rx) = bound_connection(&state).await;
    for target in [
        None,
        Some(DOMAIN),
        Some("bob@example.com"),
        Some("example.com/server-resource"),
        Some("room@muc.example.com/nick"),
        Some("bot@extensions.example.com/resource"),
        Some("bob@remote.test/phone"),
        Some("bob@example.com.attacker.test/phone"),
    ] {
        let (tx, mut rx) = mpsc::channel(4);
        if let Some(full) = target.and_then(|target| target.parse::<FullJid>().ok()) {
            // Register even unsupported addresses so locality must be checked
            // before registry delivery, rather than relying on an empty lookup.
            register_test_connection(&state, &full, tx).await;
        }
        for reply in replies(target, None) {
            assert!(dispatch(&state, &mut conn, reply).await.is_empty());
        }
        assert!(
            rx.try_recv().is_err(),
            "unsupported destination: {target:?}"
        );
    }
    let (tx, mut rx) = mpsc::channel(4);
    register_test_connection(&state, &PEER.parse().expect("peer"), tx).await;
    for phase in [
        ConnectionPhase::new(),
        ConnectionPhase::authenticated(&origin()),
    ] {
        let mut unbound = WsConnState::new();
        unbound.phase = phase;
        for reply in replies(Some(PEER), Some(ORIGIN)) {
            assert!(dispatch(&state, &mut unbound, reply).await.is_empty());
        }
    }
    assert!(rx.try_recv().is_err(), "no unbound peer routing");
    assert!(origin_rx.try_recv().is_err());
}

#[tokio::test]
async fn peer_reply_id_collision_cannot_consume_server_caps_pending() {
    let state = create_test_websocket_state().await;
    let (mut conn, _origin_rx) = bound_connection(&state).await;
    let (tx, mut rx) = mpsc::channel(4);
    register_test_connection(&state, &PEER.parse().expect("peer"), tx).await;
    let identities = vec![Identity::new("client", "web", Some("Reply fixture"))];
    let features = vec![Feature::new(NS_PING)];
    let ver = compute_caps_hash(&identities, &features);
    let caps = Caps::new("https://example.test/caps", &ver);
    let query = build_caps_disco_info_query(
        &state.deps.auth_state.caps_server_domain,
        &origin(),
        &caps,
        "shared-peer-and-caps-id",
    );
    let resolver = &state.deps.protocol.caps_resolver;
    resolver.begin_pending(query.id().into(), origin(), caps.clone());
    let authentic =
        build_disco_info_response(&query, &identities, &features, Some(&caps.node_ver()));
    for mut peer_reply in replies(Some(PEER), None) {
        *peer_reply.id_mut() = query.id().into();
        assert!(dispatch(&state, &mut conn, peer_reply).await.is_empty());
        receive(&mut rx).await;
        assert_eq!(resolver.pending_len(), 1, "peer reply leaves CAPS pending");
        assert!(resolver.key_for_resource(&origin()).is_none());
    }
    assert!(dispatch(&state, &mut conn, authentic).await.is_empty());
    assert_eq!(resolver.pending_len(), 0);
    assert_eq!(
        resolver.key_for_resource(&origin()),
        Some(CapsCacheKey::new("sha-1", &ver))
    );
}

#[tokio::test]
async fn replies_settle_sm_once_and_a_following_local_ping_progresses() {
    let state = create_test_websocket_state().await;
    let (mut conn, _origin_rx) = bound_connection(&state).await;
    let (tx, mut rx) = mpsc::channel(4);
    register_test_connection(&state, &PEER.parse().expect("peer"), tx).await;
    conn.sm_state
        .enable("local-iq-reply-sm".into(), false, None);
    let mut count = 0;
    for target in [PEER, "offline@example.com/gone", "bob@remote.test/phone"] {
        for reply in replies(Some(target), None) {
            assert!(dispatch(&state, &mut conn, reply).await.is_empty());
            if target == PEER {
                receive(&mut rx).await;
            }
            count += 1;
            assert_eq!(conn.sm_state.inbound_count(), count);
            assert_eq!(conn.sm_inbound_completion.pending_count(), 0);
            assert!(!conn.sm_inbound_completion.has_unhandled_hole());
        }
    }
    let ping = request(
        IqPayload::Get(Element::builder("ping", NS_PING).build()),
        Some(DOMAIN),
        None,
        "local-ping-after-replies",
    );
    assert!(matches!(
        dispatch(&state, &mut conn, ping).await.as_slice(),
        [Iq::Result { .. }]
    ));
    assert_eq!(conn.sm_state.inbound_count(), count + 1);
}
