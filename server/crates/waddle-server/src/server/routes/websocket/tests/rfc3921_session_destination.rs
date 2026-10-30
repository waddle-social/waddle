//! Bound legacy Session namespace requests must not borrow a foreign identity.
//! Local namespace-only compatibility behavior is deliberately preserved; this
//! does not validate Session payloads or repair RFC 6120 §7.1 pre-bind policy.

use std::sync::Arc;
use std::time::Duration;

use jid::{FullJid, Jid};
use minidom::Element;
use tokio::sync::mpsc;
use waddle_xmpp::protocol::{ConnectionPhase, StanzaDispatcher};
use waddle_xmpp::registry::OutboundStanza;
use waddle_xmpp::{ns::SESSION, Stanza};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::super::frame::handle_xmpp_frame;
use super::super::state::WsConnState;
use super::super::WebSocketState;
use super::{
    create_test_server_owner_session, create_test_websocket_state, register_test_connection,
    stanza_to_xml,
};

const DOMAIN: &str = "example.com";
const ORIGIN: &str = "alice@example.com/web";

fn origin() -> FullJid {
    ORIGIN.parse().expect("bound origin")
}

async fn connection(state: &WebSocketState) -> (WsConnState, mpsc::Receiver<OutboundStanza>) {
    let (tx, rx) = mpsc::channel(8);
    let owner = register_test_connection(state, &origin(), tx).await;
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(origin(), false);
    conn.registry_owner = Some(owner);
    (conn, rx)
}

fn request(target: Option<&str>, set: bool, from: Option<&str>) -> Iq {
    let from = from.map(|value| value.parse().expect("request sender"));
    let to = target.map(|value| value.parse().expect("request destination"));
    let id = "session<&-correlation".into();
    let payload = Element::builder("session", SESSION).build();
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
    .map(|frame| Iq::try_from(frame.parse::<Element>().expect("response XML")).expect("IQ"))
    .collect()
}

fn assert_remote_error(reply: &Iq, request: &Iq) {
    assert_eq!(reply.id(), request.id());
    assert_eq!(reply.from(), request.to());
    assert_eq!(reply.to(), Some(&Jid::from(origin())), "bound origin");
    let Iq::Error { error, payload, .. } = reply else {
        panic!("foreign Session must not receive local success: {reply:?}")
    };
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(error.defined_condition, DefinedCondition::NotAllowed);
    assert_eq!(error.by, Some(DOMAIN.parse().expect("home domain")));
    assert!(error
        .texts
        .values()
        .any(|text| text == "Remote Session routing is not allowed by this server."));
    let original = match request {
        Iq::Get { payload, .. } | Iq::Set { payload, .. } => payload,
        _ => panic!("request IQ"),
    };
    assert_eq!(payload.as_ref(), Some(original));
}

fn assert_ready_unchanged(state: &WebSocketState, conn: &WsConnState) {
    assert_eq!(conn.phase.bound_jid(), Some(&origin()));
    assert!(
        conn.authenticated_session.is_none(),
        "bound phase alone is sufficient"
    );
    assert!(!conn.carbons_enabled);
    assert!(!conn.roster_interested);
    assert!(!conn.blocklist_interested);
    assert!(!state
        .deps
        .protocol
        .connection_registry
        .is_carbons_enabled(&origin()));
}

#[tokio::test]
async fn bound_foreign_session_gets_and_sets_return_exact_locally_attributed_errors() {
    let state = create_test_websocket_state().await;
    let (mut conn, _rx) = connection(&state).await;
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
                let replies = dispatch(&state, &mut conn, iq.clone()).await;
                assert_eq!(replies.len(), 1, "{target}, set={set}");
                assert_remote_error(&replies[0], &iq);
                assert_ready_unchanged(&state, &conn);
            }
        }
    }
}

