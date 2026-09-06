use super::*;
use std::path::PathBuf;

fn jid(value: &str) -> BareJid {
    value.parse().expect("valid JID")
}

async fn groupchat_notification_recovery_row_count(storage: &DatabaseInboxStorage) -> i64 {
    let mut rows = storage
        .query("SELECT COUNT(*) FROM groupchat_notification_recovery", ())
        .await
        .expect("count recovery rows");
    let row = rows
        .next()
        .await
        .expect("advance count row")
        .expect("count row");
    row.get(0).expect("decode recovery row count")
}

async fn groupchat_notification_recovery_row_count_for_user(
    storage: &DatabaseInboxStorage,
    user: &BareJid,
) -> i64 {
    let mut rows = storage
        .query(
            "SELECT COUNT(*) FROM groupchat_notification_recovery WHERE recipient_bare_jid = ?",
            crate::db_params![user.to_string()],
        )
        .await
        .expect("count recovery rows for user");
    let row = rows
        .next()
        .await
        .expect("advance user recovery count row")
        .expect("user recovery count row");
    row.get(0).expect("decode user recovery row count")
}

fn groupchat_notification_recovery(
    recipient: BareJid,
    room: BareJid,
    stanza_id: &str,
) -> GroupchatNotificationRecovery {
    GroupchatNotificationRecovery {
        key: GroupchatNotificationRecoveryKey {
            recipient,
            room: room.clone(),
            thread_id: None,
            archive_stanza_id: StanzaId::new(stanza_id, room.into()),
        },
        sender_jid: "room@muc.example.com/alice".parse().expect("sender jid"),
        is_live_occupant: true,
        room_members_only: false,
        sender_can_broadcast_channel_mention: true,
        created_at_ms: 42,
    }
}

#[tokio::test]
async fn upsert_in_transaction_rolls_back_or_commits_with_unread_increment() {
    let storage = DatabaseInboxStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let database = storage.database();
    let user = jid("me@example.com");
    let partner = jid("alice@example.com");

    {
        let mut tx = database.begin().await.expect("begin rollback transaction");
        let entry = upsert_in_transaction(
            &mut tx,
            &user,
            InboxEntry::new(partner.clone(), ConversationKind::Direct, "rollback", 10),
            true,
        )
        .await
        .expect("upsert in rollback transaction");
        assert_eq!(entry.unread, 1);
    }
    assert!(storage
        .list(&user)
        .await
        .expect("list after rollback")
        .is_empty());

    let mut tx = database.begin().await.expect("begin commit transaction");
    let first = upsert_in_transaction(
        &mut tx,
        &user,
        InboxEntry::new(partner.clone(), ConversationKind::Direct, "first", 20),
        true,
    )
    .await
    .expect("first upsert in commit transaction");
    let second = upsert_in_transaction(
        &mut tx,
        &user,
        InboxEntry::new(partner.clone(), ConversationKind::Direct, "second", 30),
        true,
    )
    .await
    .expect("second upsert in commit transaction");
    assert_eq!(first.unread, 1);
    assert_eq!(second.unread, 2);
    tx.commit().await.expect("commit transaction");

    let entries = storage.list(&user).await.expect("list after commit");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].last_stanza_id, "second");
    assert_eq!(entries[0].unread, 2);
}

#[tokio::test]
async fn groupchat_notification_recovery_in_transaction_rolls_back_or_commits() {
    let storage = DatabaseInboxStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let database = storage.database();
    let user = jid("me@example.com");
    let room = jid("room@muc.example.com");

    {
        let mut tx = database.begin().await.expect("begin rollback transaction");
        upsert_in_transaction(
            &mut tx,
            &user,
            InboxEntry::new(room.clone(), ConversationKind::MucRoom, "rollback", 10),
            true,
        )
        .await
        .expect("upsert before rollback");
        insert_groupchat_notification_recovery_in_transaction(
            &mut tx,
            groupchat_notification_recovery(user.clone(), room.clone(), "rollback"),
        )
        .await
        .expect("insert recovery before rollback");
    }
    assert!(storage
        .list(&user)
        .await
        .expect("list after rollback")
        .is_empty());
    assert_eq!(groupchat_notification_recovery_row_count(&storage).await, 0);

    let mut tx = database.begin().await.expect("begin commit transaction");
    upsert_in_transaction(
        &mut tx,
        &user,
        InboxEntry::new(room.clone(), ConversationKind::MucRoom, "commit", 20),
        true,
    )
    .await
    .expect("upsert before commit");
    let recovery = groupchat_notification_recovery(user.clone(), room, "commit");
    insert_groupchat_notification_recovery_in_transaction(&mut tx, recovery.clone())
        .await
        .expect("insert recovery before commit");
    tx.commit().await.expect("commit transaction");

    assert_eq!(
        storage.list(&user).await.expect("list after commit").len(),
        1
    );
    assert_eq!(
        storage
            .list_pending_groupchat_notification_recoveries(16)
            .await
            .expect("list committed recovery"),
        vec![recovery]
    );
}

