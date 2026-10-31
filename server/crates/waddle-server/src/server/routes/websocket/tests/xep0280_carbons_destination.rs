//! XEP-0280 destination policy through the active WebSocket frame dispatcher.

use std::sync::Arc;
use std::time::Duration;

use jid::{FullJid, Jid};
use minidom::Element;
use tokio::sync::mpsc;
use waddle_xmpp::protocol::{ConnectionPhase, StanzaDispatcher};
use waddle_xmpp::registry::OutboundStanza;
use waddle_xmpp::Stanza;
use xmpp_parsers::carbons::{Disable, Enable};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::super::frame::handle_xmpp_frame;
use super::super::state::WsConnState;
use super::super::WebSocketState;
use super::{create_test_websocket_state, register_test_connection, stanza_to_xml};

const DOMAIN: &str = "example.com";
const ORIGIN: &str = "alice@example.com/web";

fn origin() -> FullJid {
    ORIGIN.parse().expect("bound origin")
}

async fn connection(
    state: &WebSocketState,
    enabled: bool,
) -> (WsConnState, mpsc::Receiver<OutboundStanza>) {
    let (tx, rx) = mpsc::channel(8);
    let owner = register_test_connection(state, &origin(), tx).await;
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(origin(), false);
    conn.registry_owner = Some(owner);
    conn.carbons_enabled = enabled;
    state
        .deps
        .protocol
        .connection_registry
        .set_carbons_enabled(&origin(), enabled);
    (conn, rx)
}

