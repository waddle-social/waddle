//! XEP-0359 across S2S, inbound: the identifiers of a message an occupant
//! at another server sends to a room of this deployment (ADR-0020 D10).
//!
//! The `origin-id` the sender's client gave the message is carried. A
//! `stanza-id` is the room's to assign (§3): one the sender supplies in
//! the room's name is not carried, and the room stamps its own.
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
    body, local_receives, naming, remote_groupchat, room_with_remote_occupant, stanza_id, ARRIVES,
    BOB, BOB_OCCUPANT, ROOM,
};
use waddle_server::s2s::route::RouterLimits;

const NS_SID: &str = "urn:xmpp:sid:0";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

#[tokio::test]
async fn the_origin_id_is_carried_and_a_stanza_id_in_the_rooms_name_is_replaced() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    let mut message = remote_groupchat(BOB, ROOM, "ids-1", "with identifiers");
    message
        .payloads
        .push(naming("origin-id", NS_SID, "client-origin-1"));
    message.payloads.push(
        Element::builder("stanza-id", NS_SID)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "forged-1")
            .attr(minidom::rxml::xml_ncname!("by").to_owned(), ROOM)
            .build(),
    );
    hosted.peer_sends(message).await;
    let received = local_receives(&mut alice, ARRIVES, |stanza| {
        body(stanza).as_deref() == Some("with identifiers")
    })
    .await
    .expect("the message for the local occupant");
    let origin = received
        .children()
        .find(|child| child.is("origin-id", NS_SID))
        .and_then(|child| child.attr("id").map(str::to_owned));
    assert_eq!(origin.as_deref(), Some("client-origin-1"), "{received:?}");
    let by_room: Vec<&Element> = received
        .children()
        .filter(|child| child.is("stanza-id", NS_SID) && child.attr("by") == Some(ROOM))
        .collect();
    assert_eq!(by_room.len(), 1, "{received:?}");
    assert_ne!(stanza_id(&received, ROOM).as_deref(), Some("forged-1"));
    // The sender's reflection carries the room's identifier too.
    let reflection = hosted
        .at_peer(|stanza| {
            stanza.name() == "message"
                && stanza.attr("from") == Some(BOB_OCCUPANT)
                && stanza.attr("to") == Some(BOB)
                && body(stanza).as_deref() == Some("with identifiers")
        })
        .await
        .expect("the reflection for the remote sender");
    assert_eq!(stanza_id(&reflection, ROOM), stanza_id(&received, ROOM));
}
