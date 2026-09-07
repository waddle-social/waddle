use super::*;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use jid::{BareJid, Jid};
use std::path::PathBuf;
use waddle_xmpp_core::mam::{ArchivedMessage, MamFilterStanzaId, MamQuery, MamResult};

fn filter_id(s: &str) -> MamFilterStanzaId {
    MamFilterStanzaId::new(s).expect("valid test fixture id")
}

async fn create_test_storage() -> SqlxMamStorage {
    SqlxMamStorage::open_in_memory().await.unwrap()
}

fn jid(value: &str) -> Jid {
    value.parse::<Jid>().expect("valid jid literal")
}

fn bare(value: &str) -> BareJid {
    value.parse::<BareJid>().expect("valid bare jid literal")
}

fn user_device() -> Jid {
    jid("user@example.com/device")
}

fn archive_alice(archive: &BareJid) -> Jid {
    format!("{archive}/alice")
        .parse::<Jid>()
        .expect("valid jid")
}

fn expect_stored(outcome: StoreOutcome) -> String {
    match outcome {
        StoreOutcome::Stored(id) => id,
        other => panic!("expected a newly stored row, got {other:?}"),
    }
}

fn muc_rich(real_jid: &str) -> waddle_xmpp_core::mam::ArchivedRichMessage {
    use waddle_xmpp_core::mam::{ArchivedMucSender, ArchivedRichMessage};
    use waddle_xmpp_core::types::{Affiliation, Role};

    ArchivedRichMessage {
        muc_sender: Some(ArchivedMucSender {
            jid: jid(real_jid),
            affiliation: Affiliation::Member,
            role: Role::Participant,
        }),
        ..ArchivedRichMessage::default()
    }
}

async fn assert_sender_origin_lookup_precedence_and_room_scope(storage: &dyn MamStorage) {
    use waddle_xmpp_core::xep0359::{OriginId, StanzaId};

    let personal_archive = bare("alice@example.com");
    let collision = OriginId::new("collision-id");
    let base = DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
        .expect("valid timestamp")
        .with_timezone(&Utc);
    storage
        .store_message(
            &personal_archive,
            &ArchivedMessage {
                id: "legacy-wire-collision".to_string(),
                timestamp: base,
                stanza_id: Some(StanzaId::new(
                    collision.as_str(),
                    Jid::from(personal_archive.clone()),
                )),
                ..ArchivedMessage::for_test(
                    jid("alice@example.com/phone"),
                    jid("bob@example.com/laptop"),
                )
            },
        )
        .await
        .expect("store wire-id collision");
    storage
        .store_message(
            &personal_archive,
            &ArchivedMessage {
                id: "explicit-origin-target".to_string(),
                timestamp: base + ChronoDuration::seconds(1),
                origin_id: Some(collision.clone()),
                stanza_id: Some(StanzaId::new(
                    "explicit-wire-id",
                    Jid::from(personal_archive.clone()),
                )),
                ..ArchivedMessage::for_test(
                    jid("alice@example.com/tablet"),
                    jid("bob@example.com/laptop"),
                )
            },
        )
        .await
        .expect("store explicit origin target");

    let personal = storage
        .get_message_by_sender_and_origin_id(
            &personal_archive,
            MamArchiveKind::Personal,
            &jid("alice@example.com"),
            &collision,
        )
        .await
        .expect("personal lookup")
        .expect("personal target");
    assert_eq!(personal.id, "explicit-origin-target");

    let room = bare("room@conference.example");
    for (id, sender) in [
        ("alice-room-message", "room@conference.example/alice"),
        ("bob-room-message", "room@conference.example/bob"),
    ] {
        storage
            .store_message(
                &room,
                &ArchivedMessage {
                    id: id.to_string(),
                    origin_id: Some(OriginId::new("shared-room-origin")),
                    ..ArchivedMessage::for_test(jid(sender), jid("room@conference.example"))
                },
            )
            .await
            .expect("store room occupant row");
    }
    let room_target = storage
        .get_message_by_sender_and_origin_id(
            &room,
            MamArchiveKind::Room,
            &jid("room@conference.example/alice"),
            &OriginId::new("shared-room-origin"),
        )
        .await
        .expect("room lookup")
        .expect("room target");
    assert_eq!(room_target.id, "alice-room-message");
}

#[tokio::test]
async fn inmemory_sender_origin_lookup_prefers_origin_and_scopes_room_occupants() {
    assert_sender_origin_lookup_precedence_and_room_scope(&InMemoryMamStorage::new()).await;
}

#[tokio::test]
async fn sqlite_sender_origin_lookup_prefers_origin_and_scopes_room_occupants() {
    assert_sender_origin_lookup_precedence_and_room_scope(&create_test_storage().await).await;
}

#[tokio::test]
async fn test_store_and_retrieve_message() {
    let storage = create_test_storage().await;

    let archive = bare("room@conference.example.com");
    let msg = ArchivedMessage {
        body: Some("Hello, world!".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "abc123",
            jid(&archive.to_string()),
        )),
        ..ArchivedMessage::for_test(
            jid("user@example.com/nick"),
            jid("room@conference.example.com"),
        )
    };

    let archive_id = expect_stored(storage.store_message(&archive, &msg).await.unwrap());
    assert!(!archive_id.is_empty());

    let retrieved = storage.get_message(&archive_id).await.unwrap();
    assert!(retrieved.is_some());

    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.id, archive_id);
    assert_eq!(retrieved.body.as_deref(), Some("Hello, world!"));
    let sid = retrieved.stanza_id.expect("stanza_id round-trips");
    assert_eq!(sid.id, "abc123");
    assert_eq!(sid.by, jid(&archive.to_string()));
}

#[tokio::test]
async fn test_sqlite_groupchat_origin_retry_honors_tombstones() {
    let storage = create_test_storage().await;
    assert_groupchat_origin_retry_honors_tombstones(&storage).await;
}

#[tokio::test]
async fn test_inmemory_groupchat_origin_retry_honors_tombstones() {
    let storage = InMemoryMamStorage::new();
    assert_groupchat_origin_retry_honors_tombstones(&storage).await;
}

