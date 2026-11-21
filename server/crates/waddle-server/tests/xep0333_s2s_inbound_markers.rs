//! XEP-0333 across S2S, inbound: the chat markers of an occupant at
//! another server in a room of this deployment (ADR-0020 D10).
//!
//! The room relays a remote occupant's marker to its occupants. A
//! displayed marker marks an inbox row read only for an account of this
//! deployment: a remote occupant's read state is its own server's, and no
//! row is written for it here.
//!
//! A marker that answers a message (`received`, `displayed`,
//! `acknowledged`) counts against its account's share of its domain's
//! request rate like any stanza, and one beyond the share is neither
//! relayed nor answered: nothing in §4 has a sender wait for an answer to
//! a marker.
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
    body, error_condition, local_groupchat, local_receives_payload, naming, remote_groupchat,
    remote_payload, room_with_remote_occupant, stanza_id, BOB, BOB_OCCUPANT, QUIET, ROOM,
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
        .peer_sends(remote_payload(
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

#[tokio::test]
async fn a_remote_occupants_markers_count_against_its_accounts_share_and_are_not_answered() {
    let limits = RouterLimits {
        // Two stanzas of one account at once, one more each second.
        room_request_rate_per_account: 1,
        room_request_burst_per_account: 2,
        ..RouterLimits::default()
    };
    let (hosted, mut alice, _own) = room_with_remote_occupant(limits, no_change).await;
    local_groupchat(&mut alice, ROOM, "local-1", "read this").await;
    let copy = hosted
        .at_peer(|stanza| body(stanza).as_deref() == Some("read this"))
        .await
        .expect("the remote occupant's copy");
    let read = stanza_id(&copy, ROOM).expect("the room's stanza-id");
    // The join spent one of the two; give the share its time to refill.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let ids: Vec<String> = (0..6).map(|index| format!("marker-{index}")).collect();
    for id in &ids {
        hosted
            .peer_sends(remote_payload(
                BOB,
                ROOM,
                id,
                naming("displayed", NS_MARKERS, &read),
            ))
            .await;
    }
    // The share is spent by markers as by any stanza: a message that
    // follows at once is refused, and answered, since it carries a text.
    hosted
        .peer_sends(remote_groupchat(BOB, ROOM, "text-1", "too soon"))
        .await;
    let refused = hosted.answered("text-1").await;
    assert_eq!(refused.attr("type"), Some("error"), "{refused:?}");
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("resource-constraint")
    );
    // A marker within the share reached the room.
    let marker = local_receives_payload(&mut alice, BOB_OCCUPANT, "displayed", NS_MARKERS).await;
    assert!(marker.is_some(), "a marker for the local occupant");
    // Those beyond it are dropped, and none is answered with an error.
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
    assert!(reflected < ids.len(), "every marker was taken");
}
