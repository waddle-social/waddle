//! XEP-0424 across S2S, inbound: an occupant at another server retracts
//! its own message in a room of this deployment (ADR-0020 D10).
//!
//! The room applies to a remote occupant the rule it applies to a session
//! (§4: only the original sender), and for an occupant at another server,
//! which chooses its nickname, decides "the original sender" by the author
//! the archived message records, not by the occupant address alone. A
//! moderator's retraction of another occupant's message is XEP-0425.
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
    body, error_condition, local_groupchat, local_receives, local_receives_payload, naming,
    presence_between, remote_groupchat, remote_join, remote_leave, room_with_remote_occupant,
    stanza_id, statuses, ARRIVES, BOB, BOB_OCCUPANT, ROOM,
};
use waddle_server::s2s::route::RouterLimits;
use xmpp_parsers::message::Message;

const NS_RETRACT: &str = "urn:xmpp:message-retract:1";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

/// A retraction of the message the room identified as `target` (§4), with
/// the body a client that does not know the extension shows instead.
fn retraction(from: &str, id: &str, target: &str) -> Message {
    let mut message = remote_groupchat(from, ROOM, id, "/me retracted a previous message");
    message.payloads.push(naming("retract", NS_RETRACT, target));
    message
}

#[tokio::test]
async fn a_remote_occupant_retracts_its_own_message_and_no_other() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    hosted
        .from_peer(remote_groupchat(BOB, ROOM, "own-1", "said in haste"))
        .await;
    let said = local_receives(&mut alice, ARRIVES, |stanza| {
        body(stanza).as_deref() == Some("said in haste")
    })
    .await
    .expect("the message for the local occupant");
    // §3: in a room a retraction names the room's stanza-id.
    let own = stanza_id(&said, ROOM).expect("the room's stanza-id");
    local_groupchat(&mut alice, ROOM, "theirs-1", "hands off").await;
    let copy = hosted
        .at_peer(|stanza| body(stanza).as_deref() == Some("hands off"))
        .await
        .expect("the remote occupant's copy");
    let theirs = stanza_id(&copy, ROOM).expect("the room's stanza-id");

    hosted.from_peer(retraction(BOB, "meddle-1", &theirs)).await;
    let refused = hosted.answered("meddle-1").await;
    assert_eq!(refused.attr("type"), Some("error"), "{refused:?}");
    assert_eq!(error_condition(&refused).as_deref(), Some("forbidden"));

    hosted.from_peer(retraction(BOB, "own-2", &own)).await;
    let retracted = local_receives_payload(&mut alice, BOB_OCCUPANT, "retract", NS_RETRACT).await;
    assert!(retracted.is_some(), "the retraction for the local occupant");
    assert!(hosted
        .captured
        .with_id("own-2")
        .iter()
        .all(|stanza| stanza.attr("type") != Some("error")));
}

#[tokio::test]
async fn an_occupant_that_takes_a_departed_occupants_nickname_retracts_none_of_its_messages() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    hosted
        .from_peer(remote_groupchat(BOB, ROOM, "kept-1", "said by bob"))
        .await;
    let said = local_receives(&mut alice, ARRIVES, |stanza| {
        body(stanza).as_deref() == Some("said by bob")
    })
    .await
    .expect("the message for the local occupant");
    let kept = stanza_id(&said, ROOM).expect("the room's stanza-id");
    hosted
        .from_peer(remote_leave(BOB, ROOM, "bob", "leave-1"))
        .await;
    assert!(hosted.unseated(ROOM, BOB).await);

    // Another account of the same server enters under the nickname the
    // archived message carries. The room compares a retraction's sender
    // with the archived occupant address, which now matches.
    let mallory = "mallory@home-b.test/phone";
    hosted
        .from_peer(remote_join(mallory, ROOM, "bob", "join-2"))
        .await;
    hosted
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, mallory)
                && statuses(stanza).contains(&"110".to_owned())
        })
        .await
        .expect("the room's own presence for the second occupant");
    hosted.from_peer(retraction(mallory, "take-1", &kept)).await;
    let refused = hosted.answered("take-1").await;
    assert_eq!(error_condition(&refused).as_deref(), Some("forbidden"));
    assert!(
        local_receives_payload(&mut alice, BOB_OCCUPANT, "retract", NS_RETRACT)
            .await
            .is_none(),
        "a retraction by another account reached the room"
    );
}
