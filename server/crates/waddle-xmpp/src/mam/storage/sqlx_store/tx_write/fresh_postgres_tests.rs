use super::*;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::time::Duration;

fn archive(local: &str) -> BareJid {
    format!("{local}@rooms.example")
        .parse()
        .expect("valid archive JID")
}

fn message(archive: &BareJid, id: &str) -> ArchivedMessage {
    ArchivedMessage {
        id: id.to_owned(),
        body: Some("fresh conflict fixture".to_owned()),
        ..ArchivedMessage::for_test(
            "alice@example.com/device".parse().expect("sender JID"),
            archive.clone().into(),
        )
    }
}

async fn create_tables(connection: &mut PgConnection, schema: &str) {
    let messages = super::super::postgres_relation(Some(schema), "mam_messages");
    let sequences = super::super::postgres_relation(Some(schema), "mam_archive_sequences");
    sqlx::query(&format!(
        "CREATE TABLE {messages} (\
           id TEXT PRIMARY KEY, room_jid TEXT NOT NULL, timestamp TIMESTAMPTZ NOT NULL, \
           from_jid TEXT NOT NULL, to_jid TEXT NOT NULL, body TEXT, stanza_id TEXT, \
           thread_id TEXT, reply_to_id TEXT, reply_to_jid TEXT, origin_id TEXT, \
           message_type TEXT NOT NULL, stanza_xml TEXT, rich_payload TEXT, \
           nickname_generation BIGINT, parent_thread_id TEXT, archive_seq BIGINT)"
    ))
    .execute(&mut *connection)
    .await
    .expect("message table");
    sqlx::query(&format!(
        "CREATE TABLE {sequences} (archive_jid TEXT PRIMARY KEY, next_seq BIGINT NOT NULL)"
    ))
    .execute(connection)
    .await
    .expect("counter table");
}

fn quoted_schema(schema: &str) -> String {
    format!("\"{}\"", schema.replace('"', "\"\""))
}

fn assert_conflict(error: MamTxStoreError, archive: &BareJid, id: &str) {
    assert!(
        matches!(error, MamTxStoreError::Conflict { stanza_id }
            if stanza_id == StanzaId::new(id, archive.clone().into())),
        "expected the exact requested archive and message ID"
    );
}

#[test]
fn fresh_probe_projects_only_a_scalar_and_quotes_the_schema() {
    assert_eq!(
        postgres_fresh_conflict_query(None),
        "SELECT 1 FROM mam_messages WHERE id = $1"
    );
    assert_eq!(
        postgres_fresh_conflict_query(Some("odd\"schema")),
        "SELECT 1 FROM \"odd\"\"schema\".mam_messages WHERE id = $1"
    );
}

#[tokio::test]
async fn fresh_conflicts_ignore_large_and_corrupt_fields_in_the_exact_schema() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping Postgres MAM Fresh conflict test: WADDLE_TEST_POSTGRES_URL is unset");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("Postgres pool");
    let mut tx = pool.begin().await.expect("begin fixture transaction");
    let schema = format!("mam_fresh_\"{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {}", quoted_schema(&schema)))
        .execute(&mut *tx)
        .await
        .expect("schema");
    create_tables(&mut tx, &schema).await;
    let messages = super::super::postgres_relation(Some(&schema), "mam_messages");
    let sequences = super::super::postgres_relation(Some(&schema), "mam_archive_sequences");

    let first = archive("first");
    let other = archive("other");
    let id = "large-existing";
    let malformed_id = "null-ordinal";
    let mut rich = ArchivedRichMessage::default();
    rich.subjects
        .insert(String::new(), "x".repeat(2 * 1024 * 1024));
    let rich = serde_json::to_string(&rich).expect("large valid rich payload");
    for (row_id, ordinal) in [(id, Some(7_i64)), (malformed_id, None)] {
        sqlx::query(&format!(
            "INSERT INTO {messages} (id, room_jid, timestamp, from_jid, to_jid, \
             message_type, rich_payload, archive_seq) VALUES ($1, $2, now(), $3, $4, 'chat', $5, $6)"
        ))
        .bind(row_id)
        .bind(first.to_string())
        .bind("alice@example.com/device")
        .bind(first.to_string())
        .bind(&rich)
        .bind(ordinal)
        .execute(&mut *tx)
        .await
        .expect("stored fixture");
    }
    for (archive, value) in [(&first, 19_i64), (&other, 23_i64)] {
        sqlx::query(&format!(
            "INSERT INTO {sequences} (archive_jid, next_seq) VALUES ($1, $2)"
        ))
        .bind(archive.to_string())
        .bind(value)
        .execute(&mut *tx)
        .await
        .expect("counter fixture");
    }
    let size: i64 = sqlx::query_scalar(&format!(
        "SELECT pg_catalog.octet_length(rich_payload)::bigint FROM {messages} WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .expect("rich payload size");
    assert!(size > 2 * 1024 * 1024);

    // An unqualified probe would read this table instead of the target.
    sqlx::query("CREATE TEMP TABLE mam_messages (id TEXT PRIMARY KEY)")
        .execute(&mut *tx)
        .await
        .expect("shadow table");
    sqlx::query("INSERT INTO pg_temp.mam_messages (id) VALUES ('shadow-only')")
        .execute(&mut *tx)
        .await
        .expect("shadow row");

    for (target, row_id) in [(&first, id), (&other, id), (&first, malformed_id)] {
        let error = store_archived_message_on_connection_in_schema(
            &mut tx,
            &schema,
            target,
            &message(target, row_id),
            ArchiveExpectation::Fresh,
        )
        .await
        .expect_err("existing primary key must conflict");
        assert_conflict(error, target, row_id);
    }
    let malformed_replay = store_archived_message_on_connection_in_schema(
        &mut tx,
        &schema,
        &first,
        &message(&first, malformed_id),
        ArchiveExpectation::Existing {
            stanza_id: StanzaId::new(malformed_id, first.clone().into()),
            archived_at: Utc::now(),
            ordinal: None,
        },
    )
    .await;
    assert!(
        matches!(malformed_replay, Err(MamTxStoreError::Database(_))),
        "Existing must still decode and reject the malformed ordinal"
    );
    let counters: Vec<(String, i64)> = sqlx::query_as(&format!(
        "SELECT archive_jid, next_seq FROM {sequences} ORDER BY archive_jid"
    ))
    .fetch_all(&mut *tx)
    .await
    .expect("unchanged counters");
    assert_eq!(
        counters,
        vec![(first.to_string(), 19), (other.to_string(), 23)]
    );

    let shadow_only = message(&first, "shadow-only");
    assert!(matches!(
        store_archived_message_on_connection_in_schema(
            &mut tx,
            &schema,
            &first,
            &shadow_only,
            ArchiveExpectation::Fresh,
        )
        .await
        .expect("target schema has no such row"),
        MamTxStoreOutcome::Inserted { .. }
    ));
    let stored: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM {messages} WHERE id = 'shadow-only')"
    ))
    .fetch_one(&mut *tx)
    .await
    .expect("target insert");
    assert!(stored);
    tx.rollback()
        .await
        .expect("rollback fixture and inserted row");
}

