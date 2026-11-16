//! XEP-0085 across S2S, inbound: the chat states of an occupant at another
//! server in a room of this deployment (ADR-0020 D10).
//!
//! The room relays a remote occupant's chat state to its occupants (§5.5)
//! and archives nothing for it. A chat state feeds this deployment's push
//! filter only for its own accounts: none is recorded for a remote
//! occupant, which has no device here.
//!
//! A chat state is a hint nothing depends on (§5.1 lets an entity ignore
//! one). The copies a room sends remote occupants are bounded for each
//! sender, and a copy beyond the bound is dropped rather than queued; a
//! remote occupant's chat states count against its account's share of its
//! domain's request rate, and one beyond the share is neither relayed nor
//! answered. A message that carries only a chat state, of any type, and a
//! headline are never answered with an error.
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
    body, error_condition, local_groupchat, local_payload, local_receives, local_receives_payload,
    remote_groupchat, remote_payload, room_with_remote_occupant, ARRIVES, BOB, BOB_OCCUPANT, QUIET,
    ROOM,
};
use waddle_server::s2s::route::RouterLimits;
use xmpp_parsers::message::{Id, Message, MessageType};

const NS_CHATSTATES: &str = "http://jabber.org/protocol/chatstates";
/// Alice's occupant JID in [`ROOM`].
const ALICE_OCCUPANT: &str = "lounge@muc.example.com/alice";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

#[tokio::test]
async fn a_remote_occupants_chat_state_reaches_the_room_and_records_no_activity() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    for (id, state) in [("state-1", "composing"), ("state-2", "paused")] {
        hosted
            .from_peer(remote_payload(
                BOB,
                ROOM,
                id,
                Element::builder(state, NS_CHATSTATES).build(),
            ))
            .await;
        let relayed = local_receives_payload(&mut alice, BOB_OCCUPANT, state, NS_CHATSTATES).await;
        assert!(
            relayed.is_some(),
            "the {state} state for the local occupant"
        );
        assert!(hosted
            .captured
            .with_id(id)
            .iter()
            .all(|stanza| stanza.attr("type") != Some("error")));
    }
    assert_eq!(
        hosted
            .rows_naming("notification_activity", "owner_bare_jid", "bob@home-b.test")
            .await,
        0
    );
    assert!(hosted.seat(ROOM, BOB).is_some());
}

#[tokio::test]
async fn a_local_occupants_chat_states_for_a_remote_occupant_are_bounded_and_end_no_seat() {
    let limits = RouterLimits {
        // Four copies of one sender's chat states at once, one more each
        // second.
        seat_ephemeral_rate_per_sender: 1,
        ..RouterLimits::default()
    };
    let (hosted, mut alice, _own) = room_with_remote_occupant(limits, no_change).await;
    for index in 0..12 {
        local_payload(
            &mut alice,
            ROOM,
            &format!("state-{index}"),
            Element::builder("composing", NS_CHATSTATES).build(),
        )
        .await;
    }
    // A message with a text is always queued for the seat.
    local_groupchat(&mut alice, ROOM, "text-1", "after the chat states").await;
    hosted
        .at_peer(|stanza| body(stanza).as_deref() == Some("after the chat states"))
        .await
        .expect("the remote occupant's copy of the message");
    tokio::time::sleep(QUIET).await;
    let copies = hosted.count_at_peer(|stanza| {
        stanza.attr("to") == Some(BOB)
            && stanza
                .children()
                .any(|child| child.is("composing", NS_CHATSTATES))
    });
    assert!((1..=6).contains(&copies), "{copies}");
    // Dropping a chat state leaves the occupant seated.
    assert!(hosted.seat(ROOM, BOB).is_some());
}

#[tokio::test]
async fn a_remote_occupants_chat_states_count_against_its_accounts_share_and_are_not_answered() {
    let limits = RouterLimits {
        // Two stanzas of one account at once, one more each second.
        room_request_rate_per_account: 1,
        room_request_burst_per_account: 2,
        ..RouterLimits::default()
    };
    let (hosted, mut alice, _own) = room_with_remote_occupant(limits, no_change).await;
    // The join spent one of the two; give the share its time to refill.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let ids: Vec<String> = (0..6).map(|index| format!("state-{index}")).collect();
    for id in &ids {
        hosted
            .from_peer(remote_payload(
                BOB,
                ROOM,
                id,
                Element::builder("composing", NS_CHATSTATES).build(),
            ))
            .await;
    }
    // The share is spent by chat states as by any stanza: a message that
    // follows at once is refused, and answered, since it carries a text.
    hosted
        .from_peer(remote_groupchat(BOB, ROOM, "text-1", "too soon"))
        .await;
    let refused = hosted.answered("text-1").await;
    assert_eq!(refused.attr("type"), Some("error"), "{refused:?}");
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("resource-constraint")
    );
    // The chat states within the share reached the room.
    let relayed =
        local_receives_payload(&mut alice, BOB_OCCUPANT, "composing", NS_CHATSTATES).await;
    assert!(relayed.is_some(), "a chat state for the local occupant");
    // Those beyond it are dropped unanswered: an error for a chat state
    // would tell a client nothing it can act on. Only the sender's own
    // copies of those the room took come back with their ids.
    tokio::time::sleep(QUIET).await;
    let mut reflected = 0;
    for id in &ids {
        let answers = hosted.captured.with_id(id);
        assert!(
            answers
                .iter()
                .all(|stanza| stanza.attr("type") != Some("error")),
            "{id}: {answers:?}"
        );
        reflected += usize::from(!answers.is_empty());
    }
    assert!(reflected < ids.len(), "every chat state was taken");
    assert!(hosted.seat(ROOM, BOB).is_some());
}

#[tokio::test]
async fn a_lone_chat_state_or_a_headline_is_never_answered_with_an_error() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    // A chat state alone in a private message to an occupant, which the
    // service does not relay (XEP-0045 §7.5 private messages are not
    // offered to remote occupants), and a headline to the room: neither is
    // taken, and neither is answered.
    let mut private = Message::new_with_type(
        MessageType::Chat,
        Some(s2s_inbound_muc_support::jid(ALICE_OCCUPANT)),
    );
    private.from = Some(s2s_inbound_muc_support::jid(BOB));
    private.id = Some(Id("state-private".to_owned()));
    private
        .payloads
        .push(Element::builder("composing", NS_CHATSTATES).build());
    hosted.from_peer(private).await;
    let mut headline = remote_groupchat(BOB, ROOM, "headline-1", "news");
    headline.type_ = MessageType::Headline;
    hosted.from_peer(headline).await;
    // A message of the same room that follows is decided after both.
    hosted
        .from_peer(remote_groupchat(BOB, ROOM, "after-1", "after"))
        .await;
    let after = local_receives(&mut alice, ARRIVES, |stanza| {
        body(stanza).as_deref() == Some("after")
    })
    .await;
    assert!(after.is_some(), "the message that follows reaches the room");
    tokio::time::sleep(QUIET).await;
    for id in ["state-private", "headline-1"] {
        assert!(
            hosted
                .captured
                .with_id(id)
                .iter()
                .all(|stanza| stanza.attr("type") != Some("error")),
            "{id}"
        );
    }
}