#[tokio::test]
async fn transaction_taking_inbox_helpers_work_on_postgres() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set \
             (postgres-backed transaction-taking inbox helpers)"
        );
        return;
    };

    let storage = DatabaseInboxStorage::open(Some(&database_url))
        .await
        .expect("open postgres storage");
    let database = storage.database();
    let run_id = uuid::Uuid::new_v4();
    let user = jid(&format!("inbox-tx-{run_id}@example.com"));
    let room = jid(&format!("room-{run_id}@muc.example.com"));

    {
        let mut tx = database
            .begin()
            .await
            .expect("begin postgres rollback transaction");
        upsert_in_transaction(
            &mut tx,
            &user,
            InboxEntry::new(room.clone(), ConversationKind::MucRoom, "rollback", 10),
            true,
        )
        .await
        .expect("upsert postgres rollback transaction");
        insert_groupchat_notification_recovery_in_transaction(
            &mut tx,
            groupchat_notification_recovery(user.clone(), room.clone(), "rollback"),
        )
        .await
        .expect("insert postgres recovery before rollback");
    }
    assert!(storage
        .list(&user)
        .await
        .expect("list postgres rollback result")
        .is_empty());
    assert_eq!(
        groupchat_notification_recovery_row_count_for_user(&storage, &user).await,
        0
    );

    let mut tx = database
        .begin()
        .await
        .expect("begin postgres commit transaction");
    let entry = upsert_in_transaction(
        &mut tx,
        &user,
        InboxEntry::new(room.clone(), ConversationKind::MucRoom, "commit", 20),
        true,
    )
    .await
    .expect("upsert postgres commit transaction");
    let recovery = groupchat_notification_recovery(user.clone(), room.clone(), "commit");
    insert_groupchat_notification_recovery_in_transaction(&mut tx, recovery)
        .await
        .expect("insert postgres recovery before commit");
    tx.commit().await.expect("commit postgres transaction");

    assert_eq!(entry.unread, 1);
    assert_eq!(
        storage
            .list(&user)
            .await
            .expect("list postgres commit result")
            .len(),
        1
    );
    assert_eq!(
        groupchat_notification_recovery_row_count_for_user(&storage, &user).await,
        1
    );

    storage
        .execute(
            "DELETE FROM groupchat_notification_recovery WHERE recipient_bare_jid = ?",
            crate::db_params![user.to_string()],
        )
        .await
        .expect("delete postgres recovery test rows");
    storage
        .execute(
            "DELETE FROM inbox_entries WHERE user_jid = ?",
            crate::db_params![user.to_string()],
        )
        .await
        .expect("delete postgres inbox test rows");
}

#[tokio::test]
async fn sqlx_inbox_storage_round_trips_entries() {
    let storage = DatabaseInboxStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let user = jid("me@example.com");
    storage
        .upsert(
            &user,
            InboxEntry::new(jid("alice@example.com"), ConversationKind::Direct, "s1", 10)
                .with_preview("hello"),
            true,
        )
        .await
        .expect("upsert");
    storage
        .upsert(
            &user,
            InboxEntry::new(
                jid("room@muc.example.com"),
                ConversationKind::MucRoom,
                "s2",
                20,
            ),
            false,
        )
        .await
        .expect("upsert");

    let entries = storage.list(&user).await.expect("list");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].partner, jid("room@muc.example.com"));
    assert_eq!(storage.total_unread(&user).await.expect("unread"), 1);

    let updated = storage
        .mark_read(&user, &jid("alice@example.com"), None)
        .await
        .expect("mark read")
        .expect("entry returned for fan-out");
    assert_eq!(updated.unread, 0);
    assert_eq!(updated.partner, jid("alice@example.com"));
    assert!(updated.thread_id.is_none());
    assert_eq!(storage.total_unread(&user).await.expect("unread"), 0);
}