async fn wait_for_insert_lock(pool: &PgPool, pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE pid = $1 AND wait_event_type = 'Lock' \
                 AND query LIKE '%INSERT INTO%mam_messages%')",
            )
            .bind(pid)
            .fetch_one(pool)
            .await
            .expect("observe competing insert");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("competing insert reached the primary-key wait");
}

#[tokio::test]
async fn fresh_racing_insert_conflicts_and_caller_rollback_undoes_the_counter() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping Postgres MAM Fresh race test: WADDLE_TEST_POSTGRES_URL is unset");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("Postgres pool");
    let schema = format!("mam_fresh_race_{}", uuid::Uuid::now_v7().simple());
    let mut setup = pool.acquire().await.expect("setup connection");
    sqlx::query(&format!("CREATE SCHEMA {}", quoted_schema(&schema)))
        .execute(&mut *setup)
        .await
        .expect("schema");
    create_tables(&mut setup, &schema).await;
    drop(setup);

    let first_archive = archive("first-race");
    let other_archive = archive("other-race");
    let id = "same-primary-key";
    let mut first = pool.begin().await.expect("first transaction");
    assert!(matches!(
        store_archived_message_on_connection_in_schema(
            &mut first,
            &schema,
            &first_archive,
            &message(&first_archive, id),
            ArchiveExpectation::Fresh,
        )
        .await
        .expect("first insert"),
        MamTxStoreOutcome::Inserted { .. }
    ));

    let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn({
        let pool = pool.clone();
        let schema = schema.clone();
        let archive = other_archive.clone();
        async move {
            let mut tx = pool.begin().await.expect("second transaction");
            let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
                .fetch_one(&mut *tx)
                .await
                .expect("second backend");
            pid_tx.send(pid).expect("report backend");
            let result = store_archived_message_on_connection_in_schema(
                &mut tx,
                &schema,
                &archive,
                &message(&archive, id),
                ArchiveExpectation::Fresh,
            )
            .await;
            let sequences = super::super::postgres_relation(Some(&schema), "mam_archive_sequences");
            let allocated: i64 = sqlx::query_scalar(&format!(
                "SELECT next_seq FROM {sequences} WHERE archive_jid = $1"
            ))
            .bind(archive.to_string())
            .fetch_one(&mut *tx)
            .await
            .expect("second transaction's allocated counter");
            tx.rollback().await.expect("caller rolls back conflict");
            (result, allocated)
        }
    });
    let pid = tokio::time::timeout(Duration::from_secs(5), pid_rx)
        .await
        .expect("second backend timed out")
        .expect("second backend started");
    wait_for_insert_lock(&pool, pid).await;
    first.commit().await.expect("commit first insert");
    let (result, allocated) = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("second insert timed out")
        .expect("second task joined");
    assert_conflict(
        result.expect_err("racing primary key must conflict"),
        &other_archive,
        id,
    );
    assert_eq!(
        allocated, 1,
        "allocation remains caller-owned until rollback"
    );

    let sequences = super::super::postgres_relation(Some(&schema), "mam_archive_sequences");
    let counters: Vec<(String, i64)> = sqlx::query_as(&format!(
        "SELECT archive_jid, next_seq FROM {sequences} ORDER BY archive_jid"
    ))
    .fetch_all(&pool)
    .await
    .expect("committed counters");
    assert_eq!(counters, vec![(first_archive.to_string(), 1)]);
    sqlx::query(&format!("DROP SCHEMA {} CASCADE", quoted_schema(&schema)))
        .execute(&pool)
        .await
        .expect("remove race fixture");
}
