//! Portable promotion keeps its transfer receipt independently of pending delivery.

use super::{
    bare, create_postgres_test_schema, drop_postgres_test_schema, full, transient_row, Arc,
    Database, DatabaseConfig, DatabaseDriver, DatabasePendingDeliveryStorage, InsertOutcome,
    PendingDeliveryStorage, QuotaPolicy, SmSessionId, Utc,
};

async fn portable_promotion_retry_survives_restart_and_delivery(database_url: &str) {
    let storage = DatabasePendingDeliveryStorage::open(
        Some(database_url),
        QuotaPolicy::CountCap { max_rows: 1 },
    )
    .await
    .expect("open portable pending storage without SM tables");
    let recipient = bare("alice@example.com");
    assert_eq!(
        storage
            .insert_fenced_and_prune_unacked(
                transient_row("alice@example.com", "original promotion"),
                "interrupted-stream",
                7,
            )
            .await
            .expect("commit pending handoff"),
        InsertOutcome::Inserted
    );
    drop(storage);

    // The pending commit survived, but the independent SM database still
    // contains the replay row because the process stopped before pruning it.
    let restarted = DatabasePendingDeliveryStorage::open(
        Some(database_url),
        QuotaPolicy::CountCap { max_rows: 1 },
    )
    .await
    .expect("restart pending storage");
    assert_eq!(
        restarted
            .insert_fenced_and_prune_unacked(
                transient_row("alice@example.com", "retry after restart"),
                "interrupted-stream",
                7,
            )
            .await
            .expect("repeat committed promotion at quota"),
        InsertOutcome::Inserted
    );
    let rows = restarted
        .list(&recipient)
        .await
        .expect("list single handoff");
    assert_eq!(rows.len(), 1, "retry must not duplicate pending custody");
    assert_eq!(
        restarted
            .insert_fenced_and_prune_unacked(
                transient_row("alice@example.com", "independent next sequence"),
                "interrupted-stream",
                8,
            )
            .await
            .expect("new sequence respects quota"),
        InsertOutcome::QuotaExceeded
    );

    restarted
        .delete_row(&rows[0].id)
        .await
        .expect("complete pending delivery before SM prune retry");
    assert_eq!(
        restarted
            .insert_fenced_and_prune_unacked(
                transient_row("alice@example.com", "retry after pending delivery"),
                "interrupted-stream",
                7,
            )
            .await
            .expect("completed handoff remains settled"),
        InsertOutcome::Inserted
    );
    assert!(restarted.list(&recipient).await.expect("list").is_empty());
    assert_eq!(
        restarted
            .insert_fenced_and_prune_unacked(
                transient_row("alice@example.com", "retry quota-rejected sequence"),
                "interrupted-stream",
                8,
            )
            .await
            .expect("quota rejection did not settle a new sequence"),
        InsertOutcome::Inserted
    );
    assert_eq!(restarted.list(&recipient).await.expect("list").len(), 1);
}

#[tokio::test]
async fn sqlite_portable_promotion_retry_survives_restart_and_delivery() {
    let directory = tempfile::tempdir().expect("promotion handoff directory");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("handoff.db").display()
    );
    portable_promotion_retry_survives_restart_and_delivery(&database_url).await;
}

#[tokio::test]
async fn postgres_portable_promotion_retry_survives_restart_and_delivery() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (portable promotion)");
        return;
    };
    let (schema, scoped_url) =
        create_postgres_test_schema(&database_url, "promotion_handoff").await;
    portable_promotion_retry_survives_restart_and_delivery(&scoped_url).await;
    drop_postgres_test_schema(&database_url, &schema).await;
}

async fn receipt_schema_migration_and_confirmation(database: Database) {
    database.guard().await.expect("database guard").execute(
        "CREATE TABLE pending_delivery_sm_promotions (origin_stream_id TEXT NOT NULL, sequence BIGINT NOT NULL, PRIMARY KEY (origin_stream_id, sequence))",
        (),
    ).await.expect("legacy receipt schema");
    database.guard().await.expect("database guard").execute(
        "INSERT INTO pending_delivery_sm_promotions (origin_stream_id, sequence) VALUES ('legacy-source', 1)", (),
    ).await.expect("legacy receipt");
    let storage =
        DatabasePendingDeliveryStorage::from_database(database.clone(), QuotaPolicy::Unlimited)
            .await
            .expect("migrate receipt table");
    let mut rows = database.guard().await.expect("guard").query(
        "SELECT created_at_ms FROM pending_delivery_sm_promotions WHERE origin_stream_id = 'legacy-source'", (),
    ).await.expect("read migrated stamp");
    assert_eq!(
        rows.next()
            .await
            .expect("row read")
            .expect("legacy row")
            .get::<i64>(0)
            .expect("stamp"),
        0
    );
    let before = crate::time::now_ms();
    let pending = transient_row("alice@example.com", "new portable handoff");
    storage
        .insert_fenced_and_prune_unacked(pending, "new-source", 3)
        .await
        .expect("insert new receipt");
    let mut rows = database.guard().await.expect("guard").query(
        "SELECT created_at_ms FROM pending_delivery_sm_promotions WHERE origin_stream_id = 'new-source'", (),
    ).await.expect("read new stamp");
    assert!(
        rows.next()
            .await
            .expect("row read")
            .expect("new row")
            .get::<i64>(0)
            .expect("stamp")
            >= before
    );
    assert_eq!(
        storage
            .promotion_receipt_sources_before(0, None, 8)
            .await
            .expect("legacy candidates"),
        vec![SmSessionId::new("legacy-source")]
    );
    let first = storage
        .promotion_receipt_sources_before(i64::MAX, None, 1)
        .await
        .expect("first page");
    assert_eq!(first, vec![SmSessionId::new("legacy-source")]);
    assert_eq!(
        storage
            .promotion_receipt_sources_before(i64::MAX, first.last(), 1)
            .await
            .expect("second page"),
        vec![SmSessionId::new("new-source")]
    );
    assert!(storage
        .promotion_receipt_sources_before(i64::MAX, None, 0)
        .await
        .expect("empty page")
        .is_empty());
    // Only the caller's canonical source retirement authorizes receipt removal.
    // Pending delivery survives that cleanup, and other stream receipts remain.
    storage
        .confirm_promotion(&SmSessionId::new("new-source"))
        .await
        .expect("confirm source retired");
    assert_eq!(
        storage
            .promotion_receipt_sources_before(i64::MAX, None, 8)
            .await
            .expect("remaining receipts"),
        first
    );
    assert_eq!(
        storage
            .list(&bare("alice@example.com"))
            .await
            .expect("pending custody")
            .len(),
        1
    );
    let restarted = DatabasePendingDeliveryStorage::from_database(database, QuotaPolicy::Unlimited)
        .await
        .expect("idempotent migration");
    assert_eq!(
        restarted
            .promotion_receipt_sources_before(i64::MAX, None, 8)
            .await
            .expect("receipts after restart"),
        first
    );
}

