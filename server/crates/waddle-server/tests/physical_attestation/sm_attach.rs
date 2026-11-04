//! Restricted, read-only attachment of the portable SM schema.

use super::{
    attested_pool, install, test_url, url_with_schema, Database, DatabaseConfig, DatabaseDriver,
    DatabaseError, Duration, Fixture, MigrationRunner, PhysicalAttestationConfig,
    CATALOG_GRANT_LOCK,
};
use waddle_server::db::DatabasePool;
use waddle_server::sm_persistence::{
    bootstrap_portable_sm_owner_schema, DatabaseSmPersistence, SmRuntimeAttachError,
};

async fn attach(db: &Database) -> Result<DatabaseSmPersistence, SmRuntimeAttachError> {
    DatabaseSmPersistence::attach_existing_postgres(db.clone()).await
}

async fn admin(fixture: &Fixture, sql: &str) {
    sqlx::raw_sql(sql).execute(&fixture.admin).await.expect(sql);
}

async fn setup(fixture: &Fixture) -> (Database, DatabasePool, PhysicalAttestationConfig) {
    let owner = Database::from_config(
        "portable SM owner",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            url_with_schema(&fixture.base_url, &fixture.target),
        ),
    )
    .await
    .expect("owner database");
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install physical attestor");
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "portable SM restricted runtime",
        attested_pool(
            fixture.runtime_options.clone(),
            config.clone(),
            2,
            Duration::from_secs(8),
        )
        .expect("attested pool"),
        1,
        1,
    )
    .await
    .expect("main and ingress lanes");
    (owner, pool, config)
}

async fn grant_reads(fixture: &Fixture) {
    for table in [
        "sm_sessions",
        "sm_unacked",
        "sm_acknowledged_windows",
        "sm_ingress_appends",
    ] {
        admin(
            fixture,
            &format!(
                "GRANT SELECT ON {}.{table} TO {}",
                fixture.target, fixture.runtime
            ),
        )
        .await;
    }
}