#[tokio::test]
async fn upsert_persists_call_thread_metadata_and_mark_ended_updates_all_users() {
    use waddle_xmpp::xep::{CallThreadDuration, CallThreadKind, CallThreadMedia};
    let storage = DatabaseInboxStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let room: BareJid = "general@conference.example.com".parse().expect("room jid");
    let alice: BareJid = "alice@example.com".parse().expect("alice jid");
    let bob: BareJid = "bob@example.com".parse().expect("bob jid");
    let anchor = || {
        InboxEntry::new(
            room.clone(),
            ConversationKind::MucRoom,
            "anchor-stanza",
            1_700_000_000,
        )
        .with_thread("call-thread-uuid")
        .with_call_thread(
            CallThreadKind::Muc,
            CallThreadMedia {
                audio: true,
                video: false,
            },
        )
    };

    storage.upsert(&alice, anchor(), true).await.expect("alice");
    storage.upsert(&bob, anchor(), true).await.expect("bob");

    let entry = storage
        .list_all_threads(&alice)
        .await
        .expect("list_all_threads")
        .into_iter()
        .find(|e| e.thread_id.as_deref() == Some("call-thread-uuid"))
        .expect("anchor thread");
    assert_eq!(entry.call_thread_kind, Some(CallThreadKind::Muc));
    assert_eq!(
        entry.call_thread_media,
        Some(CallThreadMedia {
            audio: true,
            video: false,
        })
    );
    assert!(entry.call_ended_at.is_none());

    let ended = chrono::DateTime::parse_from_rfc3339("2026-06-07T14:35:00Z")
        .expect("ended")
        .with_timezone(&chrono::Utc);
    storage
        .mark_call_thread_ended(
            &room,
            "call-thread-uuid",
            ended,
            &CallThreadDuration::parse("PT5M").expect("duration"),
        )
        .await
        .expect("mark ended");

    for who in [&alice, &bob] {
        let e = storage
            .list_all_threads(who)
            .await
            .expect("list_all_threads")
            .into_iter()
            .find(|e| e.thread_id.as_deref() == Some("call-thread-uuid"))
            .expect("anchor thread");
        assert_eq!(e.call_ended_at, Some(ended), "ended marked for {who}");
        assert_eq!(
            e.call_duration,
            Some(CallThreadDuration::parse("PT5M").expect("duration"))
        );
        assert_eq!(
            e.call_thread_kind,
            Some(CallThreadKind::Muc),
            "anchor kind survived ended UPDATE for {who}"
        );
    }
}

#[tokio::test]
async fn mark_ended_skips_reply_only_rows_without_call_thread_kind() {
    use waddle_xmpp::xep::{CallThreadDuration, CallThreadKind, CallThreadMedia};
    let storage = DatabaseInboxStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let room: BareJid = "general@conference.example.com".parse().expect("room jid");
    let alice: BareJid = "alice@example.com".parse().expect("alice jid");
    let bob: BareJid = "bob@example.com".parse().expect("bob jid");

    // Alice received the anchor-root projection: a genuine call-thread
    // row with kind + media.
    let anchor = InboxEntry::new(
        room.clone(),
        ConversationKind::MucRoom,
        "anchor-stanza",
        1_700_000_000,
    )
    .with_thread("call-thread-uuid")
    .with_call_thread(
        CallThreadKind::Muc,
        CallThreadMedia {
            audio: true,
            video: false,
        },
    );
    storage.upsert(&alice, anchor, true).await.expect("alice");

    // Bob is a durable user who only received a thread reply, not the
    // anchor projection — a reply-only row on the SAME (room, thread_id)
    // with call_thread_kind / call_thread_media NULL.
    let reply_only = InboxEntry::new(
        room.clone(),
        ConversationKind::MucRoom,
        "reply-stanza",
        1_700_000_100,
    )
    .with_thread("call-thread-uuid");
    assert!(reply_only.call_thread_kind.is_none());
    assert!(reply_only.call_thread_media.is_none());
    storage.upsert(&bob, reply_only, true).await.expect("bob");

    let ended = chrono::DateTime::parse_from_rfc3339("2026-06-07T14:35:00Z")
        .expect("ended")
        .with_timezone(&chrono::Utc);
    storage
        .mark_call_thread_ended(
            &room,
            "call-thread-uuid",
            ended,
            &CallThreadDuration::parse("PT5M").expect("duration"),
        )
        .await
        .expect("mark ended");

    // Alice's genuine call-thread row gets the ended summary stamped.
    let alice_row = storage
        .list_all_threads(&alice)
        .await
        .expect("list_all_threads")
        .into_iter()
        .find(|e| e.thread_id.as_deref() == Some("call-thread-uuid"))
        .expect("alice anchor thread");
    assert_eq!(alice_row.call_ended_at, Some(ended));
    assert_eq!(
        alice_row.call_duration,
        Some(CallThreadDuration::parse("PT5M").expect("duration"))
    );

    // Bob's reply-only row is NOT stamped — stamping it would produce an
    // ended summary without kind/media that the frontend silently drops.
    let bob_row = storage
        .list_all_threads(&bob)
        .await
        .expect("list_all_threads")
        .into_iter()
        .find(|e| e.thread_id.as_deref() == Some("call-thread-uuid"))
        .expect("bob reply-only thread");
    assert!(
        bob_row.call_ended_at.is_none(),
        "reply-only row (call_thread_kind NULL) must not be stamped with the ended summary"
    );
    assert!(
        bob_row.call_duration.is_none(),
        "reply-only row must not gain a call_duration"
    );
    assert!(bob_row.call_thread_kind.is_none());
}

