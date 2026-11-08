use super::{
    lock_archive_sequence_on_connection, lock_archive_sequence_on_connection_in_schema,
    lock_archive_sequence_on_sqlite_connection, store_archived_message_on_connection,
    store_archived_message_on_connection_in_schema, store_archived_message_on_sqlite_connection,
    ArchiveExpectation, MamTxStoreError, MamTxStoreOutcome, SqlxMamStorage,
};
use jid::BareJid;
use sqlx::Connection;
use std::time::Duration;
use waddle_xmpp_core::mam::ArchivedMessage;
use waddle_xmpp_core::xep0359::StanzaId;

fn archive(label: &str) -> BareJid {
    format!("{label}-{}@rooms.example", uuid::Uuid::now_v7())
        .parse()
        .expect("archive JID")
}

fn message(archive: &BareJid) -> ArchivedMessage {
    ArchivedMessage {
        id: uuid::Uuid::now_v7().to_string(),
        body: Some("transaction guard fixture".to_owned()),
        ..ArchivedMessage::for_test(
            "alice@example.com/device".parse().expect("sender JID"),
            archive.clone().into(),
        )
    }
}

fn invalid_existing() -> ArchiveExpectation {
    ArchiveExpectation::Existing {
        stanza_id: StanzaId::new("wrong-archive", archive("wrong").into()),
        archived_at: chrono::Utc::now(),
        ordinal: None,
    }
}

fn assert_store_refused(result: Result<MamTxStoreOutcome, MamTxStoreError>) {
    assert!(matches!(result, Err(MamTxStoreError::TransactionRequired)));
}

fn assert_lock_refused(result: Result<(), sqlx::Error>) {
    assert!(matches!(result, Err(sqlx::Error::InvalidArgument(message))
        if message == "MAM archive counter lock requires an SQLx-tracked transaction"));
}

async fn wait_for_counter_lock(pool: &sqlx::PgPool, pid: i32) {
    loop {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
             WHERE pid = $1 AND wait_event_type = 'Lock' \
             AND query LIKE '%mam_archive_sequences%')",
        )
        .bind(pid)
        .fetch_one(pool)
        .await
        .expect("inspect competing counter lock");
        if waiting {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn sqlite_bare_and_raw_begin_refuse_store_and_lock_without_writes() {
    let storage = SqlxMamStorage::open_in_memory().await.expect("MAM schema");
    let mut connection = storage
        .sqlite_pool()
        .expect("SQLite pool")
        .acquire()
        .await
        .expect("connection");
    let archive = archive("sqlite-bare");
    let message = message(&archive);

    assert_store_refused(
        store_archived_message_on_sqlite_connection(
            &mut connection,
            &archive,
            &message,
            ArchiveExpectation::Fresh,
        )
        .await,
    );
    // This malformed expectation would otherwise fail in expected_message;
    // the transaction refusal must happen before that work as well.
    assert_store_refused(
        store_archived_message_on_sqlite_connection(
            &mut connection,
            &archive,
            &message,
            invalid_existing(),
        )
        .await,
    );
    assert_lock_refused(
        lock_archive_sequence_on_sqlite_connection(&mut connection, &archive).await,
    );

    sqlx::query("BEGIN")
        .execute(&mut *connection)
        .await
        .expect("raw BEGIN");
    assert!(
        !connection.is_in_transaction(),
        "raw BEGIN is not tracked by SQLx"
    );
    assert_store_refused(
        store_archived_message_on_sqlite_connection(
            &mut connection,
            &archive,
            &message,
            ArchiveExpectation::Fresh,
        )
        .await,
    );
    assert_lock_refused(
        lock_archive_sequence_on_sqlite_connection(&mut connection, &archive).await,
    );
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .expect("raw rollback");
    let (messages, counters): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM main.mam_messages), \
                (SELECT COUNT(*) FROM main.mam_archive_sequences)",
    )
    .fetch_one(&mut *connection)
    .await
    .expect("counts after refusal");
    assert_eq!((messages, counters), (0, 0));
}

#[tokio::test]
async fn sqlite_managed_transaction_keeps_store_and_ordered_locks_atomic() {
    let storage = SqlxMamStorage::open_in_memory().await.expect("MAM schema");
    let mut connection = storage
        .sqlite_pool()
        .expect("SQLite pool")
        .acquire()
        .await
        .expect("connection");
    let mut archives = [archive("z"), archive("a")];
    archives.sort();
    let message = message(&archives[0]);
    let mut tx = connection.begin().await.expect("tracked transaction");
    for archive in &archives {
        lock_archive_sequence_on_sqlite_connection(&mut tx, archive)
            .await
            .expect("ordered lock");
    }
    let inserted = store_archived_message_on_sqlite_connection(
        &mut tx,
        &archives[0],
        &message,
        ArchiveExpectation::Fresh,
    )
    .await
    .expect("fresh store");
    let MamTxStoreOutcome::Inserted { stanza_id, ordinal } = inserted else {
        panic!("fresh insert expected")
    };
    assert_eq!(
        store_archived_message_on_sqlite_connection(
            &mut tx,
            &archives[0],
            &message,
            ArchiveExpectation::Existing {
                stanza_id: stanza_id.clone(),
                archived_at: message.timestamp,
                ordinal: Some(ordinal),
            },
        )
        .await
        .expect("existing replay"),
        MamTxStoreOutcome::Existing { stanza_id, ordinal }
    );
    let (messages, counters): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM main.mam_messages), \
                (SELECT COUNT(*) FROM main.mam_archive_sequences)",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("within-transaction counts");
    assert_eq!((messages, counters), (1, 2));
    tx.rollback().await.expect("rollback");
    let (messages, counters): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM main.mam_messages), \
                (SELECT COUNT(*) FROM main.mam_archive_sequences)",
    )
    .fetch_one(&mut *connection)
    .await
    .expect("counts after rollback");
    assert_eq!((messages, counters), (0, 0));
}

