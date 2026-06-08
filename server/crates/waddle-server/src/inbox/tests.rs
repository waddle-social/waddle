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
