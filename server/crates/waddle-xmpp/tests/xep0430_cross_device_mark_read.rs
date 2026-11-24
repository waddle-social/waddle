//! XEP-0430 — cross-device mark-read fan-out tests.
//!
//! When a user marks a conversation read on Device A, the server MUST
//! propagate the unread-state flip to the user's other resources so
//! Device B can clear its unread badges without waiting for a fresh
//! inbox query.
//!
//! These tests pin the storage-trait contract that makes that
//! possible: `InboxStorage::mark_read` returns the post-update
//! `InboxEntry` (suitable for re-broadcasting via `build_inbox_push`)
//! or `None` when no row matched, so the IQ handler can short-circuit
//! the fan-out for no-op requests.
//!
//! They also pin the wire shape of the resulting headline push so
//! clients can parse the unread=0 flip from a Waddle-private push
//! marker without putting Waddle semantics in the official XEP-0430
//! namespace.

use jid::{BareJid, Jid};
use waddle_xmpp::inbox::storage::{InMemoryInboxStorage, InboxStorage};
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
use waddle_xmpp::xep::xep0430::{build_inbox_push, NS_INBOX, NS_WADDLE_INBOX};
use xmpp_parsers::message::MessageType;

fn jid(s: &str) -> BareJid {
    s.parse().expect("bare jid")
}

#[tokio::test]
async fn mark_read_returns_post_update_entry_suitable_for_fanout() {
    let store = InMemoryInboxStorage::new();
    let me = jid("me@example.com");
    let alice = jid("alice@example.com");

    store
        .upsert(
            &me,
            InboxEntry::new(alice.clone(), ConversationKind::Direct, "sid-1", 100)
                .with_preview("hi"),
            true,
        )
        .await
        .expect("upsert");

    let updated = store
        .mark_read(&me, &alice, None)
        .await
        .expect("mark_read ok")
        .expect("entry returned so the IQ handler can fan it out");

    assert_eq!(
        updated.unread, 0,
        "post-update entry must reflect unread=0 so other devices clear badges"
    );
    assert_eq!(
        updated.partner, alice,
        "the entry returned must identify the conversation that flipped"
    );
    assert!(
        updated.thread_id.is_none(),
        "channel-level mark-read returns a channel-level entry"
    );
    assert_eq!(
        updated.preview.as_deref(),
        Some("hi"),
        "preview must round-trip so other devices don't lose context on fan-out"
    );
}

#[tokio::test]
async fn mark_read_returns_thread_entry_when_thread_id_specified() {
    let store = InMemoryInboxStorage::new();
    let me = jid("me@example.com");
    let room = jid("room@muc.example.com");

    store
        .upsert(
            &me,
            InboxEntry::new(room.clone(), ConversationKind::MucRoom, "sid-1", 100)
                .with_thread("t1")
                .with_thread_title("Lunch plans"),
            true,
        )
        .await
        .expect("upsert thread");

    let updated = store
        .mark_read(&me, &room, Some("t1"))
        .await
        .expect("mark_read ok")
        .expect("thread entry returned");

    assert_eq!(updated.unread, 0);
    assert_eq!(updated.thread_id.as_deref(), Some("t1"));
    assert_eq!(
        updated.thread_title.as_deref(),
        Some("Lunch plans"),
        "thread metadata must round-trip so the push doesn't clobber it on Device B"
    );
}

#[tokio::test]
async fn mark_read_returns_none_when_no_row_matches() {
    // The IQ handler relies on this to skip fan-out when the
    // mark-read is a no-op (typo'd JID, race against retract, etc.) —
    // emitting a headline push with a synthetic entry would clobber
    // the receiver's local state with empty fields.
    let store = InMemoryInboxStorage::new();
    let me = jid("me@example.com");
    let ghost = jid("ghost@example.com");

    let result = store
        .mark_read(&me, &ghost, None)
        .await
        .expect("mark_read ok");

    assert!(
        result.is_none(),
        "no-op mark-read must NOT return a synthetic entry — fan-out is skipped"
    );
}

