use std::time::{Duration, Instant};

use super::{IngressAuthority, IngressUnitOfWork, IngressUowError, SmSessionId, WireHandledCount};
use crate::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};

async fn fixture(pool_size: u32) -> (tempfile::TempDir, Database, IngressAuthority, SmSessionId) {
    let directory = tempfile::tempdir().expect("database directory");
    let config = DatabaseConfig {
        pool_size,
        ..DatabaseConfig::new(
            DatabaseDriver::Sqlite,
            directory.path().join("checkpoint.db").to_string_lossy(),
        )
    };
    let database = Database::from_config("resume-checkpoint", &config)
        .await
        .expect("file-backed SQLite database");
    MigrationRunner::global()
        .run(&database)
        .await
        .expect("migrate");
    let connection = database.guard().await.expect("connection");
    let mut rows = connection
        .query("PRAGMA journal_mode", ())
        .await
        .expect("journal mode");
    let mode: String = rows
        .next()
        .await
        .expect("row")
        .expect("mode")
        .get(0)
        .expect("text");
    assert_eq!(mode, "wal");
    drop(rows);
    let authority = IngressAuthority::for_test(database.clone()).await;
    let stream = SmSessionId::new("resume-checkpoint");
    let id = authority.enroll_stream(&stream).await.expect("enroll");
    authority
        .flush_checkpoint(id, WireHandledCount::from_storage(3))
        .await
        .expect("seed checkpoint");
    (directory, database, authority, stream)
}

async fn assert_checkpoint(authority: &IngressAuthority, stream: &SmSessionId, expected: u32) {
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(2),
            authority.load_resume_checkpoint(stream)
        )
        .await
        .expect("fresh checkpoint acquisition completes")
        .expect("checkpoint"),
        Some(WireHandledCount::from_storage(expected))
    );
}

#[tokio::test]
async fn resume_checkpoint_waits_past_old_bound_and_reads_committed_writer() {
    let (_directory, database, authority, stream) = fixture(3).await;
    let mut writer = database.begin_immediate().await.expect("hold writer");
    writer
        .execute(
            "UPDATE ingress_sm_streams SET checkpoint_h = 7 WHERE stream_id = ?",
            crate::db_params![stream.as_str()],
        )
        .await
        .expect("uncommitted checkpoint advancement");
    let load = authority.load_resume_checkpoint(&stream);
    tokio::pin!(load);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut load)
            .await
            .is_err(),
        "loader remains pending beyond the previous 100ms bound"
    );
    // A read can check out a third connection while the known writer and
    // checkpoint acquisition are pending: this is writer contention, not a
    // saturated pool. WAL readers still see the old committed checkpoint.
    let connection = database.guard().await.expect("connection");
    let mut rows = tokio::time::timeout(
        Duration::from_millis(250),
        connection.query("SELECT checkpoint_h FROM ingress_sm_streams", ()),
    )
    .await
    .expect("spare pool capacity")
    .expect("read committed snapshot");
    let old: i64 = rows
        .next()
        .await
        .expect("row")
        .expect("checkpoint")
        .get(0)
        .expect("integer");
    assert_eq!(old, 3);
    drop(rows);
    writer.commit().await.expect("publish checkpoint");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), &mut load)
            .await
            .expect("loader completes after writer")
            .expect("checkpoint"),
        Some(WireHandledCount::from_storage(7))
    );
    assert_checkpoint(&authority, &stream, 7).await;
}

#[tokio::test]
async fn resume_checkpoint_held_writer_times_out_and_releases_acquisition() {
    let (_directory, database, authority, stream) = fixture(2).await;
    let writer = database.begin_immediate().await.expect("hold writer");
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        authority.load_resume_checkpoint(&stream),
    )
    .await
    .expect("bounded checkpoint acquisition");
    assert!(matches!(result, Err(IngressUowError::Timeout)));
    assert!(started.elapsed() >= Duration::from_secs(1));
    writer.commit().await.expect("release writer");
    assert_checkpoint(&authority, &stream, 3).await;
}

#[tokio::test]
async fn resume_checkpoint_saturated_pool_times_out_and_releases_acquisition() {
    let (_directory, database, authority, stream) = fixture(1).await;
    let held = database.begin().await.expect("saturate pool");
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        authority.load_resume_checkpoint(&stream),
    )
    .await
    .expect("bounded checkout");
    assert!(matches!(result, Err(IngressUowError::Timeout)));
    assert!(started.elapsed() >= Duration::from_secs(1));
    held.commit().await.expect("release pooled connection");
    assert_checkpoint(&authority, &stream, 3).await;
}

