//! The pending-delivery attachment requires owner preparation and only reads it.

use super::{
    attested_pool, install, test_url, url_with_schema, Database, DatabaseConfig, DatabaseDriver,
    DatabaseError, Duration, Fixture, CATALOG_GRANT_LOCK,
};
use waddle_server::pending_delivery::{
    bootstrap_pending_delivery_owner_schema, DatabasePendingDeliveryStorage,
    PendingRuntimeAttachError,
};
use waddle_server::server::ResolvedXmppDatabaseUrl;
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId, QuotaPolicy};

async fn attach(
    db: &Database,
) -> Result<DatabasePendingDeliveryStorage, PendingRuntimeAttachError> {
    DatabasePendingDeliveryStorage::attach_existing(
        db.clone(),
        QuotaPolicy::CountCap { max_rows: 42 },
    )
    .await
}

async fn admin(fixture: &Fixture, sql: &str) {
    sqlx::query(sql).execute(&fixture.admin).await.expect(sql);
}

#[tokio::test]
async fn pending_startup_uses_attested_shared_pool_without_owner_repairs() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let owner = Database::from_config(
        "pending startup owner",
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
    .expect("install attestor");
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "pending startup runtime",
        attested_pool(
            fixture.runtime_options.clone(),
            config,
            2,
            Duration::from_secs(8),
        )
        .expect("attested pool"),
        1,
        1,
    )
    .await
    .expect("main and ingress lanes");
    let runtime = pool.global();
    let inherited = ResolvedXmppDatabaseUrl::InheritedGlobal("not a reconnect URL".into());
    let error = match waddle_server::test_support::attach_pending_startup(&inherited, runtime).await
    {
        Ok(_) => panic!("missing owner schema must fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error.downcast_ref::<PendingRuntimeAttachError>(),
        Some(PendingRuntimeAttachError::Relation {
            relation: "pending_delivery",
            ..
        })
    ));
    for selection in [
        ResolvedXmppDatabaseUrl::Explicit("not a reconnect URL".into()),
        ResolvedXmppDatabaseUrl::EphemeralFallback,
    ] {
        let error =
            match waddle_server::test_support::attach_pending_startup(&selection, runtime).await {
                Ok(_) => panic!("non-inherited pending store must fail"),
                Err(error) => error,
            };
        assert!(error
            .to_string()
            .contains("must inherit WADDLE_DATABASE_URL"));
    }

    bootstrap_pending_delivery_owner_schema(&owner)
        .await
        .expect("owner prepares pending store");
    for table in [
        "pending_delivery",
        "pending_delivery_sm_promotions",
        "pending_delivery_startup_migrations",
    ] {
        admin(
            &fixture,
            &format!(
                "GRANT SELECT ON {}.{table} TO {}",
                fixture.target, fixture.runtime
            ),
        )
        .await;
    }
    let error = match waddle_server::test_support::attach_pending_startup(&inherited, runtime).await
    {
        Ok(_) => panic!("a read-only role cannot run pending delivery"),
        Err(error) => error,
    };
    assert!(matches!(
        error.downcast_ref::<PendingRuntimeAttachError>(),
        Some(PendingRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "pending_delivery"
        })
    ));
    admin(
        &fixture,
        &format!(
            "GRANT INSERT, UPDATE, DELETE ON {}.pending_delivery TO {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    admin(
        &fixture,
        &format!(
            "GRANT INSERT, DELETE ON {}.pending_delivery_sm_promotions TO {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    let sequence: String = sqlx::query_scalar("SELECT pg_catalog.pg_get_serial_sequence(pg_catalog.format('%I.%I', $1, 'pending_delivery'), 'notification_recovery_ordinal')")
        .bind(&fixture.target).fetch_one(&fixture.admin).await.expect("identity sequence");
    admin(
        &fixture,
        &format!("GRANT USAGE ON SEQUENCE {sequence} TO {}", fixture.runtime),
    )
    .await;
    let storage = waddle_server::test_support::attach_pending_startup(&inherited, runtime)
        .await
        .expect("read-only shared-pool startup attachment");
    let recipient: jid::BareJid = "alice@example.test".parse().expect("recipient");
    assert_eq!(storage.count(&recipient).await.expect("pending read"), 0);
    let row_id = PendingRowId::new("runtime-row");
    storage
        .insert(PendingRow {
            id: row_id.clone(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(
                "runtime-stanza",
                recipient.clone().into(),
            )),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("restricted runtime inserts a pending row");
    assert_eq!(storage.count(&recipient).await.expect("pending read"), 1);
    assert_eq!(
        storage
            .delete_row(&row_id)
            .await
            .expect("restricted runtime deletes a pending row"),
        1
    );
    drop(storage);
    admin(
        &fixture,
        &format!(
            "GRANT INSERT ON {}.pending_delivery_startup_migrations TO {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    let error = match waddle_server::test_support::attach_pending_startup(&inherited, runtime).await
    {
        Ok(_) => panic!("runtime must not mutate owner migration markers"),
        Err(error) => error,
    };
    assert!(matches!(
        error.downcast_ref::<PendingRuntimeAttachError>(),
        Some(PendingRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "pending_delivery_startup_migrations"
        })
    ));
    admin(
        &fixture,
        &format!(
            "REVOKE INSERT ON {}.pending_delivery_startup_migrations FROM {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    admin(&fixture, &format!("DELETE FROM {}.pending_delivery_startup_migrations WHERE name = 'legacy_mam_query_frames_v1'", fixture.target)).await;
    let error = match waddle_server::test_support::attach_pending_startup(&inherited, runtime).await
    {
        Ok(_) => panic!("missing migration marker must fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error.downcast_ref::<PendingRuntimeAttachError>(),
        Some(PendingRuntimeAttachError::MissingMarker { .. })
    ));
    let remaining: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {}.pending_delivery_startup_migrations WHERE name = 'legacy_mam_query_frames_v1'", fixture.target))
        .fetch_one(&fixture.admin).await.expect("marker count");
    assert_eq!(
        remaining, 0,
        "runtime attachment must not repair the marker"
    );

    pool.global_actor().kill();
    pool.global_actor().wait_for_shutdown().await;
    drop(pool);
    drop(owner);
    fixture.cleanup().await;
}

#[tokio::test]
async fn pending_runtime_attach_checks_owner_preparation_and_restricted_snapshot() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let owner = Database::from_config(
        "pending owner",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            url_with_schema(&fixture.base_url, &fixture.target),
        ),
    )
    .await
    .expect("owner database");
    assert!(matches!(
        attach(&owner).await,
        Err(PendingRuntimeAttachError::AttestedPostgresPoolRequired)
    ));
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install attestor");
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "pending runtime",
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
    let runtime = pool.global().clone();
    assert!(matches!(
        attach(&pool.ingress()).await,
        Err(PendingRuntimeAttachError::MainLaneRequired)
    ));
    assert!(matches!(
        attach(&runtime).await,
        Err(PendingRuntimeAttachError::Relation {
            relation: "pending_delivery",
            ..
        })
    ));
    bootstrap_pending_delivery_owner_schema(&owner)
        .await
        .expect("owner prepares pending store");
    for table in [
        "pending_delivery",
        "pending_delivery_sm_promotions",
        "pending_delivery_startup_migrations",
    ] {
        admin(
            &fixture,
            &format!(
                "GRANT SELECT ON {}.{table} TO {}",
                fixture.target, fixture.runtime
            ),
        )
        .await;
    }
    let guard = owner.guard().await.expect("owner guard");
    guard.execute("INSERT INTO pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id, flushed_in_session, outbound_sequence, claimed_at_ms) VALUES ('row-a', 'alice@example.test', 1000, 'archived', 'alice@example.test', 'stanza-a', 'held-session', 12, 1000)", ()).await.expect("seed pending row");
    guard.execute("INSERT INTO pending_delivery_sm_promotions (origin_stream_id, sequence, created_at_ms) VALUES ('stream-a', 1, 1000)", ()).await.expect("seed receipt");
    drop(guard);
    let guard = runtime.guard().await.expect("runtime guard");
    assert!(guard
        .execute("CREATE TABLE forbidden_pending_ddl (id INTEGER)", ())
        .await
        .is_err());
    assert!(guard
        .execute("DELETE FROM pending_delivery", ())
        .await
        .is_err());
    drop(guard);
    let storage = attach(&runtime).await.expect("restricted attach");
    let recipient = "alice@example.test".parse().expect("recipient");
    let rows = storage
        .list(&recipient)
        .await
        .expect("restricted existing read");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id.as_str(), "row-a");
    assert_eq!(
        storage.count(&recipient).await.expect("restricted count"),
        1
    );
    assert!(matches!(
        storage.quota_policy(),
        QuotaPolicy::CountCap { max_rows: 42 }
    ));
    drop(storage);

    admin(&fixture, &format!("INSERT INTO {}.pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id, notification_recovery_ordinal) OVERRIDING SYSTEM VALUE VALUES ('bad-zero', 'alice@example.test', 1001, 'archived', 'alice@example.test', 'bad-zero-stanza', 0), ('bad-negative', 'alice@example.test', 1002, 'archived', 'alice@example.test', 'bad-negative-stanza', -7)", fixture.target)).await;
    assert!(matches!(
        attach(&runtime).await,
        Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "PostgreSQL ordinal values"
        })
    ));
    let ordinals: Vec<i64> = sqlx::query_scalar(&format!("SELECT notification_recovery_ordinal FROM {}.pending_delivery WHERE row_id IN ('bad-zero', 'bad-negative') ORDER BY notification_recovery_ordinal", fixture.target)).fetch_all(&fixture.admin).await.expect("invalid rows remain unchanged");
    assert_eq!(ordinals, vec![-7, 0]);
    admin(
        &fixture,
        &format!(
            "DELETE FROM {}.pending_delivery WHERE row_id IN ('bad-zero', 'bad-negative')",
            fixture.target
        ),
    )
    .await;

    let mut wrong_identity = fixture.lineage.clone();
    wrong_identity
        .postgres_identity
        .as_mut()
        .expect("PostgreSQL identity")
        .schema
        .oid ^= 1;
    let wrong_database = waddle_server::test_support::database_from_attested_postgres_pool(
        "wrong pending boundary identity",
        attested_pool(
            fixture.runtime_options.clone(),
            fixture.with_lineage(wrong_identity),
            1,
            Duration::from_secs(2),
        )
        .expect("mismatched attested pool"),
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(4), attach(&wrong_database))
            .await
            .expect("identity refusal is bounded")
            .is_err(),
        "the wrong physical schema OID must never attach"
    );
    drop(wrong_database);

    for marker in [
        "ingress_v1014_pending_claim_reset_v1",
        "legacy_mam_query_frames_v1",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {}.pending_delivery_startup_migrations WHERE name = $1",
            fixture.target
        ))
        .bind(marker)
        .execute(&fixture.admin)
        .await
        .expect("remove marker");
        assert!(
            matches!(attach(&runtime).await, Err(PendingRuntimeAttachError::MissingMarker { marker: actual }) if actual == marker)
        );
        let missing: bool = sqlx::query_scalar(&format!("SELECT NOT EXISTS (SELECT 1 FROM {}.pending_delivery_startup_migrations WHERE name = $1)", fixture.target)).bind(marker).fetch_one(&fixture.admin).await.expect("marker remains absent");
        assert!(missing);
        let held: (Option<String>, Option<i32>, Option<i64>) = sqlx::query_as(&format!("SELECT flushed_in_session, outbound_sequence, claimed_at_ms FROM {}.pending_delivery WHERE row_id = 'row-a'", fixture.target)).fetch_one(&fixture.admin).await.expect("claim survives refusal");
        assert_eq!(
            held,
            (Some("held-session".to_owned()), Some(12), Some(1000))
        );
        sqlx::query(&format!("INSERT INTO {}.pending_delivery_startup_migrations (name, completed_at) VALUES ($1, 1)", fixture.target)).bind(marker).execute(&fixture.admin).await.expect("restore marker");
    }
    admin(
        &fixture,
        &format!(
            "DROP INDEX {}.idx_pending_delivery_archived_unique",
            fixture.target
        ),
    )
    .await;
    admin(&fixture, &format!("CREATE INDEX idx_pending_delivery_archived_unique ON {}.pending_delivery (recipient_jid, archive_stanza_id) WHERE payload_kind = 'archived'", fixture.target)).await;
    assert!(matches!(
        attach(&runtime).await,
        Err(PendingRuntimeAttachError::InvalidIndex {
            index: "idx_pending_delivery_archived_unique"
        })
    ));
    let unique: bool = sqlx::query_scalar(&format!("SELECT i.indisunique FROM pg_catalog.pg_index i WHERE i.indexrelid = '{}.idx_pending_delivery_archived_unique'::regclass", fixture.target)).fetch_one(&fixture.admin).await.expect("inspect index");
    assert!(!unique, "attach did not repair index");
    admin(
        &fixture,
        &format!(
            "DROP INDEX {}.idx_pending_delivery_archived_unique",
            fixture.target
        ),
    )
    .await;
    admin(&fixture, &format!("CREATE UNIQUE INDEX idx_pending_delivery_archived_unique ON {}.pending_delivery (recipient_jid, archive_stanza_id) WHERE payload_kind = 'archived'", fixture.target)).await;
    admin(
        &fixture,
        &format!(
            "DROP INDEX {}.idx_pending_delivery_archived_unique",
            fixture.target
        ),
    )
    .await;
    admin(&fixture, &format!("CREATE UNIQUE INDEX idx_pending_delivery_archived_unique ON {}.pending_delivery (recipient_jid, archive_stanza_id) WHERE payload_kind = 'transient'", fixture.target)).await;
    assert!(matches!(
        attach(&runtime).await,
        Err(PendingRuntimeAttachError::InvalidIndex {
            index: "idx_pending_delivery_archived_unique"
        })
    ));
    let predicate: String = sqlx::query_scalar(&format!("SELECT pg_catalog.pg_get_expr(i.indpred, i.indrelid) FROM pg_catalog.pg_index i WHERE i.indexrelid = '{}.idx_pending_delivery_archived_unique'::regclass", fixture.target)).fetch_one(&fixture.admin).await.expect("inspect changed predicate");
    assert!(
        predicate.contains("transient"),
        "attach did not repair the predicate"
    );
    admin(
        &fixture,
        &format!(
            "DROP INDEX {}.idx_pending_delivery_archived_unique",
            fixture.target
        ),
    )
    .await;
    admin(&fixture, &format!("CREATE UNIQUE INDEX idx_pending_delivery_archived_unique ON {}.pending_delivery (recipient_jid, archive_stanza_id) WHERE payload_kind = 'archived'", fixture.target)).await;
    admin(
        &fixture,
        &format!(
            "DROP INDEX {}.idx_pending_notification_recovery_due",
            fixture.target
        ),
    )
    .await;
    assert!(matches!(
        attach(&runtime).await,
        Err(PendingRuntimeAttachError::InvalidIndex {
            index: "idx_pending_notification_recovery_due"
        })
    ));
    admin(&fixture, &format!("CREATE INDEX idx_pending_notification_recovery_due ON {}.pending_delivery (notification_recovery_ordinal) WHERE payload_kind = 'archived' AND flushed_in_session IS NULL AND notification_outboxed_at_ms IS NULL", fixture.target)).await;
    admin(&fixture, &format!("ALTER TABLE {}.pending_delivery DISABLE TRIGGER pending_notification_recovery_ordinal_immutable", fixture.target)).await;
    assert!(matches!(
        attach(&runtime).await,
        Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "PostgreSQL immutable trigger and function"
        })
    ));
    admin(&fixture, &format!("ALTER TABLE {}.pending_delivery ENABLE TRIGGER pending_notification_recovery_ordinal_immutable", fixture.target)).await;
    admin(&fixture, &format!("CREATE OR REPLACE FUNCTION {}.pending_notification_recovery_ordinal_immutable() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$", fixture.target)).await;
    assert!(matches!(
        attach(&runtime).await,
        Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "PostgreSQL immutable trigger and function"
        })
    ));
    bootstrap_pending_delivery_owner_schema(&owner)
        .await
        .expect("owner restores trigger function");
    let sequence: String = sqlx::query_scalar(
        "SELECT pg_catalog.pg_get_serial_sequence($1, 'notification_recovery_ordinal')",
    )
    .bind(format!("{}.pending_delivery", fixture.target))
    .fetch_one(&fixture.admin)
    .await
    .expect("identity sequence");
    admin(&fixture, &format!("ALTER SEQUENCE {sequence} CACHE 2")).await;
    assert!(matches!(
        attach(&runtime).await,
        Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "PostgreSQL identity sequence"
        })
    ));
    admin(&fixture, &format!("ALTER SEQUENCE {sequence} CACHE 1")).await;
    admin(&fixture, &format!("ALTER TABLE {}.pending_delivery ALTER COLUMN notification_recovery_ordinal SET GENERATED BY DEFAULT", fixture.target)).await;
    assert!(matches!(
        attach(&runtime).await,
        Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "PostgreSQL identity sequence"
        })
    ));
    admin(&fixture, &format!("ALTER TABLE {}.pending_delivery ALTER COLUMN notification_recovery_ordinal SET GENERATED ALWAYS", fixture.target)).await;
    attach(&runtime)
        .await
        .expect("attach after owner restores state");

    let held_main_lane = runtime.begin().await.expect("occupy the only main lane");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), attach(&runtime))
            .await
            .is_err(),
        "attachment waits for main-lane acquisition"
    );
    held_main_lane
        .commit()
        .await
        .expect("release held main lane");
    attach(&runtime)
        .await
        .expect("cancelled acquisition did not strand the lane");

    let strict = waddle_server::test_support::database_from_attested_postgres_pool(
        "strict pending runtime",
        attested_pool(
            fixture
                .runtime_options
                .clone()
                .options([("lock_timeout", "200ms")]),
            config,
            1,
            Duration::from_secs(3),
        )
        .expect("strict attested pool"),
    );
    let mut blocker = fixture.admin.begin().await.expect("owner lock transaction");
    sqlx::query(&format!(
        "LOCK TABLE {}.pending_delivery IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *blocker)
    .await
    .expect("hold owner lock");
    let error = tokio::time::timeout(Duration::from_secs(3), attach(&strict))
        .await
        .expect("strict timeout bound");
    assert!(
        matches!(error, Err(PendingRuntimeAttachError::Relation { source: DatabaseError::Internal(sqlx::Error::Database(error)), .. }) if error.code().as_deref() == Some("55P03"))
    );
    let cancelled = tokio::time::timeout(Duration::from_millis(50), attach(&runtime)).await;
    assert!(
        cancelled.is_err(),
        "owner lock should leave verification in flight"
    );
    // The abandoned PostgreSQL statement may retain its lease until the
    // verifier's five-second lock timeout cancels the server-side query.
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut tx = runtime.begin().await.expect("main lane after cancellation");
        let mut rows = tx.query("SELECT 1::bigint", ()).await.expect("main query");
        assert_eq!(
            rows.next()
                .await
                .expect("row")
                .expect("value")
                .get::<i64>(0)
                .expect("integer"),
            1
        );
        drop(rows);
        tx.commit().await.expect("release main lane");
    })
    .await
    .expect("cancelled attachment releases main lane within the lock bound");
    blocker.rollback().await.expect("release owner lock");
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
async fn pending_runtime_attach_observes_owner_ddl_before_snapshot() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let owner = Database::from_config(
        "pending owner",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            url_with_schema(&fixture.base_url, &fixture.target),
        ),
    )
    .await
    .expect("owner database");
    bootstrap_pending_delivery_owner_schema(&owner)
        .await
        .expect("owner prepares pending store");
    for table in [
        "pending_delivery",
        "pending_delivery_sm_promotions",
        "pending_delivery_startup_migrations",
    ] {
        admin(
            &fixture,
            &format!(
                "GRANT SELECT ON {}.{table} TO {}",
                fixture.target, fixture.runtime
            ),
        )
        .await;
    }
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install attestor");
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "pending runtime",
        attested_pool(
            fixture.runtime_options.clone(),
            config,
            2,
            Duration::from_secs(8),
        )
        .expect("attested pool"),
        1,
        1,
    )
    .await
    .expect("main and ingress lanes");
    let runtime = pool.global().clone();
    attach(&runtime).await.expect("prepared schema attaches");

    // Hold ACCESS EXCLUSIVE until the runtime is waiting on the table. It must
    // inspect the committed identity mode, not a catalog snapshot from before
    // the owner changed it.
    let mut blocker = fixture.admin.begin().await.expect("owner DDL transaction");
    sqlx::query(&format!(
        "ALTER TABLE {}.pending_delivery ALTER COLUMN notification_recovery_ordinal SET GENERATED BY DEFAULT",
        fixture.target
    ))
    .execute(&mut *blocker)
    .await
    .expect("change identity mode under the owner lock");
    let attaching = runtime.clone();
    let pending = tokio::spawn(async move { attach(&attaching).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE datname = $1 AND usename = $2 AND wait_event_type = 'Lock' \
                 AND query LIKE '%pending_delivery%')",
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
            Err(PendingRuntimeAttachError::InvalidRecovery {
                component: "PostgreSQL identity sequence"
            })
        ),
        "attachment must inspect the committed identity mode"
    );
    let identity: String = sqlx::query_scalar(
        "SELECT a.attidentity::text FROM pg_catalog.pg_attribute a \
         JOIN pg_catalog.pg_class t ON t.oid = a.attrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
         WHERE n.nspname = $1 AND t.relname = 'pending_delivery' \
           AND a.attname = 'notification_recovery_ordinal'",
    )
    .bind(&fixture.target)
    .fetch_one(&fixture.admin)
    .await
    .expect("owner DDL remains");
    assert_eq!(identity, "d");
    admin(&fixture, &format!("ALTER TABLE {}.pending_delivery ALTER COLUMN notification_recovery_ordinal SET GENERATED ALWAYS", fixture.target)).await;
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