async fn assert_groupchat_origin_retry_honors_tombstones(storage: &dyn MamStorage) {
    use waddle_xmpp_core::mam::ArchivedTombstone;
    use waddle_xmpp_core::xep0359::OriginId;

    fn message(id: &str, origin_id: &str, body: &str, generation: u64) -> ArchivedMessage {
        ArchivedMessage {
            id: id.to_string(),
            body: Some(body.to_string()),
            origin_id: Some(OriginId::new(origin_id)),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            nickname_generation: Some(generation),
            rich: Some(muc_rich("alice@example.com/session")),
            ..ArchivedMessage::for_test(
                jid("room@conference.example.com/alice"),
                jid("room@conference.example.com"),
            )
        }
    }

    fn tombstone() -> ArchivedTombstone {
        ArchivedTombstone {
            retraction_id: None,
            stamp: Utc::now(),
            moderation: None,
            sender_scope: None,
        }
    }

    let archive = bare("room@conference.example.com");
    let original = message("tombstone-original", "tombstone-origin", "secret", 1);
    assert_eq!(
        storage.store_message(&archive, &original).await.unwrap(),
        StoreOutcome::Stored("tombstone-original".to_string())
    );
    assert!(storage
        .replace_with_tombstone("tombstone-original", tombstone())
        .await
        .unwrap());

    let retry = message("tombstone-retry", "tombstone-origin", "secret", 2);
    assert_eq!(
        storage.store_message(&archive, &retry).await.unwrap(),
        StoreOutcome::TombstoneHit("tombstone-original".to_string())
    );
    assert_eq!(storage.count_messages(&archive).await.unwrap(), 1);

    // Construct the pathological ordering before retracting the older row:
    // the tombstone must survive even when a live row shares its origin-id.
    let old = message("ordering-old", "ordering-origin", "old content", 3);
    let live = message("ordering-live", "ordering-origin", "live content", 4);
    assert_eq!(
        storage.store_message(&archive, &old).await.unwrap(),
        StoreOutcome::Stored("ordering-old".to_string())
    );
    assert_eq!(
        storage.store_message(&archive, &live).await.unwrap(),
        StoreOutcome::Stored("ordering-live".to_string())
    );
    assert!(storage
        .replace_with_tombstone("ordering-old", tombstone())
        .await
        .unwrap());
    let live_retry = message("ordering-retry", "ordering-origin", "live content", 5);
    assert_eq!(
        storage.store_message(&archive, &live_retry).await.unwrap(),
        StoreOutcome::TombstoneHit("ordering-old".to_string())
    );
    assert_eq!(storage.count_messages(&archive).await.unwrap(), 3);

    // A tombstone that retained the internal `sender_scope` swallows only
    // retries from the SAME real bare JID; a different account reusing the
    // nick + origin-id archives distinctly (Codex review on PR #1412).
    let scoped_original = message("scoped-original", "scoped-origin", "scoped secret", 6);
    assert_eq!(
        storage
            .store_message(&archive, &scoped_original)
            .await
            .unwrap(),
        StoreOutcome::Stored("scoped-original".to_string())
    );
    let scoped_tombstone = ArchivedTombstone {
        sender_scope: Some("alice@example.com".parse().expect("valid bare JID")),
        ..tombstone()
    };
    assert_eq!(
        storage
            .replace_with_terminal_tombstone("scoped-original", scoped_tombstone)
            .await
            .unwrap(),
        crate::mam::storage::TerminalTombstoneOutcome::Replaced
    );
    let same_user_retry = message("scoped-retry", "scoped-origin", "scoped secret", 7);
    assert_eq!(
        storage
            .store_message(&archive, &same_user_retry)
            .await
            .unwrap(),
        StoreOutcome::TombstoneHit("scoped-original".to_string())
    );
    let mut different_user = message("scoped-other", "scoped-origin", "fresh content", 1);
    different_user.rich = Some(muc_rich("mallory@example.com/session"));
    assert_eq!(
        storage
            .store_message(&archive, &different_user)
            .await
            .unwrap(),
        StoreOutcome::Stored("scoped-other".to_string())
    );
}

#[tokio::test]
async fn test_store_and_retrieve_reply_thread_metadata() {
    let storage = create_test_storage().await;

    let msg = ArchivedMessage {
            body: Some("Reply body".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "archive-stanza-1",
                jid("room@conference.example.com"),
            )),
            thread: Some(waddle_xmpp_core::xep0201::ThreadInfo::root(
                waddle_xmpp_core::mam::ThreadId::new("thread-root-1").expect("thread id"),
            )),
            reply: Some(waddle_xmpp_core::mam::ArchivedReply {
                id: waddle_xmpp_core::mam::RichMessageId::new("parent-message-1")
                    .expect("non-empty reply id"),
                to: Some(jid("bob@example.com")),
            }),
            origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("origin-abc")),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='archive-stanza-1'><body>Reply body</body></message>".to_string(),
            ),
            ..ArchivedMessage::for_test(
                jid("room@conference.example.com/alice"),
                jid("room@conference.example.com"),
            )
        };

    let archive_id = expect_stored(
        storage
            .store_message(&bare("room@conference.example.com"), &msg)
            .await
            .unwrap(),
    );

    let retrieved = storage
        .get_message(&archive_id)
        .await
        .unwrap()
        .expect("archived message");

    assert_eq!(
        retrieved.thread.as_ref().map(|t| t.id.as_str()),
        Some("thread-root-1")
    );
    let reply = retrieved.reply.as_ref().expect("reply present");
    assert_eq!(reply.id.as_str(), "parent-message-1");
    assert_eq!(
        reply.to.as_ref().map(|jid| jid.to_string()),
        Some("bob@example.com".to_string())
    );
    assert_eq!(
        retrieved.origin_id.as_ref().map(|o| o.id.as_str()),
        Some("origin-abc")
    );
    assert_eq!(
        retrieved.message_type,
        xmpp_parsers::message::MessageType::Groupchat
    );
    assert!(retrieved.stanza_xml.is_some());
}

#[tokio::test]
async fn xep_0201_parent_thread_id_round_trips_through_storage() {
    // Locks the column-level round-trip for the new parent_thread_id
    // column. Replay of `<thread parent>` is covered separately by the
    // mam.rs replay-builder tests in commit 4.
    let storage = create_test_storage().await;
    let msg = ArchivedMessage {
        body: Some("Nested-thread reply".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "archive-stanza-2",
            jid("room@conference.example.com"),
        )),
        thread: Some(waddle_xmpp_core::xep0201::ThreadInfo::child(
            waddle_xmpp_core::mam::ThreadId::new("child-thread").expect("thread id"),
            waddle_xmpp_core::mam::ThreadId::new("root-thread").expect("parent id"),
        )),
        message_type: xmpp_parsers::message::MessageType::Groupchat,
        ..ArchivedMessage::for_test(
            jid("room@conference.example.com/alice"),
            jid("room@conference.example.com"),
        )
    };

    let archive_id = expect_stored(
        storage
            .store_message(&bare("room@conference.example.com"), &msg)
            .await
            .unwrap(),
    );

    let retrieved = storage
        .get_message(&archive_id)
        .await
        .unwrap()
        .expect("archived message");

    let thread = retrieved.thread.as_ref().expect("thread present");
    assert_eq!(thread.id.as_str(), "child-thread");
    assert_eq!(
        thread.parent.as_ref().map(|t| t.as_str()),
        Some("root-thread")
    );
}

#[tokio::test]
async fn test_query_with_pagination() {
    let storage = create_test_storage().await;
    let archive = bare("room@conference.example.com");
    let archive_jid = jid("room@conference.example.com");

    for body in ["one", "two", "three"] {
        let msg = ArchivedMessage {
            body: Some(body.to_string()),
            ..ArchivedMessage::for_test(user_device(), archive_jid.clone())
        };
        storage.store_message(&archive, &msg).await.unwrap();
    }

    let page_one = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                max: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page_one.messages.len(), 2);
    assert!(!page_one.complete);

    let page_two = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                after_id: page_one.last_id.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page_two.messages.len(), 1);
    assert_eq!(page_two.messages[0].body.as_deref(), Some("three"));
}

#[tokio::test]
async fn sqlite_full_occupant_with_excludes_siblings_before_pagination() {
    let storage = create_test_storage().await;
    assert_full_occupant_with_excludes_siblings_before_pagination(&storage).await;
}

#[tokio::test]
async fn inmemory_full_occupant_with_excludes_siblings_before_pagination() {
    let storage = InMemoryMamStorage::new();
    assert_full_occupant_with_excludes_siblings_before_pagination(&storage).await;
}

