//! XEP-0045 §§7.2/7.14: the two orderings of fresh-bind replacement.
//!
//! Await the production join and terminal-cleanup paths in either order, then
//! inspect their typed outbound channel. This proves presence decisions without
//! timing a socket race or accepting both outcomes in a single assertion.

use super::super::cleanup::{cleanup_connection_shutdown, ConnectionShutdownOutcome};
use super::*;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

struct ReplacementFixture {
    state: Arc<WebSocketState>,
    room: BareJid,
    alice: FullJid,
    bob: FullJid,
    old: WsConnState,
    old_rx: mpsc::Receiver<OutboundStanza>,
    replacement: WsConnState,
    _replacement_rx: mpsc::Receiver<OutboundStanza>,
    observer_rx: mpsc::Receiver<OutboundStanza>,
}

impl ReplacementFixture {
    async fn new() -> Self {
        let state = create_test_websocket_state().await;
        let session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room: BareJid = "reconnect-contract@muc.example.com"
            .parse()
            .expect("room JID");
        let alice: FullJid = "alice@example.com/phone".parse().expect("alice JID");
        let bob: FullJid = "bob@example.com/web".parse().expect("bob JID");
        let (old_tx, old_rx) = mpsc::channel(16);
        let mut old = WsConnState::new();
        old.phase = ConnectionPhase::ready(alice.clone(), false);
        old.authenticated_session = Some(session.clone());
        old.registry_owner = Some(register_test_connection(&state, &alice, old_tx).await);
        let responses = handle_muc_join_with_occupancy_session(
            &state,
            "example.com",
            &room,
            &alice,
            "alice",
            None,
            (old.occupancy_session, &old.authenticated_session),
        )
        .await;
        assert!(responses.iter().any(|frame| frame.contains("<subject")));

        let (observer_tx, observer_rx) = mpsc::channel(16);
        register_test_connection(&state, &bob, observer_tx).await;
        let responses =
            handle_muc_join(&state, "example.com", &room, &bob, "bob", None, &None).await;
        assert!(responses.iter().any(|frame| frame.contains("<subject")));

        // The replacement has bound the same full JID but joined no rooms.
        let (replacement_tx, replacement_rx) = mpsc::channel(16);
        let mut replacement = WsConnState::new();
        replacement.phase = ConnectionPhase::ready(alice.clone(), false);
        replacement.authenticated_session = Some(session);
        replacement.registry_owner =
            Some(register_test_connection(&state, &alice, replacement_tx).await);
        assert_ne!(old.occupancy_session, replacement.occupancy_session);

        Self {
            state,
            room,
            alice,
            bob,
            old,
            old_rx,
            replacement,
            _replacement_rx: replacement_rx,
            observer_rx,
        }
    }

    async fn cleanup_old(&mut self) {
        assert_eq!(
            cleanup_connection_shutdown(&self.state, &mut self.old_rx, &mut self.old, true).await,
            ConnectionShutdownOutcome::NotPersisted,
        );
        assert!(self
            .state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(
                &self.alice,
                self.replacement
                    .registry_owner
                    .as_ref()
                    .expect("replacement owner"),
            )
            .is_some());
    }

    async fn rejoin(&self) {
        let responses = handle_muc_join_real(
            &self.state,
            "example.com",
            &self.room,
            &self.alice,
            "alice",
            None,
            handlers::presence::MucJoinConnectionContext {
                registry_owner: self.replacement.registry_owner.as_ref(),
                occupancy_session: self.replacement.occupancy_session,
                authenticated_session: &self.replacement.authenticated_session,
            },
        )
        .await;
        assert!(responses.iter().any(|frame| frame.contains("<subject")));
    }

    fn take_presence(&mut self, expected_type: PresenceType) -> Presence {
        let outbound = self.observer_rx.try_recv().expect("observer presence");
        let Stanza::Presence(presence) = outbound.stanza else {
            panic!("expected typed presence");
        };
        assert_eq!(presence.type_, expected_type);
        assert_eq!(
            presence.from,
            Some(
                self.room
                    .with_resource_str("alice")
                    .expect("occupant JID")
                    .into()
            )
        );
        assert_eq!(presence.to, Some(self.bob.clone().into()));
        presence
    }

    fn assert_no_more_presence(&mut self) {
        assert!(
            matches!(
                self.observer_rx.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "completed join/cleanup must not enqueue another presence for the observer"
        );
    }

    async fn assert_replacement_seated(&self) {
        let room = snapshot_room(&self.state, &self.room).await.room;
        assert_eq!(
            room.session_generation(&self.alice),
            Some(self.replacement.occupancy_session)
        );
        assert_eq!(room.occupants.len(), 2);
    }
}

#[tokio::test]
async fn cleanup_before_rejoin_announces_unavailable_then_available() {
    let mut fixture = ReplacementFixture::new().await;
    fixture.cleanup_old().await;
    let leave = fixture.take_presence(PresenceType::Unavailable);
    let muc_user = leave
        .payloads
        .iter()
        .find(|payload| payload.is("x", waddle_xmpp::muc::presence::NS_MUC_USER))
        .expect("XEP-0045 user payload");
    assert_eq!(
        muc_user
            .get_child("item", waddle_xmpp::muc::presence::NS_MUC_USER)
            .expect("leave item")
            .attr("role"),
        Some("none")
    );
    assert_eq!(
        snapshot_room(&fixture.state, &fixture.room)
            .await
            .room
            .session_generation(&fixture.alice),
        None,
        "fresh binding alone must not inherit the departed room occupancy"
    );
    fixture.assert_no_more_presence();

    fixture.rejoin().await;
    fixture.take_presence(PresenceType::None);
    fixture.assert_no_more_presence();
    fixture.assert_replacement_seated().await;
}

#[tokio::test]
async fn rejoin_before_cleanup_suppresses_the_predecessors_departure() {
    let mut fixture = ReplacementFixture::new().await;
    fixture.rejoin().await;
    fixture.assert_replacement_seated().await;
    fixture.assert_no_more_presence();

    fixture.cleanup_old().await;
    fixture.assert_replacement_seated().await;
    fixture.assert_no_more_presence();

    // The surviving generation can still explicitly leave. A blanket leave
    // suppression would pass the absence assertion but fail this broadcast.
    handle_muc_leave_with_occupancy_session(
        &fixture.state,
        &fixture.room,
        &fixture.alice,
        "alice",
        fixture.replacement.occupancy_session,
        None,
    )
    .await;
    fixture.take_presence(PresenceType::Unavailable);
    fixture.assert_no_more_presence();
}