#[test]
fn build_inbox_push_emits_headline_with_waddle_push_payload() {
    // The fan-out path is `mark_read` → `Option<InboxEntry>` →
    // `build_inbox_push` → routed to every resource. This test pins
    // the resulting Message shape so any change to the push wire
    // format is caught by a XEP-test, not by a downstream client
    // regression.
    let entry = InboxEntry::new(
        jid("alice@example.com"),
        ConversationKind::Direct,
        "sid-1",
        100,
    )
    .with_preview("hi")
    .with_unread(0);

    let recipient: Jid = "me@example.com/desktop".parse().expect("full jid");
    let msg = build_inbox_push(recipient, &entry);

    assert_eq!(
        msg.type_,
        MessageType::Headline,
        "Waddle inbox push updates are headlines so they bypass offline storage"
    );

    let push = msg
        .payloads
        .iter()
        .find(|p| p.is("push", NS_WADDLE_INBOX))
        .expect("waddle inbox push payload present");
    let entry = push
        .get_child("entry", NS_INBOX)
        .expect("push carries a conformant XEP-0430 entry");
    assert_eq!(entry.attr("jid"), Some("alice@example.com"));
    assert_eq!(entry.attr("id"), Some("sid-1"));
    assert_eq!(entry.attr("unread"), Some("0"));
    assert_eq!(entry.attr("preview"), None);
    let metadata = push
        .get_child("metadata", NS_WADDLE_INBOX)
        .expect("push carries Waddle metadata separately");
    assert_eq!(metadata.attr("preview"), Some("hi"));
}

// ---------------------------------------------------------------------------
// Chat-marker read position (XEP-0430 §Unread Messages, XEP-0333 §Business Rules)
// ---------------------------------------------------------------------------

fn through(ordinal: i64, unread_cleared: u32) -> waddle_xmpp::inbox::ReadThrough {
    waddle_xmpp::inbox::ReadThrough {
        ordinal: waddle_xmpp::mam::ArchiveOrdinal::from_storage(ordinal).expect("ordinal"),
        unread_cleared,
    }
}

async fn seeded(unread: usize) -> (InMemoryInboxStorage, BareJid, BareJid) {
    let store = InMemoryInboxStorage::new();
    let me = jid("me@example.com");
    let alice = jid("alice@example.com");
    for _ in 0..unread {
        store
            .upsert(
                &me,
                InboxEntry::new(alice.clone(), ConversationKind::Direct, "sid", 100),
                true,
            )
            .await
            .expect("upsert");
    }
    (store, me, alice)
}

#[tokio::test]
async fn read_through_clears_only_messages_up_to_the_marker() {
    let (store, me, alice) = seeded(5).await;

    let updated = store
        .mark_read_through(&me, &alice, None, through(2, 2))
        .await
        .expect("mark_read_through ok")
        .expect("entry returned for fan-out");

    assert_eq!(updated.unread, 3, "messages after the marker stay unread");
    assert_eq!(
        store.read_position(&me, &alice, None).await.expect("read"),
        Some(waddle_xmpp::mam::ArchiveOrdinal::from_storage(2).expect("ordinal"))
    );
}

#[tokio::test]
async fn read_position_only_moves_forward() {
    let (store, me, alice) = seeded(5).await;
    store
        .mark_read_through(&me, &alice, None, through(4, 4))
        .await
        .expect("advance");

    // A marker for an older message, or the same one again, is ignored.
    for ordinal in [2, 4] {
        let stale = store
            .mark_read_through(&me, &alice, None, through(ordinal, 2))
            .await
            .expect("stale ok");
        assert!(stale.is_none(), "marker at {ordinal} must be ignored");
    }
    assert_eq!(store.total_unread(&me).await.expect("total"), 1);
    assert_eq!(
        store.read_position(&me, &alice, None).await.expect("read"),
        Some(waddle_xmpp::mam::ArchiveOrdinal::from_storage(4).expect("ordinal"))
    );
}

#[tokio::test]
async fn read_through_keeps_messages_that_arrive_after_it_was_computed() {
    let (store, me, alice) = seeded(3).await;
    // A fourth message lands before the marker is applied.
    store
        .upsert(
            &me,
            InboxEntry::new(alice.clone(), ConversationKind::Direct, "sid-4", 101),
            true,
        )
        .await
        .expect("upsert");

    let updated = store
        .mark_read_through(&me, &alice, None, through(2, 2))
        .await
        .expect("mark_read_through ok")
        .expect("entry");

    assert_eq!(updated.unread, 2);
}

#[tokio::test]
async fn read_through_without_a_row_is_a_no_op() {
    let store = InMemoryInboxStorage::new();
    let me = jid("me@example.com");
    let alice = jid("alice@example.com");

    assert!(store
        .mark_read_through(&me, &alice, None, through(1, 1))
        .await
        .expect("ok")
        .is_none());
    assert_eq!(
        store.read_position(&me, &alice, None).await.expect("read"),
        None
    );
}
