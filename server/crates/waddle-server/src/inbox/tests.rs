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
