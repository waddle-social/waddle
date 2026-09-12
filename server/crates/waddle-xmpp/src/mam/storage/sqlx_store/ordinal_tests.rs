use super::tx_write::{
    store_archived_message_on_connection, store_archived_message_on_sqlite_connection,
    ArchiveExpectation, MamTxStoreError, MamTxStoreOutcome,
};
use super::{MamDatabaseBackend, SqlxMamStorage};
use crate::mam::storage::{MamArchiveKind, MamStorage, StoreOutcome};
use chrono::{Duration, Utc};
use jid::BareJid;
use waddle_xmpp_core::mam::{ArchiveOrdinal, ArchivedMessage, MamQuery};
use waddle_xmpp_core::xep0359::StanzaId;

fn message(archive: &BareJid, id: &str) -> ArchivedMessage {
    ArchivedMessage {
        id: id.to_owned(),
        body: Some("ordinal fixture".to_owned()),
        ..ArchivedMessage::for_test(archive.clone().into(), archive.clone().into())
    }
}

async fn store_expected(
    storage: &SqlxMamStorage,
    archive: &BareJid,
    message: &ArchivedMessage,
    expectation: ArchiveExpectation,
) -> Result<MamTxStoreOutcome, MamTxStoreError> {
    match &storage.backend {
        MamDatabaseBackend::Postgres(pool) => {
            let mut tx = pool.begin().await?;
            let result =
                store_archived_message_on_connection(&mut tx, archive, message, expectation)
                    .await?;
            tx.commit().await?;
            Ok(result)
        }
        MamDatabaseBackend::Sqlite(pool) => {
            let mut tx = pool.begin().await?;
            let result =
                store_archived_message_on_sqlite_connection(&mut tx, archive, message, expectation)
                    .await?;
            tx.commit().await?;
            Ok(result)
        }
    }
}