async fn snapshot(fixture: &Fixture) -> (String, String) {
    let catalog: String = sqlx::query_scalar(
        "SELECT COALESCE(pg_catalog.string_agg(item, E'\\n' ORDER BY item), '') FROM ( \
           SELECT pg_catalog.jsonb_build_array('column', t.relname, a.attname, \
             pg_catalog.format_type(a.atttypid, a.atttypmod), a.attnotnull, \
             pg_catalog.pg_get_expr(d.adbin, d.adrelid))::text AS item \
           FROM pg_catalog.pg_class t JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
           JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum > 0 AND NOT a.attisdropped \
           LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = t.oid AND d.adnum = a.attnum \
           WHERE n.nspname = $1 AND t.relname IN ('sm_sessions', 'sm_unacked', 'sm_acknowledged_windows', 'sm_ingress_appends') \
           UNION ALL \
           SELECT pg_catalog.jsonb_build_array('constraint', t.relname, c.conname, pg_catalog.pg_get_constraintdef(c.oid))::text \
           FROM pg_catalog.pg_constraint c JOIN pg_catalog.pg_class t ON t.oid = c.conrelid \
           JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace WHERE n.nspname = $1 AND t.relname LIKE 'sm_%' \
           UNION ALL \
           SELECT pg_catalog.jsonb_build_array('index', t.relname, i.relname, pg_catalog.pg_get_indexdef(i.oid))::text \
           FROM pg_catalog.pg_index x JOIN pg_catalog.pg_class i ON i.oid = x.indexrelid \
           JOIN pg_catalog.pg_class t ON t.oid = x.indrelid JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
           WHERE n.nspname = $1 AND t.relname LIKE 'sm_%') entries",
    ).bind(&fixture.target).fetch_one(&fixture.admin).await.expect("SM catalog snapshot");
    let sentinel: String = sqlx::query_scalar(&format!(
        "SELECT pg_catalog.to_jsonb(s)::text FROM {}.sm_sessions s WHERE stream_id = 'sentinel'",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("sentinel snapshot");
    (catalog, sentinel)
}

async fn drift(
    fixture: &Fixture,
    runtime: &Database,
    mutate: &str,
    restore: &str,
    expected: fn(&SmRuntimeAttachError) -> bool,
) {
    admin(fixture, mutate).await;
    let before = snapshot(fixture).await;
    let error = match attach(runtime).await {
        Ok(_) => panic!("accepted invalid SM schema after {mutate}"),
        Err(error) => error,
    };
    assert!(
        expected(&error),
        "unexpected refusal after {mutate}: {error:?}"
    );
    assert_eq!(
        snapshot(fixture).await,
        before,
        "refusal must not mutate catalog or sentinel"
    );
    admin(fixture, restore).await;
    attach(runtime)
        .await
        .expect("owner-restored schema attaches");
}

#[tokio::test]
async fn portable_sm_attach_rejects_sqlite_before_inspection() {
    let sqlite = Database::in_memory("sm attachment refuses sqlite")
        .await
        .expect("SQLite");
    assert!(matches!(
        attach(&sqlite).await,
        Err(SmRuntimeAttachError::PostgresRequired)
    ));
}

#[tokio::test]
async fn portable_sm_attach_checks_restricted_snapshot_and_schema_drift() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (owner, pool, config) = setup(&fixture).await;
    let runtime = pool.global().clone();
    assert!(matches!(
        attach(&owner).await,
        Err(SmRuntimeAttachError::AttestedPostgresPoolRequired)
    ));
    assert!(matches!(
        attach(&pool.ingress()).await,
        Err(SmRuntimeAttachError::MainLaneRequired)
    ));
    assert!(matches!(
        attach(&runtime).await,
        Err(SmRuntimeAttachError::Relation {
            relation: "sm_sessions",
            ..
        })
    ));

    bootstrap_portable_sm_owner_schema(&owner)
        .await
        .expect("real portable owner initialization");
    grant_reads(&fixture).await;
    admin(&fixture, &format!("INSERT INTO {}.sm_sessions (stream_id, user_id, full_jid, inbound_count, outbound_count, last_acked, detached_at_ms, max_resume_duration_ms, carbons_enabled, roster_interested, presence_available, presence_priority) VALUES ('sentinel', 'alice', 'alice@example.test/phone', 3, 4, 2, 1000, 60000, 0, 1, 1, 0)", fixture.target)).await;
    let guard = runtime.guard().await.expect("restricted runtime guard");
    assert!(guard
        .execute("CREATE TABLE forbidden_sm_ddl (id integer)", ())
        .await
        .is_err());
    assert!(guard.execute("DELETE FROM sm_sessions", ()).await.is_err());
    drop(guard);
    // A raw pool for the same role, endpoint and schema has no capability.
    // Holding the target lock proves rejection does not wait to inspect it.
    let ordinary_pool = sqlx::postgres::PgPool::connect_with(fixture.runtime_options.clone())
        .await
        .expect("ordinary runtime pool");
    let ordinary = waddle_server::test_support::database_from_unattested_postgres_pool(
        "same-schema wrong pool",
        ordinary_pool,
    );
    let mut early_blocker = fixture.admin.begin().await.expect("early lock transaction");
    sqlx::query(&format!(
        "LOCK TABLE {}.sm_sessions IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *early_blocker)
    .await
    .expect("block schema inspection");
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(200), attach(&ordinary)).await,
        Ok(Err(SmRuntimeAttachError::MainLaneRequired))
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(200), attach(&pool.ingress())).await,
        Ok(Err(SmRuntimeAttachError::MainLaneRequired))
    ));
    early_blocker.rollback().await.expect("release early lock");
    drop(ordinary);
    let before = snapshot(&fixture).await;
    let storage = attach(&runtime)
        .await
        .expect("restricted SELECT-only attach");
    let guard = storage
        .database()
        .guard()
        .await
        .expect("attached runtime guard");
    let mut rows = guard
        .query(
            "SELECT inbound_count FROM sm_sessions WHERE stream_id = 'sentinel'",
            (),
        )
        .await
        .expect("read sentinel through supplied database");
    assert_eq!(
        rows.next()
            .await
            .expect("row result")
            .expect("sentinel row")
            .get::<i64>(0)
            .expect("count"),
        3
    );
    drop(rows);
    drop(guard);
    drop(storage);
    assert_eq!(
        snapshot(&fixture).await,
        before,
        "successful attach is read-only"
    );

    let s = &fixture.target;
    drift(
        &fixture,
        &runtime,
        &format!("ALTER TABLE {s}.sm_acknowledged_windows RENAME TO hidden_sm_windows"),
        &format!("ALTER TABLE {s}.hidden_sm_windows RENAME TO sm_acknowledged_windows"),
        |e| {
            matches!(
                e,
                SmRuntimeAttachError::Relation {
                    relation: "sm_acknowledged_windows",
                    ..
                }
            )
        },
    )
    .await;
    drift(&fixture, &runtime, &format!("ALTER TABLE {s}.sm_acknowledged_windows RENAME TO hidden_sm_windows; CREATE VIEW {s}.sm_acknowledged_windows AS SELECT * FROM {s}.hidden_sm_windows; GRANT SELECT ON {s}.sm_acknowledged_windows TO {}", fixture.runtime), &format!("DROP VIEW {s}.sm_acknowledged_windows; ALTER TABLE {s}.hidden_sm_windows RENAME TO sm_acknowledged_windows"), |e| matches!(e, SmRuntimeAttachError::InvalidTable { table: "sm_acknowledged_windows" })).await;
    drift(
        &fixture,
        &runtime,
        &format!("ALTER TABLE {s}.sm_sessions ADD COLUMN shadow_ordinal bigint"),
        &format!("ALTER TABLE {s}.sm_sessions DROP COLUMN shadow_ordinal"),
        |e| {
            matches!(
                e,
                SmRuntimeAttachError::InvalidColumn {
                    table: "sm_sessions",
                    column: "shadow_ordinal"
                }
            )
        },
    )
    .await;
    drift(
        &fixture,
        &runtime,
        &format!("ALTER TABLE {s}.sm_sessions ALTER COLUMN blocklist_interested DROP DEFAULT"),
        &format!("ALTER TABLE {s}.sm_sessions ALTER COLUMN blocklist_interested SET DEFAULT 0"),
        |e| {
            matches!(
                e,
                SmRuntimeAttachError::InvalidColumn {
                    table: "sm_sessions",
                    column: "blocklist_interested"
                }
            )
        },
    )
    .await;
    drift(
        &fixture,
        &runtime,
        &format!("ALTER TABLE {s}.sm_unacked ALTER COLUMN pair_sequence TYPE integer"),
        &format!("ALTER TABLE {s}.sm_unacked ALTER COLUMN pair_sequence TYPE bigint"),
        |e| {
            matches!(
                e,
                SmRuntimeAttachError::InvalidColumn {
                    table: "sm_unacked",
                    column: "pair_sequence"
                }
            )
        },
    )
    .await;
    drift(
        &fixture,
        &runtime,
        &format!("ALTER TABLE {s}.sm_unacked ALTER COLUMN stanza_xml DROP NOT NULL"),
        &format!("ALTER TABLE {s}.sm_unacked ALTER COLUMN stanza_xml SET NOT NULL"),
        |e| {
            matches!(
                e,
                SmRuntimeAttachError::InvalidColumn {
                    table: "sm_unacked",
                    column: "stanza_xml"
                }
            )
        },
    )
    .await;
    drift(&fixture, &runtime, &format!("ALTER TABLE {s}.sm_acknowledged_windows RENAME COLUMN to_inclusive TO hidden_to_inclusive"), &format!("ALTER TABLE {s}.sm_acknowledged_windows RENAME COLUMN hidden_to_inclusive TO to_inclusive"), |e| matches!(e, SmRuntimeAttachError::InvalidColumn { table: "sm_acknowledged_windows", column: "to_inclusive" })).await;
    drift(&fixture, &runtime, &format!("ALTER TABLE {s}.sm_acknowledged_windows DROP CONSTRAINT sm_acknowledged_windows_pkey; ALTER TABLE {s}.sm_acknowledged_windows ADD PRIMARY KEY (to_inclusive, from_exclusive, stream_id)"), &format!("ALTER TABLE {s}.sm_acknowledged_windows DROP CONSTRAINT sm_acknowledged_windows_pkey; ALTER TABLE {s}.sm_acknowledged_windows ADD PRIMARY KEY (stream_id, from_exclusive, to_inclusive)"), |e| matches!(e, SmRuntimeAttachError::InvalidPrimaryKey { table: "sm_acknowledged_windows" })).await;
    drift(&fixture, &runtime, &format!("ALTER TABLE {s}.sm_ingress_appends DROP CONSTRAINT sm_ingress_appends_disposition_check"), &format!("ALTER TABLE {s}.sm_ingress_appends ADD CONSTRAINT sm_ingress_appends_disposition_check CHECK (disposition IN (0, 1, 2, 3))"), |e| matches!(e, SmRuntimeAttachError::InvalidDispositionConstraint)).await;
    drift(&fixture, &runtime, &format!("ALTER TABLE {s}.sm_ingress_appends DROP CONSTRAINT sm_ingress_appends_disposition_check; ALTER TABLE {s}.sm_ingress_appends ADD CONSTRAINT sm_ingress_appends_disposition_check CHECK (disposition IN (0, 1, 2))"), &format!("ALTER TABLE {s}.sm_ingress_appends DROP CONSTRAINT sm_ingress_appends_disposition_check; ALTER TABLE {s}.sm_ingress_appends ADD CONSTRAINT sm_ingress_appends_disposition_check CHECK (disposition IN (0, 1, 2, 3))"), |e| matches!(e, SmRuntimeAttachError::InvalidDispositionConstraint)).await;
    drift(
        &fixture,
        &runtime,
        &format!("DROP INDEX {s}.idx_sm_sessions_detached"),
        &format!("CREATE INDEX idx_sm_sessions_detached ON {s}.sm_sessions (detached_at_ms)"),
        |e| {
            matches!(
                e,
                SmRuntimeAttachError::InvalidIndex {
                    index: "idx_sm_sessions_detached"
                }
            )
        },
    )
    .await;
    drift(&fixture, &runtime, &format!("DROP INDEX {s}.idx_sm_unacked_dedup; CREATE INDEX idx_sm_unacked_dedup ON {s}.sm_unacked (stream_id, origin_stream_id, inbound_seq) WHERE origin_stream_id IS NOT NULL AND inbound_seq IS NOT NULL"), &format!("DROP INDEX {s}.idx_sm_unacked_dedup; CREATE UNIQUE INDEX idx_sm_unacked_dedup ON {s}.sm_unacked (stream_id, origin_stream_id, inbound_seq) WHERE origin_stream_id IS NOT NULL AND inbound_seq IS NOT NULL"), |e| matches!(e, SmRuntimeAttachError::InvalidIndex { index: "idx_sm_unacked_dedup" })).await;
    drift(&fixture, &runtime, &format!("DROP INDEX {s}.idx_sm_unacked_dedup; CREATE UNIQUE INDEX idx_sm_unacked_dedup ON {s}.sm_unacked (stream_id, origin_stream_id, inbound_seq) WHERE origin_stream_id IS NOT NULL"), &format!("DROP INDEX {s}.idx_sm_unacked_dedup; CREATE UNIQUE INDEX idx_sm_unacked_dedup ON {s}.sm_unacked (stream_id, origin_stream_id, inbound_seq) WHERE origin_stream_id IS NOT NULL AND inbound_seq IS NOT NULL"), |e| matches!(e, SmRuntimeAttachError::InvalidIndex { index: "idx_sm_unacked_dedup" })).await;
    drift(&fixture, &runtime, &format!("DROP INDEX {s}.idx_sm_ingress_appends_pending; CREATE INDEX idx_sm_ingress_appends_pending ON {s}.sm_ingress_appends (appended_at_ms, disposition)"), &format!("DROP INDEX {s}.idx_sm_ingress_appends_pending; CREATE INDEX idx_sm_ingress_appends_pending ON {s}.sm_ingress_appends (disposition, appended_at_ms)"), |e| matches!(e, SmRuntimeAttachError::InvalidIndex { index: "idx_sm_ingress_appends_pending" })).await;

    drift(&fixture, &runtime, &format!("DROP INDEX {s}.idx_sm_ingress_appends_stream; CREATE INDEX idx_sm_ingress_appends_stream ON {s}.sm_ingress_appends (resource)"), &format!("DROP INDEX {s}.idx_sm_ingress_appends_stream; CREATE INDEX idx_sm_ingress_appends_stream ON {s}.sm_ingress_appends (accepting_stream_id)"), |e| matches!(e, SmRuntimeAttachError::InvalidIndex { index: "idx_sm_ingress_appends_stream" })).await;

    let mut wrong_identity = fixture.lineage.clone();
    wrong_identity
        .postgres_identity
        .as_mut()
        .expect("PostgreSQL identity")
        .schema
        .oid ^= 1;
    let wrong = waddle_server::test_support::database_from_attested_postgres_pool(
        "wrong SM schema OID",
        attested_pool(
            fixture.runtime_options.clone(),
            fixture.with_lineage(wrong_identity),
            1,
            Duration::from_secs(2),
        )
        .expect("construct mismatched pool"),
    );
    assert!(tokio::time::timeout(Duration::from_secs(4), attach(&wrong))
        .await
        .expect("bounded wrong-schema refusal")
        .is_err());
    drop(wrong);

    // The only physical runtime session is terminated and must be freshly
    // attested on replacement before the next successful snapshot.
    let guard = runtime.guard().await.expect("runtime connection");
    let mut rows = guard
        .query("SELECT pg_catalog.pg_backend_pid()::bigint", ())
        .await
        .expect("backend pid");
    let old_pid = rows
        .next()
        .await
        .expect("pid result")
        .expect("pid row")
        .get::<i64>(0)
        .expect("pid");
    drop(rows);
    drop(guard);
    let killed: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1::integer)")
        .bind(old_pid as i32)
        .fetch_one(&fixture.admin)
        .await
        .expect("terminate test backend");
    assert!(killed);
    attach(&runtime)
        .await
        .expect("replacement session attests and attaches");
    let guard = runtime.guard().await.expect("replacement connection");
    let mut rows = guard
        .query("SELECT pg_catalog.pg_backend_pid()::bigint", ())
        .await
        .expect("replacement pid");
    let new_pid = rows
        .next()
        .await
        .expect("pid result")
        .expect("pid row")
        .get::<i64>(0)
        .expect("pid");
    assert_ne!(old_pid, new_pid);
    drop(rows);
    drop(guard);

    let held = runtime.begin().await.expect("occupy main lane");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), attach(&runtime))
            .await
            .is_err()
    );
    held.commit().await.expect("release main lane");
    attach(&runtime)
        .await
        .expect("cancelled acquisition releases lane");
    let strict = waddle_server::test_support::database_from_attested_postgres_pool(
        "strict portable SM timeout",
        attested_pool(
            fixture
                .runtime_options
                .clone()
                .options([("lock_timeout", "200ms")]),
            config,
            1,
            Duration::from_secs(3),
        )
        .expect("strict pool"),
    );
    let mut blocker = fixture.admin.begin().await.expect("owner lock transaction");
    sqlx::query(&format!(
        "LOCK TABLE {s}.sm_sessions IN ACCESS EXCLUSIVE MODE"
    ))
    .execute(&mut *blocker)
    .await
    .expect("hold owner lock");
    let error = tokio::time::timeout(Duration::from_secs(3), attach(&strict))
        .await
        .expect("bounded lock refusal");
    assert!(
        matches!(error, Err(SmRuntimeAttachError::Relation { source: DatabaseError::Internal(sqlx::Error::Database(error)), .. }) if error.code().as_deref() == Some("55P03"))
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), attach(&runtime))
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(8), async {
        let tx = runtime.begin().await.expect("main lane after cancellation");
        tx.commit().await.expect("release main lane");
    })
    .await
    .expect("cancelled query releases lane within lock bound");
    blocker.rollback().await.expect("release lock");
    attach(&runtime).await.expect("attach after lock release");
    drop(strict);
    drop(runtime);
    pool.global_actor().kill();
    pool.global_actor().wait_for_shutdown().await;
    drop(pool);
    drop(owner);
    fixture.cleanup().await;
}

