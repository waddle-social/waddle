//! Parser-valid bound discovery requests must not borrow a foreign identity.
//! The existing parser checks IQ type/query name/namespace, not a full XML schema.
//! Pre-bind foreign discovery policy is deliberately outside this slice.

use std::sync::Arc;
use std::time::Duration;

use jid::{FullJid, Jid};
use minidom::Element;
use tokio::sync::mpsc;
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use waddle_xmpp::protocol::ConnectionPhase;
use waddle_xmpp::registry::OutboundStanza;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::super::frame::handle_xmpp_frame;
use super::super::state::WsConnState;
use super::super::WebSocketState;
use super::create_test_websocket_state_with_fixture_bot;
use super::{
    create_test_websocket_state, register_test_connection, seed_local_account, stanza_to_xml,
};

const DOMAIN: &str = "example.com";
const ORIGIN: &str = "alice@example.com/web";

#[derive(Clone, Copy, Debug)]
enum Discovery {
    Info,
    Items,
}

impl Discovery {
    fn namespace(self) -> &'static str {
        match self {
            Self::Info => DISCO_INFO_NS,
            Self::Items => DISCO_ITEMS_NS,
        }
    }
}

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

fn request(kind: Discovery, target: Option<&str>, node: Option<&str>, from: Option<&str>) -> Iq {
    let mut query = Element::builder("query", kind.namespace());
    if let Some(node) = node {
        query = query.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    Iq::Get {
        from: from.map(|value| value.parse().expect("request from")),
        to: target.map(|value| value.parse().expect("request to")),
        id: "discovery<&-correlation".into(),
        payload: query.build(),
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

fn assert_remote_error(reply: &Iq, request: &Iq) {
    assert_eq!(reply.id(), request.id());
    assert_eq!(reply.from(), request.to());
    assert_eq!(
        reply.to(),
        Some(&Jid::from(origin())),
        "bound origin, not client from"
    );
    let Iq::Error { error, payload, .. } = reply else {
        panic!("foreign discovery cannot return local identity/items: {reply:?}")
    };
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(error.defined_condition, DefinedCondition::NotAllowed);
    assert_eq!(error.by, Some(DOMAIN.parse().expect("home domain")));
    assert!(error
        .texts
        .values()
        .any(|text| text == "Remote service discovery routing is not allowed by this server."));
    let Iq::Get {
        payload: original, ..
    } = request
    else {
        panic!("discovery request")
    };
    assert_eq!(payload.as_ref(), Some(original));
}

fn result_query(reply: &Iq, kind: Discovery) -> &Element {
    let Iq::Result {
        payload: Some(query),
        ..
    } = reply
    else {
        panic!("local discovery result: {reply:?}")
    };
    assert_eq!(query.name(), "query");
    assert_eq!(query.ns(), kind.namespace());
    query
}

#[tokio::test]
async fn bound_foreign_discovery_gets_return_exact_locally_attributed_errors() {
    let state = create_test_websocket_state().await;
    let (mut conn, mut rx) = connection(&state).await;
    assert!(
        conn.authenticated_session.is_none(),
        "bound phase is the routing identity"
    );
    for kind in [Discovery::Info, Discovery::Items] {
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
            for node in [None, Some("foreign-node<&")] {
                for from in [None, Some(ORIGIN), Some("forged@elsewhere.test/resource")] {
                    let iq = request(kind, Some(target), node, from);
                    let replies = dispatch(&state, &mut conn, iq.clone()).await;
                    assert_eq!(replies.len(), 1, "{kind:?} {target}");
                    assert_remote_error(&replies[0], &iq);
                    assert!(conn.phase.is_ready());
                }
            }
        }
    }
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn configured_components_are_local_and_replaced_defaults_are_foreign() {
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
    for (target, category, identity_type) in [
        ("rooms.custom.test", "conference", "text"),
        ("spaces.custom.test", "pubsub", "service"),
        ("upload.custom.test", "store", "file"),
        ("community.custom.test", "pubsub", "service"),
        ("extensions.custom.test", "pubsub", "service"),
        ("push.custom.test", "pubsub", "push"),
        ("calls.example.com", "conference", "audio-video"),
    ] {
        let replies = dispatch(
            &state,
            &mut conn,
            request(Discovery::Info, Some(target), None, None),
        )
        .await;
        let [reply] = replies.as_slice() else {
            panic!("component reply: {replies:?}")
        };
        assert!(result_query(reply, Discovery::Info)
            .children()
            .any(|child| child.name() == "identity"
                && child.attr("category") == Some(category)
                && child.attr("type") == Some(identity_type)));
    }
    for kind in [Discovery::Info, Discovery::Items] {
        for target in [
            "muc.example.com",
            "spaces.example.com",
            "upload.example.com",
            "community.example.com",
            "extensions.example.com",
            "push.example.com",
        ] {
            let iq = request(kind, Some(target), None, None);
            let replies = dispatch(&state, &mut conn, iq.clone()).await;
            assert_eq!(replies.len(), 1);
            assert_remote_error(&replies[0], &iq);
        }
    }
}

#[tokio::test]
async fn local_server_and_bare_accounts_keep_their_discovery_handlers() {
    let state = create_test_websocket_state().await;
    seed_local_account(&state, "bob").await;
    let (mut conn, _rx) = connection(&state).await;
    for kind in [Discovery::Info, Discovery::Items] {
        for target in [
            None,
            Some(DOMAIN),
            Some("alice@example.com"),
            Some("bob@example.com"),
        ] {
            let replies =
                dispatch(&state, &mut conn, request(kind, target, None, Some(ORIGIN))).await;
            let [reply] = replies.as_slice() else {
                panic!("local reply: {replies:?}")
            };
            result_query(reply, kind);
        }
    }
    let replies = dispatch(
        &state,
        &mut conn,
        request(Discovery::Info, Some("bob@example.com"), None, None),
    )
    .await;
    let query = result_query(&replies[0], Discovery::Info);
    assert!(query
        .children()
        .any(|child| child.name() == "identity" && child.attr("category") == Some("pubsub")));
    assert!(!query
        .children()
        .any(|child| child.name() == "identity" && child.attr("category") == Some("server")));
    let replies = dispatch(
        &state,
        &mut conn,
        request(Discovery::Info, Some("missing@example.com"), None, None),
    )
    .await;
    assert!(
        matches!(replies.as_slice(), [Iq::Error { error, .. }] if error.defined_condition == DefinedCondition::ItemNotFound)
    );
}

#[tokio::test]
async fn local_full_jid_discovery_still_forwards_to_the_peer() {
    let state = create_test_websocket_state().await;
    let (mut conn, _origin_rx) = connection(&state).await;
    let peer: FullJid = "bob@example.com/phone".parse().expect("peer");
    let (tx, mut rx) = mpsc::channel(8);
    register_test_connection(&state, &peer, tx).await;
    for kind in [Discovery::Info, Discovery::Items] {
        let mut iq = request(
            kind,
            Some(peer.as_str()),
            Some("peer-node"),
            Some("forged@elsewhere.test/resource"),
        );
        assert!(dispatch(&state, &mut conn, iq.clone()).await.is_empty());
        let forwarded = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("prompt forwarding")
            .expect("connected peer");
        let Stanza::Iq(forwarded) = forwarded.stanza else {
            panic!("peer IQ")
        };
        *iq.from_mut() = Some(Jid::from(origin()));
        assert_eq!(*forwarded, iq);
    }
    for kind in [Discovery::Info, Discovery::Items] {
        let replies = dispatch(
            &state,
            &mut conn,
            request(kind, Some("bob@example.com/offline"), None, None),
        )
        .await;
        assert!(
            matches!(replies.as_slice(), [Iq::Error { error, .. }] if error.defined_condition == DefinedCondition::ServiceUnavailable && error.by.is_none())
        );
    }
}

#[tokio::test]
async fn local_occupant_nodes_and_installed_bots_keep_specialized_handlers() {
    let state = create_test_websocket_state_with_fixture_bot().await;
    let (mut conn, _rx) = connection(&state).await;
    for kind in [Discovery::Info, Discovery::Items] {
        let replies = dispatch(
            &state,
            &mut conn,
            request(kind, Some("room@muc.example.com/nick"), None, None),
        )
        .await;
        assert!(
            matches!(replies.as_slice(), [Iq::Error { error, .. }] if error.defined_condition == DefinedCondition::BadRequest)
        );
        for target in [
            "message-hook-fixture@extensions.example.com",
            "message-hook-fixture@extensions.example.com/bot",
        ] {
            let replies =
                dispatch(&state, &mut conn, request(kind, Some(target), None, None)).await;
            let query = result_query(&replies[0], kind);
            match kind {
                Discovery::Info => {
                    assert!(query.children().any(|child| child.name() == "identity"
                        && child.attr("category") == Some("client")
                        && child.attr("type") == Some("bot")))
                }
                Discovery::Items => assert_eq!(query.children().count(), 0),
            }
        }
    }
    let replies = dispatch(
        &state,
        &mut conn,
        request(
            Discovery::Items,
            Some("muc.example.com"),
            Some("unknown-node"),
            None,
        ),
    )
    .await;
    assert!(
        matches!(replies.as_slice(), [Iq::Error { error, .. }] if error.defined_condition == DefinedCondition::ItemNotFound)
    );
}

#[tokio::test]
async fn malformed_payloads_and_iq_sets_keep_parser_errors() {
    let state = create_test_websocket_state().await;
    let (mut conn, _rx) = connection(&state).await;
    for kind in [Discovery::Info, Discovery::Items] {
        for set in [false, true] {
            let iq = request(kind, Some("remote.test"), None, None);
            let Iq::Get {
                from,
                to,
                id,
                payload,
            } = iq
            else {
                panic!("get")
            };
            let iq = if set {
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
                    payload: Element::builder("not-query", kind.namespace()).build(),
                }
            };
            let replies = dispatch(&state, &mut conn, iq).await;
            assert!(
                matches!(replies.as_slice(), [Iq::Error { error, .. }] if error.defined_condition == DefinedCondition::BadRequest && error.by.is_none())
            );
        }
    }
}

#[tokio::test]
async fn disco_results_and_errors_never_generate_a_reply() {
    let state = create_test_websocket_state().await;
    let (mut conn, mut rx) = connection(&state).await;
    for kind in [Discovery::Info, Discovery::Items] {
        for error in [false, true] {
            let Iq::Get {
                from,
                to,
                id,
                payload,
            } = request(kind, Some("remote.test"), None, None)
            else {
                panic!("get")
            };
            let iq = if error {
                Iq::Error {
                    from,
                    to,
                    id,
                    payload: Some(payload),
                    error: StanzaError::new(
                        ErrorType::Cancel,
                        DefinedCondition::NotAllowed,
                        "en",
                        "peer error",
                    ),
                }
            } else {
                Iq::Result {
                    from,
                    to,
                    id,
                    payload: Some(payload),
                }
            };
            assert!(dispatch(&state, &mut conn, iq).await.is_empty());
        }
    }
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn prebind_discovery_keeps_local_component_support_and_no_bound_attribution() {
    let state = create_test_websocket_state().await;
    for phase in [
        ConnectionPhase::new(),
        ConnectionPhase::authenticated(&origin()),
    ] {
        let (mut conn, _rx) = connection(&state).await;
        conn.phase = phase;
        let replies = dispatch(
            &state,
            &mut conn,
            request(Discovery::Info, Some("upload.example.com"), None, None),
        )
        .await;
        result_query(&replies[0], Discovery::Info);
        for kind in [Discovery::Info, Discovery::Items] {
            let replies = dispatch(
                &state,
                &mut conn,
                request(kind, Some("remote.test"), None, None),
            )
            .await;
            // This slice does not define or fix pre-bind foreign discovery.
            assert!(!replies
                .iter()
                .any(|reply| matches!(reply, Iq::Error { error, .. } if error.by.is_some())));
        }
    }
}

#[tokio::test]
async fn foreign_discovery_settles_sm_once_and_local_queries_continue() {
    let state = create_test_websocket_state().await;
    let (mut conn, _rx) = connection(&state).await;
    conn.sm_state
        .enable("discovery-routing-sm".into(), false, None);
    let mut count = 0;
    for kind in [Discovery::Info, Discovery::Items] {
        let iq = request(kind, Some("remote.test"), None, None);
        let replies = dispatch(&state, &mut conn, iq.clone()).await;
        assert_eq!(replies.len(), 1);
        assert_remote_error(&replies[0], &iq);
        count += 1;
        assert_eq!(conn.sm_state.inbound_count(), count);
        assert_eq!(conn.sm_inbound_completion.pending_count(), 0);
        assert!(!conn.sm_inbound_completion.has_unhandled_hole());
        let replies = dispatch(
            &state,
            &mut conn,
            request(kind, Some(DOMAIN), None, Some(ORIGIN)),
        )
        .await;
        result_query(&replies[0], kind);
        count += 1;
        assert_eq!(conn.sm_state.inbound_count(), count);
        assert_eq!(conn.sm_inbound_completion.pending_count(), 0);
        assert!(!conn.sm_inbound_completion.has_unhandled_hole());
        assert!(conn.phase.is_ready());
    }
}
