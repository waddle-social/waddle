//! XEP-0461 across S2S, inbound: an occupant at another server replies to
//! a message in a room of this deployment (ADR-0020 D10).
//!
//! The room carries the `reply` element and the XEP-0428 `fallback` that
//! marks the quoted part of the body, and applies to them the checks it
//! applies to a session's.
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
    body, error_condition, local_groupchat, local_receives, naming, remote_groupchat,
    room_with_remote_occupant, stanza_id, ARRIVES, BOB, BOB_OCCUPANT, QUIET, ROOM,
};
use waddle_server::s2s::route::RouterLimits;

const NS_REPLY: &str = "urn:xmpp:reply:0";
const NS_FALLBACK: &str = "urn:xmpp:fallback:0";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

#[tokio::test]
async fn a_remote_occupants_reply_reaches_the_room_with_its_fallback() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    local_groupchat(&mut alice, ROOM, "asked-1", "who is in?").await;
    let copy = hosted
        .at_peer(|stanza| body(stanza).as_deref() == Some("who is in?"))
        .await
        .expect("the remote occupant's copy");
    // §3.1: in a room a reply names the room's stanza-id.
    let asked = stanza_id(&copy, ROOM).expect("the room's stanza-id");
    let mut reply = remote_groupchat(BOB, ROOM, "reply-1", "> who is in?\nme");
    reply.payloads.push(naming("reply", NS_REPLY, &asked));
    reply.payloads.push(
        Element::builder("fallback", NS_FALLBACK)
            .attr(minidom::rxml::xml_ncname!("for").to_owned(), NS_REPLY)
            .build(),
    );
    hosted.from_peer(reply).await;
    let received = local_receives(&mut alice, ARRIVES, |stanza| {
        stanza.name() == "message" && stanza.attr("from") == Some(BOB_OCCUPANT)
    })
    .await
    .expect("the reply for the local occupant");
    let named = received
        .children()
        .find(|child| child.is("reply", NS_REPLY))
        .and_then(|child| child.attr("id").map(str::to_owned));
    assert_eq!(named.as_deref(), Some(asked.as_str()), "{received:?}");
    assert!(
        received
            .children()
            .any(|child| child.is("fallback", NS_FALLBACK)),
        "{received:?}"
    );
}

#[tokio::test]
async fn a_reply_that_names_no_message_is_refused_as_a_sessions_is() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    let mut reply = remote_groupchat(BOB, ROOM, "reply-2", "to nothing");
    reply
        .payloads
        .push(Element::builder("reply", NS_REPLY).build());
    hosted.from_peer(reply).await;
    let refused = hosted.answered("reply-2").await;
    assert_eq!(refused.attr("type"), Some("error"), "{refused:?}");
    assert_eq!(error_condition(&refused).as_deref(), Some("bad-request"));
    assert!(
        local_receives(&mut alice, QUIET, |stanza| body(stanza).as_deref()
            == Some("to nothing"))
        .await
        .is_none(),
        "a malformed reply reached the room"
    );
}
