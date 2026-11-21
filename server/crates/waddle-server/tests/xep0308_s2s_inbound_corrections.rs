//! XEP-0308 across S2S, inbound: an occupant at another server corrects
//! its own message in a room of this deployment (ADR-0020 D10).
//!
//! The room applies to a remote occupant the rule it applies to a session
//! (§3: only the original sender), and for an occupant at another server,
//! which chooses its nickname, decides "the original sender" by the author
//! the archived message records, not by the occupant address alone.
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
    statuses, ARRIVES, BOB, BOB_OCCUPANT, ROOM,
};
use waddle_server::s2s::route::RouterLimits;

const NS_CORRECT: &str = "urn:xmpp:message-correct:0";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

#[tokio::test]
async fn a_remote_occupant_corrects_its_own_message_and_no_other() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    hosted
        .peer_sends(remote_groupchat(BOB, ROOM, "own-1", "frist"))
        .await;
    local_receives(&mut alice, ARRIVES, |stanza| {
        body(stanza).as_deref() == Some("frist")
    })
    .await
    .expect("the message for the local occupant");
    local_groupchat(&mut alice, ROOM, "theirs-1", "hands off").await;
    hosted
        .at_peer(|stanza| body(stanza).as_deref() == Some("hands off"))
        .await
        .expect("the remote occupant's copy");

    // §3: a correction of somebody else's message is refused.
    let mut meddling = remote_groupchat(BOB, ROOM, "meddle-1", "mine now");
    meddling
        .payloads
        .push(naming("replace", NS_CORRECT, "theirs-1"));
    hosted.peer_sends(meddling).await;
    let refused = hosted.answered("meddle-1").await;
    assert_eq!(refused.attr("type"), Some("error"), "{refused:?}");
    assert_eq!(error_condition(&refused).as_deref(), Some("forbidden"));

    // Its own message it corrects, and the room relays the correction.
    let mut correction = remote_groupchat(BOB, ROOM, "own-2", "first");
    correction
        .payloads
        .push(naming("replace", NS_CORRECT, "own-1"));
    hosted.peer_sends(correction).await;
    let corrected = local_receives_payload(&mut alice, BOB_OCCUPANT, "replace", NS_CORRECT)
        .await
        .expect("the correction for the local occupant");
    assert_eq!(body(&corrected).as_deref(), Some("first"));
}

#[tokio::test]
async fn an_occupant_that_takes_a_departed_occupants_nickname_corrects_none_of_its_messages() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    hosted
        .peer_sends(remote_groupchat(BOB, ROOM, "kept-1", "said by bob"))
        .await;
    local_receives(&mut alice, ARRIVES, |stanza| {
        body(stanza).as_deref() == Some("said by bob")
    })
    .await
    .expect("the message for the local occupant");
    hosted
        .peer_sends(remote_leave(BOB, ROOM, "bob", "leave-1"))
        .await;
    assert!(hosted.unseated(ROOM, BOB).await);

    // Another account of the same server enters under the nickname the
    // archived message carries.
    let mallory = "mallory@home-b.test/phone";
    hosted
        .peer_sends(remote_join(mallory, ROOM, "bob", "join-2"))
        .await;
    hosted
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, mallory)
                && statuses(stanza).contains(&"110".to_owned())
        })
        .await
        .expect("the room's own presence for the second occupant");
    let mut correction = remote_groupchat(mallory, ROOM, "take-1", "said by mallory");
    correction
        .payloads
        .push(naming("replace", NS_CORRECT, "kept-1"));
    hosted.peer_sends(correction).await;
    let refused = hosted.answered("take-1").await;
    assert_eq!(error_condition(&refused).as_deref(), Some("forbidden"));
    assert!(
        local_receives_payload(&mut alice, BOB_OCCUPANT, "replace", NS_CORRECT)
            .await
            .is_none(),
        "a correction by another account reached the room"
    );
}