#[tokio::test]
async fn portable_sm_attach_accepts_owner_upgrade_and_migration_profiles() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (owner, pool, _) = setup(&fixture).await;
    let runtime = pool.global().clone();
    bootstrap_portable_sm_owner_schema(&owner)
        .await
        .expect("fresh owner initialization");
    grant_reads(&fixture).await;
    attach(&runtime).await.expect("fresh profile");
    let s = &fixture.target;
    for sql in [
        format!("ALTER TABLE {s}.sm_sessions ALTER COLUMN inbound_count TYPE integer"),
        format!("ALTER TABLE {s}.sm_sessions ADD COLUMN shadow_ordinal bigint"),
        format!("ALTER TABLE {s}.sm_sessions DROP COLUMN occupancy_session"),
        format!("ALTER TABLE {s}.sm_unacked DROP COLUMN ingress_receipts"),
        format!("ALTER TABLE {s}.sm_unacked DROP COLUMN pair_sequence"),
    ] {
        admin(&fixture, &sql).await;
    }
    assert!(attach(&runtime).await.is_err());
    bootstrap_portable_sm_owner_schema(&owner)
        .await
        .expect("real owner upgrade");
    attach(&runtime).await.expect("upgraded owner profile");
    // Run the real migration catalog through current V1028. Its cutover
    // recreates the SM tables and must have the same attachable profile.
    MigrationRunner::single()
        .run(&owner)
        .await
        .expect("actual PostgreSQL migration catalog");
    grant_reads(&fixture).await;
    attach(&runtime).await.expect("migrated profile");
    drop(runtime);
    pool.global_actor().kill();
    pool.global_actor().wait_for_shutdown().await;
    drop(pool);
    drop(owner);
    fixture.cleanup().await;
}

