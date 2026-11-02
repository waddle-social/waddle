use super::*;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use waddle_server::db::physical_attestation::{RetentionLab, RetentionLabBatch};
use waddle_server::ingress_substrate::ALIAS_RETENTION;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-01T12:00:00Z")
        .expect("fixed time")
        .with_timezone(&Utc)
}

async fn sqlite_fixture() -> (Database, RetentionLab) {
    let url = format!(
        "sqlite:file:retention_lab_{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4().simple()
    );
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, &url);
    config.pool_size = 1;
    let db = Database::from_config("shared retention laboratory", &config)
        .await
        .expect("open runtime database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("apply migrations");
    let lab = RetentionLab::sqlite(&db, typed_bounds()).expect("bind exact runtime pool");
    (db, lab)
}

async fn sqlite_exec(db: &Database, sql: &str) {
    db.guard()
        .await
        .expect("guard")
        .execute(sql, ())
        .await
        .expect("fixture statement");
}

async fn sqlite_count(db: &Database, sql: &str) -> i64 {
    let guard = db.guard().await.expect("guard");
    let mut rows = guard.query(sql, ()).await.expect("query");
    rows.next()
        .await
        .expect("row read")
        .expect("row")
        .get(0)
        .expect("count")
}

async fn seed_sqlite(db: &Database, key: &str, frontier: Option<DateTime<Utc>>) {
    let guard = db.guard().await.expect("guard");
    guard.execute(
        "INSERT INTO ingress_messages (message_key, digest_version, digest, terminal_at, retention_eligible_at) VALUES (?, 1, ?, ?, ?)",
        waddle_server::db_params![key, vec![1_u8; 32], (now() - ChronoDuration::days(100)).to_rfc3339(), frontier.map(|at| at.to_rfc3339())],
    ).await.expect("seed canonical");
    guard.execute(
        "INSERT INTO ingress_origin_aliases (alias_key_hash, sender_bare_jid, target_kind, origin_id, message_key) VALUES (?, 'sender@example.test', 0, 'origin', ?)",
        waddle_server::db_params![uuid::Uuid::parse_str(key).expect("UUID").as_bytes().repeat(2), key],
    ).await.expect("seed alias");
    guard
        .execute(
            "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES (?, ?)",
            waddle_server::db_params![uuid::Uuid::new_v4().to_string(), key],
        )
        .await
        .expect("seed delivery");
}

#[tokio::test]
async fn sqlite_shared_pool_fails_closed_and_adopts_full_tail() {
    let (db, lab) = sqlite_fixture().await;
    let key = uuid::Uuid::new_v4().to_string();
    seed_sqlite(&db, &key, None).await;
    assert!(
        matches!(
            lab.collect(now(), 1).await,
            Err(DarkIngressAuthorityError::Database)
        ),
        "missing pending relation is not permission"
    );
    sqlite_exec(
        &db,
        "CREATE TABLE pending_delivery (row_id TEXT PRIMARY KEY)",
    )
    .await;
    assert!(matches!(
        lab.collect(now(), 0).await,
        Err(DarkIngressAuthorityError::InvalidBounds)
    ));
    assert!(matches!(
        lab.collect(now(), 257).await,
        Err(DarkIngressAuthorityError::InvalidBounds)
    ));
    assert_eq!(
        lab.collect(now(), 1).await.expect("adopt"),
        RetentionLabBatch {
            examined: 1,
            adopted: 1,
            deleted: 0
        }
    );
    assert_eq!(sqlite_count(&db, "SELECT COUNT(*) FROM ingress_messages WHERE retention_eligible_at = '2026-11-01T12:00:00+00:00'").await, 1);
    assert_eq!(
        lab.collect(now() + ALIAS_RETENTION - ChronoDuration::microseconds(1), 1)
            .await
            .expect("not yet")
            .deleted,
        0
    );
    assert_eq!(
        lab.collect(now() + ALIAS_RETENTION, 1)
            .await
            .expect("at cutoff")
            .deleted,
        1
    );
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_deliveries",
    ] {
        assert_eq!(
            sqlite_count(&db, &format!("SELECT COUNT(*) FROM {table}")).await,
            0,
            "{table}"
        );
    }
    sqlite_exec(
        &db,
        "ALTER TABLE sm_ingress_appends RENAME TO hidden_sm_ingress_appends",
    )
    .await;
    assert!(
        matches!(
            lab.collect(now(), 1).await,
            Err(DarkIngressAuthorityError::Database)
        ),
        "missing SM relation fails even with no candidates"
    );
}

