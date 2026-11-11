//! Physical PostgreSQL witnesses for the Pending Delivery catalog edge.

use super::{
    attested_pool, install, lose_commit_reply_proxy, postgres_proxy, test_url, url_with_schema,
    Database, DatabaseConfig, DatabaseDriver, Duration, Fixture, CATALOG_GRANT_LOCK,
};
use waddle_server::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY;
use waddle_server::pending_delivery::{
    bootstrap_pending_delivery_owner_schema, DatabasePendingDeliveryStorage,
    PendingRuntimeAttachError,
};
use waddle_xmpp::pending_delivery::storage::PendingStorageError;
use waddle_xmpp::pending_delivery::QuotaPolicy;

const RECEIPT_KEY: i64 = 7_305_784_319_670_198;
const RESET_MARKER: &str = "ingress_v1014_pending_claim_reset_v1";
const RECOVERY_KEY: &str = "pending_notification_recovery_ordinal";

fn owner_config(fixture: &Fixture, application: &str) -> DatabaseConfig {
    let mut url =
        url::Url::parse(&url_with_schema(&fixture.base_url, &fixture.target)).expect("owner URL");
    url.query_pairs_mut()
        .append_pair("application_name", application);
    DatabaseConfig {
        pool_size: 1,
        ..DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string())
    }
}

async fn retrying_backend(fixture: &Fixture, application: &str) -> i32 {
    // One rollback may belong to cleanup's marker read or the attested pool's
    // checkout hook. Require a later, distinct rollback while the lower gate
    // is held, so those preliminary reads cannot satisfy this witness.
    let mut rollbacks = std::collections::HashSet::new();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let observed: Option<(i32, String)> = sqlx::query_as(
                "SELECT pid, query_start::pg_catalog.text FROM pg_catalog.pg_stat_activity WHERE application_name = $1 \
                 AND state = 'idle' AND xact_start IS NULL \
                 AND pg_catalog.starts_with(query, 'ROLLBACK') \
                 ORDER BY query_start DESC LIMIT 1",
            )
            .bind(application)
            .fetch_optional(&fixture.admin)
            .await
            .expect("observe catalog retry");
            if let Some((pid, started)) = observed {
                rollbacks.insert((pid, started));
                if rollbacks.len() >= 2 {
                    break pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("catalog retry acknowledged a rollback")
}

async fn assert_catalog_available_and_no_waiter(fixture: &Fixture, pid: i32) {
    let mut probe = fixture.admin.begin().await.expect("probe transaction");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let available: bool =
                sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)")
                    .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                    .fetch_one(&mut *probe)
                    .await
                    .expect("probe catalog lock");
            if available {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("lower-gate retries released catalog-X");
    let waiting: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid = $1 \
         AND locktype = 'advisory' AND NOT granted)",
    )
    .bind(pid)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect physical advisory waiters");
    assert!(!waiting, "retry leaves no queued advisory waiter");
    probe.rollback().await.expect("release probe");
}