#[tokio::test]
async fn mark_ended_skips_rows_with_kind_but_null_media() {
    // Defense-in-depth for the wire `<call>` condition: it emits only
    // when BOTH kind and media are present. After Fix 1+2 the encoder
    // never persists a kind-without-media row, but a row could exist from
    // a legacy/corrupt write. Insert one directly via raw SQL (bypassing
    // the encoder's normalization) and assert mark-ended does NOT stamp
    // it — stamping would serialize `<call-ended>` without `<call>`.
    use waddle_xmpp::xep::CallThreadDuration;
    let storage = DatabaseInboxStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let room: BareJid = "general@conference.example.com".parse().expect("room jid");
    let user: BareJid = "carol@example.com".parse().expect("carol jid");

    // Raw insert: call_thread_kind = 'muc' but call_thread_media NULL.
    storage
        .execute(
            "INSERT INTO inbox_entries (user_jid, partner_jid, thread_id, kind, last_stanza_id, \
             last_updated, unread, reply_count, call_thread_kind, call_thread_media) \
             VALUES (?, ?, ?, 'muc', 'anchor-stanza', 1700000000, 0, 0, 'muc', NULL)",
            crate::db_params![
                user.to_string(),
                room.to_string(),
                "call-thread-uuid".to_string(),
            ],
        )
        .await
        .expect("raw insert kind-without-media row");

    let ended = chrono::DateTime::parse_from_rfc3339("2026-06-07T14:35:00Z")
        .expect("ended")
        .with_timezone(&chrono::Utc);
    storage
        .mark_call_thread_ended(
            &room,
            "call-thread-uuid",
            ended,
            &CallThreadDuration::parse("PT5M").expect("duration"),
        )
        .await
        .expect("mark ended");

    let row = storage
        .list_all_threads(&user)
        .await
        .expect("list_all_threads")
        .into_iter()
        .find(|e| e.thread_id.as_deref() == Some("call-thread-uuid"))
        .expect("kind-without-media thread");
    assert!(
        row.call_ended_at.is_none(),
        "row with kind set but media NULL must not be stamped with the ended summary"
    );
    assert!(
        row.call_duration.is_none(),
        "row with kind set but media NULL must not gain a call_duration"
    );
}

#[tokio::test]
async fn upsert_reply_preserves_anchor_call_thread_metadata() {
    use waddle_xmpp::xep::{CallThreadKind, CallThreadMedia};
    let storage = DatabaseInboxStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let room: BareJid = "general@conference.example.com".parse().expect("room jid");
    let alice: BareJid = "alice@example.com".parse().expect("alice jid");

    // Anchor the call thread with kind + media.
    let anchor = InboxEntry::new(
        room.clone(),
        ConversationKind::MucRoom,
        "anchor-stanza",
        1_700_000_000,
    )
    .with_thread("call-thread-uuid")
    .with_call_thread(
        CallThreadKind::Muc,
        CallThreadMedia {
            audio: true,
            video: false,
        },
    );
    storage.upsert(&alice, anchor, true).await.expect("anchor");

    // A later PLAIN reply to the same thread carries no call metadata
    // (its call_* columns are NULL); the COALESCE upsert must not wipe
    // the anchor's kind/media.
    let plain_reply = InboxEntry::new(
        room.clone(),
        ConversationKind::MucRoom,
        "reply-stanza",
        1_700_000_100,
    )
    .with_thread("call-thread-uuid");
    assert!(plain_reply.call_thread_kind.is_none());
    assert!(plain_reply.call_thread_media.is_none());
    storage
        .upsert(&alice, plain_reply, true)
        .await
        .expect("plain reply");

    let entry = storage
        .list_all_threads(&alice)
        .await
        .expect("list_all_threads")
        .into_iter()
        .find(|e| e.thread_id.as_deref() == Some("call-thread-uuid"))
        .expect("anchor thread");
    assert_eq!(
        entry.call_thread_kind,
        Some(CallThreadKind::Muc),
        "anchor kind survived a plain reply upsert"
    );
    assert_eq!(
        entry.call_thread_media,
        Some(CallThreadMedia {
            audio: true,
            video: false,
        }),
        "anchor media survived a plain reply upsert"
    );
}