async fn assert_full_occupant_with_excludes_siblings_before_pagination(storage: &dyn MamStorage) {
    let archive = bare("user@example.com");
    let owner_account = jid("user@example.com");
    let alice_occupant = jid("room@conference.example.com/alice");
    let bob_occupant = jid("room@conference.example.com/bob");
    let base = DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
        .expect("valid timestamp")
        .with_timezone(&Utc);

    for (id, seconds, from, to, body) in [
        (
            "alice-incoming",
            0,
            alice_occupant.clone(),
            owner_account.clone(),
            "alice incoming",
        ),
        (
            "bob-incoming",
            1,
            bob_occupant.clone(),
            owner_account.clone(),
            "bob incoming",
        ),
        (
            "alice-outgoing",
            2,
            owner_account.clone(),
            alice_occupant.clone(),
            "alice outgoing",
        ),
        (
            "bob-outgoing",
            3,
            owner_account.clone(),
            bob_occupant.clone(),
            "bob outgoing",
        ),
        (
            "alice-later",
            4,
            alice_occupant.clone(),
            owner_account.clone(),
            "alice later",
        ),
    ] {
        storage
            .store_message(
                &archive,
                &ArchivedMessage {
                    id: id.to_string(),
                    timestamp: base + ChronoDuration::seconds(seconds),
                    body: Some(body.to_string()),
                    ..ArchivedMessage::for_test(from, to)
                },
            )
            .await
            .expect("store interleaved MUC PM");
    }

    let first = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                with: Some(alice_occupant.clone()),
                max: Some(2),
                ..Default::default()
            },
        )
        .await
        .expect("query alice first page");
    assert_eq!(bodies(&first), vec!["alice incoming", "alice outgoing"]);
    assert_eq!(first.count, Some(3));
    assert!(!first.complete);

    let second = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                with: Some(alice_occupant),
                after_id: first.last_id.clone(),
                max: Some(2),
                ..Default::default()
            },
        )
        .await
        .expect("query alice second page");
    assert_eq!(bodies(&second), vec!["alice later"]);
    assert_eq!(second.count, Some(3));
    assert!(second.complete);
}

#[tokio::test]
async fn sqlite_owner_with_returns_only_self_chat_rows() {
    let storage = create_test_storage().await;
    assert_owner_with_returns_only_self_chat_rows(&storage).await;
}

#[tokio::test]
async fn inmemory_owner_with_returns_only_self_chat_rows() {
    let storage = InMemoryMamStorage::new();
    assert_owner_with_returns_only_self_chat_rows(&storage).await;
}

async fn assert_owner_with_returns_only_self_chat_rows(storage: &dyn MamStorage) {
    let archive = bare("juliet@example.com");
    let owner = jid("juliet@example.com");
    let base = DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
        .expect("valid timestamp")
        .with_timezone(&Utc);

    for (id, seconds, from, to) in [
        (
            "self-chat-one",
            0,
            jid("juliet@example.com/balcony"),
            jid("juliet@example.com/chamber"),
        ),
        (
            "ordinary-outgoing",
            1,
            jid("juliet@example.com/phone"),
            jid("romeo@example.com/mobile"),
        ),
        (
            "ordinary-incoming",
            2,
            jid("romeo@example.com/desktop"),
            jid("juliet@example.com/tablet"),
        ),
        ("self-chat-two", 3, jid("juliet@example.com/balcony"), owner),
        (
            "unrelated",
            4,
            jid("mercutio@example.com/phone"),
            jid("benvolio@example.com/tablet"),
        ),
    ] {
        storage
            .store_message(
                &archive,
                &ArchivedMessage {
                    id: id.to_string(),
                    timestamp: base + ChronoDuration::seconds(seconds),
                    body: Some(id.to_string()),
                    ..ArchivedMessage::for_test(from, to)
                },
            )
            .await
            .expect("store archive fixture");
    }

    let bare_owner = storage
        .query_messages(
            &archive,
            MamArchiveKind::Personal,
            &MamQuery {
                with: Some(jid("juliet@example.com")),
                ..Default::default()
            },
        )
        .await
        .expect("query self-chat archive");

    let ids = bare_owner
        .messages
        .iter()
        .map(|message| message.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["self-chat-one", "self-chat-two"]);
    assert_eq!(bare_owner.count, Some(2));

    let full_owner_resource = storage
        .query_messages(
            &archive,
            MamArchiveKind::Personal,
            &MamQuery {
                with: Some(jid("juliet@example.com/query-device")),
                ..Default::default()
            },
        )
        .await
        .expect("query exact full owner resource");
    assert!(full_owner_resource.messages.is_empty());
    assert_eq!(full_owner_resource.count, Some(0));
    assert!(full_owner_resource.complete);

    let ordinary_bare = storage
        .query_messages(
            &archive,
            MamArchiveKind::Personal,
            &MamQuery {
                with: Some(jid("romeo@example.com")),
                ..Default::default()
            },
        )
        .await
        .expect("query ordinary contact by bare JID");
    let ordinary_ids = ordinary_bare
        .messages
        .iter()
        .map(|message| message.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ordinary_ids, vec!["ordinary-outgoing", "ordinary-incoming"]);
    assert_eq!(ordinary_bare.count, Some(2));

    let ordinary_full = storage
        .query_messages(
            &archive,
            MamArchiveKind::Personal,
            &MamQuery {
                with: Some(jid("romeo@example.com/desktop")),
                ..Default::default()
            },
        )
        .await
        .expect("query ordinary contact by full JID");
    assert_eq!(ordinary_full.messages[0].id, "ordinary-incoming");
    assert_eq!(ordinary_full.messages.len(), 1);
    assert_eq!(ordinary_full.count, Some(1));

    let empty = storage
        .query_messages(
            &archive,
            MamArchiveKind::Personal,
            &MamQuery {
                with: Some(jid("tybalt@example.com")),
                ..Default::default()
            },
        )
        .await
        .expect("query contact absent from archive");
    assert!(empty.messages.is_empty());
    assert_eq!(empty.count, Some(0));
    assert!(empty.complete);
}

#[tokio::test]
async fn sqlite_room_archive_full_occupant_with_is_exact() {
    let storage = create_test_storage().await;
    assert_room_archive_full_occupant_with_is_exact(&storage).await;
}

#[tokio::test]
async fn inmemory_room_archive_full_occupant_with_is_exact() {
    let storage = InMemoryMamStorage::new();
    assert_room_archive_full_occupant_with_is_exact(&storage).await;
}