#[tokio::test]
async fn local_session_destinations_keep_namespace_only_empty_results() {
    let state = create_test_websocket_state().await;
    let (mut conn, _rx) = connection(&state).await;
    for target in [
        None,
        Some(DOMAIN),
        Some("alice@example.com"),
        Some("bob@example.com"),
        Some("muc.example.com"),
        Some("room@muc.example.com/nick"),
        Some("spaces.example.com"),
        Some("upload.example.com"),
        Some("community.example.com"),
        Some("extensions.example.com"),
        Some("push.example.com"),
        Some("calls.example.com"),
    ] {
        for set in [false, true] {
            let iq = request(target, set, Some(ORIGIN));
            let replies = dispatch(&state, &mut conn, iq.clone()).await;
            assert!(
                matches!(replies.as_slice(), [Iq::Result { payload: None, .. }]),
                "{target:?}: {replies:?}"
            );
            assert_eq!(replies[0].id(), iq.id());
            assert_eq!(replies[0].from(), iq.to());
            assert_eq!(replies[0].to(), iq.from());
            assert_ready_unchanged(&state, &conn);
        }
    }
}

#[tokio::test]
async fn configured_service_domains_remain_local_and_replaced_defaults_are_foreign() {
    let mut state = create_test_websocket_state().await;
    let services = &mut Arc::get_mut(&mut state)
        .expect("exclusive state")
        .deps
        .service_domains;
    services.muc = "rooms.custom.test".into();
    services.spaces = "spaces.custom.test".into();
    services.upload = "upload.custom.test".into();
    services.community = "community.custom.test".into();
    services.extensions = "extensions.custom.test".into();
    services.push = "push.custom.test".into();
    let (mut conn, _rx) = connection(&state).await;
    for target in [
        "rooms.custom.test",
        "spaces.custom.test",
        "upload.custom.test",
        "community.custom.test",
        "extensions.custom.test",
        "push.custom.test",
        "calls.example.com",
    ] {
        let replies = dispatch(&state, &mut conn, request(Some(target), true, Some(ORIGIN))).await;
        assert!(
            matches!(replies.as_slice(), [Iq::Result { payload: None, .. }]),
            "{target}: {replies:?}"
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
        let iq = request(Some(target), true, None);
        let replies = dispatch(&state, &mut conn, iq.clone()).await;
        assert_eq!(replies.len(), 1);
        assert_remote_error(&replies[0], &iq);
    }
    assert_ready_unchanged(&state, &conn);
}

#[tokio::test]
async fn session_namespace_policy_preserves_local_unvalidated_payload_compatibility() {
    let state = create_test_websocket_state().await;
    let (mut conn, _rx) = connection(&state).await;
    for set in [false, true] {
        for target in [None, Some(DOMAIN), Some("remote.test")] {
            let mut iq = request(target, set, Some(ORIGIN));
            let payload = match &mut iq {
                Iq::Get { payload, .. } | Iq::Set { payload, .. } => payload,
                _ => unreachable!(),
            };
            *payload = Element::builder("not-session", SESSION)
                .attr(
                    minidom::rxml::xml_ncname!("extension").to_owned(),
                    "value<&",
                )
                .append(Element::builder("child", SESSION).build())
                .build();
            let replies = dispatch(&state, &mut conn, iq.clone()).await;
            assert_eq!(replies.len(), 1);
            if target == Some("remote.test") {
                assert_remote_error(&replies[0], &iq);
            } else {
                assert!(matches!(replies[0], Iq::Result { payload: None, .. }));
            }
        }
    }
}

#[tokio::test]
async fn local_full_jid_session_requests_still_forward_to_the_real_peer() {
    let state = create_test_websocket_state().await;
    let peer: FullJid = "bob@example.com/phone".parse().expect("peer");
    let (tx, mut rx) = mpsc::channel(8);
    register_test_connection(&state, &peer, tx).await;
    let (mut conn, _origin_rx) = connection(&state).await;
    for set in [false, true] {
        let mut expected = request(
            Some(peer.as_str()),
            set,
            Some("forged@elsewhere.test/resource"),
        );
        assert!(dispatch(&state, &mut conn, expected.clone())
            .await
            .is_empty());
        let forwarded = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("forward promptly")
            .expect("peer connected");
        let Stanza::Iq(forwarded) = forwarded.stanza else {
            panic!("forwarded IQ")
        };
        *expected.from_mut() = Some(Jid::from(origin()));
        assert_eq!(*forwarded, expected);
        assert_ready_unchanged(&state, &conn);
    }
}

#[tokio::test]
async fn disconnected_local_full_jid_keeps_existing_delivery_error() {
    let state = create_test_websocket_state().await;
    let (mut conn, _rx) = connection(&state).await;
    let replies = dispatch(
        &state,
        &mut conn,
        request(Some("bob@example.com/offline"), true, Some(ORIGIN)),
    )
    .await;
    let [Iq::Error { error, .. }] = replies.as_slice() else {
        panic!("local delivery error: {replies:?}")
    };
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ServiceUnavailable
    );
    assert!(error.by.is_none());
    assert_ready_unchanged(&state, &conn);
}

