use super::*;

fn jid(s: &str) -> BareJid {
    s.parse().unwrap()
}

fn entry(partner: &str, kind: ConversationKind, id: &str, ts: i64) -> InboxEntry {
    InboxEntry::new(jid(partner), kind, id, ts)
}

#[test]
fn test_observe_new_and_update() {
    let mut inbox = InboxView::new();
    inbox.observe_message(
        entry("alice@example.com", ConversationKind::Direct, "sid-1", 100).with_preview("hi"),
        true,
    );
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox.total_unread(), 1);
    inbox.observe_message(
        entry("alice@example.com", ConversationKind::Direct, "sid-2", 200).with_preview("!"),
        true,
    );
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox.total_unread(), 2);
    let e = inbox.get(&jid("alice@example.com")).unwrap();
    assert_eq!(e.last_stanza_id, "sid-2");
    assert_eq!(e.preview.as_deref(), Some("!"));
}

#[test]
fn test_mark_read_resets_only_that_partner() {
    let mut inbox = InboxView::new();
    inbox.observe_message(
        entry("a@example.com", ConversationKind::Direct, "s1", 1),
        true,
    );
    inbox.observe_message(
        entry("b@example.com", ConversationKind::Direct, "s2", 2),
        true,
    );
    inbox.mark_read(&jid("a@example.com"));
    assert_eq!(inbox.get(&jid("a@example.com")).unwrap().unread, 0);
    assert_eq!(inbox.get(&jid("b@example.com")).unwrap().unread, 1);
    assert_eq!(inbox.total_unread(), 1);
}

#[test]
fn test_snapshot_sorted_newest_first() {
    let mut inbox = InboxView::new();
    inbox.observe_message(
        entry("a@example.com", ConversationKind::Direct, "s1", 10),
        false,
    );
    inbox.observe_message(
        entry("b@example.com", ConversationKind::Direct, "s2", 30),
        false,
    );
    inbox.observe_message(
        entry(
            "g@conference.example.com",
            ConversationKind::MucRoom,
            "s3",
            20,
        ),
        false,
    );
    let snap = inbox.snapshot();
    assert_eq!(snap.len(), 3);
    assert_eq!(snap[0].last_stanza_id, "s2");
    assert_eq!(snap[1].last_stanza_id, "s3");
    assert_eq!(snap[2].last_stanza_id, "s1");
}

#[test]
fn test_observe_without_increment_leaves_unread_alone() {
    let mut inbox = InboxView::new();
    inbox.observe_message(
        entry("a@example.com", ConversationKind::Direct, "s1", 1),
        true,
    );
    assert_eq!(inbox.total_unread(), 1);
    inbox.observe_message(
        entry("a@example.com", ConversationKind::Direct, "s2", 2),
        false,
    );
    assert_eq!(inbox.total_unread(), 1);
}

#[test]
fn test_remove() {
    let mut inbox = InboxView::new();
    inbox.observe_message(
        entry("a@example.com", ConversationKind::Direct, "s1", 1),
        false,
    );
    assert!(inbox.remove(&jid("a@example.com")).is_some());
    assert!(inbox.is_empty());
}

#[test]
fn test_thread_entry_separate_from_channel() {
    let mut inbox = InboxView::new();
    let room = "room@muc.example.com";

    // Channel-level entry
    inbox.observe_message(entry(room, ConversationKind::MucRoom, "s1", 100), true);

    // Thread-level entry
    inbox.observe_message(
        entry(room, ConversationKind::MucRoom, "s2", 200)
            .with_thread("thread-1")
            .with_thread_title("Discussion")
            .with_author("alice"),
        true,
    );

    assert_eq!(inbox.len(), 2);
    // Channel unread excludes threads
    assert_eq!(inbox.total_unread(), 1);

    let channel = inbox.get(&jid(room)).unwrap();
    assert!(channel.thread_id.is_none());

    let threads = inbox.threads_for_room(&jid(room));
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].thread_id.as_deref(), Some("thread-1"));
    assert_eq!(threads[0].thread_title.as_deref(), Some("Discussion"));
    assert_eq!(threads[0].unread, 1);
}

#[test]
fn test_thread_reply_count_increments() {
    let mut inbox = InboxView::new();
    let room = "room@muc.example.com";

    inbox.observe_message(
        entry(room, ConversationKind::MucRoom, "s1", 100)
            .with_thread("t1")
            .with_thread_title("Topic"),
        true,
    );
    inbox.observe_message(
        entry(room, ConversationKind::MucRoom, "s2", 200).with_thread("t1"),
        true,
    );

    let key = InboxKey::thread(jid(room), "t1");
    let e = inbox.get_by_key(&key).unwrap();
    assert_eq!(e.reply_count, 1);
    assert_eq!(e.unread, 2);
    // Title preserved from first message
    assert_eq!(e.thread_title.as_deref(), Some("Topic"));
}

#[test]
fn test_mark_read_by_key_for_thread() {
    let mut inbox = InboxView::new();
    let room = "room@muc.example.com";

    inbox.observe_message(
        entry(room, ConversationKind::MucRoom, "s1", 100).with_thread("t1"),
        true,
    );
    inbox.observe_message(
        entry(room, ConversationKind::MucRoom, "s2", 200).with_thread("t2"),
        true,
    );

    let key = InboxKey::thread(jid(room), "t1");
    inbox.mark_read_by_key(&key);

    let t1 = inbox.get_by_key(&key).unwrap();
    assert_eq!(t1.unread, 0);

    let t2 = inbox
        .get_by_key(&InboxKey::thread(jid(room), "t2"))
        .unwrap();
    assert_eq!(t2.unread, 1);
}

#[test]
fn test_snapshot_excludes_thread_entries() {
    let mut inbox = InboxView::new();
    inbox.observe_message(
        entry("room@muc.example.com", ConversationKind::MucRoom, "s1", 10),
        false,
    );
    inbox.observe_message(
        entry("room@muc.example.com", ConversationKind::MucRoom, "s2", 20).with_thread("t1"),
        false,
    );

    let snap = inbox.snapshot();
    assert_eq!(snap.len(), 1);
    assert!(snap[0].thread_id.is_none());
}

#[test]
fn test_inbox_key_equality() {
    let k1 = InboxKey::channel(jid("a@example.com"));
    let k2 = InboxKey::channel(jid("a@example.com"));
    let k3 = InboxKey::thread(jid("a@example.com"), "t1");

    assert_eq!(k1, k2);
    assert_ne!(k1, k3);
    assert!(k3.is_thread());
    assert!(!k1.is_thread());
}