#[tokio::test]
async fn postgres_bare_and_raw_begin_refuse_both_store_and_lock_paths_without_writes() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping Postgres transaction guard test: WADDLE_TEST_POSTGRES_URL is unset");
        return;
    };
    let storage = SqlxMamStorage::open(&url).await.expect("MAM schema");
    let mut connection = storage
        .postgres_pool()
        .expect("Postgres pool")
        .acquire()
        .await
        .expect("connection");
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&mut *connection)
        .await
        .expect("active schema");
    let archive = archive("postgres-bare");
    let message = message(&archive);

    assert_store_refused(
        store_archived_message_on_connection(
            &mut connection,
            &archive,
            &message,
            ArchiveExpectation::Fresh,
        )
        .await,
    );
    assert_store_refused(
        store_archived_message_on_connection_in_schema(
            &mut connection,
            "missing_schema",
            &archive,
            &message,
            invalid_existing(),
        )
        .await,
    );
    assert_lock_refused(lock_archive_sequence_on_connection(&mut connection, &archive).await);
    assert_lock_refused(
        lock_archive_sequence_on_connection_in_schema(&mut connection, "missing_schema", &archive)
            .await,
    );

    sqlx::query("BEGIN")
        .execute(&mut *connection)
        .await
        .expect("raw BEGIN");
    assert!(
        !connection.is_in_transaction(),
        "raw BEGIN is not tracked by SQLx"
    );
    assert_store_refused(
        store_archived_message_on_connection_in_schema(
            &mut connection,
            &schema,
            &archive,
            &message,
            ArchiveExpectation::Fresh,
        )
        .await,
    );
    assert_lock_refused(
        lock_archive_sequence_on_connection_in_schema(&mut connection, &schema, &archive).await,
    );
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .expect("raw rollback");
    let (messages, counters): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM mam_messages WHERE id = $1), \
                (SELECT COUNT(*) FROM mam_archive_sequences WHERE archive_jid = $2)",
    )
    .bind(&message.id)
    .bind(archive.to_string())
    .fetch_one(&mut *connection)
    .await
    .expect("counts after refusal");
    assert_eq!((messages, counters), (0, 0));
}

#[tokio::test]
async fn postgres_managed_transaction_keeps_store_and_ordered_locks_atomic() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping Postgres transaction guard test: WADDLE_TEST_POSTGRES_URL is unset");
        return;
    };
    let storage = SqlxMamStorage::open(&url).await.expect("MAM schema");
    let pool = storage.postgres_pool().expect("Postgres pool");
    let mut connection = pool.acquire().await.expect("connection");
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&mut *connection)
        .await
        .expect("active schema");
    let mut archives = [archive("z"), archive("a")];
    archives.sort();
    let message = message(&archives[0]);
    let mut tx = connection.begin().await.expect("tracked transaction");
    lock_archive_sequence_on_connection(&mut tx, &archives[0])
        .await
        .expect("unqualified first lock");
    lock_archive_sequence_on_connection_in_schema(&mut tx, &schema, &archives[1])
        .await
        .expect("qualified second lock");
    let inserted = store_archived_message_on_connection_in_schema(
        &mut tx,
        &schema,
        &archives[0],
        &message,
        ArchiveExpectation::Fresh,
    )
    .await
    .expect("fresh store");
    let MamTxStoreOutcome::Inserted { stanza_id, ordinal } = inserted else {
        panic!("fresh insert expected")
    };
    assert_eq!(
        store_archived_message_on_connection(
            &mut tx,
            &archives[0],
            &message,
            ArchiveExpectation::Existing {
                stanza_id: stanza_id.clone(),
                archived_at: message.timestamp,
                ordinal: Some(ordinal),
            },
        )
        .await
        .expect("existing replay"),
        MamTxStoreOutcome::Existing { stanza_id, ordinal }
    );

    let mut second = pool.begin().await.expect("second transaction");
    let second_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *second)
        .await
        .expect("second backend PID");
    {
        let waiting_lock = lock_archive_sequence_on_connection(&mut second, &archives[0]);
        tokio::pin!(waiting_lock);
        tokio::select! {
            result = &mut waiting_lock => panic!("counter lock returned before rollback: {result:?}"),
            observed = tokio::time::timeout(Duration::from_secs(5), wait_for_counter_lock(pool, second_pid)) => {
                observed.expect("second backend must wait for the counter lock");
            }
        }
        tx.rollback().await.expect("rollback first transaction");
        tokio::time::timeout(Duration::from_secs(5), waiting_lock)
            .await
            .expect("second lock timed out")
            .expect("second acquires after rollback");
    }
    lock_archive_sequence_on_connection_in_schema(&mut second, &schema, &archives[1])
        .await
        .expect("second ordered lock");
    second
        .rollback()
        .await
        .expect("rollback second transaction");

    let (messages, counters): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM mam_messages WHERE id = $1), \
                (SELECT COUNT(*) FROM mam_archive_sequences WHERE archive_jid = ANY($2))",
    )
    .bind(&message.id)
    .bind(archives.iter().map(ToString::to_string).collect::<Vec<_>>())
    .fetch_one(&mut *connection)
    .await
    .expect("counts after both rollbacks");
    assert_eq!((messages, counters), (0, 0));
}
