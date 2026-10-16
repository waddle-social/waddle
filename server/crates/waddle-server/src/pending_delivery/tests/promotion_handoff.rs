//! Portable promotion keeps its transfer receipt independently of pending delivery.

use super::*;

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
