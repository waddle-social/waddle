//! XEP-0199 destination ownership through the active WebSocket frame dispatch.

use std::sync::Arc;
use std::time::Duration;

use jid::{FullJid, Jid};
use minidom::Element;
use tokio::sync::mpsc;
use waddle_xmpp::protocol::ConnectionPhase;
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::xep::xep0410::{interpret_self_ping_response, SelfPingResult};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::super::frame::handle_xmpp_frame;
use super::super::state::WsConnState;
use super::super::WebSocketState;
use super::{create_test_websocket_state, register_test_connection, stanza_to_xml};

const DOMAIN: &str = "example.com";
const ORIGIN: &str = "alice@example.com/web";

fn origin() -> FullJid {
    ORIGIN.parse().expect("origin JID")
}

fn connection() -> WsConnState {
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(origin(), false);
    conn
}

fn request(target: Option<&str>, set: bool, from: Option<&str>) -> Iq {
    let payload = Element::builder("ping", NS_PING).build();
    let from = from.map(|from| from.parse().expect("request sender"));
    let to = target.map(|target| target.parse().expect("request target"));
    let id = "ping<&-correlation".to_owned();
    if set {
        Iq::Set {
            from,
            to,
            id,
            payload,
        }
    } else {
        Iq::Get {
            from,
            to,
            id,
            payload,
        }
    }
}

async fn dispatch(state: &WebSocketState, conn: &mut WsConnState, iq: Iq) -> Vec<Iq> {
    handle_xmpp_frame(
        &stanza_to_xml(&Stanza::Iq(Box::new(iq))),
        DOMAIN,
        state,
        conn,
    )
    .await
    .into_iter()
    .map(|frame| {
        Iq::try_from(frame.parse::<Element>().expect("response XML")).expect("response IQ")
    })
    .collect()
}

fn assert_remote_not_allowed(reply: &Iq, request: &Iq) {
    assert_eq!(reply.id(), request.id());
    assert_eq!(reply.from(), request.to(), "RFC 6120 swapped target");
    assert_eq!(reply.to(), Some(&Jid::from(origin())), "bound origin");
    let Iq::Error { error, payload, .. } = reply else {
        panic!("foreign destination must never produce a pong: {reply:?}");
    };
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(error.defined_condition, DefinedCondition::NotAllowed);
    assert_eq!(
        error.by,
        Some(DOMAIN.parse().expect("local error generator"))
    );
    let original_payload = match request {
        Iq::Get { payload, .. } | Iq::Set { payload, .. } => payload,
        _ => panic!("request IQ"),
    };
    assert_eq!(payload.as_ref(), Some(original_payload));
}

#[tokio::test]
async fn foreign_ping_get_and_set_return_locally_attributed_error() {
    let state = create_test_websocket_state().await;
    let mut conn = connection();
    for target in [
        "remote.test",
        "bob@remote.test",
        "bob@remote.test/phone",
        "room@muc.remote.test/nick",
        "example.com.attacker.test",
        "notexample.com",
        "unserved.example.com",
        "muc.example.com.attacker.test",
        "calls.example.com.attacker.test",
    ] {
        for set in [false, true] {
            for from in [None, Some(ORIGIN), Some("forged@elsewhere.test/resource")] {
                let iq = request(Some(target), set, from);
                let responses = dispatch(&state, &mut conn, iq.clone()).await;
                assert_eq!(responses.len(), 1, "{target}, set={set}, from={from:?}");
                assert_remote_not_allowed(&responses[0], &iq);
            }
        }
    }
}

#[tokio::test]
async fn foreign_occupant_ping_cannot_confirm_room_membership() {
    let state = create_test_websocket_state().await;
    let mut conn = connection();
    let iq = request(Some("room@muc.remote.test/alice"), false, Some(ORIGIN));
    let responses = dispatch(&state, &mut conn, iq.clone()).await;
    assert_eq!(responses.len(), 1);
    assert_remote_not_allowed(&responses[0], &iq);
    // Exercise the existing XEP-0410 interpretation on the actual parsed wire
    // response: service-unavailable would incorrectly mean still joined.
    let membership = interpret_self_ping_response(&responses[0]);
    assert_eq!(membership, SelfPingResult::Disconnected);
    assert!(membership.should_rejoin());
}