fn request(target: Option<&str>, enable: bool, set: bool, from: Option<&str>) -> Iq {
    let payload: Element = if enable {
        Enable.into()
    } else {
        Disable.into()
    };
    let from = from.map(|value| value.parse().expect("request sender"));
    let to = target.map(|value| value.parse().expect("request destination"));
    let id = "carbons<&-correlation".into();
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

fn assert_state(state: &WebSocketState, conn: &WsConnState, enabled: bool) {
    assert_eq!(conn.carbons_enabled, enabled, "connection flag");
    assert_eq!(
        state
            .deps
            .protocol
            .connection_registry
            .is_carbons_enabled(&origin()),
        enabled,
        "registered resource flag"
    );
}

fn assert_remote_error(reply: &Iq, request: &Iq) {
    assert_eq!(reply.id(), request.id());
    assert_eq!(reply.from(), request.to());
    assert_eq!(reply.to(), Some(&Jid::from(origin())), "bound origin");
    let Iq::Error { error, payload, .. } = reply else {
        panic!("foreign Carbons request must not be acknowledged: {reply:?}")
    };
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(error.defined_condition, DefinedCondition::NotAllowed);
    assert_eq!(error.by, Some(DOMAIN.parse().expect("home domain")));
    let original = match request {
        Iq::Get { payload, .. } | Iq::Set { payload, .. } => payload,
        _ => panic!("request IQ"),
    };
    assert_eq!(payload.as_ref(), Some(original));
}

#[tokio::test]
async fn foreign_carbons_requests_never_acknowledge_or_change_connection_and_registry() {
    let state = create_test_websocket_state().await;
    for enabled in [false, true] {
        let (mut conn, _rx) = connection(&state, enabled).await;
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
            for enable in [false, true] {
                for set in [false, true] {
                    for from in [None, Some(ORIGIN), Some("forged@elsewhere.test/resource")] {
                        let iq = request(Some(target), enable, set, from);
                        let replies = dispatch(&state, &mut conn, iq.clone()).await;
                        assert_eq!(replies.len(), 1, "{target}, enable={enable}, set={set}");
                        assert_remote_error(&replies[0], &iq);
                        assert_state(&state, &conn, enabled);
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn local_carbons_controls_keep_existing_results_and_update_both_flags() {
    let state = create_test_websocket_state().await;
    let (mut conn, _rx) = connection(&state, false).await;
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
        for enable in [true, false] {
            let iq = request(target, enable, true, Some(ORIGIN));
            let replies = dispatch(&state, &mut conn, iq.clone()).await;
            assert!(
                matches!(replies.as_slice(), [Iq::Result { payload: None, .. }]),
                "{target:?}: {replies:?}"
            );
            assert_eq!(replies[0].id(), iq.id());
            assert_state(&state, &conn, enable);
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
    let (mut conn, _rx) = connection(&state, false).await;
    for target in [
        "rooms.custom.test",
        "spaces.custom.test",
        "upload.custom.test",
        "community.custom.test",
        "extensions.custom.test",
        "push.custom.test",
    ] {
        for enable in [true, false] {
            let replies = dispatch(
                &state,
                &mut conn,
                request(Some(target), enable, true, Some(ORIGIN)),
            )
            .await;
            assert!(
                matches!(replies.as_slice(), [Iq::Result { .. }]),
                "{target}: {replies:?}"
            );
            assert_state(&state, &conn, enable);
        }
    }
    for target in [
        "muc.example.com",
        "spaces.example.com",
        "upload.example.com",
        "community.example.com",
        "extensions.example.com",
        "push.example.com",
    ] {
        let iq = request(Some(target), true, true, None);
        let replies = dispatch(&state, &mut conn, iq.clone()).await;
        assert_eq!(replies.len(), 1);
        assert_remote_error(&replies[0], &iq);
        assert_state(&state, &conn, false);
    }
}

#[tokio::test]
async fn local_full_jid_carbons_requests_still_forward_without_mutating_origin() {
    let state = create_test_websocket_state().await;
    let peer: FullJid = "bob@example.com/phone".parse().expect("peer");
    let (tx, mut rx) = mpsc::channel(8);
    register_test_connection(&state, &peer, tx).await;
    for enabled in [false, true] {
        let (mut conn, _origin_rx) = connection(&state, enabled).await;
        for set in [false, true] {
            let iq = request(
                Some(peer.as_str()),
                !enabled,
                set,
                Some("forged@elsewhere.test/resource"),
            );
            assert!(dispatch(&state, &mut conn, iq.clone()).await.is_empty());
            let forwarded = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("forward promptly")
                .expect("peer connected");
            let Stanza::Iq(forwarded) = forwarded.stanza else {
                panic!("forwarded IQ")
            };
            let mut expected = iq;
            *expected.from_mut() = Some(Jid::from(origin()));
            assert_eq!(*forwarded, expected);
            assert_state(&state, &conn, enabled);
            assert!(!state
                .deps
                .protocol
                .connection_registry
                .is_carbons_enabled(&peer));
        }
    }
}

#[tokio::test]
async fn disconnected_local_full_jid_keeps_delivery_error_without_mutating_origin() {
    let state = create_test_websocket_state().await;
    let (mut conn, _rx) = connection(&state, true).await;
    let iq = request(Some("bob@example.com/offline"), false, true, Some(ORIGIN));
    let replies = dispatch(&state, &mut conn, iq.clone()).await;
    let [Iq::Error { error, .. }] = replies.as_slice() else {
        panic!("local delivery error: {replies:?}")
    };
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ServiceUnavailable
    );
    assert!(error.by.is_none(), "existing full-JID delivery path");
    assert_state(&state, &conn, true);
}

#[tokio::test]
async fn carbons_results_and_errors_do_not_reply_or_change_state() {
    let state = create_test_websocket_state().await;
    for enabled in [false, true] {
        let (mut conn, _rx) = connection(&state, enabled).await;
        let request = request(Some("remote.test"), !enabled, true, Some(ORIGIN));
        let payload: Element = if enabled {
            Disable.into()
        } else {
            Enable.into()
        };
        for iq in [
            Iq::Result {
                from: request.from().cloned(),
                to: request.to().cloned(),
                id: "carbons-result".into(),
                payload: Some(payload.clone()),
            },
            Iq::Error {
                from: request.from().cloned(),
                to: request.to().cloned(),
                id: "carbons-error".into(),
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
            assert_state(&state, &conn, enabled);
        }
    }
}

#[tokio::test]
async fn foreign_carbons_before_binding_retains_authentication_error() {
    let state = create_test_websocket_state().await;
    for phase in [
        ConnectionPhase::new(),
        ConnectionPhase::authenticated(&origin()),
    ] {
        let (mut conn, _rx) = connection(&state, false).await;
        conn.phase = phase;
        let replies = dispatch(
            &state,
            &mut conn,
            request(Some("remote.test"), true, true, None),
        )
        .await;
        let [Iq::Error { error, .. }] = replies.as_slice() else {
            panic!("authentication error: {replies:?}")
        };
        assert_eq!(error.defined_condition, DefinedCondition::NotAuthorized);
        assert!(error.by.is_none(), "routing gate runs after binding");
        assert_state(&state, &conn, false);
    }
}

#[tokio::test]
async fn unregistered_carbons_namespace_keeps_feature_fallback_without_mutation() {
    let mut state = create_test_websocket_state().await;
    Arc::get_mut(&mut state)
        .expect("exclusive state")
        .deps
        .protocol
        .dispatcher = Arc::new(StanzaDispatcher::new());
    let (mut conn, _rx) = connection(&state, false).await;
    let replies = dispatch(
        &state,
        &mut conn,
        request(Some("remote.test"), true, true, Some(ORIGIN)),
    )
    .await;
    let [Iq::Error { error, .. }] = replies.as_slice() else {
        panic!("unregistered feature fallback: {replies:?}")
    };
    assert_eq!(
        error.defined_condition,
        DefinedCondition::FeatureNotImplemented
    );
    assert!(
        error.by.is_none(),
        "preflight requires a registered handler"
    );
    assert_state(&state, &conn, false);
}

#[tokio::test]
async fn foreign_carbons_settles_sm_once_and_local_controls_still_progress() {
    let state = create_test_websocket_state().await;
    let (mut conn, _rx) = connection(&state, false).await;
    conn.sm_state
        .enable("carbons-routing-sm".into(), false, None);
    let iq = request(Some("remote.test"), true, true, Some(ORIGIN));
    let replies = dispatch(&state, &mut conn, iq.clone()).await;
    assert_eq!(replies.len(), 1);
    assert_remote_error(&replies[0], &iq);
    assert_state(&state, &conn, false);
    assert_eq!(conn.sm_state.inbound_count(), 1);
    for (enable, count) in [(true, 2), (false, 3)] {
        let replies = dispatch(&state, &mut conn, request(None, enable, true, Some(ORIGIN))).await;
        assert!(matches!(replies.as_slice(), [Iq::Result { .. }]));
        assert_state(&state, &conn, enable);
        assert_eq!(conn.sm_state.inbound_count(), count);
    }
}