#[tokio::test]
async fn sqlite_custody_and_atomic_rollback() {
    let (db, lab) = sqlite_fixture().await;
    sqlite_exec(
        &db,
        "CREATE TABLE pending_delivery (row_id TEXT PRIMARY KEY)",
    )
    .await;
    let key = uuid::Uuid::new_v4().to_string();
    seed_sqlite(&db, &key, Some(now() - ALIAS_RETENTION)).await;
    let guard = db.guard().await.expect("guard");
    guard.execute("INSERT INTO sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES (?, 1, ?, 'user@example.test/r', 's', 1, 1, 'payload', 1, 0)", waddle_server::db_params![key.clone(), vec![2_u8; 32]]).await.expect("seed pending SM custody");
    drop(guard);
    assert_eq!(
        lab.collect(now(), 2).await.expect("custody retained"),
        RetentionLabBatch {
            examined: 1,
            adopted: 0,
            deleted: 0
        }
    );
    assert_eq!(
        sqlite_count(
            &db,
            "SELECT COUNT(*) FROM ingress_messages WHERE retention_eligible_at IS NULL"
        )
        .await,
        1,
        "observed custody invalidates an old frontier"
    );
    assert_eq!(
        sqlite_count(
            &db,
            "SELECT COUNT(*) FROM sm_ingress_appends WHERE disposition = 0"
        )
        .await,
        1
    );
    sqlite_exec(&db, "UPDATE sm_ingress_appends SET disposition = 1").await;
    assert_eq!(
        lab.collect(now(), 2)
            .await
            .expect("adopt after settlement")
            .adopted,
        1
    );
    sqlite_exec(&db, "CREATE TRIGGER reject_retention BEFORE DELETE ON ingress_messages BEGIN SELECT RAISE(ABORT, 'fail after child deletes'); END").await;
    assert!(matches!(
        lab.collect(now() + ALIAS_RETENTION, 2).await,
        Err(DarkIngressAuthorityError::Database)
    ));
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_deliveries",
        "sm_ingress_appends",
    ] {
        assert_eq!(
            sqlite_count(&db, &format!("SELECT COUNT(*) FROM {table}")).await,
            1,
            "rollback preserves {table}"
        );
    }
    sqlite_exec(&db, "DROP TRIGGER reject_retention").await;
    assert_eq!(
        lab.collect(now() + ALIAS_RETENTION, 2)
            .await
            .expect("delete after retry")
            .deleted,
        1
    );
    assert_eq!(
        sqlite_count(&db, "SELECT COUNT(*) FROM sm_ingress_appends").await,
        0
    );
}

