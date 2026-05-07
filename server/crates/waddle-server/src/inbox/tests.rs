use super::*;
use std::path::PathBuf;

fn jid(value: &str) -> BareJid {
    value.parse().expect("valid JID")
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

    storage
        .mark_read(&user, &jid("alice@example.com"), None)
        .await
        .expect("mark read");
    assert_eq!(storage.total_unread(&user).await.expect("unread"), 0);
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
    storage
        .mark_read(&user, &room, Some("t1"))
        .await
        .expect("mark thread read");
    let threads = storage
        .list_threads(&user, &room)
        .await
        .expect("list_threads");
    assert_eq!(threads[0].unread, 0);

    // Channel unread unaffected
    assert_eq!(storage.total_unread(&user).await.expect("unread"), 1);
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
