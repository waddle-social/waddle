use super::super::frame::{handle_xmpp_frame, TEST_LOCAL_ROOM_PLAN_ORIGINS};
use super::super::state::{authenticated_client_room_origin, WsConnState};
use super::{create_test_server_owner_session, create_test_websocket_state, snapshot_room};
use crate::auth::Session;
use crate::server::routes::interpret::effects::ClientRoomOrigin;
#[cfg(feature = "clustering")]
use crate::server::routes::interpret::effects::RoomExecutionPath;
use crate::server::routes::websocket::WebSocketState;
use jid::{BareJid, FullJid};
use std::sync::{Arc, Mutex};
use waddle_xmpp::protocol::ConnectionPhase;

fn origin(conn: &WsConnState) -> Option<ClientRoomOrigin> {
    authenticated_client_room_origin(
        &conn.phase,
        conn.authenticated_session.as_ref(),
        conn.occupancy_session,
    )
}

async fn bound_connection(state: &WebSocketState, session: &Session, mobile: bool) -> WsConnState {
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::authenticated(&jid);
    conn.authenticated_session = Some(session.clone());
    let bind = if mobile {
        "<iq xmlns='jabber:client' type='set' id='bind'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource>mobile</resource></bind></iq>"
    } else {
        "<iq xmlns='jabber:client' type='set' id='bind'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource>web</resource></bind></iq>"
    };
    let responses = handle_xmpp_frame(bind, "example.com", state, &mut conn).await;
    assert!(responses.iter().any(|response| response.contains("result")));
    let bound = conn.phase.bound_jid().expect("bound jid").clone();
    conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        bound,
        false,
        Default::default(),
    );
    conn
}

#[tokio::test]
async fn local_join_and_following_groupchat_share_authenticated_origin() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(&state, "alice").await;
    let mut conn = bound_connection(&state, &session, false).await;
    let expected = origin(&conn).expect("authenticated bound origin");
    let room: BareJid = "origin-test@muc.example.com".parse().expect("room");
    let jid = expected.full_jid.clone();

    let join = "<presence xmlns='jabber:client' from='mallory@example.com/forged' to='origin-test@muc.example.com/alice'><x xmlns='http://jabber.org/protocol/muc'/></presence>";
    let join_responses = handle_xmpp_frame(join, "example.com", &state, &mut conn).await;
    assert!(!join_responses
        .iter()
        .any(|response| response.contains("type='error'")));
    let snapshot = snapshot_room(&state, &room).await;
    assert_eq!(
        snapshot.room.session_generation(&jid),
        Some(expected.occupancy_session),
        "the actual join must use the bound JID and connection generation"
    );

    let captured = Arc::new(Mutex::new(Vec::new()));
    let message = "<message xmlns='jabber:client' from='mallory@example.com/forged' to='origin-test@muc.example.com' type='groupchat' id='after-join'><body>hello</body></message>";
    TEST_LOCAL_ROOM_PLAN_ORIGINS
        .scope(Arc::clone(&captured), async {
            handle_xmpp_frame(message, "example.com", &state, &mut conn).await;
        })
        .await;
    assert_eq!(
        *captured.lock().expect("room plan origins"),
        vec![Some(expected.clone())],
        "the following groupchat plan must retain the joined connection origin"
    );

    // Owner-side relay planning has no local connection generation, even when
    // its sender JID matches the locally joined resource.
    #[cfg(feature = "clustering")]
    {
        let deps = super::super::interpret_loop::build_interpret_deps(&state, None);
        let mut relayed = xmpp_parsers::message::Message::new(Some(room.clone().into()));
        relayed.from = Some(jid.into());
        relayed.type_ = xmpp_parsers::message::MessageType::Groupchat;
        relayed
            .bodies
            .insert(Default::default(), "relay".to_owned());
        let plan = crate::server::routes::interpret::plan_muc_for_relay(&deps, room, relayed).await;
        assert!(matches!(
            plan.room_execution,
            RoomExecutionPath::Local {
                client_origin: None,
                ..
            }
        ));
    }
}

#[tokio::test]
async fn fresh_rebind_and_sibling_resource_have_distinct_origins() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(&state, "alice").await;
    let first = bound_connection(&state, &session, false).await;
    let replacement = bound_connection(&state, &session, false).await;
    let sibling = bound_connection(&state, &session, true).await;
    let first_origin = origin(&first).expect("first origin");
    let replacement_origin = origin(&replacement).expect("replacement origin");
    let sibling_origin = origin(&sibling).expect("sibling origin");
    assert_eq!(first_origin.full_jid, replacement_origin.full_jid);
    assert_ne!(
        first_origin.occupancy_session,
        replacement_origin.occupancy_session
    );
    assert_ne!(first_origin, replacement_origin);
    assert_ne!(first_origin.full_jid, sibling_origin.full_jid);
    assert_ne!(first_origin, sibling_origin);

    let mut unauthenticated = bound_connection(&state, &session, false).await;
    unauthenticated.authenticated_session = None;
    assert!(origin(&unauthenticated).is_none());
    unauthenticated.authenticated_session =
        Some(Session::new("mallory@example.com", "mallory", "mallory"));
    assert!(origin(&unauthenticated).is_none());
}
