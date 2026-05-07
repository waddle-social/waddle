use std::sync::Arc;
use std::time::Duration;

use bench_core::message::{ArchivedMessage, MamQuery};
use bench_core::store::StanzaStore;
use bench_sqlite::{SqliteBacking, SqliteStore};
use rusqlite::Connection;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn insert_and_query_roundtrip_disk() {
    let dir = tempdir();
    let path = dir.join("bench.db");
    let store = SqliteStore::open(&path, 4).unwrap();
    store.init().await.unwrap();
    populate_and_check(&store).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn insert_and_query_roundtrip_memory() {
    let dir = tempdir();
    let snapshot = dir.join("snapshot.db");
    let store = SqliteStore::open_with_backing(
        SqliteBacking::Memory {
            name: format!("bench-test-{}", uuid_like()),
            snapshot_to: snapshot.clone(),
            flush_interval: Duration::from_millis(200),
        },
        4,
    )
    .unwrap();
    store.init().await.unwrap();
    populate_and_check(&store).await;

    // Let at least one flush cycle run, then check the snapshot exists
    // and contains data.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let bytes = std::fs::metadata(&snapshot).unwrap().len();
    assert!(bytes > 0, "snapshot file should be non-empty");

    // Independent connection reading the snapshot should see the rows.
    let snap = Connection::open(&snapshot).unwrap();
    let n: i64 = snap
        .query_row(
            "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ?1",
            ["room1@conference.bench.local"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1_000);
}

async fn populate_and_check(store: &Arc<SqliteStore>) {
    for i in 0..1_000 {
        let mut m = ArchivedMessage::new_chat(
            "room1@conference.bench.local",
            &format!("user{i}@bench.local/c"),
            "room1@conference.bench.local",
            &format!("body {i}"),
        );
        m.message_type = "groupchat".into();
        store.store_message(&m).await.unwrap();
    }
    let count = store
        .count_messages("room1@conference.bench.local")
        .await
        .unwrap();
    assert_eq!(count, 1_000);
    let q = MamQuery {
        room_jid: "room1@conference.bench.local".into(),
        limit: 50,
        ..Default::default()
    };
    let rows = store.query_messages(&q).await.unwrap();
    assert_eq!(rows.len(), 50);
}

fn tempdir() -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("bench-sqlite-test-{}", uuid_like()));
    std::fs::create_dir_all(&base).unwrap();
    base
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{n}")
}