#[tokio::test]
async fn session_results_and_errors_never_generate_a_reply() {
    let state = create_test_websocket_state().await;
    let (mut conn, _rx) = connection(&state).await;
    for target in ["remote.test", "bob@example.com/phone", DOMAIN] {
        let request = request(Some(target), true, Some(ORIGIN));
        let payload = Element::builder("session", SESSION).build();
        for iq in [
            Iq::Result {
                from: request.from().cloned(),
                to: request.to().cloned(),
                id: "session-result".into(),
                payload: Some(payload.clone()),
            },
            Iq::Error {
                from: request.from().cloned(),
                to: request.to().cloned(),
                id: "session-error".into(),
                payload: Some(payload),
                error: StanzaError::new(
                    ErrorType::Cancel,
                    DefinedCondition::ServiceUnavailable,
                    "en",
                    "",
                ),
            },
        ] {
            assert!(dispatch(&state, &mut conn, iq).await.is_empty());
            assert_ready_unchanged(&state, &conn);
        }
    }
}

#[tokio::test]
async fn foreign_session_before_binding_retains_existing_authentication_error() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(&state, "alice").await;
    for has_session in [false, true] {
        for phase in [
            ConnectionPhase::new(),
            ConnectionPhase::authenticated(&origin()),
        ] {
            let mut conn = WsConnState::new();
            conn.phase = phase;
            conn.authenticated_session = has_session.then(|| session.clone());
            for set in [false, true] {
                let replies =
                    dispatch(&state, &mut conn, request(Some("remote.test"), set, None)).await;
                let [Iq::Error { error, .. }] = replies.as_slice() else {
                    panic!("authentication error: {replies:?}")
                };
                assert_eq!(error.defined_condition, DefinedCondition::NotAuthorized);
                assert!(
                    error.by.is_none(),
                    "no bound origin for routing attribution"
                );
                assert!(conn.phase.bound_jid().is_none());
            }
        }
    }
}

#[tokio::test]
async fn unregistered_session_namespace_keeps_feature_fallback() {
    let mut state = create_test_websocket_state().await;
    Arc::get_mut(&mut state)
        .expect("exclusive state")
        .deps
        .protocol
        .dispatcher = Arc::new(StanzaDispatcher::new());
    let (mut conn, _rx) = connection(&state).await;
    let replies = dispatch(
        &state,
        &mut conn,
        request(Some("remote.test"), true, Some(ORIGIN)),
    )
    .await;
    let [Iq::Error { error, .. }] = replies.as_slice() else {
        panic!("feature fallback: {replies:?}")
    };
    assert_eq!(
        error.defined_condition,
        DefinedCondition::FeatureNotImplemented
    );
    assert!(error.by.is_none());
}

#[tokio::test]
async fn foreign_session_settles_sm_once_and_local_requests_still_progress() {
    let state = create_test_websocket_state().await;
    let (mut conn, _rx) = connection(&state).await;
    conn.sm_state
        .enable("session-routing-sm".into(), false, None);
    let mut count = 0;
    for set in [false, true] {
        let iq = request(Some("remote.test"), set, None);
        let replies = dispatch(&state, &mut conn, iq.clone()).await;
        assert_eq!(replies.len(), 1);
        assert_remote_error(&replies[0], &iq);
        count += 1;
        assert_eq!(conn.sm_state.inbound_count(), count);
        assert_eq!(conn.sm_inbound_completion.pending_count(), 0);
        assert!(!conn.sm_inbound_completion.has_unhandled_hole());
        let replies = dispatch(&state, &mut conn, request(None, true, Some(ORIGIN))).await;
        assert!(matches!(
            replies.as_slice(),
            [Iq::Result { payload: None, .. }]
        ));
        count += 1;
        assert_eq!(conn.sm_state.inbound_count(), count);
        assert_eq!(conn.sm_inbound_completion.pending_count(), 0);
        assert!(!conn.sm_inbound_completion.has_unhandled_hole());
        assert_ready_unchanged(&state, &conn);
    }
}