#[tokio::test]
async fn pending_owner_legacy_gates_retry_without_holding_catalog() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let db = Database::from_config(
        "pending catalog owner",
        &owner_config(&fixture, "pending_catalog_owner"),
    )
    .await
    .expect("owner pool");
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("prepare schema");

    for gate in ["receipts", "v1014", "recovery"] {
        let mut blocker = fixture
            .admin
            .begin()
            .await
            .expect("hold old initializer gate");
        match gate {
            "receipts" => {
                sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
                    .bind(RECEIPT_KEY)
                    .execute(&mut *blocker)
                    .await
                    .expect("receipt gate");
            }
            "v1014" => {
                sqlx::query(
                    "SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext($1)::bigint)",
                )
                .bind(RESET_MARKER)
                .execute(&mut *blocker)
                .await
                .expect("reset gate");
            }
            _ => {
                sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext($1), pg_catalog.hashtext($2))")
                    .bind(&fixture.target).bind(RECOVERY_KEY).execute(&mut *blocker).await.expect("recovery gate");
            }
        }
        let running_db = db.clone();
        let bootstrap =
            tokio::spawn(async move { bootstrap_pending_delivery_owner_schema(&running_db).await });
        let pid = retrying_backend(&fixture, "pending_catalog_owner").await;
        assert!(
            !bootstrap.is_finished(),
            "{gate} gate must stop its actual stage"
        );
        assert_catalog_available_and_no_waiter(&fixture, pid).await;
        blocker.rollback().await.expect("release legacy gate");
        tokio::time::timeout(Duration::from_secs(10), bootstrap)
            .await
            .expect("initializer progresses")
            .expect("task")
            .expect("bootstrap succeeds");
    }

    // Observe the base owner's actual backend while a table operation is
    // blocked. The catalog-X lock must be on that backend, not a coordinator.
    let mut table_blocker = fixture.admin.begin().await.expect("table blocker");
    sqlx::query(&format!(
        "LOCK TABLE {}.pending_delivery IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *table_blocker)
    .await
    .expect("hold base relation");
    let running_db = db.clone();
    let base_owner =
        tokio::spawn(async move { bootstrap_pending_delivery_owner_schema(&running_db).await });
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let witnessed: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity a \
                 JOIN pg_catalog.pg_locks relation_wait ON relation_wait.pid = a.pid \
                 JOIN pg_catalog.pg_class c ON c.oid = relation_wait.relation \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_catalog.pg_locks catalog_lock ON catalog_lock.pid = a.pid \
                 WHERE a.application_name = $1 AND a.xact_start IS NOT NULL \
                   AND a.wait_event_type = 'Lock' \
                   AND n.nspname = $2 AND c.relname = 'pending_delivery' \
                   AND relation_wait.locktype = 'relation' AND NOT relation_wait.granted \
                   AND catalog_lock.locktype = 'advisory' \
                   AND catalog_lock.objsubid = 1 AND catalog_lock.mode = 'ExclusiveLock' \
                   AND catalog_lock.granted \
                   AND catalog_lock.classid::bigint = (($3::bigint >> 32) & 4294967295::bigint) \
                   AND catalog_lock.objid::bigint = ($3::bigint & 4294967295::bigint))",
            )
            .bind("pending_catalog_owner")
            .bind(&fixture.target)
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&fixture.admin)
            .await
            .expect("observe base owner backend");
            if witnessed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("base mutation holds catalog-X on its backend");
    table_blocker
        .rollback()
        .await
        .expect("release base relation");
    tokio::time::timeout(Duration::from_secs(10), base_owner)
        .await
        .expect("base owner progresses")
        .expect("base owner task")
        .expect("base owner succeeds");
    drop(db);
    fixture.cleanup().await;
}

