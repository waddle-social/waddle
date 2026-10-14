//! XEP-0045: rejoining rooms ahead of a failed XEP-0198 resume's retries.
//!
//! A fresh stream after `<failed/>` holds no MUC occupancy, and a room
//! refuses groupchat from a non-occupant (XEP-0045 §7.4). The client core
//! retries the unacknowledged tail on that fresh stream, so the host writes
//! the join presences it deferred during negotiation first. These tests pin
//! the two halves of that contract the hosts rely on: join presences are
//! recognisable, and the fresh-stream retry still carries the groupchat tail.

use chrono::{TimeZone, Utc};
use minidom::Element;
use waddle_xmpp_client::messaging::{build_muc_join_presence, is_muc_join_presence};
use waddle_xmpp_client::{
    stream_management::SmState, SmResumeState, StreamId, UnhandledOutboundEntry,
};

#[test]
fn xep0045_join_presences_are_told_apart_from_other_stanzas() {
    let room = "general@muc.example".parse().expect("valid room JID");
    let join = build_muc_join_presence(&room, "alice").expect("join presence");
    assert!(
        is_muc_join_presence(&join),
        "§7.2.2 join carries the MUC `<x/>`"
    );

    let status = "<presence xmlns='jabber:client' to='general@muc.example/alice'><show>away</show></presence>"
        .parse::<Element>()
        .expect("presence parses");
    assert!(
        !is_muc_join_presence(&status),
        "an occupant status update is not a join",
    );
    let groupchat = "<message xmlns='jabber:client' type='groupchat' to='general@muc.example'><body>hi</body></message>"
        .parse::<Element>()
        .expect("message parses");
    assert!(!is_muc_join_presence(&groupchat));
}

#[test]
fn xep0045_fresh_stream_retry_keeps_the_groupchat_tail_for_after_the_rejoin() {
    let sent_at = Utc
        .with_ymd_and_hms(2026, 7, 27, 12, 0, 0)
        .single()
        .expect("test timestamp is valid");
    let groupchat = "<message xmlns='jabber:client' type='groupchat' id='room-1' to='general@muc.example'><body>hi</body></message>"
        .parse::<Element>()
        .expect("message parses");
    let tail = UnhandledOutboundEntry::try_new(groupchat, sent_at).expect("countable stanza");
    let resume = SmResumeState::from_unhandled_outbound_entries(
        StreamId::new("previous-stream"),
        0,
        1,
        [tail],
    )
    .expect("XEP-0198 state is valid");

    let retries = SmState::from_resume_state(&resume).unhandled_stanzas_for_fallback_retry();

    assert_eq!(
        retries.len(),
        1,
        "the groupchat tail is retried, not dropped"
    );
    assert_eq!(retries[0].attr("type"), Some("groupchat"));
    assert!(
        !retries.iter().any(is_muc_join_presence),
        "the retry carries no join of its own; the host's rejoin must precede it",
    );
}