#[tokio::test]
async fn portable_sm_attach_observes_owner_ddl_before_snapshot() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (owner, pool, _) = setup(&fixture).await;
    let runtime = pool.global().clone();
    bootstrap_portable_sm_owner_schema(&owner)
        .await
        .expect("owner prepares the portable SM schema");
    grant_reads(&fixture).await;

    // This ALTER holds ACCESS EXCLUSIVE until commit. The attachment must wait
    // before establishing its catalog snapshot, then see the committed default.
    let mut blocker = fixture.admin.begin().await.expect("owner DDL transaction");
    sqlx::query(&format!(
        "ALTER TABLE {}.sm_ingress_appends ALTER COLUMN disposition SET DEFAULT 3",
        fixture.target
    ))
    .execute(&mut *blocker)
    .await
    .expect("change a required default under the owner lock");
    let attaching = runtime.clone();
    let pending = tokio::spawn(async move { attach(&attaching).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE datname = $1 AND usename = $2 AND wait_event_type = 'Lock' \
                 AND query LIKE '%sm_ingress_appends%')",
            )
            .bind(&fixture.database_name)
            .bind(&fixture.runtime)
            .fetch_one(&fixture.admin)
            .await
            .expect("observe attachment lock wait");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("attachment reaches the blocked relation");
    blocker.commit().await.expect("commit owner DDL");
    let result = pending.await.expect("join attachment");
    assert!(
        matches!(
            result,
            Err(SmRuntimeAttachError::InvalidColumn {
                table: "sm_ingress_appends",
                column: "disposition"
            })
        ),
        "the attachment must inspect the committed default"
    );
    let default: String = sqlx::query_scalar(
        "SELECT pg_catalog.pg_get_expr(d.adbin, d.adrelid) FROM pg_catalog.pg_attrdef d \
         JOIN pg_catalog.pg_class t ON t.oid = d.adrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
         JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = d.adnum \
         WHERE n.nspname = $1 AND t.relname = 'sm_ingress_appends' AND a.attname = 'disposition'",
    )
    .bind(&fixture.target)
    .fetch_one(&fixture.admin)
    .await
    .expect("owner DDL remains");
    assert_eq!(default, "3");
    admin(
        &fixture,
        &format!(
            "ALTER TABLE {}.sm_ingress_appends ALTER COLUMN disposition DROP DEFAULT",
            fixture.target
        ),
    )
    .await;
    attach(&runtime)
        .await
        .expect("owner-restored schema attaches");
    drop(runtime);
    pool.global_actor().kill();
    pool.global_actor().wait_for_shutdown().await;
    drop(pool);
    drop(owner);
    fixture.cleanup().await;
}