#[tokio::test]
async fn sqlite_epoch_and_one_slot_immediate_are_bounded() {
    let (db, lab) = sqlite_fixture().await;
    sqlite_exec(
        &db,
        "CREATE TABLE pending_delivery (row_id TEXT PRIMARY KEY)",
    )
    .await;
    let holder = db.begin_immediate().await.expect("hold writer slot");
    let bounded = RetentionLab::sqlite(
        &db,
        TransactionBounds::new(
            Duration::from_millis(100),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("bounds"),
    )
    .expect("lab");
    assert!(matches!(
        bounded.collect(now(), 1).await,
        Err(DarkIngressAuthorityError::Timeout)
    ));
    holder.rollback().await.expect("release writer");
    assert_eq!(
        lab.collect(now(), 1).await.expect("recovered"),
        RetentionLabBatch::default()
    );
    sqlite_exec(&db, "UPDATE ingress_protocol_epoch SET epoch = 2, activated_at = '2026-11-01T12:00:00Z', lineage_uuid = '00000000-0000-0000-0000-000000000001' WHERE id = 1").await;
    assert!(matches!(
        lab.collect(now(), 1).await,
        Err(DarkIngressAuthorityError::UnsupportedEpoch)
    ));
}

async fn postgres_fixture(fixture: &Fixture) -> (PhysicalAttestationConfig, RetentionLab) {
    let (config, key) = typed_authority_fixture(fixture).await;
    let mut seed = pg_seed_transaction(fixture).await;
    sqlx::query(&format!(
        "DELETE FROM {}.ingress_messages WHERE message_key = $1",
        fixture.target
    ))
    .bind(key.to_storage())
    .execute(&mut *seed)
    .await
    .expect("remove seed");
    seed.commit().await.expect("commit seed removal");
    sqlx::query(&format!(
        "CREATE TABLE {}.pending_delivery (row_id TEXT PRIMARY KEY)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("pending relation");
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_deliveries",
        "ingress_sm_refs",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_effect_descendants",
        "ingress_archive_dispatch",
        "pending_delivery",
        "sm_ingress_appends",
    ] {
        sqlx::query(&format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant lab relation");
    }
    let lab = RetentionLab::postgres(
        fixture.runtime_options.clone(),
        config.clone(),
        1,
        typed_bounds(),
    )
    .expect("lab");
    (config, lab)
}

async fn pg_seed_transaction(fixture: &Fixture) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = fixture.admin.begin().await.expect("seed transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS SHARE MODE",
        fixture.target
    ))
    .execute(&mut *tx)
    .await
    .expect("epoch prelock");
    sqlx::query(
        "SELECT pg_catalog.set_config('waddle.protocol_epoch', '0', true), \
         pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)",
    )
    .execute(&mut *tx)
    .await
    .expect("seed epoch proof");
    tx
}

async fn seed_pg(fixture: &Fixture, key: uuid::Uuid, frontier: Option<DateTime<Utc>>) {
    let schema = &fixture.target;
    let mut tx = pg_seed_transaction(fixture).await;
    sqlx::query(&format!("INSERT INTO {schema}.ingress_messages (message_key, digest_version, digest, terminal_at, retention_eligible_at) VALUES ($1, 1, $2, $3, $4)"))
        .bind(key).bind([1_u8; 32].as_slice()).bind(now() - ChronoDuration::days(100)).bind(frontier).execute(&mut *tx).await.expect("canonical");
    sqlx::query(&format!("INSERT INTO {schema}.ingress_origin_aliases (alias_key_hash, sender_bare_jid, target_kind, origin_id, message_key) VALUES ($1, 'sender@example.test', 0, 'origin', $2)"))
        .bind(key.as_bytes().repeat(2)).bind(key).execute(&mut *tx).await.expect("alias");
    sqlx::query(&format!(
        "INSERT INTO {schema}.ingress_deliveries (delivery_key, message_key) VALUES ($1, $2)"
    ))
    .bind(uuid::Uuid::new_v4())
    .bind(key)
    .execute(&mut *tx)
    .await
    .expect("delivery");
    tx.commit().await.expect("commit canonical seed");
}

async fn pg_count(fixture: &Fixture, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}.{table}", fixture.target))
        .fetch_one(&fixture.admin)
        .await
        .expect("count")
}