#[tokio::test]
async fn pending_owner_pins_persistent_schema_and_reset_is_atomic() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    for sql in [
        format!(
            "CREATE FUNCTION {}.reject_name_eq(pg_catalog.name, pg_catalog.name) RETURNS boolean LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'shadowed name operator'; END $$",
            fixture.target
        ),
        format!(
            "CREATE OPERATOR {}.= (FUNCTION = {}.reject_name_eq, LEFTARG = pg_catalog.name, RIGHTARG = pg_catalog.name)",
            fixture.target, fixture.target
        ),
    ] {
        sqlx::query(&sql)
            .execute(&fixture.admin)
            .await
            .expect("install name-operator shadow");
    }
    let mut config = owner_config(&fixture, "pending_shadow_owner");
    let mut scoped = url::Url::parse(&config.database_url).expect("owner URL");
    let retained = scoped
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    scoped
        .query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair(
            "options",
            &format!("-c search_path={},pg_catalog", fixture.target),
        );
    config.database_url = scoped.to_string();
    let db = Database::from_config("pending shadow owner", &config)
        .await
        .expect("owner pool");
    {
        let conn = db.guard().await.expect("same backend");
        conn.execute("SELECT 'x'::pg_catalog.name = 'x'::pg_catalog.name", ())
            .await
            .expect_err("the caller path selects the hostile name operator");
        conn.execute("CREATE TEMP TABLE pending_delivery (trap TEXT)", ())
            .await
            .expect("temporary shadow");
    }
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("target schema is used despite temporary shadow");
    let target_exists: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass(pg_catalog.format('%I.%I', $1, 'pending_delivery')) IS NOT NULL")
        .bind(&fixture.target).fetch_one(&fixture.admin).await.expect("target table");
    assert!(target_exists);
    {
        let conn = db.guard().await.expect("same pooled backend");
        let mut rows = conn
            .query("SELECT count(*) FROM pg_temp.pending_delivery", ())
            .await
            .expect("temporary shadow remains");
        assert_eq!(
            rows.next()
                .await
                .expect("row")
                .expect("count")
                .get::<i64>(0)
                .expect("count"),
            0
        );
    }
    for sql in [
        format!(
            "DELETE FROM {}.pending_delivery_startup_migrations WHERE name = '{RESET_MARKER}'",
            fixture.target
        ),
        format!(
            "INSERT INTO {}.pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, flushed_in_session, outbound_sequence, claimed_at_ms) VALUES ('claimed', 'a@example.test', 1, 'transient', 'session', 4, 5)",
            fixture.target
        ),
        format!(
            "ALTER TABLE {}.pending_delivery_startup_migrations ADD CONSTRAINT reject_reset_marker CHECK (name <> '{RESET_MARKER}')",
            fixture.target
        ),
    ] {
        sqlx::query(&sql)
            .execute(&fixture.admin)
            .await
            .expect("prepare reset fault");
    }
    let failure = bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect_err("reject reset marker after claim UPDATE");
    assert!(failure.to_string().contains("reject_reset_marker"));
    let state: (Option<String>, Option<i32>, Option<i64>, bool, bool) = sqlx::query_as(&format!(
        "SELECT flushed_in_session, outbound_sequence, claimed_at_ms, \
         EXISTS (SELECT 1 FROM {0}.pending_delivery_startup_migrations WHERE name = $1), \
         pg_catalog.to_regclass('{0}.pending_delivery_sm_promotions') IS NOT NULL \
         FROM {0}.pending_delivery WHERE row_id = 'claimed'",
        fixture.target
    ))
    .bind(RESET_MARKER)
    .fetch_one(&fixture.admin)
    .await
    .expect("failed reset state");
    assert_eq!(
        state,
        (Some("session".into()), Some(4), Some(5), false, true)
    );
    sqlx::query(&format!(
        "ALTER TABLE {}.pending_delivery_startup_migrations DROP CONSTRAINT reject_reset_marker",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove reset fault");
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("retry reset");
    let state: (Option<String>, Option<i32>, Option<i64>, bool) = sqlx::query_as(&format!(
        "SELECT flushed_in_session, outbound_sequence, claimed_at_ms, \
         EXISTS (SELECT 1 FROM {0}.pending_delivery_startup_migrations WHERE name = $1) \
         FROM {0}.pending_delivery WHERE row_id = 'claimed'",
        fixture.target
    ))
    .bind(RESET_MARKER)
    .fetch_one(&fixture.admin)
    .await
    .expect("reset state");
    assert_eq!(state, (None, None, None, true));
    drop(db);
    fixture.cleanup().await;
}

#[tokio::test]
async fn pending_read_attachment_waits_at_catalog_before_relation_locks() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let owner = Database::from_config(
        "pending reader owner",
        &owner_config(&fixture, "pending_reader_owner"),
    )
    .await
    .expect("owner pool");
    bootstrap_pending_delivery_owner_schema(&owner)
        .await
        .expect("owner preparation");
    for table in [
        "pending_delivery",
        "pending_delivery_sm_promotions",
        "pending_delivery_startup_migrations",
    ] {
        sqlx::query(&format!(
            "GRANT SELECT ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("read grant");
    }
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("attestor");
    let runtime = waddle_server::test_support::database_from_attested_postgres_pool(
        "pending catalog reader",
        attested_pool(
            fixture
                .runtime_options
                .clone()
                .application_name("pending_catalog_reader"),
            config,
            1,
            Duration::from_secs(8),
        )
        .expect("attested pool"),
    );
    {
        let connection = runtime.guard().await.expect("prewarm checkout hook");
        connection
            .execute("SELECT 1", ())
            .await
            .expect("run attested checkout hook");
    }
    let mut blocker = fixture.admin.begin().await.expect("catalog blocker");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *blocker)
        .await
        .expect("catalog-X");
    let reader = tokio::spawn(async move {
        DatabasePendingDeliveryStorage::attach_existing(runtime, QuotaPolicy::Unlimited).await
    });
    let pid = retrying_backend(&fixture, "pending_catalog_reader").await;
    assert!(!reader.is_finished(), "reader must wait for catalog-S");
    let relations_locked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks l JOIN pg_catalog.pg_class c ON c.oid = l.relation \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE l.pid = $1 AND n.nspname = $2 \
         AND c.relname IN ('pending_delivery', 'pending_delivery_sm_promotions', 'pending_delivery_startup_migrations'))",
    ).bind(pid).bind(&fixture.target).fetch_one(&fixture.admin).await.expect("inspect exact reader backend");
    assert!(
        !relations_locked,
        "reader has not reached named relation locks"
    );
    blocker.rollback().await.expect("release catalog-X");
    let storage = tokio::time::timeout(Duration::from_secs(10), reader)
        .await
        .expect("reader progresses")
        .expect("task")
        .expect("read-only grants are sufficient");
    // Ordinary inheritance adds rows without inheriting the parent's PK or
    // identity constraint. A duplicate child row must invalidate attachment
    // even when the runtime role can read the child.
    for statement in [
        format!(
            "INSERT INTO {}.pending_delivery \
             (row_id, recipient_jid, original_receipt_at, payload_kind) \
             VALUES ('duplicate', 'u@example.com', 1, 'transient')",
            fixture.target
        ),
        format!(
            "CREATE TABLE {}.pending_delivery_child () INHERITS ({}.pending_delivery)",
            fixture.target, fixture.target
        ),
        format!(
            "INSERT INTO {}.pending_delivery_child \
             (row_id, recipient_jid, original_receipt_at, payload_kind, notification_recovery_ordinal) \
             VALUES ('duplicate', 'u@example.com', 2, 'transient', 1000)",
            fixture.target
        ),
        format!(
            "GRANT SELECT ON {}.pending_delivery_child TO {}",
            fixture.target, fixture.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .expect("prepare inherited Pending Delivery row");
    }
    let error = match DatabasePendingDeliveryStorage::attach_existing(
        storage.database(),
        QuotaPolicy::Unlimited,
    )
    .await
    {
        Ok(_) => panic!("attachment accepted an unchecked inherited child"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("inherit"));
    sqlx::query(&format!(
        "DROP TABLE {}.pending_delivery_child",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove inherited child");

    sqlx::query(&format!(
        "ALTER TABLE ONLY {}.pending_delivery SET UNLOGGED",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("make pending table crash-volatile");
    assert!(matches!(
        DatabasePendingDeliveryStorage::attach_existing(
            storage.database(),
            QuotaPolicy::Unlimited,
        )
        .await,
        Err(PendingRuntimeAttachError::NonPermanentTable {
            table: "pending_delivery"
        })
    ));
    sqlx::query(&format!(
        "ALTER TABLE ONLY {}.pending_delivery SET LOGGED",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore durable pending table");

    let identity_sequence: String = sqlx::query_scalar(
        "SELECT pg_catalog.pg_get_serial_sequence( \
         pg_catalog.format('%I.%I', $1, 'pending_delivery'), \
         'notification_recovery_ordinal')",
    )
    .bind(&fixture.target)
    .fetch_one(&fixture.admin)
    .await
    .expect("find Pending Delivery identity sequence");
    sqlx::query(&format!("ALTER SEQUENCE {identity_sequence} SET UNLOGGED"))
        .execute(&fixture.admin)
        .await
        .expect("make recovery ordinal allocator crash-volatile");
    assert!(matches!(
        DatabasePendingDeliveryStorage::attach_existing(
            storage.database(),
            QuotaPolicy::Unlimited,
        )
        .await,
        Err(PendingRuntimeAttachError::InvalidRecovery { .. })
    ));
    sqlx::query(&format!("ALTER SEQUENCE {identity_sequence} SET LOGGED"))
        .execute(&fixture.admin)
        .await
        .expect("restore durable recovery ordinal allocator");

    // This view has the expected projected columns but an immutable function
    // that raises during planning. The verifier must reject relkind before it
    // issues its `WHERE 1 = 0` shape projection.
    for statement in [
        format!(
            "DROP TABLE {}.pending_delivery_sm_promotions",
            fixture.target
        ),
        format!(
            "CREATE FUNCTION {}.hostile_pending_projection() RETURNS text \
             LANGUAGE plpgsql IMMUTABLE AS $$ BEGIN RAISE EXCEPTION 'hostile pending view executed'; END $$",
            fixture.target
        ),
        format!(
            "CREATE VIEW {}.pending_delivery_sm_promotions AS \
             SELECT {}.hostile_pending_projection() AS origin_stream_id, \
                    1::bigint AS sequence, 0::bigint AS created_at_ms",
            fixture.target, fixture.target
        ),
        format!(
            "GRANT SELECT ON {}.pending_delivery_sm_promotions TO {}",
            fixture.target, fixture.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .expect("prepare hostile pending view");
    }
    let direct_error = sqlx::query(&format!(
        "SELECT origin_stream_id, sequence, created_at_ms \
         FROM {}.pending_delivery_sm_promotions WHERE 1 = 0",
        fixture.target
    ))
    .fetch_all(&fixture.admin)
    .await
    .expect_err("hostile view projection executes during planning");
    assert!(direct_error
        .to_string()
        .contains("hostile pending view executed"));
    assert!(matches!(
        DatabasePendingDeliveryStorage::attach_existing(
            storage.database(),
            QuotaPolicy::Unlimited,
        )
        .await,
        Err(PendingRuntimeAttachError::InvalidTable {
            table: "pending_delivery_sm_promotions"
        })
    ));
    drop(storage);
    drop(owner);
    fixture.cleanup().await;
}

#[tokio::test]
async fn pending_owner_lost_commit_ack_is_typed_and_discards_the_backend() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let held = std::sync::Arc::new(tokio::sync::Notify::new());
    let disconnect = std::sync::Arc::new(tokio::sync::Notify::new());
    let proxy = tokio::spawn(lose_commit_reply_proxy(
        listener,
        fixture.admin_options.clone(),
        held.clone(),
        disconnect.clone(),
    ));
    let mut url =
        postgres_proxy::local_proxy_url(&url_with_schema(&fixture.base_url, &fixture.target), port)
            .expect("local PostgreSQL proxy URL");
    url.query_pairs_mut()
        .append_pair("application_name", "pending_lost_commit");
    // The shared proxy serves one PostgreSQL connection. Keep the one-slot
    // pool alive across the failure so backend disappearance proves checkout
    // discard rather than mere pool destruction.
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string());
    config.pool_size = 1;
    let db = Database::from_config("pending owner lost commit", &config)
        .await
        .expect("one-slot proxied owner");
    let running_db = db.clone();
    let mut owner =
        tokio::spawn(async move { bootstrap_pending_delivery_owner_schema(&running_db).await });
    tokio::select! {
        _ = held.notified() => {},
        result = &mut owner => match result {
            Ok(Ok(_)) => panic!("owner succeeded before proxy observed base COMMIT"),
            Ok(Err(error)) => panic!("owner failed before base COMMIT: {error}"),
            Err(error) => panic!("owner task ended before base COMMIT: {error}"),
        },
        _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("base COMMIT acknowledgement not observed"),
    }
    let pid: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name = 'pending_lost_commit'",
    )
    .fetch_one(&fixture.admin)
    .await
    .expect("original backend");
    let phase_state: (bool, bool) = sqlx::query_as(
        "SELECT pg_catalog.to_regclass(pg_catalog.format('%I.%I', $1, 'pending_delivery')) IS NOT NULL, \
                pg_catalog.to_regclass(pg_catalog.format('%I.%I', $1, 'pending_delivery_sm_promotions')) IS NOT NULL",
    )
    .bind(&fixture.target)
    .fetch_one(&fixture.admin)
    .await
    .expect("independently observe committed base and absent later phase");
    assert_eq!(phase_state, (true, false));
    // Keep the processed COMMIT reply withheld. The owner must hit its own
    // bounded commit wait and close the lease; the proxy should then observe
    // client EOF without being told to disconnect.
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(40), owner)
            .await
            .expect("owner commit wait is bounded")
            .expect("owner task"),
        Err(PendingStorageError::OwnerBootstrapCommitIndeterminate { stage: "base" })
    ));
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .expect("discarded checkout closes the proxy client")
        .expect("proxy task")
        .expect("proxy I/O");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let gone: bool = sqlx::query_scalar(
                "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
            )
            .bind(pid)
            .fetch_one(&fixture.admin)
            .await
            .expect("observe original backend");
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("unconfirmed checkout was discarded");
    drop(db);
    fixture.cleanup().await;
}