#[tokio::test]
async fn cancelling_resume_checkpoint_releases_writer_and_pool_waits() {
    for pool_size in [1, 2] {
        let (_directory, database, authority, stream) = fixture(pool_size).await;
        let held = database
            .begin_immediate()
            .await
            .expect("hold connection and writer");
        {
            let load = authority.load_resume_checkpoint(&stream);
            tokio::pin!(load);
            assert!(tokio::time::timeout(Duration::from_millis(200), &mut load)
                .await
                .is_err());
            // Dropping the caller cancels either pool checkout (one connection)
            // or BEGIN IMMEDIATE (two connections), with one acquisition only.
        }
        held.commit().await.expect("release writer");
        assert_checkpoint(&authority, &stream, 3).await;
        assert!(
            authority.drain_and_join(Duration::from_secs(1)).await,
            "cancelled loader releases its admission guard"
        );
    }
}

#[tokio::test]
async fn resume_checkpoint_observes_prior_writer_retirement() {
    let (_directory, database, authority, stream) = fixture(2).await;
    let mut writer = database.begin_immediate().await.expect("hold writer");
    writer
        .execute(
            "DELETE FROM ingress_sm_streams WHERE stream_id = ?",
            crate::db_params![stream.as_str()],
        )
        .await
        .expect("uncommitted retirement");
    let load = authority.load_resume_checkpoint(&stream);
    tokio::pin!(load);
    assert!(tokio::time::timeout(Duration::from_millis(200), &mut load)
        .await
        .is_err());
    writer.commit().await.expect("commit retirement");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), &mut load)
            .await
            .expect("loader completes after retirement")
            .expect("checkpoint"),
        None
    );
}

#[tokio::test]
async fn resume_checkpoint_preserves_missing_malformed_epoch_and_lineage_failures() {
    let (_directory, database, mut authority, stream) = fixture(2).await;
    assert_eq!(
        authority
            .load_resume_checkpoint(&SmSessionId::new("missing"))
            .await
            .expect("missing stream lookup"),
        None
    );
    let id = authority
        .lookup_stream(&stream)
        .await
        .expect("lookup")
        .expect("enrolled");
    let connection = database.guard().await.expect("connection");
    connection
        .execute(
            "UPDATE ingress_sm_streams SET sm_ingress_id = 'malformed'",
            (),
        )
        .await
        .expect("malformed stored stream identity");
    assert!(matches!(
        authority.load_resume_checkpoint(&stream).await,
        Err(IngressUowError::InvalidStoredSmIngressId)
    ));
    connection
        .execute(
            "UPDATE ingress_sm_streams SET sm_ingress_id = ?",
            crate::db_params![id.to_storage().to_string()],
        )
        .await
        .expect("restore identity");
    connection.execute("UPDATE ingress_protocol_epoch SET epoch = 2, activated_at = '2026-09-06T00:00:00Z', lineage_uuid = '018f47b2-4b2e-7a3a-9a4c-52a5a6a90001' WHERE id = 1", ())
        .await.expect("future epoch");
    assert!(matches!(
        authority.load_resume_checkpoint(&stream).await,
        Err(IngressUowError::EpochUnsupported { .. })
    ));
    connection.execute("UPDATE ingress_protocol_epoch SET epoch = 0, activated_at = NULL, lineage_uuid = NULL WHERE id = 1", ())
        .await.expect("restore epoch");
    let mut lineage = super::contract::test_lineage_config();
    lineage.deployment_uuid = Some(
        "018f47b2-4b2e-7a3a-9a4c-52a5a6a90002"
            .parse()
            .expect("UUID"),
    );
    authority.uow = IngressUnitOfWork::open(database.clone(), lineage).expect("mismatched UOW");
    assert!(matches!(
        authority.load_resume_checkpoint(&stream).await,
        Err(IngressUowError::Lineage(crate::db::DatabaseError::Lineage(
            crate::db::lineage::LineageError::DeploymentUuidMismatch { .. }
        )))
    ));
    authority.uow =
        IngressUnitOfWork::open(database.clone(), super::contract::test_lineage_config())
            .expect("restore UOW");
    connection
        .execute("DELETE FROM _lineage", ())
        .await
        .expect("remove lineage");
    assert!(matches!(
        authority.load_resume_checkpoint(&stream).await,
        Err(IngressUowError::Lineage(crate::db::DatabaseError::Lineage(
            crate::db::lineage::LineageError::MissingRow
        )))
    ));
}
