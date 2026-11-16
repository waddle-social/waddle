//! XEP-0333 across S2S, inbound: the chat markers of an occupant at
//! another server in a room of this deployment (ADR-0020 D10).
//!
//! The room relays a remote occupant's marker to its occupants. A
//! displayed marker marks an inbox row read only for an account of this
//! deployment: a remote occupant's read state is its own server's, and no
//! row is written for it here.
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

use s2s_inbound_muc_support::{
    body, local_groupchat, local_receives_payload, naming, remote_payload,
    room_with_remote_occupant, stanza_id, BOB, BOB_OCCUPANT, ROOM,
};
use waddle_server::s2s::route::RouterLimits;

const NS_MARKERS: &str = "urn:xmpp:chat-markers:0";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

#[tokio::test]
async fn a_remote_occupants_displayed_marker_reaches_the_room_and_marks_no_inbox_row() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    local_groupchat(&mut alice, ROOM, "local-1", "read this").await;
    let copy = hosted
        .at_peer(|stanza| body(stanza).as_deref() == Some("read this"))
        .await
        .expect("the remote occupant's copy");
    // §4: in a room a marker names the room's stanza-id.
    let read = stanza_id(&copy, ROOM).expect("the room's stanza-id");
    hosted
        .from_peer(remote_payload(
            BOB,
            ROOM,
            "marker-1",
            naming("displayed", NS_MARKERS, &read),
        ))
        .await;
    let marker = local_receives_payload(&mut alice, BOB_OCCUPANT, "displayed", NS_MARKERS).await;
    assert!(marker.is_some(), "the marker for the local occupant");
    assert!(hosted
        .captured
        .with_id("marker-1")
        .iter()
        .all(|stanza| stanza.attr("type") != Some("error")));
    let bob = "bob@home-b.test";
    assert_eq!(
        hosted.rows_naming("inbox_entries", "user_jid", bob).await,
        0
    );
    assert_eq!(
        hosted
            .rows_naming("notification_activity", "owner_bare_jid", bob)
            .await,
        0
    );
}