async fn assert_room_archive_full_occupant_with_is_exact(storage: &dyn MamStorage) {
    let room = bare("room@example.com");
    let room_jid = jid("room@example.com");
    let alice = jid("room@example.com/alice");
    let bob = jid("room@example.com/bob");
    let base = DateTime::parse_from_rfc3339("2026-07-02T12:00:00Z")
        .expect("valid timestamp")
        .with_timezone(&Utc);

    for (id, seconds, from) in [
        ("alice-one", 0, alice.clone()),
        ("bob-one", 1, bob),
        ("alice-two", 2, alice.clone()),
    ] {
        storage
            .store_message(
                &room,
                &ArchivedMessage {
                    id: id.to_string(),
                    timestamp: base + ChronoDuration::seconds(seconds),
                    ..ArchivedMessage::for_test(from, room_jid.clone())
                },
            )
            .await
            .expect("store room archive fixture");
    }

    let first = storage
        .query_messages(
            &room,
            MamArchiveKind::Room,
            &MamQuery {
                with: Some(alice.clone()),
                max: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("query first Alice page");
    assert_eq!(first.messages[0].id, "alice-one");
    assert_eq!(first.count, Some(2));
    assert!(!first.complete);

    let second = storage
        .query_messages(
            &room,
            MamArchiveKind::Room,
            &MamQuery {
                with: Some(alice),
                after_id: first.last_id,
                max: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("query second Alice page");
    assert_eq!(second.messages[0].id, "alice-two");
    assert_eq!(second.count, Some(2));
    assert!(second.complete);

    let bare_room = storage
        .query_messages(
            &room,
            MamArchiveKind::Room,
            &MamQuery {
                with: Some(room_jid),
                ..Default::default()
            },
        )
        .await
        .expect("query room by bare JID");
    assert_eq!(bare_room.messages.len(), 3);
    assert_eq!(bare_room.count, Some(3));

    let empty = storage
        .query_messages(
            &room,
            MamArchiveKind::Room,
            &MamQuery {
                with: Some(jid("room@example.com/charlie")),
                ..Default::default()
            },
        )
        .await
        .expect("query absent occupant");
    assert!(empty.messages.is_empty());
    assert_eq!(empty.count, Some(0));
}

#[tokio::test]
async fn test_sqlite_rsm_after_uses_archive_order_not_lexical_id_order() {
    let storage = create_test_storage().await;
    assert_rsm_after_uses_archive_order_not_lexical_id_order(&storage).await;
}

#[tokio::test]
async fn test_inmemory_rsm_after_uses_archive_order_not_lexical_id_order() {
    let storage = InMemoryMamStorage::new();
    assert_rsm_after_uses_archive_order_not_lexical_id_order(&storage).await;
}

async fn assert_rsm_after_uses_archive_order_not_lexical_id_order(storage: &dyn MamStorage) {
    let archive = bare("room@conference.example.com");
    let base = Utc::now();
    store_nonlexical_archive_order_messages(storage, &archive, base).await;

    let page_one = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                max: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(bodies(&page_one), vec!["one", "two"]);
    assert_eq!(page_one.last_id.as_deref(), Some("a-second"));

    let page_two = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                max: Some(2),
                after_id: page_one.last_id.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(bodies(&page_two), vec!["three", "four"]);
    assert_eq!(page_two.last_id.as_deref(), Some("b-fourth"));

    let page_three = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                max: Some(2),
                after_id: page_two.last_id.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(bodies(&page_three), vec!["five"]);
    assert!(page_three.complete);
}

#[tokio::test]
async fn test_sqlite_rsm_before_uses_archive_order_not_lexical_id_order() {
    let storage = create_test_storage().await;
    assert_rsm_before_uses_archive_order_not_lexical_id_order(&storage).await;
}

#[tokio::test]
async fn test_inmemory_rsm_before_uses_archive_order_not_lexical_id_order() {
    let storage = InMemoryMamStorage::new();
    assert_rsm_before_uses_archive_order_not_lexical_id_order(&storage).await;
}

async fn assert_rsm_before_uses_archive_order_not_lexical_id_order(storage: &dyn MamStorage) {
    let archive = bare("room@conference.example.com");
    let base = Utc::now();
    store_nonlexical_archive_order_messages(storage, &archive, base).await;

    let page = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                max: Some(2),
                before_id: Some("x-fifth".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(bodies(&page), vec!["three", "four"]);
    assert_eq!(page.first_id.as_deref(), Some("y-third"));
    assert_eq!(page.last_id.as_deref(), Some("b-fourth"));
}

#[tokio::test]
async fn test_sqlite_rsm_cursors_page_same_timestamp_by_archive_id() {
    let storage = create_test_storage().await;
    assert_rsm_cursors_page_same_timestamp_by_archive_id(&storage).await;
}

#[tokio::test]
async fn test_inmemory_rsm_cursors_page_same_timestamp_by_archive_id() {
    let storage = InMemoryMamStorage::new();
    assert_rsm_cursors_page_same_timestamp_by_archive_id(&storage).await;
}

async fn assert_rsm_cursors_page_same_timestamp_by_archive_id(storage: &dyn MamStorage) {
    let archive = bare("room@conference.example.com");
    let timestamp = chrono::DateTime::parse_from_rfc3339("2026-05-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for id in ["id-c", "id-a", "id-d", "id-b"] {
        storage
            .store_message(
                &archive,
                &ArchivedMessage {
                    id: id.to_string(),
                    timestamp,
                    body: Some(id.to_string()),
                    ..archived_groupchat(&archive)
                },
            )
            .await
            .unwrap();
    }

    let after = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                max: Some(2),
                after_id: Some("id-b".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let after_ids: Vec<&str> = after.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(after_ids, vec!["id-c", "id-d"]);
    assert_eq!(after.first_id.as_deref(), Some("id-c"));
    assert_eq!(after.last_id.as_deref(), Some("id-d"));
    assert!(after.complete);

    let before = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                max: Some(2),
                before_id: Some("id-d".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let before_ids: Vec<&str> = before.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(before_ids, vec!["id-b", "id-c"]);
    assert_eq!(before.first_id.as_deref(), Some("id-b"));
    assert_eq!(before.last_id.as_deref(), Some("id-c"));
    assert!(!before.complete);
}

#[tokio::test]
async fn test_sqlite_extended_before_id_filters_without_flipping_order() {
    let storage = create_test_storage().await;
    assert_extended_before_id_filters_without_flipping_order(&storage).await;
}

#[tokio::test]
async fn test_inmemory_extended_before_id_filters_without_flipping_order() {
    let storage = InMemoryMamStorage::new();
    assert_extended_before_id_filters_without_flipping_order(&storage).await;
}

async fn assert_extended_before_id_filters_without_flipping_order(storage: &dyn MamStorage) {
    let archive = bare("room@conference.example.com");
    let base = Utc::now();
    store_nonlexical_archive_order_messages(storage, &archive, base).await;

    let result = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                filter_before_id: Some("x-fifth".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(bodies(&result), vec!["one", "two", "three", "four"]);
    assert_eq!(result.first_id.as_deref(), Some("z-first"));
    assert_eq!(result.last_id.as_deref(), Some("b-fourth"));
    assert_eq!(result.count, Some(4));
}

#[tokio::test]
async fn test_sqlite_extended_ids_query_returns_specific_messages() {
    let storage = create_test_storage().await;
    assert_extended_ids_query_returns_specific_messages(&storage).await;
}

#[tokio::test]
async fn test_inmemory_extended_ids_query_returns_specific_messages() {
    let storage = InMemoryMamStorage::new();
    assert_extended_ids_query_returns_specific_messages(&storage).await;
}

async fn assert_extended_ids_query_returns_specific_messages(storage: &dyn MamStorage) {
    let archive = bare("room@conference.example.com");
    let base = Utc::now();
    store_nonlexical_archive_order_messages(storage, &archive, base).await;

    let result = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                ids: vec!["x-fifth".to_string(), "a-second".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let ids: Vec<&str> = result
        .messages
        .iter()
        .map(|message| message.id.as_str())
        .collect();
    assert_eq!(ids, vec!["a-second", "x-fifth"]);
    assert_eq!(bodies(&result), vec!["two", "five"]);
    assert_eq!(result.count, Some(2));
}

async fn store_nonlexical_archive_order_messages(
    storage: &dyn MamStorage,
    archive: &BareJid,
    base: DateTime<Utc>,
) {
    let archive_jid = jid(&archive.to_string());
    for (offset, id, body) in [
        (0, "z-first", "one"),
        (1, "a-second", "two"),
        (2, "y-third", "three"),
        (3, "b-fourth", "four"),
        (4, "x-fifth", "five"),
    ] {
        storage
            .store_message(
                archive,
                &ArchivedMessage {
                    id: id.to_string(),
                    timestamp: base + ChronoDuration::seconds(offset),
                    body: Some(body.to_string()),
                    ..ArchivedMessage::for_test(user_device(), archive_jid.clone())
                },
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn test_sqlite_missing_rsm_cursor_returns_not_found() {
    let storage = create_test_storage().await;
    assert_missing_rsm_cursor_returns_not_found(&storage).await;
}

#[tokio::test]
async fn test_inmemory_missing_rsm_cursor_returns_not_found() {
    let storage = InMemoryMamStorage::new();
    assert_missing_rsm_cursor_returns_not_found(&storage).await;
}

async fn assert_missing_rsm_cursor_returns_not_found(storage: &dyn MamStorage) {
    let archive = bare("room@conference.example.com");
    let archive_jid = jid("room@conference.example.com");
    storage
        .store_message(
            &archive,
            &ArchivedMessage {
                id: "known-id".to_string(),
                body: Some("known".to_string()),
                ..ArchivedMessage::for_test(user_device(), archive_jid)
            },
        )
        .await
        .unwrap();

    let error = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                after_id: Some("missing-id".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("missing cursor must be an error");

    assert!(matches!(error, MamStorageError::NotFound(ref id) if id == "missing-id"));

    let error = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                before_id: Some("missing-before-id".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("missing before cursor must be an error");

    assert!(matches!(error, MamStorageError::NotFound(ref id) if id == "missing-before-id"));

    let error = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                filter_before_id: Some("missing-filter-before-id".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("missing extended before-id must be an error");

    assert!(matches!(error, MamStorageError::NotFound(ref id) if id == "missing-filter-before-id"));

    let error = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                filter_after_id: Some("missing-filter-after-id".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("missing extended after-id must be an error");

    assert!(matches!(error, MamStorageError::NotFound(ref id) if id == "missing-filter-after-id"));

    let error = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                ids: vec!["known-id".to_string(), "missing-query-id".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect_err("missing ids entry must be an error");

    assert!(matches!(error, MamStorageError::NotFound(ref id) if id == "missing-query-id"));
}

#[tokio::test]
async fn test_sqlite_rsm_cursor_outside_query_filters_still_pages() {
    let storage = create_test_storage().await;
    assert_rsm_cursor_outside_query_filters_still_pages(&storage).await;
}

#[tokio::test]
async fn test_inmemory_rsm_cursor_outside_query_filters_still_pages() {
    let storage = InMemoryMamStorage::new();
    assert_rsm_cursor_outside_query_filters_still_pages(&storage).await;
}

async fn assert_rsm_cursor_outside_query_filters_still_pages(storage: &dyn MamStorage) {
    let archive = bare("room@conference.example.com");
    let base = Utc::now();
    store_nonlexical_archive_order_messages(storage, &archive, base).await;

    let result = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                start: Some(base + ChronoDuration::seconds(3)),
                after_id: Some("a-second".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(bodies(&result), vec!["four", "five"]);
}

#[tokio::test]
async fn test_sqlite_rsm_max_zero_returns_count_only() {
    let storage = create_test_storage().await;
    assert_rsm_max_zero_returns_count_only(&storage).await;
}

#[tokio::test]
async fn test_inmemory_rsm_max_zero_returns_count_only() {
    let storage = InMemoryMamStorage::new();
    assert_rsm_max_zero_returns_count_only(&storage).await;
}

async fn assert_rsm_max_zero_returns_count_only(storage: &dyn MamStorage) {
    let archive = bare("room@conference.example.com");
    let base = Utc::now();
    store_nonlexical_archive_order_messages(storage, &archive, base).await;

    let result = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                max: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert!(result.messages.is_empty());
    assert_eq!(result.first_id, None);
    assert_eq!(result.last_id, None);
    assert_eq!(result.count, Some(5));
    assert!(result.complete);
}

#[tokio::test]
async fn test_sqlite_rsm_empty_edge_pages_omit_first_and_last() {
    let storage = create_test_storage().await;
    assert_rsm_empty_edge_pages_omit_first_and_last(&storage).await;
}

#[tokio::test]
async fn test_inmemory_rsm_empty_edge_pages_omit_first_and_last() {
    let storage = InMemoryMamStorage::new();
    assert_rsm_empty_edge_pages_omit_first_and_last(&storage).await;
}

async fn assert_rsm_empty_edge_pages_omit_first_and_last(storage: &dyn MamStorage) {
    let archive = bare("room@conference.example.com");
    let base = Utc::now();
    store_nonlexical_archive_order_messages(storage, &archive, base).await;

    let after_last = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                max: Some(2),
                after_id: Some("x-fifth".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(after_last.messages.is_empty());
    assert!(after_last.complete);
    assert_eq!(after_last.first_id, None);
    assert_eq!(after_last.last_id, None);
    assert_eq!(after_last.count, Some(5));

    let before_first = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                max: Some(2),
                before_id: Some("z-first".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(before_first.messages.is_empty());
    assert!(before_first.complete);
    assert_eq!(before_first.first_id, None);
    assert_eq!(before_first.last_id, None);
    assert_eq!(before_first.count, Some(5));
}

fn bodies(result: &MamResult) -> Vec<&str> {
    result
        .messages
        .iter()
        .map(|message| message.body.as_deref().unwrap_or(""))
        .collect()
}

#[tokio::test]
async fn test_thread_query_filters_before_pagination_and_count() {
    let storage = create_test_storage().await;
    let archive = bare("room@conference.example.com");

    for msg in [
        ArchivedMessage {
            id: "a-thread-root".to_string(),
            body: Some("root".to_string()),
            ..archived_groupchat(&archive)
        },
        ArchivedMessage {
            id: "b-thread-reply".to_string(),
            thread: Some(waddle_xmpp_core::xep0201::ThreadInfo::root(
                waddle_xmpp_core::mam::ThreadId::new("a-thread-root").expect("thread id"),
            )),
            body: Some("reply".to_string()),
            ..archived_groupchat(&archive)
        },
        ArchivedMessage {
            id: "c-legacy-reply".to_string(),
            reply: Some(waddle_xmpp_core::mam::ArchivedReply {
                id: waddle_xmpp_core::mam::RichMessageId::new("a-thread-root")
                    .expect("non-empty reply id"),
                to: None,
            }),
            body: Some("legacy".to_string()),
            ..archived_groupchat(&archive)
        },
        ArchivedMessage {
            id: "unrelated".to_string(),
            thread: Some(waddle_xmpp_core::xep0201::ThreadInfo::root(
                waddle_xmpp_core::mam::ThreadId::new("other-thread").expect("thread id"),
            )),
            body: Some("unrelated".to_string()),
            ..archived_groupchat(&archive)
        },
    ] {
        storage.store_message(&archive, &msg).await.unwrap();
    }

    let result = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                thread_id: waddle_xmpp_core::mam::ThreadId::new("a-thread-root"),
                max: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let ids: Vec<&str> = result
        .messages
        .iter()
        .map(|message| message.id.as_str())
        .collect();
    assert_eq!(ids, vec!["a-thread-root", "b-thread-reply"]);
    assert_eq!(result.count, Some(3));
    assert!(!result.complete);
}

#[tokio::test]
async fn test_fulltext_query_filters_before_pagination_and_count() {
    let storage = create_test_storage().await;
    let archive = bare("room@conference.example.com");

    for msg in [
        ArchivedMessage {
            id: "a-alpha".to_string(),
            body: Some("release notes alpha".to_string()),
            ..archived_groupchat(&archive)
        },
        ArchivedMessage {
            id: "b-beta".to_string(),
            body: Some("release notes beta".to_string()),
            ..archived_groupchat(&archive)
        },
        ArchivedMessage {
            id: "c-other".to_string(),
            body: Some("standup notes".to_string()),
            ..archived_groupchat(&archive)
        },
    ] {
        storage.store_message(&archive, &msg).await.unwrap();
    }

    let result = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                fulltext: waddle_xmpp_core::mam::RichText::new("release notes"),
                max: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let ids: Vec<&str> = result
        .messages
        .iter()
        .map(|message| message.id.as_str())
        .collect();
    assert_eq!(ids, vec!["a-alpha"]);
    assert_eq!(result.count, Some(2));
    assert!(!result.complete);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_sqlite_file_backing_persists() {
    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    let path = artifacts.join(format!("mam-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", path.display());
    let archive = bare("room@conference.example.com");
    let archive_jid = jid("room@conference.example.com");

    {
        let storage = SqlxMamStorage::open(&database_url).await.expect("storage");
        let msg = ArchivedMessage {
            body: Some("persisted".to_string()),
            ..ArchivedMessage::for_test(user_device(), archive_jid.clone())
        };
        storage.store_message(&archive, &msg).await.expect("store");
    }

    let reopened = SqlxMamStorage::open(&database_url).await.expect("reopen");
    let result = reopened
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("query");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].body.as_deref(), Some("persisted"));

    for cleanup in [
        path.clone(),
        PathBuf::from(format!("{}-shm", path.display())),
        PathBuf::from(format!("{}-wal", path.display())),
    ] {
        let _ = std::fs::remove_file(cleanup);
    }
}

#[tokio::test]
async fn reaction_round_trips_through_in_memory_storage() {
    let storage = create_test_storage().await;
    let archive = bare("room@conference.example.com");
    let archive_jid = jid("room@conference.example.com");

    let target_id = waddle_xmpp_core::mam::RichMessageId::new("room-stanza-original")
        .expect("non-empty target id");
    let thumbs_up = waddle_xmpp_core::mam::RichText::new("👍").expect("non-empty emoji literal");
    let reactions = waddle_xmpp_core::mam::ArchivedReactionSet {
        target_id: target_id.clone(),
        emojis: vec![thumbs_up.clone()],
    };
    let msg = ArchivedMessage {
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "room-stanza-reaction",
            archive_jid.clone(),
        )),
        message_type: xmpp_parsers::message::MessageType::Groupchat,
        rich: Some(waddle_xmpp_core::mam::ArchivedRichMessage {
            payload: Some(waddle_xmpp_core::mam::ArchivedRichPayload::Reactions(
                reactions,
            )),
            ..Default::default()
        }),
        ..ArchivedMessage::for_test(archive_alice(&archive), archive_jid.clone())
    };

    storage.store_message(&archive, &msg).await.expect("store");

    let result = storage
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("query");

    assert_eq!(result.messages.len(), 1);
    let archived = &result.messages[0];
    assert!(
        archived.body.is_none(),
        "reaction-only row must have no body"
    );
    let rich = archived.rich.as_ref().expect("rich payload survives");
    match rich.payload.as_ref() {
        Some(waddle_xmpp_core::mam::ArchivedRichPayload::Reactions(set)) => {
            assert_eq!(set.target_id.as_str(), "room-stanza-original");
            assert_eq!(
                set.emojis.iter().map(|e| e.as_str()).collect::<Vec<_>>(),
                vec!["👍"]
            );
        }
        other => panic!("expected Reactions rich payload, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reaction_round_trips_through_persistent_sqlite() {
    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    let path = artifacts.join(format!("mam-reaction-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", path.display());
    let archive = bare("room@conference.example.com");
    let archive_jid = jid("room@conference.example.com");

    let target_id = waddle_xmpp_core::mam::RichMessageId::new("room-stanza-original")
        .expect("non-empty target id");
    let thumbs_up = waddle_xmpp_core::mam::RichText::new("👍").expect("non-empty emoji literal");

    {
        let storage = SqlxMamStorage::open(&database_url).await.expect("storage");
        let msg = ArchivedMessage {
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "room-stanza-reaction",
                archive_jid.clone(),
            )),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            rich: Some(waddle_xmpp_core::mam::ArchivedRichMessage {
                payload: Some(waddle_xmpp_core::mam::ArchivedRichPayload::Reactions(
                    waddle_xmpp_core::mam::ArchivedReactionSet {
                        target_id: target_id.clone(),
                        emojis: vec![thumbs_up.clone()],
                    },
                )),
                ..Default::default()
            }),
            ..ArchivedMessage::for_test(archive_alice(&archive), archive_jid.clone())
        };
        storage.store_message(&archive, &msg).await.expect("store");
    }

    let reopened = SqlxMamStorage::open(&database_url).await.expect("reopen");
    let result = reopened
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("query");

    assert_eq!(result.messages.len(), 1);
    let archived = &result.messages[0];
    let rich = archived
        .rich
        .as_ref()
        .expect("rich payload survives a reopen");
    match rich.payload.as_ref() {
        Some(waddle_xmpp_core::mam::ArchivedRichPayload::Reactions(set)) => {
            assert_eq!(set.target_id.as_str(), "room-stanza-original");
            assert_eq!(
                set.emojis.iter().map(|e| e.as_str()).collect::<Vec<_>>(),
                vec!["👍"]
            );
        }
        other => panic!("expected Reactions rich payload, got {other:?}"),
    }

    for cleanup in [
        path.clone(),
        PathBuf::from(format!("{}-shm", path.display())),
        PathBuf::from(format!("{}-wal", path.display())),
    ] {
        let _ = std::fs::remove_file(cleanup);
    }
}

/// Regression: pre-#228 deployments created `mam_messages` with
/// `body TEXT NOT NULL`. `CREATE TABLE IF NOT EXISTS` is a no-op
/// against the existing table, so the constraint never gets dropped
/// even after the schema source was relaxed in #228. Every body-less
/// archive write (XEP-0444 reactions, XEP-0424 retractions, sticker-
/// / shared-file-only stanzas) was then rejected by the engine with
/// `23502 not_null_violation` (Postgres) or
/// `NOT NULL constraint failed: mam_messages.body` (SQLite), dropping
/// the row entirely.
///
/// Production manifested this as "MUC reactions vanish after refresh"
/// — live reactions reflected to occupants normally, then never
/// appeared in MAM replay because the archive write was dropped.
/// This test pins the migration round-trip end-to-end:
///
/// 1. Pre-populate a SQLite file with the *legacy* `body TEXT NOT
///    NULL` schema, mirroring production drift.
/// 2. Open via `SqlxMamStorage` — `ensure_sqlite_schema` detects the
///    legacy shape via `PRAGMA table_info` and runs the 12-step table
///    rebuild that relaxes `body`.
/// 3. Write a body-less reaction `ArchivedMessage`. Pre-fix this
///    would fail with `NOT NULL constraint failed`.
/// 4. Query and assert the row lands with the reactions rich payload
///    intact.
#[tokio::test(flavor = "multi_thread")]
async fn reaction_lands_after_legacy_body_not_null_schema_migrated() {
    use sqlx::sqlite::SqlitePoolOptions;

    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    let path = artifacts.join(format!("mam-legacy-body-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}?mode=rwc", path.display());

    // 1. Pre-populate the file with the legacy `body TEXT NOT NULL`
    //    schema. Mirror exactly what a pre-#228 deploy looked like —
    //    no rich_payload column, no stanza_xml column, etc.
    {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("open raw pool");
        sqlx::query(
            r#"
            CREATE TABLE mam_messages (
                id TEXT PRIMARY KEY,
                room_jid TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                from_jid TEXT NOT NULL,
                to_jid TEXT NOT NULL,
                body TEXT NOT NULL,
                stanza_id TEXT,
                thread_id TEXT,
                reply_to_id TEXT,
                reply_to_jid TEXT,
                origin_id TEXT,
                message_type TEXT NOT NULL DEFAULT 'chat',
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("seed legacy schema");
        pool.close().await;
    }

    // 2. Open via SqlxMamStorage — schema migration runs.
    let archive = bare("room@conference.example.com");
    let archive_jid = jid("room@conference.example.com");
    let storage = SqlxMamStorage::open(&database_url)
        .await
        .expect("storage open after legacy seed");

    // 3. Write a body-less reaction. Pre-fix this would fail with
    //    `NOT NULL constraint failed: mam_messages.body`.
    let target_id = waddle_xmpp_core::mam::RichMessageId::new("room-stanza-original")
        .expect("non-empty target id");
    let thumbs_up = waddle_xmpp_core::mam::RichText::new("👍").expect("non-empty emoji literal");
    let msg = ArchivedMessage {
        body: None,
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "room-stanza-reaction",
            archive_jid.clone(),
        )),
        message_type: xmpp_parsers::message::MessageType::Groupchat,
        rich: Some(waddle_xmpp_core::mam::ArchivedRichMessage {
            payload: Some(waddle_xmpp_core::mam::ArchivedRichPayload::Reactions(
                waddle_xmpp_core::mam::ArchivedReactionSet {
                    target_id: target_id.clone(),
                    emojis: vec![thumbs_up.clone()],
                },
            )),
            ..Default::default()
        }),
        ..ArchivedMessage::for_test(archive_alice(&archive), archive_jid.clone())
    };
    storage
        .store_message(&archive, &msg)
        .await
        .expect("body-less reaction lands after legacy migration");

    // 4. Confirm the row is there with the reaction payload intact.
    let result = storage
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("query");
    assert_eq!(result.messages.len(), 1);
    let archived = &result.messages[0];
    assert!(
        archived.body.is_none(),
        "reaction row must round-trip with NULL body"
    );
    let rich = archived
        .rich
        .as_ref()
        .expect("rich payload survives the legacy migration");
    match rich.payload.as_ref() {
        Some(waddle_xmpp_core::mam::ArchivedRichPayload::Reactions(set)) => {
            assert_eq!(set.target_id.as_str(), "room-stanza-original");
            assert_eq!(
                set.emojis.iter().map(|e| e.as_str()).collect::<Vec<_>>(),
                vec!["👍"]
            );
        }
        other => panic!("expected Reactions rich payload, got {other:?}"),
    }

    for cleanup in [
        path.clone(),
        PathBuf::from(format!("{}-shm", path.display())),
        PathBuf::from(format!("{}-wal", path.display())),
    ] {
        let _ = std::fs::remove_file(cleanup);
    }
}

// XEP-0059 §2.5: an empty <before/> element requests the last page of
// results. Regression test for a bug where `before_id = Some("")` was
// collapsed to "no pagination" and the query returned the *first* page
// (oldest N) instead of the last page (newest N).
#[tokio::test]
async fn test_empty_before_returns_last_page() {
    let storage = create_test_storage().await;
    let archive = bare("room@conference.example.com");
    let archive_jid = jid("room@conference.example.com");
    let base = Utc::now();

    for (offset, body) in ["one", "two", "three", "four", "five", "six"]
        .into_iter()
        .enumerate()
    {
        let msg = ArchivedMessage {
            timestamp: base + ChronoDuration::seconds(offset as i64),
            body: Some(body.to_string()),
            ..ArchivedMessage::for_test(user_device(), archive_jid.clone())
        };
        storage.store_message(&archive, &msg).await.unwrap();
    }

    let last_page = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                max: Some(3),
                before_id: Some(String::new()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let bodies: Vec<&str> = last_page
        .messages
        .iter()
        .map(|m| m.body.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(bodies, vec!["four", "five", "six"]);
    assert!(!last_page.complete);
}

fn archived_groupchat(archive: &BareJid) -> ArchivedMessage {
    ArchivedMessage {
        message_type: xmpp_parsers::message::MessageType::Groupchat,
        ..ArchivedMessage::for_test(archive_alice(archive), jid(&archive.to_string()))
    }
}

#[tokio::test]
async fn xep_0424_tombstone_scrubs_parent_thread_id() {
    // XEP-0424 §Tombstones: replace `<body/>` and any related
    // elements which might leak information. `parent_thread_id`
    // identifies the parent thread and so must be cleared.
    use waddle_xmpp_core::mam::{ArchivedRichMessage, ArchivedTombstone, RichMessageId};

    let storage = create_test_storage().await;
    let archive = bare("room@conference.example.com");
    let archive_jid = jid("room@conference.example.com");
    let msg = ArchivedMessage {
            body: Some("secret thread content".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "wire-id-1",
                archive_jid.clone(),
            )),
            thread: Some(waddle_xmpp_core::xep0201::ThreadInfo::child(
                waddle_xmpp_core::mam::ThreadId::new("child-thread").expect("thread id"),
                waddle_xmpp_core::mam::ThreadId::new("root-thread").expect("parent id"),
            )),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client'><body>secret</body><thread parent='root-thread'>child-thread</thread></message>".to_string(),
            ),
            ..ArchivedMessage::for_test(archive_alice(&archive), archive_jid.clone())
        };
    let archive_id = expect_stored(storage.store_message(&archive, &msg).await.unwrap());

    let tombstone = ArchivedTombstone {
        retraction_id: Some(RichMessageId::new("retract-1").expect("rich id")),
        stamp: Utc::now(),
        moderation: None,
        sender_scope: None,
    };
    let replaced = storage
        .replace_with_tombstone(&archive_id, tombstone)
        .await
        .unwrap();
    assert!(replaced);

    let row = storage
        .get_message(&archive_id)
        .await
        .unwrap()
        .expect("tombstone row");

    assert!(row.body.is_none(), "body must be cleared");
    assert!(
        row.stanza_xml.is_none(),
        "stanza_xml must be cleared so the original wire form does not leak"
    );
    assert!(
        row.thread.is_none(),
        "thread (id and optional parent) is leak-prone, must be cleared"
    );
    assert!(
        row.reply.is_none(),
        "reply (id and optional sender JID) is leak-prone, must be cleared"
    );

    // The row's rich payload must be the tombstone marker — a
    // `<retracted/>`-only message with no `<thread/>` ever
    // re-emitted on replay.
    let rich = row.rich.expect("tombstone row has rich payload");
    assert!(
        matches!(
            rich,
            ArchivedRichMessage {
                payload: Some(waddle_xmpp_core::mam::ArchivedRichPayload::Tombstone(_)),
                ..
            }
        ),
        "tombstone rich payload variant must be `Tombstone`"
    );
}

#[tokio::test]
async fn xep_0425_moderation_tombstone_scrubs_parent_thread_id() {
    // XEP-0425 §Tombstones uses the same scrub rule as XEP-0424;
    // the only difference is the `<moderated/>` annotation in the
    // rich payload. Same leak-prone fields must be cleared.
    use waddle_xmpp_core::mam::{ArchivedModeration, ArchivedTombstone, RichMessageId};

    let storage = create_test_storage().await;
    let archive = bare("room@conference.example.com");
    let archive_jid = jid("room@conference.example.com");
    let msg = ArchivedMessage {
            body: Some("moderated content".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "wire-id-2",
                archive_jid.clone(),
            )),
            thread: Some(waddle_xmpp_core::xep0201::ThreadInfo::child(
                waddle_xmpp_core::mam::ThreadId::new("child-thread").expect("thread id"),
                waddle_xmpp_core::mam::ThreadId::new("root-thread").expect("parent id"),
            )),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client'><body>x</body><thread parent='root-thread'>child-thread</thread></message>".to_string(),
            ),
            ..ArchivedMessage::for_test(archive_alice(&archive), archive_jid.clone())
        };
    let archive_id = expect_stored(storage.store_message(&archive, &msg).await.unwrap());

    let moderator: jid::Jid = "mod@example.com".parse().expect("jid");
    let tombstone = ArchivedTombstone {
        retraction_id: None,
        stamp: Utc::now(),
        moderation: Some(ArchivedModeration {
            target_id: RichMessageId::new("wire-id-2").expect("rich id"),
            moderated_by: moderator,
            stamp: Some(Utc::now()),
            reason: None,
        }),
        sender_scope: None,
    };
    storage
        .replace_with_tombstone(&archive_id, tombstone)
        .await
        .unwrap();

    let row = storage
        .get_message(&archive_id)
        .await
        .unwrap()
        .expect("tombstone row");

    assert!(row.thread.is_none());
    assert!(row.body.is_none());
    assert!(row.stanza_xml.is_none());

    // Verify the tombstone is the moderation variant (covers the
    // XEP-0425 path specifically).
    let rich = row.rich.expect("tombstone row has rich payload");
    match rich.payload {
        Some(waddle_xmpp_core::mam::ArchivedRichPayload::Tombstone(t)) => {
            assert!(
                t.moderation.is_some(),
                "moderation tombstone must carry XEP-0425 moderation annotation"
            );
        }
        other => panic!("expected Tombstone, got {other:?}"),
    }
}

#[tokio::test]
async fn xep_0313_sqlx_archive_returns_messages_in_chronological_order() {
    // XEP-0313 §archive_order: results MUST be returned in the order the
    // client originally received them (chronological), with id used only
    // as a tiebreak. Sorting by id alone breaks this if id generation is
    // ever decoupled from receive time (custom assignment, backfill, etc.).
    let storage = create_test_storage().await;
    let archive = bare("room@conference.example.com");
    let t0 = chrono::DateTime::parse_from_rfc3339("2026-05-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let t1 = t0 + chrono::Duration::seconds(10);

    // Earlier message gets the lexicographically *later* id, so id-only
    // ordering would invert the chronological sequence.
    let earlier = ArchivedMessage {
        id: "zzz-earlier".to_string(),
        timestamp: t0,
        body: Some("first".to_string()),
        ..archived_groupchat(&archive)
    };
    let later = ArchivedMessage {
        id: "aaa-later".to_string(),
        timestamp: t1,
        body: Some("second".to_string()),
        ..archived_groupchat(&archive)
    };

    storage.store_message(&archive, &later).await.unwrap();
    storage.store_message(&archive, &earlier).await.unwrap();

    let result = storage
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await
        .unwrap();

    let bodies: Vec<&str> = result
        .messages
        .iter()
        .map(|m| m.body.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(
        bodies,
        vec!["first", "second"],
        "MAM results must be in chronological order, not id order"
    );
}

#[tokio::test]
async fn xep_0313_in_memory_archive_returns_messages_in_chronological_order() {
    let storage = InMemoryMamStorage::new();
    let archive = bare("room@conference.example.com");
    let t0 = chrono::DateTime::parse_from_rfc3339("2026-05-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let t1 = t0 + chrono::Duration::seconds(10);

    let earlier = ArchivedMessage {
        id: "zzz-earlier".to_string(),
        timestamp: t0,
        body: Some("first".to_string()),
        ..archived_groupchat(&archive)
    };
    let later = ArchivedMessage {
        id: "aaa-later".to_string(),
        timestamp: t1,
        body: Some("second".to_string()),
        ..archived_groupchat(&archive)
    };

    storage.store_message(&archive, &later).await.unwrap();
    storage.store_message(&archive, &earlier).await.unwrap();

    let result = storage
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await
        .unwrap();

    let bodies: Vec<&str> = result
        .messages
        .iter()
        .map(|m| m.body.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(
        bodies,
        vec!["first", "second"],
        "in-memory MAM ordering must be chronological"
    );
}

#[tokio::test]
async fn xep_0313_sqlx_archive_uses_id_as_deterministic_tiebreak_when_timestamps_match() {
    // XEP-0313 §archive_order warns that "multiple messages may share the
    // same timestamp", so the order MUST still be deterministic. We use
    // archive id as the secondary key.
    let storage = create_test_storage().await;
    let archive = bare("room@conference.example.com");
    let t = chrono::DateTime::parse_from_rfc3339("2026-05-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    let first = ArchivedMessage {
        id: "id-001".to_string(),
        timestamp: t,
        body: Some("first".to_string()),
        ..archived_groupchat(&archive)
    };
    let second = ArchivedMessage {
        id: "id-002".to_string(),
        timestamp: t,
        body: Some("second".to_string()),
        ..archived_groupchat(&archive)
    };

    // Insert out of id order to make the assertion meaningful.
    storage.store_message(&archive, &second).await.unwrap();
    storage.store_message(&archive, &first).await.unwrap();

    let result = storage
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await
        .unwrap();
    let ids: Vec<&str> = result.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["id-001", "id-002"],
        "tied timestamps must be ordered by archive id ascending"
    );
}

/// Build an `ArchivedMessage` that mirrors the production data layout:
///
/// - `archive_id`: canonical XEP-0359 room-stamped UUID — stored in the SQL
///   `id` column (primary key). This is what `MamQuery.stanza_ids` filters
///   against, and what the chat client supplies via `roomAssignedStanzaId`.
/// - `wire_id`: the client's `<message id>` attribute — stored in the SQL
///   `stanza_id` column. Different from `archive_id`.
///
/// See `groupchat_archive.rs:10,94-97` for the authoritative server-side
/// assignment.
fn archived_with_archive_and_wire_id(archive_id: &str, wire_id: &str) -> ArchivedMessage {
    let archive = bare("room@conf.example");
    ArchivedMessage {
        id: archive_id.to_string(),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            wire_id.to_string(),
            jid(&archive.to_string()),
        )),
        ..archived_groupchat(&archive)
    }
}

#[tokio::test]
async fn in_memory_query_filters_by_stanza_id() {
    // `MamQuery.stanza_ids` filters by the canonical XEP-0359 room-stamped id,
    // stored in the `id` column (not the `stanza_id` column which holds the
    // wire <message id>). Confirmed against `groupchat_archive.rs:10,94-97`.
    //
    // archive_id "uuid-A/B/C" = canonical room UUID (what pin's target_stanza_id is)
    // wire_id    "wire-A/B/C" = client's <message id> (different column)
    let store = InMemoryMamStorage::new();
    let archive = bare("room@conf.example");

    store
        .store_message(
            &archive,
            &archived_with_archive_and_wire_id("uuid-A", "wire-A"),
        )
        .await
        .expect("store m1");
    store
        .store_message(
            &archive,
            &archived_with_archive_and_wire_id("uuid-B", "wire-B"),
        )
        .await
        .expect("store m2");
    store
        .store_message(
            &archive,
            &archived_with_archive_and_wire_id("uuid-C", "wire-C"),
        )
        .await
        .expect("store m3");

    let result = store
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                stanza_ids: vec![filter_id("uuid-A"), filter_id("uuid-C")],
                ..Default::default()
            },
        )
        .await
        .expect("query ok");
    let got: std::collections::HashSet<&str> =
        result.messages.iter().map(|m| m.id.as_str()).collect();
    let want: std::collections::HashSet<&str> = ["uuid-A", "uuid-C"].into_iter().collect();
    assert_eq!(got, want);
}

#[tokio::test]
async fn in_memory_query_stanza_id_no_match_returns_empty() {
    let store = InMemoryMamStorage::new();
    let archive = bare("room@conf.example");
    store
        .store_message(
            &archive,
            &archived_with_archive_and_wire_id("uuid-A", "wire-A"),
        )
        .await
        .expect("store m1");

    let result = store
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                stanza_ids: vec![filter_id("uuid-missing")],
                ..Default::default()
            },
        )
        .await
        .expect("query ok");
    assert!(result.messages.is_empty());
    assert!(result.complete);
}

#[tokio::test]
async fn sqlx_query_filters_by_stanza_id() {
    // `MamQuery.stanza_ids` filters by the canonical XEP-0359 room-stamped id,
    // stored in the `id` column (not the `stanza_id` column which holds the
    // wire <message id>). Confirmed against `groupchat_archive.rs:10,94-97`.
    //
    // archive_id "uuid-A/B/C" = canonical room UUID (what pin's target_stanza_id is)
    // wire_id    "wire-A/B/C" = client's <message id> (different column)
    let store = create_test_storage().await;
    let archive = bare("room@conf.example");
    store
        .store_message(
            &archive,
            &archived_with_archive_and_wire_id("uuid-A", "wire-A"),
        )
        .await
        .expect("store m1");
    store
        .store_message(
            &archive,
            &archived_with_archive_and_wire_id("uuid-B", "wire-B"),
        )
        .await
        .expect("store m2");
    store
        .store_message(
            &archive,
            &archived_with_archive_and_wire_id("uuid-C", "wire-C"),
        )
        .await
        .expect("store m3");

    let result = store
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                stanza_ids: vec![filter_id("uuid-B"), filter_id("uuid-C")],
                ..Default::default()
            },
        )
        .await
        .expect("query ok");
    let got: std::collections::HashSet<&str> =
        result.messages.iter().map(|m| m.id.as_str()).collect();
    let want: std::collections::HashSet<&str> = ["uuid-B", "uuid-C"].into_iter().collect();
    assert_eq!(got, want);
}