#[tokio::test]
async fn sqlx_inbox_storage_mark_read_returns_none_for_missing_row() {
    let storage = DatabaseInboxStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let user = jid("me@example.com");
    let result = storage
        .mark_read(&user, &jid("ghost@example.com"), None)
        .await
        .expect("mark read");
    assert!(
        result.is_none(),
        "RETURNING must yield no row when no UPDATE matched so the IQ handler skips fan-out"
    );
}

#[tokio::test]
async fn sqlx_inbox_storage_thread_entries() {
    let storage = DatabaseInboxStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let user = jid("me@example.com");
    let room = jid("room@muc.example.com");

    // Channel-level entry
    storage
        .upsert(
            &user,
            InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s1", 100),
            true,
        )
        .await
        .expect("upsert channel");

    // Thread entry
    let thread_entry = storage
        .upsert(
            &user,
            InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s2", 200)
                .with_thread("t1")
                .with_thread_title("Discussion")
                .with_author("alice"),
            true,
        )
        .await
        .expect("upsert thread");
    assert_eq!(thread_entry.thread_id.as_deref(), Some("t1"));
    assert_eq!(thread_entry.thread_title.as_deref(), Some("Discussion"));

    // Channel list excludes threads
    let channels = storage.list(&user).await.expect("list");
    assert_eq!(channels.len(), 1);
    assert!(channels[0].thread_id.is_none());

    // Thread list for room
    let threads = storage
        .list_threads(&user, &room)
        .await
        .expect("list_threads");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].thread_id.as_deref(), Some("t1"));
    assert_eq!(threads[0].author.as_deref(), Some("alice"));

    // Reply increments reply_count
    let updated = storage
        .upsert(
            &user,
            InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s3", 300).with_thread("t1"),
            true,
        )
        .await
        .expect("upsert reply");
    assert_eq!(updated.reply_count, 1);
    assert_eq!(updated.unread, 2);
    // Title preserved from first upsert
    assert_eq!(updated.thread_title.as_deref(), Some("Discussion"));

    // Mark thread read
    let updated = storage
        .mark_read(&user, &room, Some("t1"))
        .await
        .expect("mark thread read")
        .expect("thread entry returned for fan-out");
    assert_eq!(updated.unread, 0);
    assert_eq!(updated.thread_id.as_deref(), Some("t1"));
    let threads = storage
        .list_threads(&user, &room)
        .await
        .expect("list_threads");
    assert_eq!(threads[0].unread, 0);

    // Channel unread unaffected
    assert_eq!(storage.total_unread(&user).await.expect("unread"), 1);
}

