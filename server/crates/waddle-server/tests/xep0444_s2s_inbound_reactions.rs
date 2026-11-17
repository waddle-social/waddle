//! XEP-0444 across S2S, inbound: an occupant at another server reacts to a
//! message in a room of this deployment (ADR-0020 D10).
//!
//! The room relays and archives the reaction as it does a session's; what
//! lets the occupant send it is its seat in the room.
//!
//! The deployments are those of `s2s_inbound_muc_support`: `example.com`
//! runs the production room host, S2S router and connection manager over a
//! WebSocket state with an in-memory SQLite database, and `home-b.test` is
//! a second connection manager that keeps what it receives. No PostgreSQL,
//! SpiceDB or second process is involved.
#![cfg(feature = "test-support")]

// The fixture serves several suites; each uses a part of it.
#[allow(dead_code, unused_imports)]
mod s2s_inbound_muc_support;

use minidom::Element;
use s2s_inbound_muc_support::{
    body, error_condition, local_groupchat, local_receives_payload, naming, remote_payload,
    room_with_remote_occupant, stanza_id, Hosted, BOB, BOB_OCCUPANT, QUIET, ROOM,
};
use waddle_server::s2s::route::RouterLimits;

const NS_REACTIONS: &str = "urn:xmpp:reactions:0";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

#[tokio::test]
async fn a_remote_occupant_reacts_to_a_message_of_the_room() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    local_groupchat(&mut alice, ROOM, "liked-1", "a fine point").await;
    let copy = hosted
        .at_peer(|stanza| body(stanza).as_deref() == Some("a fine point"))
        .await
        .expect("the remote occupant's copy");
    let liked = stanza_id(&copy, ROOM).expect("the room's stanza-id");
    // XEP-0444 §4.2: in a room a reaction names the room's stanza-id.
    let mut reactions = naming("reactions", NS_REACTIONS, &liked);
    reactions.append_child(
        Element::builder("reaction", NS_REACTIONS)
            .append("\u{1f44d}")
            .build(),
    );
    hosted
        .peer_sends(remote_payload(BOB, ROOM, "react-1", reactions))
        .await;
    let received = local_receives_payload(&mut alice, BOB_OCCUPANT, "reactions", NS_REACTIONS)
        .await
        .expect("the reaction for the local occupant");
    let named = received
        .children()
        .find(|child| child.is("reactions", NS_REACTIONS))
        .and_then(|child| child.attr("id").map(str::to_owned));
    assert_eq!(named.as_deref(), Some(liked.as_str()));
}

#[tokio::test]
async fn a_reaction_from_an_entity_that_holds_no_seat_is_refused() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    let mut alice = hosted.room_creator("alice", "desk").await;
    s2s_inbound_muc_support::local_join(&mut alice, ROOM, "alice").await;
    let mut reactions = naming("reactions", NS_REACTIONS, "anything-1");
    reactions.append_child(
        Element::builder("reaction", NS_REACTIONS)
            .append("\u{1f44d}")
            .build(),
    );
    hosted
        .peer_sends(remote_payload(BOB, ROOM, "stranger-1", reactions))
        .await;
    // XEP-0045 §7.4: only an occupant sends to the room.
    let refused = hosted.answered("stranger-1").await;
    assert_eq!(error_condition(&refused).as_deref(), Some("not-acceptable"));
    tokio::time::sleep(QUIET).await;
    assert!(
        local_receives_payload(&mut alice, BOB_OCCUPANT, "reactions", NS_REACTIONS)
            .await
            .is_none(),
        "a non-occupant's reaction reached the room"
    );
}