async fn delete(storage: &SqlxMamStorage, id: &str) {
    match &storage.backend {
        MamDatabaseBackend::Postgres(pool) => {
            sqlx::query("DELETE FROM mam_messages WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await
                .expect("delete row");
        }
        MamDatabaseBackend::Sqlite(pool) => {
            sqlx::query("DELETE FROM mam_messages WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await
                .expect("delete row");
        }
    }
}

async fn repair_contract(storage: SqlxMamStorage) {
    let archive: BareJid = format!("ordinal-{}@example.com", uuid::Uuid::now_v7())
        .parse()
        .expect("archive");
    let first = message(&archive, &uuid::Uuid::now_v7().to_string());
    let mut middle = message(&archive, &uuid::Uuid::now_v7().to_string());
    middle.timestamp = first.timestamp - Duration::days(1);
    let last = message(&archive, &uuid::Uuid::now_v7().to_string());
    let one = ArchiveOrdinal::from_storage(1).expect("one");
    let two = one.next().expect("two");
    let three = two.next().expect("three");
    assert_eq!(
        storage
            .store_message(&archive, &first)
            .await
            .expect("first"),
        StoreOutcome::Stored {
            stanza_id: first.id.clone(),
            ordinal: one
        }
    );
    assert_eq!(
        store_expected(&storage, &archive, &middle, ArchiveExpectation::Fresh)
            .await
            .expect("middle"),
        MamTxStoreOutcome::Inserted {
            stanza_id: StanzaId::new(middle.id.clone(), archive.clone().into()),
            ordinal: two
        }
    );
    delete(&storage, &middle.id).await;
    assert_eq!(
        storage
            .store_message(&archive, &last)
            .await
            .expect("new tail"),
        StoreOutcome::Stored {
            stanza_id: last.id.clone(),
            ordinal: three
        }
    );
    let expectation = ArchiveExpectation::Existing {
        stanza_id: StanzaId::new(middle.id.clone(), archive.clone().into()),
        archived_at: middle.timestamp,
        ordinal: Some(two),
    };
    assert_eq!(
        store_expected(&storage, &archive, &middle, expectation.clone())
            .await
            .expect("repair"),
        MamTxStoreOutcome::Repaired {
            stanza_id: StanzaId::new(middle.id.clone(), archive.clone().into()),
            ordinal: two
        }
    );
    assert!(
        matches!(store_expected(&storage, &archive, &middle, expectation).await.expect("existing"),
        MamTxStoreOutcome::Existing { ordinal, .. } if ordinal == two)
    );
    let result = storage
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("page");
    assert_eq!(
        result
            .messages
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec![first.id.as_str(), middle.id.as_str(), last.id.as_str()]
    );
    let mismatch = ArchiveExpectation::Existing {
        stanza_id: StanzaId::new(middle.id.clone(), archive.clone().into()),
        archived_at: middle.timestamp,
        ordinal: Some(one),
    };
    assert!(
        matches!(store_expected(&storage, &archive, &middle, mismatch).await,
        Err(MamTxStoreError::OrdinalConflict { ordinal, .. }) if ordinal == one)
    );
    let absent = message(&archive, &uuid::Uuid::now_v7().to_string());
    let collision = ArchiveExpectation::Existing {
        stanza_id: StanzaId::new(absent.id.clone(), archive.clone().into()),
        archived_at: Utc::now(),
        ordinal: Some(two),
    };
    assert!(
        matches!(store_expected(&storage, &archive, &absent, collision).await,
        Err(MamTxStoreError::OrdinalConflict { ordinal, .. }) if ordinal == two)
    );
}

#[tokio::test]
async fn sqlite_repair_preserves_position_and_tail_high_water_mark() {
    repair_contract(SqlxMamStorage::open_in_memory().await.expect("schema")).await;
}

#[tokio::test]
async fn postgres_repair_preserves_position_and_tail_high_water_mark() {
    let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping Postgres MAM ordinal test: WADDLE_TEST_POSTGRES_URL is unset");
        return;
    };
    repair_contract(SqlxMamStorage::open(&url).await.expect("schema")).await;
}

#[tokio::test]
async fn postgres_allocation_serializes_commit_and_rollback_leaves_no_phantom_row() {
    let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping Postgres MAM ordinal concurrency test: WADDLE_TEST_POSTGRES_URL is unset"
        );
        return;
    };
    let storage = SqlxMamStorage::open(&url).await.expect("schema");
    let pool = storage.postgres_pool().expect("postgres");
    let archive: BareJid = format!("concurrent-{}@example.com", uuid::Uuid::now_v7())
        .parse()
        .expect("archive");
    let first = message(&archive, &uuid::Uuid::now_v7().to_string());
    let second = message(&archive, &uuid::Uuid::now_v7().to_string());
    let mut tx1 = pool.begin().await.expect("begin first");
    let first_result =
        store_archived_message_on_connection(&mut tx1, &archive, &first, ArchiveExpectation::Fresh)
            .await
            .expect("first insert");
    let MamTxStoreOutcome::Inserted {
        ordinal: first_ordinal,
        ..
    } = first_result
    else {
        panic!("inserted")
    };
    let mut tx2 = pool.begin().await.expect("begin second");
    let second_ordinal = {
        let second_insert = store_archived_message_on_connection(
            &mut tx2,
            &archive,
            &second,
            ArchiveExpectation::Fresh,
        );
        tokio::pin!(second_insert);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut second_insert)
                .await
                .is_err(),
            "second allocator must wait for the first transaction"
        );
        tx1.commit().await.expect("commit first");
        let MamTxStoreOutcome::Inserted {
            ordinal: second_ordinal,
            ..
        } = second_insert.await.expect("second insert")
        else {
            panic!("inserted")
        };
        second_ordinal
    };
    assert!(second_ordinal > first_ordinal);
    tx2.commit().await.expect("commit second");

    let rolled_back = message(&archive, &uuid::Uuid::now_v7().to_string());
    let mut tx = pool.begin().await.expect("begin rollback");
    store_archived_message_on_connection(
        &mut tx,
        &archive,
        &rolled_back,
        ArchiveExpectation::Fresh,
    )
    .await
    .expect("rollback insert");
    tx.rollback().await.expect("rollback");
    assert!(storage
        .get_message(&rolled_back.id)
        .await
        .expect("lookup")
        .is_none());
    // The counter is transactional: rollback restores it. A deleted committed row
    // creates a real gap, which must not affect cursor pagination.
    delete(&storage, &second.id).await;
    let last = message(&archive, &uuid::Uuid::now_v7().to_string());
    let StoreOutcome::Stored { ordinal, .. } = storage
        .store_message(&archive, &last)
        .await
        .expect("after gap")
    else {
        panic!("stored")
    };
    assert!(ordinal > second_ordinal);
    let result = storage
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                after_id: Some(first.id.clone()),
                ..MamQuery::default()
            },
        )
        .await
        .expect("page after gap");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].id, last.id);
}

#[tokio::test]
async fn sqlite_file_concurrent_origin_writers_do_not_upgrade_stale_snapshots() {
    let file = tempfile::NamedTempFile::new().expect("SQLite file");
    let storage = SqlxMamStorage::open(&format!("sqlite://{}", file.path().display()))
        .await
        .expect("file schema");
    let archive: BareJid = "concurrent-origin@example.com".parse().expect("archive");
    // A real WAL file supplies multiple pooled connections. Each origin causes
    // the writer to read tombstone candidates before allocation.
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..8 {
        let storage = storage.clone();
        let archive = archive.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            let mut candidate = message(&archive, &format!("concurrent-{index}"));
            candidate.origin_id = Some(waddle_xmpp_core::xep0359::OriginId::new("shared-origin"));
            barrier.wait().await;
            storage.store_message(&archive, &candidate).await
        });
    }
    let mut ordinals = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let StoreOutcome::Stored { ordinal, .. } =
            result.expect("writer task").expect("concurrent write")
        else {
            panic!("stored")
        };
        ordinals.push(ordinal.to_storage());
    }
    ordinals.sort_unstable();
    assert_eq!(ordinals, (1..=8).collect::<Vec<_>>());
}