#[tokio::test]
async fn sqlx_inbox_storage_tracks_groupchat_notification_recovery() {
    let storage = DatabaseInboxStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let user = jid("me@example.com");
    let room = jid("room@muc.example.com");
    let stanza_id = StanzaId::new(
        "groupchat-recovery-1",
        "room@muc.example.com".parse().expect("stanza-id by"),
    );
    let recovery = GroupchatNotificationRecovery {
        key: GroupchatNotificationRecoveryKey {
            recipient: user.clone(),
            room: room.clone(),
            thread_id: Some("thread-1".to_string()),
            archive_stanza_id: stanza_id.clone(),
        },
        sender_jid: "room@muc.example.com/alice".parse().expect("sender jid"),
        is_live_occupant: true,
        room_members_only: false,
        sender_can_broadcast_channel_mention: true,
        created_at_ms: 42,
    };
    let second_recovery = GroupchatNotificationRecovery {
        key: GroupchatNotificationRecoveryKey {
            recipient: user.clone(),
            room: room.clone(),
            thread_id: None,
            archive_stanza_id: StanzaId::new(
                "groupchat-recovery-2",
                "room@muc.example.com".parse().expect("second stanza-id by"),
            ),
        },
        sender_jid: "room@muc.example.com/bob"
            .parse()
            .expect("second sender jid"),
        is_live_occupant: false,
        room_members_only: true,
        sender_can_broadcast_channel_mention: false,
        created_at_ms: 43,
    };

    storage
        .upsert_with_groupchat_notification_recovery(
            &user,
            InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s2", 200)
                .with_thread("thread-1"),
            true,
            Some(recovery.clone()),
        )
        .await
        .expect("atomic upsert + recovery");

    let pending = storage
        .list_pending_groupchat_notification_recoveries(16)
        .await
        .expect("list pending recoveries");
    assert_eq!(pending, vec![recovery.clone()]);
    assert_eq!(
        storage
            .mark_groupchat_notification_recovery_completed(&recovery.key)
            .await
            .expect("mark completed"),
        1
    );
    assert!(storage
        .list_pending_groupchat_notification_recoveries(16)
        .await
        .expect("list after complete")
        .is_empty());
    storage
        .insert_groupchat_notification_recovery(second_recovery.clone())
        .await
        .expect("insert second recovery");
    assert_eq!(
        storage
            .mark_groupchat_notification_recovery_completed(&second_recovery.key)
            .await
            .expect("mark second completed"),
        1
    );
    assert_eq!(groupchat_notification_recovery_row_count(&storage).await, 2);
    assert_eq!(
        storage
            .prune_completed_groupchat_notification_recoveries(0, 16)
            .await
            .expect("prune before cutoff"),
        0
    );
    assert_eq!(groupchat_notification_recovery_row_count(&storage).await, 2);
    let future_cutoff_ms = crate::time::now_ms().saturating_add(1_000);
    assert_eq!(
        storage
            .prune_completed_groupchat_notification_recoveries(future_cutoff_ms, 1)
            .await
            .expect("bounded prune first row"),
        1
    );
    assert_eq!(groupchat_notification_recovery_row_count(&storage).await, 1);
    assert_eq!(
        storage
            .prune_completed_groupchat_notification_recoveries(future_cutoff_ms, 16)
            .await
            .expect("prune remaining row"),
        1
    );
    assert_eq!(groupchat_notification_recovery_row_count(&storage).await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn sqlx_inbox_storage_persists_file_backing() {
    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    let path = artifacts.join(format!("inbox-{}.db", uuid::Uuid::new_v4()));
    let user = jid("me@example.com");

    {
        let storage = DatabaseInboxStorage::open(Some(&format!("sqlite://{}", path.display())))
            .await
            .expect("storage");
        storage
            .upsert(
                &user,
                InboxEntry::new(
                    jid("alice@example.com"),
                    ConversationKind::Direct,
                    "persisted",
                    30,
                )
                .with_preview("persisted"),
                true,
            )
            .await
            .expect("upsert");
    }

    let reopened = DatabaseInboxStorage::open(Some(&format!("sqlite://{}", path.display())))
        .await
        .expect("reopened storage");
    let entries = reopened.list(&user).await.expect("list");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].last_stanza_id, "persisted");
    assert_eq!(entries[0].preview.as_deref(), Some("persisted"));
    assert_eq!(reopened.total_unread(&user).await.expect("unread"), 1);

    for cleanup in [
        path.clone(),
        PathBuf::from(format!("{}-shm", path.display())),
        PathBuf::from(format!("{}-wal", path.display())),
    ] {
        let _ = std::fs::remove_file(cleanup);
    }
}

/// Regression for #456: `inbox_entries.last_updated` is an i64
/// timestamp. Postgres `INTEGER` is int4, so fresh and existing
/// Postgres tables must expose this column as BIGINT.
#[tokio::test]
async fn sqlx_inbox_postgres_handles_i32_overflow_last_updated() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set \
             (postgres-backed regression for inbox_entries.last_updated BIGINT)"
        );
        return;
    };

    let storage = DatabaseInboxStorage::open(Some(&database_url))
        .await
        .expect("open postgres inbox storage");
    let run_id = uuid::Uuid::new_v4();
    let user = jid(&format!("inbox-{run_id}@example.com"));
    let partner = jid(&format!("partner-{run_id}@example.com"));
    let last_updated = i64::from(i32::MAX) + 86_400;

    let stored = storage
        .upsert(
            &user,
            InboxEntry::new(
                partner.clone(),
                ConversationKind::Direct,
                format!("stanza-{run_id}"),
                last_updated,
            ),
            false,
        )
        .await
        .expect("BIGINT last_updated accepts values past i32::MAX");
    assert_eq!(stored.last_updated, last_updated);

    let listed = storage.list(&user).await.expect("list postgres inbox");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].partner, partner);
    assert_eq!(listed[0].last_updated, last_updated);

    let deleted = storage
        .execute(
            "DELETE FROM inbox_entries WHERE user_jid = ? AND partner_jid = ?",
            crate::db_params![user.to_string(), partner.to_string()],
        )
        .await
        .expect("cleanup inbox postgres row");
    assert_eq!(deleted, 1);
}