#[tokio::test]
async fn missing_to_home_bare_account_and_served_components_keep_local_ping_behavior() {
    let state = create_test_websocket_state().await;
    let mut conn = connection();
    for target in [
        None,
        Some(DOMAIN),
        Some("alice@example.com"),
        Some("bob@example.com"),
        Some("muc.example.com"),
        Some("room@muc.example.com"),
        Some("spaces.example.com"),
        Some("upload.example.com"),
        Some("community.example.com"),
        Some("extensions.example.com"),
        Some("push.example.com"),
        Some("calls.example.com"),
    ] {
        let iq = request(target, false, Some(ORIGIN));
        let responses = dispatch(&state, &mut conn, iq.clone()).await;
        assert_eq!(responses.len(), 1, "{target:?}");
        assert_eq!(responses[0].id(), iq.id());
        assert!(
            matches!(&responses[0], Iq::Result { payload: None, .. }),
            "{target:?}: {responses:?}"
        );
    }
}

#[tokio::test]
async fn configured_service_domains_are_owned_and_replaced_defaults_are_foreign() {
    let mut state = create_test_websocket_state().await;
    let services = &mut Arc::get_mut(&mut state)
        .expect("exclusive test state")
        .deps
        .service_domains;
    services.muc = "rooms.custom.test".into();
    services.spaces = "spaces.custom.test".into();
    services.upload = "upload.custom.test".into();
    services.community = "community.custom.test".into();
    services.extensions = "extensions.custom.test".into();
    services.push = "push.custom.test".into();
    let mut conn = connection();
    for service in [
        "rooms",
        "spaces",
        "upload",
        "community",
        "extensions",
        "push",
    ] {
        let target = format!("{service}.custom.test");
        let responses = dispatch(
            &state,
            &mut conn,
            request(Some(&target), false, Some(ORIGIN)),
        )
        .await;
        assert!(
            matches!(responses.as_slice(), [Iq::Result { .. }]),
            "{target}: {responses:?}"
        );
    }
    for target in [
        "muc.example.com",
        "spaces.example.com",
        "upload.example.com",
        "community.example.com",
        "extensions.example.com",
        "push.example.com",
    ] {
        let iq = request(Some(target), false, Some(ORIGIN));
        let responses = dispatch(&state, &mut conn, iq.clone()).await;
        assert_eq!(responses.len(), 1);
        assert_remote_not_allowed(&responses[0], &iq);
    }
}

#[tokio::test]
async fn local_full_jid_ping_is_forwarded_to_the_actual_client() {
    let state = create_test_websocket_state().await;
    let mut conn = connection();
    let peer: FullJid = "bob@example.com/phone".parse().expect("peer JID");
    let (tx, mut rx) = mpsc::channel(4);
    register_test_connection(&state, &peer, tx).await;
    for set in [false, true] {
        let iq = request(
            Some(peer.as_str()),
            set,
            Some("forged@elsewhere.test/resource"),
        );
        let responses = dispatch(&state, &mut conn, iq.clone()).await;
        assert!(
            responses.is_empty(),
            "local client must answer: {responses:?}"
        );
        let outbound = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("forwarded IQ promptly")
            .expect("peer channel open");
        let Stanza::Iq(forwarded) = outbound.stanza else {
            panic!("forwarded IQ")
        };
        let mut expected = iq;
        *expected.from_mut() = Some(Jid::from(origin()));
        assert_eq!(*forwarded, expected);
    }
}

#[tokio::test]
async fn disconnected_local_full_jid_keeps_existing_delivery_error() {
    let state = create_test_websocket_state().await;
    let mut conn = connection();
    let iq = request(Some("bob@example.com/offline"), false, Some(ORIGIN));
    let responses = dispatch(&state, &mut conn, iq.clone()).await;
    let [Iq::Error { error, .. }] = responses.as_slice() else {
        panic!("disconnected local resource: {responses:?}")
    };
    assert_eq!(responses[0].id(), iq.id());
    assert_eq!(responses[0].from(), iq.to());
    assert_eq!(responses[0].to(), iq.from());
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ServiceUnavailable
    );
    assert!(error.by.is_none(), "existing local delivery path");
}