#[tokio::test]
async fn sqlite_receipt_schema_migration_and_confirmation() {
    receipt_schema_migration_and_confirmation(
        Database::in_memory("legacy_receipts")
            .await
            .expect("database"),
    )
    .await;
}

#[tokio::test]
async fn postgres_receipt_schema_migration_and_confirmation() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        return;
    };
    let (schema, scoped_url) = create_postgres_test_schema(&database_url, "receipt_schema").await;
    let database = Database::from_config(
        "legacy_receipts",
        &DatabaseConfig::new(DatabaseDriver::Postgres, scoped_url),
    )
    .await
    .expect("postgres database");
    receipt_schema_migration_and_confirmation(database).await;
    drop_postgres_test_schema(&database_url, &schema).await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn fenced_completion_does_not_query_portable_receipts() {
    use waddle_xmpp::ownership::{InProcessClaimStore, NodeIdentity, SharedNodeIdentity};
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .expect("storage")
        .with_cluster_fencing(
            Arc::new(InProcessClaimStore::new()),
            SharedNodeIdentity::new(NodeIdentity::new("node", "epoch")),
        );
    storage
        .database()
        .guard()
        .await
        .expect("guard")
        .execute("DROP TABLE pending_delivery_sm_promotions", ())
        .await
        .expect("receipt table unused in fenced mode");
    storage
        .confirm_promotion(&SmSessionId::new("fenced-source"))
        .await
        .expect("fenced completion has no portable receipt work");
    assert!(storage
        .promotion_receipt_sources_before(i64::MAX, None, 8)
        .await
        .expect("fenced enumeration has no portable receipt work")
        .is_empty());
}

#[tokio::test]
async fn portable_orphan_receipt_gc_proves_separate_source_absent() {
    use waddle_xmpp::stream_management::persistence::{PersistedSession, SmPersistenceStorage};
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
    let pending = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .expect("independent pending database");
    let source = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(None)
            .await
            .expect("independent canonical SM database"),
    );
    let stream = SmSessionId::new("separate-database-source");
    source
        .upsert_session(PersistedSession {
            stream_id: stream.clone(),
            user_id: "alice".to_owned(),
            jid: full("alice@example.com/phone"),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: 1,
            last_acked: 0,
            replay_gap_through: None,
            max_resume_time: Some(0),
            detached_at: Utc::now(),
            max_resume_duration: std::time::Duration::ZERO,
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
        })
        .await
        .expect("canonical inventory row");
    pending
        .insert_fenced_and_prune_unacked(
            transient_row("alice@example.com", "retained handoff"),
            stream.as_str(),
            1,
        )
        .await
        .expect("portable transfer receipt");
    let registry = InMemorySmSessionRegistry::new().with_persistence(source.clone());
    let candidates = pending
        .promotion_receipt_sources_before(i64::MAX, None, 16)
        .await
        .expect("candidate IDs");
    assert_eq!(candidates, vec![stream.clone()]);
    assert!(!registry
        .retire_orphaned_promotion_receipts(&stream, &pending)
        .await
        .expect("live source retained despite arbitrary age cutoff"));
    assert_eq!(
        pending
            .promotion_receipt_sources_before(i64::MAX, None, 16)
            .await
            .expect("receipt preserved"),
        candidates
    );
    // Simulate a crash after canonical retirement but before confirm_promotion.
    source
        .delete_session(&stream)
        .await
        .expect("canonical source retired");
    assert!(registry
        .retire_orphaned_promotion_receipts(&stream, &pending)
        .await
        .expect("source-proven orphan cleaned"));
    assert!(pending
        .promotion_receipt_sources_before(i64::MAX, None, 16)
        .await
        .expect("receipt removed")
        .is_empty());
    assert_eq!(
        pending
            .list(&bare("alice@example.com"))
            .await
            .expect("pending payload unaffected")
            .len(),
        1
    );
}