/// Regression for the reviewer feedback on PR #738: an existing
/// `groupchat_notification_recovery` table created BEFORE the
/// `sender_can_broadcast_channel_mention` column landed must be
/// migrated forward — without the targeted ALTER, every recovery
/// SELECT errors with "no such column". This test simulates that
/// state by:
///
///   1. opening a fresh DB (CREATE TABLE includes the column),
///   2. DROPping it,
///   3. recreating it WITHOUT the column (the pre-PR shape),
///   4. re-opening / re-initialising the storage,
///   5. asserting the column is present after init.
#[tokio::test(flavor = "multi_thread")]
async fn migration_adds_sender_can_broadcast_channel_mention_to_legacy_recovery_table() {
    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    let path = artifacts.join(format!("inbox-migration-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());

    // Step 1-3: build a DB whose recovery table predates the new
    // column. Open fresh, drop, recreate without the column.
    {
        let storage = DatabaseInboxStorage::open(Some(&url))
            .await
            .expect("open fresh storage");
        storage
            .execute("DROP TABLE groupchat_notification_recovery", ())
            .await
            .expect("drop fresh recovery table");
        storage
            .execute(
                r#"
                CREATE TABLE groupchat_notification_recovery (
                    recipient_bare_jid TEXT NOT NULL,
                    room_jid TEXT NOT NULL,
                    thread_id TEXT NOT NULL DEFAULT '',
                    stanza_id_by TEXT NOT NULL,
                    stanza_id TEXT NOT NULL,
                    sender_jid TEXT NOT NULL,
                    is_live_occupant INTEGER NOT NULL,
                    room_members_only INTEGER NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    completed_at_ms INTEGER,
                    PRIMARY KEY (recipient_bare_jid, room_jid, thread_id, stanza_id_by, stanza_id)
                )
                "#,
                (),
            )
            .await
            .expect("create legacy recovery table");
    }

    // Step 4-5: re-open. The init path should detect the missing
    // column and run the ALTER. Confirm by:
    //   (a) the column is reported by PRAGMA, and
    //   (b) a recovery write succeeds (it would fail with "no such
    //       column" if the migration didn't run).
    let storage = DatabaseInboxStorage::open(Some(&url))
        .await
        .expect("re-open storage triggers migration");

    let mut cols = storage
        .query("PRAGMA table_info(groupchat_notification_recovery)", ())
        .await
        .expect("PRAGMA table_info");
    let mut has_column = false;
    while let Some(row) = cols.next().await.expect("advance pragma row") {
        let name: String = row.get(1).expect("col name");
        if name == "sender_can_broadcast_channel_mention" {
            has_column = true;
            break;
        }
    }
    assert!(
        has_column,
        "migration must add `sender_can_broadcast_channel_mention` to \
         pre-existing groupchat_notification_recovery tables"
    );

    let recovery = waddle_xmpp::inbox::storage::GroupchatNotificationRecovery {
        key: waddle_xmpp::inbox::storage::GroupchatNotificationRecoveryKey {
            recipient: jid("recipient@example.com"),
            room: jid("room@muc.example.com"),
            thread_id: None,
            archive_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
                "migration-test",
                "room@muc.example.com".parse().expect("stanza-id by"),
            ),
        },
        sender_jid: "room@muc.example.com/alice".parse().expect("sender jid"),
        is_live_occupant: true,
        room_members_only: false,
        sender_can_broadcast_channel_mention: true,
        created_at_ms: 42,
    };
    storage
        .insert_groupchat_notification_recovery(recovery.clone())
        .await
        .expect("insert after migration must succeed");
    let pending = storage
        .list_pending_groupchat_notification_recoveries(16)
        .await
        .expect("list after migration must succeed");
    assert_eq!(pending.len(), 1);
    assert!(
        pending[0].sender_can_broadcast_channel_mention,
        "the persisted bool must round-trip through the migrated column"
    );

    // Cleanup
    std::fs::remove_file(&path).ok();
}

/// Issue #919 migration regression: an existing `inbox_entries` table
/// created BEFORE the four `call_*` columns landed must be migrated
/// forward. Without the targeted ALTERs, every `list*` SELECT errors
/// with "no such column". Simulates the pre-#919 shape, re-opens to
/// trigger the migration, then asserts a call-anchor upsert round-trips
/// through the migrated columns.
#[tokio::test(flavor = "multi_thread")]
async fn migration_adds_call_thread_columns_to_legacy_inbox_table() {
    use waddle_xmpp::xep::{CallThreadKind, CallThreadMedia};
    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    let path = artifacts.join(format!("inbox-call-migration-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());

    // Build a DB whose inbox table predates the call-thread columns:
    // open fresh, drop, recreate without the four `call_*` columns.
    {
        let storage = DatabaseInboxStorage::open(Some(&url))
            .await
            .expect("open fresh storage");
        storage
            .execute("DROP TABLE inbox_entries", ())
            .await
            .expect("drop fresh inbox table");
        storage
            .execute(
                r#"
                CREATE TABLE inbox_entries (
                    user_jid TEXT NOT NULL,
                    partner_jid TEXT NOT NULL,
                    thread_id TEXT NOT NULL DEFAULT '',
                    kind TEXT NOT NULL,
                    last_stanza_id TEXT NOT NULL,
                    last_updated INTEGER NOT NULL,
                    unread INTEGER NOT NULL DEFAULT 0,
                    preview TEXT,
                    thread_title TEXT,
                    reply_count INTEGER NOT NULL DEFAULT 0,
                    author TEXT,
                    PRIMARY KEY (user_jid, partner_jid, thread_id)
                )
                "#,
                (),
            )
            .await
            .expect("create legacy inbox table");
    }

    // Re-open. The init path detects the missing columns and runs the
    // ALTERs; a call-anchor upsert then round-trips through them (it
    // would fail with "no such column" if the migration didn't run).
    let storage = DatabaseInboxStorage::open(Some(&url))
        .await
        .expect("re-open storage triggers migration");

    for column in [
        "call_thread_kind",
        "call_thread_media",
        "call_ended_at",
        "call_duration",
    ] {
        let mut cols = storage
            .query("PRAGMA table_info(inbox_entries)", ())
            .await
            .expect("PRAGMA table_info");
        let mut present = false;
        while let Some(row) = cols.next().await.expect("advance pragma row") {
            let name: String = row.get(1).expect("col name");
            if name == column {
                present = true;
                break;
            }
        }
        assert!(
            present,
            "migration must add `{column}` to legacy inbox_entries"
        );
    }

    let user = jid("me@example.com");
    let room = jid("room@muc.example.com");
    storage
        .upsert(
            &user,
            InboxEntry::new(room.clone(), ConversationKind::MucRoom, "anchor", 100)
                .with_thread("t1")
                .with_call_thread(CallThreadKind::Muc, CallThreadMedia::audio_video()),
            true,
        )
        .await
        .expect("upsert after migration must succeed");
    let threads = storage
        .list_threads(&user, &room)
        .await
        .expect("list_threads after migration must succeed");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].call_thread_kind, Some(CallThreadKind::Muc));
    assert_eq!(
        threads[0].call_thread_media,
        Some(CallThreadMedia::audio_video())
    );

    std::fs::remove_file(&path).ok();
}

async fn assert_inbox_upsert_is_monotonic(storage: DatabaseInboxStorage) {
    let user = jid(&format!("monotonic-{}@example.com", uuid::Uuid::new_v4()));
    let peer = jid("peer@example.com");
    let mut older = InboxEntry::new(peer.clone(), ConversationKind::Direct, "a", 10);
    older.preview = Some("older".into());
    older.author = Some("older-author".into());
    let mut newer = InboxEntry::new(peer, ConversationKind::MucRoom, "b", 20);
    newer.preview = Some("newer".into());
    newer.author = Some("newer-author".into());
    storage
        .upsert(&user, older.clone(), true)
        .await
        .expect("insert A");
    storage
        .upsert(&user, newer.clone(), true)
        .await
        .expect("insert B");
    // The ingress ledger suppresses a duplicate's increments; the storage
    // primitive independently prevents its content from rewinding the row.
    let actual = storage.upsert(&user, older, false).await.expect("retry A");
    assert_eq!(actual.last_stanza_id, newer.last_stanza_id);
    assert_eq!(actual.last_updated, newer.last_updated);
    assert_eq!(actual.kind, newer.kind);
    assert_eq!(actual.preview, newer.preview);
    assert_eq!(actual.author, newer.author);
    assert_eq!(actual.unread, 2);

    let equal_timestamp = InboxEntry::new(newer.partner.clone(), ConversationKind::Direct, "a", 20);
    let actual = storage
        .upsert(&user, equal_timestamp, true)
        .await
        .expect("older id at same timestamp");
    assert_eq!(actual.last_stanza_id, "b");
    assert_eq!(
        actual.unread, 3,
        "new older messages still increment unread"
    );
    let newer_id = InboxEntry::new(newer.partner, ConversationKind::Direct, "c", 20);
    let actual = storage
        .upsert(&user, newer_id, true)
        .await
        .expect("newer id at same timestamp");
    assert_eq!(actual.last_stanza_id, "c");
    assert_eq!(actual.unread, 4);
}

#[tokio::test]
async fn inbox_upsert_a_b_retry_a_preserves_latest_sqlite() {
    assert_inbox_upsert_is_monotonic(
        DatabaseInboxStorage::open(Some("sqlite::memory:"))
            .await
            .expect("sqlite inbox"),
    )
    .await;
}

#[tokio::test]
async fn inbox_upsert_a_b_retry_a_preserves_latest_postgres() {
    let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (monotonic inbox upsert)");
        return;
    };
    assert_inbox_upsert_is_monotonic(
        DatabaseInboxStorage::open(Some(&url))
            .await
            .expect("postgres inbox"),
    )
    .await;
}