#[tokio::test]
async fn postgres_qualified_bounded_batch_and_missing_relations() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (_config, lab) = postgres_fixture(&fixture).await;
    let adopt = uuid::Uuid::from_u128(1);
    let expired = uuid::Uuid::from_u128(2);
    seed_pg(&fixture, adopt, None).await;
    seed_pg(&fixture, expired, Some(now() - ALIAS_RETENTION)).await;
    // Poison search_path during the first update. Subsequent statements must
    // continue using the attested schema even though the session changed.
    let shadow = format!("shadow_{}", uuid::Uuid::new_v4().simple());
    for statement in [
        format!("CREATE SCHEMA {shadow}"),
        format!("CREATE TABLE {shadow}.ingress_messages (LIKE {}.ingress_messages INCLUDING ALL)", fixture.target),
        format!("CREATE TABLE {shadow}.pending_delivery (row_id text PRIMARY KEY)"),
        format!("INSERT INTO {shadow}.ingress_messages SELECT * FROM {}.ingress_messages", fixture.target),
        format!("GRANT USAGE ON SCHEMA {shadow} TO {}", fixture.runtime),
        format!("GRANT ALL ON ALL TABLES IN SCHEMA {shadow} TO {}", fixture.runtime),
        format!("CREATE FUNCTION {}.poison_retention_path() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_catalog.set_config('search_path', '{shadow}, {}', true); RETURN NEW; END $$", fixture.target, fixture.target),
        format!("CREATE TRIGGER poison_retention_path AFTER UPDATE ON {}.ingress_messages FOR EACH ROW EXECUTE FUNCTION {}.poison_retention_path()", fixture.target, fixture.target),
    ] { sqlx::query(&statement).execute(&fixture.admin).await.expect("shadow setup"); }
    assert_eq!(
        lab.collect(now(), 2).await.expect("bounded batch"),
        RetentionLabBatch {
            examined: 2,
            adopted: 1,
            deleted: 1
        }
    );
    assert_eq!(pg_count(&fixture, "ingress_messages").await, 1);
    let shadow_count: i64 =
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {shadow}.ingress_messages"))
            .fetch_one(&fixture.admin)
            .await
            .expect("shadow count");
    assert_eq!(shadow_count, 2);
    assert_eq!(
        lab.collect(now() + ALIAS_RETENTION - ChronoDuration::microseconds(1), 1)
            .await
            .expect("tail")
            .deleted,
        0
    );
    assert_eq!(
        lab.collect(now() + ALIAS_RETENTION, 1)
            .await
            .expect("cutoff")
            .deleted,
        1
    );
    sqlx::query(&format!(
        "ALTER TABLE {}.pending_delivery RENAME TO hidden_pending_delivery",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove colocated pending");
    assert!(
        matches!(
            lab.collect(now(), 1).await,
            Err(DarkIngressAuthorityError::Database)
        ),
        "a shadow relation cannot supply missing custody"
    );
    sqlx::query(&format!(
        "ALTER TABLE {}.hidden_pending_delivery RENAME TO pending_delivery",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore pending relation");
    sqlx::query(&format!(
        "ALTER TABLE {}.sm_ingress_appends RENAME TO hidden_sm_ingress_appends",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove colocated SM relation");
    assert!(matches!(
        lab.collect(now(), 1).await,
        Err(DarkIngressAuthorityError::Database)
    ));
    drop(lab);
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_skips_locked_and_rechecks_custody_after_writer_commits() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (_config, lab) = postgres_fixture(&fixture).await;
    let key = uuid::Uuid::new_v4();
    seed_pg(&fixture, key, Some(now() - ALIAS_RETENTION)).await;
    let mut writer = fixture.admin.begin().await.expect("writer");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.ingress_messages WHERE message_key = $1 FOR UPDATE",
        fixture.target
    ))
    .bind(key)
    .fetch_one(&mut *writer)
    .await
    .expect("lock canonical");
    sqlx::query(&format!("INSERT INTO {}.sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES ($1, 1, $2, 'user@example.test/r', 's', 1, 1, 'payload', 1, 0)", fixture.target)).bind(key.to_string()).bind([2_u8; 32].as_slice()).execute(&mut *writer).await.expect("pending custody");
    assert_eq!(
        lab.collect(now(), 1).await.expect("skip locked"),
        RetentionLabBatch::default()
    );
    writer.commit().await.expect("custody commits");
    assert_eq!(
        lab.collect(now(), 1).await.expect("custody blocks"),
        RetentionLabBatch {
            examined: 1,
            adopted: 0,
            deleted: 0
        }
    );
    assert_eq!(pg_count(&fixture, "ingress_messages").await, 1);
    assert_eq!(pg_count(&fixture, "sm_ingress_appends").await, 1);
    let invalidated: bool = sqlx::query_scalar(&format!(
        "SELECT retention_eligible_at IS NULL FROM {}.ingress_messages WHERE message_key = $1",
        fixture.target
    ))
    .bind(key)
    .fetch_one(&fixture.admin)
    .await
    .expect("frontier");
    assert!(invalidated, "observed custody invalidates an old frontier");
    sqlx::query(&format!(
        "UPDATE {}.sm_ingress_appends SET disposition = 1 WHERE message_key = $1",
        fixture.target
    ))
    .bind(key.to_string())
    .execute(&fixture.admin)
    .await
    .expect("settle custody");
    assert_eq!(lab.collect(now(), 1).await.expect("fresh tail").adopted, 1);
    drop(lab);
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_late_drained_append_demonstrates_unclosed_writer_boundary() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (_config, lab) = postgres_fixture(&fixture).await;
    let key = uuid::Uuid::new_v4();
    seed_pg(&fixture, key, Some(now() - ALIAS_RETENTION)).await;
    // Model a frame already authorized/reserved before this call. The actual
    // drained/exclusive insertion has no FK or canonical lock and can run later.
    assert_eq!(
        lab.collect(now(), 1)
            .await
            .expect("quiescent lab collection")
            .deleted,
        1
    );
    sqlx::query(&format!("INSERT INTO {}.sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES ($1, 1, $2, 'user@example.test/r', 's', 1, 1, 'payload', 1, 0) ON CONFLICT (message_key, receipt_kind, semantic_identity_hash, resource) DO NOTHING", fixture.target))
        .bind(key.to_string()).bind([3_u8; 32].as_slice()).execute(&fixture.admin).await.expect("late drained write is accepted");
    assert_eq!(pg_count(&fixture, "ingress_messages").await, 0);
    assert_eq!(pg_count(&fixture, "sm_ingress_appends").await, 1);
    assert_eq!(
        lab.collect(now(), 1).await.expect("no orphan sweep"),
        RetentionLabBatch::default()
    );
    assert_eq!(pg_count(&fixture, "sm_ingress_appends").await, 1);
    drop(lab);
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_rollback_and_indeterminate_commit_never_return_counts() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (_config, lab) = postgres_fixture(&fixture).await;
    let key = uuid::Uuid::new_v4();
    seed_pg(&fixture, key, Some(now() - ALIAS_RETENTION)).await;
    let schema = &fixture.target;
    for statement in [
        format!("CREATE FUNCTION {schema}.reject_retention() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected'; END $$"),
        format!("CREATE TRIGGER reject_retention BEFORE DELETE ON {schema}.ingress_messages FOR EACH ROW EXECUTE FUNCTION {schema}.reject_retention()"),
    ] { sqlx::query(&statement).execute(&fixture.admin).await.expect("install failure"); }
    assert!(matches!(
        lab.collect(now(), 1).await,
        Err(DarkIngressAuthorityError::Database)
    ));
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_deliveries",
    ] {
        assert_eq!(pg_count(&fixture, table).await, 1, "rollback: {table}");
    }
    sqlx::query(&format!(
        "DROP TRIGGER reject_retention ON {schema}.ingress_messages"
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove failure");
    sqlx::query(&format!("CREATE CONSTRAINT TRIGGER reject_commit AFTER DELETE ON {schema}.ingress_messages DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION {schema}.reject_retention()")).execute(&fixture.admin).await.expect("deferred failure");
    assert!(matches!(
        lab.collect(now(), 1).await,
        Err(DarkIngressAuthorityError::CommitIndeterminate)
    ));
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_deliveries",
    ] {
        assert_eq!(pg_count(&fixture, table).await, 1, "failed commit: {table}");
    }
    drop(lab);
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_fresh_recheck_handles_custody_added_or_settled_after_selection() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, unused) = postgres_fixture(&fixture).await;
    drop(unused);
    let first = uuid::Uuid::from_u128(1);
    let second = uuid::Uuid::from_u128(2);
    let third = uuid::Uuid::from_u128(3);
    seed_pg(&fixture, first, None).await;
    seed_pg(&fixture, second, Some(now() - ALIAS_RETENTION)).await;
    seed_pg(&fixture, third, Some(now() - ALIAS_RETENTION)).await;
    let schema = &fixture.target;
    sqlx::query(&format!("INSERT INTO {schema}.sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES ($1, 1, $2, 'user@example.test/r', 's', 1, 1, 'payload', 1, 0)"))
        .bind(third.to_string()).bind([8_u8; 32].as_slice()).execute(&fixture.admin).await.expect("custody before selection");
    for statement in [
        format!("CREATE FUNCTION {schema}.wait_in_retention() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_catalog.pg_advisory_xact_lock(7199143); RETURN NEW; END $$"),
        format!("CREATE TRIGGER wait_in_retention BEFORE UPDATE ON {schema}.ingress_messages FOR EACH ROW EXECUTE FUNCTION {schema}.wait_in_retention()"),
    ] { sqlx::query(&statement).execute(&fixture.admin).await.expect("install test pause"); }
    let mut blocker = fixture.admin.begin().await.expect("blocker");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock(7199143)")
        .execute(&mut *blocker)
        .await
        .expect("hold pause");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .expect("blocker pid");
    let application = format!("retention_recheck_{}", uuid::Uuid::new_v4().simple());
    let lab = Arc::new(
        RetentionLab::postgres(
            fixture
                .runtime_options
                .clone()
                .application_name(&application),
            config,
            1,
            typed_bounds(),
        )
        .expect("lab"),
    );
    let task = {
        let lab = lab.clone();
        tokio::spawn(async move { lab.collect(now(), 3).await })
    };
    wait_for_blocked_application(&fixture.admin, &application, blocker_pid).await;
    // The candidate statement already locked all rows. This deliberately
    // violates quiescence to test the subsequent READ COMMITTED recheck.
    sqlx::query(&format!("INSERT INTO {schema}.sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES ($1, 1, $2, 'user@example.test/r', 's', 1, 1, 'payload', 1, 0)"))
        .bind(second.to_string()).bind([9_u8; 32].as_slice()).execute(&fixture.admin).await.expect("concurrent custody");
    sqlx::query(&format!(
        "UPDATE {schema}.sm_ingress_appends SET disposition = 1 WHERE message_key = $1"
    ))
    .bind(third.to_string())
    .execute(&fixture.admin)
    .await
    .expect("concurrent settlement");
    blocker.commit().await.expect("resume collector");
    assert_eq!(
        task.await.expect("task").expect("batch"),
        RetentionLabBatch {
            examined: 3,
            adopted: 2,
            deleted: 0
        }
    );
    let retained: (bool, bool) = sqlx::query_as(&format!("SELECT retention_eligible_at IS NULL, EXISTS (SELECT 1 FROM {schema}.ingress_origin_aliases WHERE message_key = $1) FROM {schema}.ingress_messages WHERE message_key = $1"))
        .bind(second).fetch_one(&fixture.admin).await.expect("retained second row");
    assert_eq!(retained, (true, true));
    let fresh: DateTime<Utc> = sqlx::query_scalar(&format!(
        "SELECT retention_eligible_at FROM {schema}.ingress_messages WHERE message_key = $1"
    ))
    .bind(third)
    .fetch_one(&fixture.admin)
    .await
    .expect("settled custody restarts tail");
    assert_eq!(fresh, now());
    assert_eq!(pg_count(&fixture, "sm_ingress_appends").await, 2);
    drop(lab);
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_cancelled_epoch_wait_releases_one_slot_and_future_epoch_fails_closed() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, unused) = postgres_fixture(&fixture).await;
    drop(unused);
    let application = format!("retention_cancel_{}", uuid::Uuid::new_v4().simple());
    let lab = Arc::new(
        RetentionLab::postgres(
            fixture
                .runtime_options
                .clone()
                .application_name(&application),
            config,
            1,
            typed_bounds(),
        )
        .expect("lab"),
    );
    let mut blocker = fixture.admin.begin().await.expect("blocker");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *blocker)
    .await
    .expect("epoch relation lock");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .expect("blocker pid");
    let task = {
        let lab = lab.clone();
        tokio::spawn(async move { lab.collect(now(), 1).await })
    };
    wait_for_blocked_application(&fixture.admin, &application, blocker_pid).await;
    task.abort();
    assert!(task.await.expect_err("cancelled").is_cancelled());
    blocker.rollback().await.expect("release epoch");
    assert_eq!(
        lab.collect(now(), 1)
            .await
            .expect("one-slot pool recovered"),
        RetentionLabBatch::default()
    );
    for epoch in [1_i64, 2] {
        let mut transition = fixture.admin.begin().await.expect("transition");
        sqlx::query(&format!(
            "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
            fixture.target
        ))
        .execute(&mut *transition)
        .await
        .expect("transition lock");
        sqlx::query(&format!("UPDATE {}.ingress_protocol_epoch SET epoch = $1, activated_at = now(), lineage_uuid = $2 WHERE id = 1", fixture.target))
            .bind(epoch).bind(fixture.lineage.lineage_uuid.0).execute(&mut *transition).await.expect("advance epoch");
        transition.commit().await.expect("transition commit");
    }
    assert!(matches!(
        lab.collect(now(), 1).await,
        Err(DarkIngressAuthorityError::UnsupportedEpoch)
    ));
    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_protocol_epoch DISABLE TRIGGER ingress_protocol_epoch_guard_row",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("allow missing epoch fixture");
    sqlx::query(&format!(
        "DELETE FROM {}.ingress_protocol_epoch WHERE id = 1",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove epoch");
    assert!(matches!(
        lab.collect(now(), 1).await,
        Err(DarkIngressAuthorityError::MissingEpoch)
    ));
    drop(lab);
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_each_live_obligation_blocks_without_starving_ready_rows() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (_config, lab) = postgres_fixture(&fixture).await;
    let receipt = uuid::Uuid::from_u128(1);
    let descendant = uuid::Uuid::from_u128(2);
    let pending = uuid::Uuid::from_u128(3);
    let reference = uuid::Uuid::from_u128(4);
    let sm = uuid::Uuid::from_u128(5);
    let ready = uuid::Uuid::from_u128(6);
    for key in [receipt, descendant, pending, reference, sm, ready] {
        seed_pg(&fixture, key, None).await;
    }
    let schema = &fixture.target;
    let mut seed = pg_seed_transaction(&fixture).await;
    for key in [receipt, descendant, pending, ready] {
        sqlx::query(&format!("INSERT INTO {schema}.ingress_effect_intents (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) VALUES ($1, 0, 1, $2, 1, $3)"))
            .bind(key).bind([4_u8; 32].as_slice()).bind(Vec::<u8>::new()).execute(&mut *seed).await.expect("intent");
    }
    for key in [descendant, pending, ready] {
        sqlx::query(&format!("INSERT INTO {schema}.ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES ($1, 1, $2)"))
            .bind(key).bind([4_u8; 32].as_slice()).execute(&mut *seed).await.expect("receipt");
    }
    sqlx::query(&format!("INSERT INTO {schema}.ingress_effect_descendants (message_key, kind, semantic_identity_hash, descendant_key) VALUES ($1, 1, $2, 'child')"))
        .bind(descendant).bind([4_u8; 32].as_slice()).execute(&mut *seed).await.expect("unsettled descendant");
    sqlx::query(&format!(
        "INSERT INTO {schema}.pending_delivery (row_id) VALUES ('queued')"
    ))
    .execute(&mut *seed)
    .await
    .expect("queued payload");
    sqlx::query(&format!("INSERT INTO {schema}.ingress_archive_dispatch (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource, pending_row_id) VALUES ('user@example.test', 1, $1, 1, $2, '', 'queued')"))
        .bind(pending).bind([4_u8; 32].as_slice()).execute(&mut *seed).await.expect("pending dispatch");
    sqlx::query(&format!("INSERT INTO {schema}.ingress_archive_dispatch (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource, pending_row_id) VALUES ('user@example.test', 2, $1, 1, $2, '', 'settled')"))
        .bind(ready).bind([4_u8; 32].as_slice()).execute(&mut *seed).await.expect("settled dispatch");
    sqlx::query(&format!("INSERT INTO {schema}.ingress_sm_refs (sm_ingress_id, ingress_ordinal, wire_h, wire_generation, message_key) VALUES ($1, 1, 1, 0, $2)"))
        .bind(uuid::Uuid::new_v4()).bind(reference).execute(&mut *seed).await.expect("SM reference");
    sqlx::query(&format!("INSERT INTO {schema}.sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES ($1, 1, $2, 'user@example.test/r', 's', 1, 1, 'payload', 1, 0)"))
        .bind(sm.to_string()).bind([5_u8; 32].as_slice()).execute(&mut *seed).await.expect("SM custody");
    seed.commit().await.expect("commit custody fixtures");
    assert_eq!(
        lab.collect(now(), 1).await.expect("ready row is reachable"),
        RetentionLabBatch {
            examined: 1,
            adopted: 1,
            deleted: 0
        }
    );
    let marked: Vec<uuid::Uuid> = sqlx::query_scalar(&format!(
        "SELECT message_key FROM {schema}.ingress_messages WHERE retention_eligible_at IS NOT NULL"
    ))
    .fetch_all(&fixture.admin)
    .await
    .expect("adopted keys");
    assert_eq!(marked, vec![ready]);
    assert_eq!(pg_count(&fixture, "ingress_origin_aliases").await, 6);
    assert_eq!(pg_count(&fixture, "ingress_deliveries").await, 6);
    assert_eq!(pg_count(&fixture, "sm_ingress_appends").await, 1);
    assert_eq!(
        lab.collect(now() + ALIAS_RETENTION, 1)
            .await
            .expect("collect settled effect")
            .deleted,
        1
    );
    for table in [
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_archive_dispatch",
    ] {
        let remaining: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {schema}.{table} WHERE message_key = $1"
        ))
        .bind(ready)
        .fetch_one(&fixture.admin)
        .await
        .expect("cascade count");
        assert_eq!(remaining, 0, "canonical cascade removes {table}");
    }
    drop(lab);
    fixture.cleanup().await;
}