#[tokio::test]
async fn malformed_local_ping_and_local_ping_set_keep_bad_request() {
    let state = create_test_websocket_state().await;
    let mut conn = connection();
    let mut malformed = request(Some(DOMAIN), false, Some(ORIGIN));
    if let Iq::Get { payload, .. } = &mut malformed {
        *payload = Element::builder("ping", NS_PING)
            .append(Element::builder("extra", NS_PING).build())
            .build();
    }
    for iq in [malformed, request(Some(DOMAIN), true, Some(ORIGIN))] {
        let responses = dispatch(&state, &mut conn, iq).await;
        let [Iq::Error { error, .. }] = responses.as_slice() else {
            panic!("malformed local ping: {responses:?}")
        };
        assert_eq!(error.type_, ErrorType::Modify);
        assert_eq!(error.defined_condition, DefinedCondition::BadRequest);
        assert!(error.by.is_none(), "existing local ping validation");
    }
}

#[tokio::test]
async fn local_muc_occupant_ping_retains_self_ping_joined_and_not_joined_semantics() {
    let state = create_test_websocket_state().await;
    let mut conn = connection();
    let room = "room@muc.example.com".parse().expect("room JID");
    state.deps.protocol.remote_muc_memberships.record_join(
        &origin(),
        &room,
        "alice",
        OccupancySessionGeneration::mint(),
    );
    let joined = dispatch(
        &state,
        &mut conn,
        request(Some("room@muc.example.com/alice"), false, Some(ORIGIN)),
    )
    .await;
    assert!(matches!(joined.as_slice(), [Iq::Result { .. }]));
    let absent = dispatch(
        &state,
        &mut conn,
        request(Some("room@muc.example.com/other"), false, Some(ORIGIN)),
    )
    .await;
    let [Iq::Error { error, .. }] = absent.as_slice() else {
        panic!("not-joined IQ: {absent:?}")
    };
    assert_eq!(error.defined_condition, DefinedCondition::NotAcceptable);
}

#[tokio::test]
async fn foreign_ping_results_and_errors_never_produce_an_error_loop() {
    let state = create_test_websocket_state().await;
    let mut conn = connection();
    let result = Iq::Result {
        from: Some("bob@remote.test/phone".parse().expect("foreign JID")),
        to: Some("other@remote.test".parse().expect("foreign JID")),
        id: "ping-result".into(),
        payload: Some(Element::builder("ping", NS_PING).build()),
    };
    let error = Iq::Error {
        from: result.from().cloned(),
        to: result.to().cloned(),
        id: "ping-error".into(),
        payload: Some(Element::builder("ping", NS_PING).build()),
        error: StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "en",
            "",
        ),
    };
    for iq in [result, error] {
        assert!(dispatch(&state, &mut conn, iq).await.is_empty());
    }
    let local = dispatch(&state, &mut conn, request(None, false, Some(ORIGIN))).await;
    assert!(matches!(local.as_slice(), [Iq::Result { .. }]));
}

#[tokio::test]
async fn foreign_rejection_settles_sm_inbound_once_and_does_not_block_local_progress() {
    let state = create_test_websocket_state().await;
    let mut conn = connection();
    conn.sm_state.enable("ping-routing-sm".into(), false, None);
    let iq = request(Some("remote.test"), false, Some(ORIGIN));
    let responses = dispatch(&state, &mut conn, iq.clone()).await;
    assert_eq!(responses.len(), 1);
    assert_remote_not_allowed(&responses[0], &iq);
    assert_eq!(conn.sm_state.inbound_count(), 1);
    let local = dispatch(&state, &mut conn, request(None, false, Some(ORIGIN))).await;
    assert!(matches!(local.as_slice(), [Iq::Result { .. }]));
    assert_eq!(conn.sm_state.inbound_count(), 2);
}

#[tokio::test]
async fn foreign_ping_before_binding_keeps_authentication_error() {
    let state = create_test_websocket_state().await;
    for phase in [
        ConnectionPhase::new(),
        ConnectionPhase::authenticated(&origin()),
    ] {
        let mut conn = WsConnState::new();
        conn.phase = phase;
        let responses =
            dispatch(&state, &mut conn, request(Some("remote.test"), false, None)).await;
        let [Iq::Error { error, .. }] = responses.as_slice() else {
            panic!("authentication error: {responses:?}")
        };
        assert_eq!(error.defined_condition, DefinedCondition::NotAuthorized);
        assert!(
            error.by.is_none(),
            "remote routing gate must run after authentication"
        );
    }
}
