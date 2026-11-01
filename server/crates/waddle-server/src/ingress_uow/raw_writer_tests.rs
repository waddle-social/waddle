use super::effect_descendants::begin_raw_ingress_writer_with_expected_pg_schema;
use super::{begin_raw_ingress_writer, CanonicalMessageRepository, IngressLineage};
use crate::db::lineage::PgSchemaIdentity;
use crate::db::{Database, DatabaseConfig, DatabaseDriver};
use crate::ingress::test_support::IngressFixture;
use crate::ingress_substrate::{
    self, EffectReceiptKind, IngressSubstrateError, PostgresIngressSubstrate,
};
use waddle_xmpp::ingress::{MessageKey, SemanticDigest};

async fn attested_pg_schema(fixture: &IngressFixture) -> PgSchemaIdentity {
    let transaction = fixture.uow.begin().await.expect("attest fixture schema");
    let schema = match transaction.lineage() {
        IngressLineage::Attested(lineage) => lineage
            .postgres_identity
            .as_ref()
            .expect("Postgres lineage identity")
            .schema
            .clone(),
        IngressLineage::Ephemeral => panic!("Postgres fixture must be durable"),
    };
    transaction
        .commit()
        .await
        .expect("close schema attestation transaction");
    schema
}

fn qualified_epoch_relation(schema: &PgSchemaIdentity) -> String {
    format!(
        "\"{}\".ingress_protocol_epoch",
        schema.name.0.replace('"', "\"\"")
    )
}

async fn wait_for_blocking_pid(observer: &sqlx::PgPool, waiter: i32, blocker: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let blocked: bool =
                sqlx::query_scalar("SELECT $1 = ANY(pg_catalog.pg_blocking_pids($2))")
                    .bind(blocker)
                    .bind(waiter)
                    .fetch_one(observer)
                    .await
                    .expect("observe transaction blocker");
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("identify expected row-lock wait");
}

#[tokio::test]
async fn postgres_raw_writer_waits_for_current_epoch_before_application_locks() {
    let Some(fixture) = IngressFixture::postgres("raw_writer_epoch").await else {
        return;
    };
    fixture
        .execute("CREATE TABLE raw_writer_probe (id integer PRIMARY KEY)", ())
        .await;
    fixture
        .execute("INSERT INTO raw_writer_probe VALUES (1)", ())
        .await;
    let application = format!("raw-writer-{}", uuid::Uuid::new_v4());
    let pool_name = application.clone();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |connection, _| {
            let application = pool_name.clone();
            Box::pin(async move {
                sqlx::query(
                    "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ",
                )
                .execute(&mut *connection)
                .await?;
                sqlx::query("SELECT set_config('application_name', $1, false)")
                    .bind(application)
                    .execute(&mut *connection)
                    .await?;
                Ok(())
            })
        })
        .connect(fixture.db.database_url())
        .await
        .expect("repeatable-read pool");
    let observer = sqlx::PgPool::connect(fixture.db.database_url())
        .await
        .expect("observer");
    let db = Database::from_postgres_pool("raw-writer-test", pool.clone());
    let mut transition = fixture.db.begin().await.expect("epoch transition");
    let mut rows = transition
        .query("SELECT pg_backend_pid()", ())
        .await
        .expect("transition PID");
    let blocker: i32 = rows
        .next()
        .await
        .expect("PID query")
        .expect("PID row")
        .get(0)
        .expect("PID");
    drop(rows);
    transition.execute(
        "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = ?::uuid WHERE id = 1",
        crate::db_params![uuid::Uuid::new_v4().to_string()],
    ).await.expect("hold uncommitted epoch transition");
    let writer = tokio::spawn(async move {
        let mut tx = begin_raw_ingress_writer(&db)
            .await
            .expect("prepare raw writer");
        let mut rows = tx.query("SELECT current_setting('transaction_isolation'), current_setting('waddle.protocol_epoch'), current_setting('lock_timeout'), current_setting('statement_timeout')", ()).await.expect("writer settings");
        let row = rows.next().await.expect("settings row").expect("settings");
        let isolation: String = row.get(0).expect("isolation");
        let epoch: String = row.get(1).expect("epoch proof");
        let lock_timeout: String = row.get(2).expect("lock timeout");
        let statement_timeout: String = row.get(3).expect("statement timeout");
        assert_eq!(lock_timeout, "100ms");
        assert_eq!(statement_timeout, "250ms");
        drop(rows);
        tx.execute("UPDATE raw_writer_probe SET id = id WHERE id = 1", ())
            .await
            .expect("application write");
        tx.execute(
            "UPDATE ingress_effect_descendants SET settled_at = settled_at WHERE false",
            (),
        )
        .await
        .expect("guard accepts current epoch proof");
        tx.commit().await.expect("writer commit");
        (isolation, epoch)
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name = $1 AND $2 = ANY(pg_blocking_pids(pid)) AND wait_event_type = 'Lock' AND query LIKE '%SELECT epoch FROM ingress_protocol_epoch%')",
            ).bind(&application).bind(blocker).fetch_one(&observer).await.expect("observe epoch waiter");
            if blocked { break; }
            assert!(!writer.is_finished(), "writer finished before waiting on the epoch");
            tokio::task::yield_now().await;
        }
    }).await.expect("identify epoch waiter");
    let mut probe = fixture.db.begin().await.expect("application-lock probe");
    probe
        .query(
            "SELECT id FROM raw_writer_probe WHERE id = 1 FOR UPDATE NOWAIT",
            (),
        )
        .await
        .expect("writer has not acquired application lock");
    probe.commit().await.expect("release application probe");
    transition.commit().await.expect("commit epoch transition");
    let (isolation, epoch) = writer.await.expect("writer task");
    assert_eq!(isolation, "read committed");
    assert_eq!(epoch, "1");
    let default: String = sqlx::query_scalar("SHOW default_transaction_isolation")
        .fetch_one(&pool)
        .await
        .expect("session default");
    assert_eq!(default, "repeatable read");
    observer.close().await;
    pool.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres_raw_substrate_rejects_unsupported_epoch_even_with_valid_gucs() {
    let Some(fixture) = IngressFixture::postgres("raw_writer_future_epoch").await else {
        return;
    };
    for epoch in [1_i64, 2] {
        fixture.execute(
            "UPDATE ingress_protocol_epoch SET epoch = ?, activated_at = now(), lineage_uuid = ?::uuid WHERE id = 1",
            crate::db_params![epoch, uuid::Uuid::new_v4().to_string()],
        ).await;
    }
    let store = PostgresIngressSubstrate::open(fixture.db.clone()).expect("substrate");
    assert!(matches!(
        store.begin().await,
        Err(IngressSubstrateError::UnsupportedLiveEpoch)
    ));
    let mut tx = fixture.db.begin().await.expect("raw transaction");
    tx.execute("SELECT set_config('waddle.protocol_epoch', '2', true), set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)", ()).await.expect("valid epoch and xid GUCs");
    tx.execute(
        "UPDATE ingress_effect_descendants SET settled_at = settled_at WHERE false",
        (),
    )
    .await
    .expect("guard accepts supplied proof");
    let key = MessageKey::new();
    let digest = SemanticDigest::from_storage(1, [7; 32]).expect("digest");
    assert!(matches!(
        ingress_substrate::record_message(&mut tx, key, &digest, None).await,
        Err(IngressSubstrateError::UnsupportedLiveEpoch)
    ));
    assert!(matches!(
        ingress_substrate::record_receipt(
            &mut tx,
            key,
            EffectReceiptKind::from_storage(1),
            &[1; 32]
        )
        .await,
        Err(IngressSubstrateError::UnsupportedLiveEpoch)
    ));
    tx.rollback().await.expect("rollback future epoch probe");
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_raw_writer_preserves_standalone_store() {
    let db = Database::in_memory("raw-writer-standalone")
        .await
        .expect("SQLite");
    let mut tx = begin_raw_ingress_writer(&db)
        .await
        .expect("standalone writer");
    tx.execute("CREATE TABLE raw_writer_probe (id integer)", ())
        .await
        .expect("create probe");
    tx.execute("INSERT INTO raw_writer_probe VALUES (1)", ())
        .await
        .expect("standalone write");
    tx.commit().await.expect("standalone commit");
}

#[tokio::test]
async fn postgres_raw_writer_preserves_standalone_store_without_foundation() {
    let Some(fixture) = IngressFixture::postgres("raw_writer_standalone").await else {
        return;
    };
    let schema = format!("raw_writer_standalone_{}", uuid::Uuid::new_v4().simple());
    fixture
        .execute(&format!("CREATE SCHEMA {schema}"), ())
        .await;
    let mut url = url::Url::parse(fixture.db.database_url()).expect("database URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair(
            "options",
            &format!("-c search_path={schema} -c lock_timeout=5s -c statement_timeout=3s -c default_transaction_isolation=repeatable\\ read"),
        );
    let config = DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string());
    let db = Database::from_config("raw-writer-standalone", &config)
        .await
        .expect("standalone pool");
    let mut tx = begin_raw_ingress_writer(&db)
        .await
        .expect("foundation-free writer");
    let mut settings = tx
        .query(
            "SELECT current_setting('lock_timeout'), current_setting('statement_timeout'), current_setting('transaction_isolation'), current_setting('default_transaction_isolation')",
            (),
        )
        .await
        .expect("standalone settings");
    let row = settings
        .next()
        .await
        .expect("settings query")
        .expect("settings row");
    assert_eq!(row.get::<String>(0).expect("lock timeout"), "5s");
    assert_eq!(row.get::<String>(1).expect("statement timeout"), "3s");
    assert_eq!(
        row.get::<String>(2).expect("transaction isolation"),
        "read committed"
    );
    assert_eq!(
        row.get::<String>(3).expect("session default"),
        "repeatable read"
    );
    drop(settings);
    tx.execute("CREATE TABLE raw_writer_probe (id integer)", ())
        .await
        .expect("create probe");
    tx.execute("INSERT INTO raw_writer_probe VALUES (1)", ())
        .await
        .expect("standalone write");
    tx.commit().await.expect("standalone commit");
    drop(db);
    fixture
        .execute(&format!("DROP SCHEMA {schema} CASCADE"), ())
        .await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres_legacy_prune_and_push_entries_wait_before_scheduler_locks() {
    #[derive(Clone, Copy)]
    enum Entry {
        Legacy,
        Prune,
        PushRetirement,
        Substrate,
    }
    for entry in [
        Entry::Legacy,
        Entry::Prune,
        Entry::PushRetirement,
        Entry::Substrate,
    ] {
        let Some(fixture) = IngressFixture::postgres("raw_writer_entries").await else {
            return;
        };
        let application = format!("raw-entry-{}", uuid::Uuid::new_v4());
        let pool_name = application.clone();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .after_connect(move |connection, _| {
                let application = pool_name.clone();
                Box::pin(async move {
                    sqlx::query("SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ").execute(&mut *connection).await?;
                    sqlx::query("SELECT set_config('application_name', $1, false)").bind(application).execute(&mut *connection).await?;
                    Ok(())
                })
            }).connect(fixture.db.database_url()).await.expect("repeatable-read entry pool");
        let db = Database::from_postgres_pool("raw-entry-test", pool.clone());
        let outbox = crate::notification_outbox::NotificationOutboxStore::try_new(db.clone())
            .await
            .expect("outbox schema");
        let push = crate::push_service::DatabasePushServiceStore::try_new(db.clone())
            .await
            .expect("push schema");
        let owner: jid::BareJid = "raw-entry@example.com".parse().expect("owner");
        fixture.execute("INSERT INTO push_owner_locks (owner_bare_jid, updated_at_ms) VALUES (?, 0) ON CONFLICT DO NOTHING", crate::db_params![owner.to_string()]).await;
        let observer = sqlx::PgPool::connect(fixture.db.database_url())
            .await
            .expect("wait observer");
        let mut transition = fixture.db.begin().await.expect("epoch transition");
        let mut rows = transition
            .query("SELECT pg_backend_pid()", ())
            .await
            .expect("transition PID");
        let blocker: i32 = rows
            .next()
            .await
            .expect("PID query")
            .expect("PID row")
            .get(0)
            .expect("PID");
        drop(rows);
        transition.execute("UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = ?::uuid WHERE id = 1", crate::db_params![uuid::Uuid::new_v4().to_string()]).await.expect("hold epoch transition");
        let writer = tokio::spawn({
            let owner = owner.clone();
            async move {
                match entry {
                    Entry::Legacy => outbox
                        .adopt_legacy_ancestry()
                        .await
                        .expect("legacy adoption"),
                    Entry::Prune => {
                        outbox
                            .prune_completed_before(crate::time::now_ms(), 8)
                            .await
                            .expect("outbox prune");
                    }
                    Entry::PushRetirement => {
                        push.disable_nodes_for_owner(&owner, None)
                            .await
                            .expect("push retirement");
                    }
                    Entry::Substrate => {
                        let store = PostgresIngressSubstrate::open(db).expect("substrate");
                        let mut tx = store.begin().await.expect("substrate transaction");
                        let mut rows = tx
                            .query("SHOW transaction_isolation", ())
                            .await
                            .expect("isolation");
                        let actual: String = rows
                            .next()
                            .await
                            .expect("isolation query")
                            .expect("isolation row")
                            .get(0)
                            .expect("isolation");
                        assert_eq!(actual, "read committed");
                        drop(rows);
                        store
                            .record_message(
                                &mut tx,
                                MessageKey::new(),
                                &SemanticDigest::from_storage(1, [9; 32]).expect("digest"),
                                None,
                            )
                            .await
                            .expect("epoch-one proof authorizes substrate write");
                        tx.commit().await.expect("substrate commit");
                    }
                }
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name = $1 AND $2 = ANY(pg_blocking_pids(pid)) AND wait_event_type = 'Lock' AND query LIKE '%SELECT epoch FROM ingress_protocol_epoch%')")
                    .bind(&application).bind(blocker).fetch_one(&observer).await.expect("observe entry waiter");
                if blocked { break; }
                assert!(!writer.is_finished(), "entry finished before waiting on epoch");
                tokio::task::yield_now().await;
            }
        }).await.expect("identify entry epoch waiter");
        // In particular, retirement must not take its owner lock first.
        let mut probe = fixture.db.begin().await.expect("owner lock probe");
        probe.query("SELECT owner_bare_jid FROM push_owner_locks WHERE owner_bare_jid = ? FOR UPDATE NOWAIT", crate::db_params![owner.to_string()]).await.expect("owner remains unlocked while epoch waits");
        probe.commit().await.expect("release owner probe");
        transition.commit().await.expect("commit epoch transition");
        writer.await.expect("entry task");
        observer.close().await;
        pool.close().await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn postgres_raw_writer_completes_with_queued_epoch_transition() {
    let Some(fixture) = IngressFixture::postgres("raw_writer_queued_epoch").await else {
        return;
    };
    let observer = sqlx::PgPool::connect(fixture.db.database_url())
        .await
        .expect("observer");
    let application = format!("raw-transition-{}", uuid::Uuid::new_v4());
    let db = fixture.db.clone();
    let mut writer = begin_raw_ingress_writer(&db)
        .await
        .expect("prepare epoch-zero writer");
    let mut rows = writer
        .query("SELECT pg_backend_pid()", ())
        .await
        .expect("writer PID");
    let blocker: i32 = rows
        .next()
        .await
        .expect("PID query")
        .expect("PID row")
        .get(0)
        .expect("PID");
    drop(rows);
    let transition = tokio::spawn({
        let url = fixture.db.database_url().to_owned();
        let application = application.clone();
        async move {
            use sqlx::Connection;
            let mut connection = sqlx::PgConnection::connect(&url)
                .await
                .expect("transition connection");
            sqlx::query("SELECT set_config('application_name', $1, false)")
                .bind(application)
                .execute(&mut connection)
                .await
                .expect("transition name");
            sqlx::query("UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = $1 WHERE id = 1").bind(uuid::Uuid::new_v4()).execute(&mut connection).await.expect("epoch advances after writer");
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name = $1 AND $2 = ANY(pg_blocking_pids(pid)) AND wait_event_type = 'Lock' AND query LIKE '%UPDATE ingress_protocol_epoch%')")
                .bind(&application).bind(blocker).fetch_one(&observer).await.expect("observe transition waiter");
            if blocked { break; }
            assert!(!transition.is_finished(), "transition finished before waiting for writer");
            tokio::task::yield_now().await;
        }
    }).await.expect("identify waiting transition");
    ingress_substrate::record_message(
        &mut writer,
        MessageKey::new(),
        &SemanticDigest::from_storage(1, [8; 32]).expect("digest"),
        None,
    )
    .await
    .expect("writer reuses epoch authority despite queued transition");
    writer.commit().await.expect("writer commit");
    transition.await.expect("transition task");
    observer.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres_substrate_begin_bounds_epoch_wait() {
    let Some(fixture) = IngressFixture::postgres("substrate_epoch_timeout").await else {
        return;
    };
    let application = format!("substrate-timeout-{}", uuid::Uuid::new_v4());
    let pool_name = application.clone();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |connection, _| {
            let application = pool_name.clone();
            Box::pin(async move {
                sqlx::query("SELECT set_config('application_name', $1, false)")
                    .bind(application)
                    .execute(&mut *connection)
                    .await?;
                Ok(())
            })
        })
        .connect(fixture.db.database_url())
        .await
        .expect("writer pool");
    let db = Database::from_postgres_pool("substrate-timeout", pool.clone());
    let observer = sqlx::PgPool::connect(fixture.db.database_url())
        .await
        .expect("observer");
    let mut transition = fixture.db.begin().await.expect("epoch transition");
    let mut rows = transition
        .query("SELECT pg_backend_pid()", ())
        .await
        .expect("transition PID");
    let blocker: i32 = rows
        .next()
        .await
        .expect("PID query")
        .expect("PID row")
        .get(0)
        .expect("PID");
    drop(rows);
    transition.execute("UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = ?::uuid WHERE id = 1", crate::db_params![uuid::Uuid::new_v4().to_string()]).await.expect("hold epoch transition");
    let writer = tokio::spawn(async move {
        let store = PostgresIngressSubstrate::open(db).expect("substrate");
        let result = store.begin().await;
        matches!(result, Err(IngressSubstrateError::Timeout))
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name = $1 AND $2 = ANY(pg_blocking_pids(pid)) AND wait_event_type = 'Lock' AND query LIKE '%SELECT epoch FROM ingress_protocol_epoch%')")
                .bind(&application).bind(blocker).fetch_one(&observer).await.expect("observe substrate waiter");
            if blocked { break; }
            assert!(!writer.is_finished(), "substrate finished before waiting on epoch");
            tokio::task::yield_now().await;
        }
    }).await.expect("identify epoch waiter");
    let timed_out = tokio::time::timeout(std::time::Duration::from_secs(2), writer)
        .await
        .expect("bounded wait")
        .expect("writer task");
    assert!(
        timed_out,
        "blocked substrate begin returns its typed timeout"
    );
    assert_eq!(fixture.count("ingress_messages").await, 0);
    transition.rollback().await.expect("release transition");
    observer.close().await;
    pool.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres_schema_bound_uow_retains_v1009_row_share_at_epoch_zero() {
    let Some(fixture) = IngressFixture::postgres("schema_bound_v1009_epoch_zero").await else {
        return;
    };
    let expected = attested_pg_schema(&fixture).await;
    let uow = fixture
        .uow
        .clone()
        .with_expected_pg_schema(expected.clone())
        .expect("Postgres schema capability");
    let mut writer = uow.begin().await.expect("schema-bound epoch-zero UoW");
    let relation = qualified_epoch_relation(&expected);
    let mut locks = writer
        .transaction_mut()
        .query(
            "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE pid = pg_backend_pid() AND relation = pg_catalog.to_regclass(?) AND mode = 'AccessShareLock' AND granted), EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE pid = pg_backend_pid() AND relation = pg_catalog.to_regclass(?) AND mode = 'RowShareLock' AND granted)",
            crate::db_params![relation.clone(), relation],
        )
        .await
        .expect("inspect epoch locks");
    let row = locks.next().await.expect("lock row").expect("lock result");
    assert!(row.get::<bool>(0).expect("relation prelock"));
    assert!(row.get::<bool>(1).expect("V1009 row-lock compatibility"));
    drop(locks);

    CanonicalMessageRepository::record_message(
        &mut writer,
        MessageKey::new(),
        &SemanticDigest::from_storage(1, [7; 32]).expect("digest"),
        None,
    )
    .await
    .expect("actual canonical writer passes V1009 at epoch zero");

    let mut writer_pid_rows = writer
        .transaction_mut()
        .query("SELECT pg_backend_pid()", ())
        .await
        .expect("writer PID");
    let writer_pid: i32 = writer_pid_rows
        .next()
        .await
        .expect("writer PID row")
        .expect("writer PID result")
        .get(0)
        .expect("writer backend PID");
    drop(writer_pid_rows);

    let observer = sqlx::PgPool::connect(fixture.db.database_url())
        .await
        .expect("lock observer");
    let owner_db = fixture.db.clone();
    let (owner_pid_tx, owner_pid_rx) = tokio::sync::oneshot::channel();
    let owner = tokio::spawn(async move {
        let mut transaction = owner_db.begin().await.expect("owner transaction");
        transaction
            .execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
            .await
            .expect("owner isolation");
        let mut pid_rows = transaction
            .query("SELECT pg_backend_pid()", ())
            .await
            .expect("owner PID");
        let owner_pid: i32 = pid_rows
            .next()
            .await
            .expect("owner PID row")
            .expect("owner PID result")
            .get(0)
            .expect("owner backend PID");
        drop(pid_rows);
        owner_pid_tx.send(owner_pid).expect("return owner PID");
        transaction
            .execute(
                "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = ?::uuid WHERE id = 1",
                crate::db_params![uuid::Uuid::new_v4().to_string()],
            )
            .await
            .expect("epoch update waits on retained V1009 row lock");
        transaction
            .rollback()
            .await
            .expect("roll back test transition");
    });
    let owner_pid = owner_pid_rx.await.expect("owner backend PID");
    wait_for_blocking_pid(&observer, owner_pid, writer_pid).await;

    writer.commit().await.expect("commit epoch-zero writer");
    owner.await.expect("join owner transition");
    let epoch: i64 = sqlx::query_scalar("SELECT epoch FROM ingress_protocol_epoch WHERE id = 1")
        .fetch_one(&observer)
        .await
        .expect("read epoch after rolled-back transition");
    assert_eq!(
        epoch, 0,
        "the disposable compatibility probe did not flip the epoch"
    );
    observer.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres_schema_bound_uow_writes_through_relation_guard_candidate() {
    let Some(fixture) = IngressFixture::postgres("schema_bound_candidate_uow").await else {
        return;
    };
    let expected = attested_pg_schema(&fixture).await;
    let admin = sqlx::PgPool::connect(fixture.db.database_url())
        .await
        .expect("candidate installer connection");
    sqlx::raw_sql(include_str!(
        "../../tests/fixtures/ingress_epoch_relation_guard_candidate.sql"
    ))
    .execute(&admin)
    .await
    .expect("install unregistered candidate in disposable schema");

    let uow = fixture
        .uow
        .clone()
        .with_expected_pg_schema(expected.clone())
        .expect("Postgres schema capability");
    let mut writer = uow.begin().await.expect("candidate-bound UoW");
    CanonicalMessageRepository::record_message(
        &mut writer,
        MessageKey::new(),
        &SemanticDigest::from_storage(1, [8; 32]).expect("digest"),
        None,
    )
    .await
    .expect("actual Waddle UoW satisfies candidate prelock and proof");

    let relation = qualified_epoch_relation(&expected);
    let mut locks = writer
        .transaction_mut()
        .query(
            "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE pid = pg_backend_pid() AND relation = pg_catalog.to_regclass(?) AND mode = 'AccessShareLock' AND granted), EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE pid = pg_backend_pid() AND relation = pg_catalog.to_regclass(?) AND mode = 'RowShareLock' AND granted)",
            crate::db_params![relation.clone(), relation],
        )
        .await
        .expect("inspect candidate-path locks");
    let row = locks.next().await.expect("lock row").expect("lock result");
    assert!(row.get::<bool>(0).expect("candidate relation prelock"));
    assert!(row.get::<bool>(1).expect("retained V1009 row lock"));
    drop(locks);
    writer
        .commit()
        .await
        .expect("commit candidate-bound writer");
    admin.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres_schema_bound_substrate_begin_writes_through_relation_guard_candidate() {
    let Some(fixture) = IngressFixture::postgres("schema_bound_substrate_candidate").await else {
        return;
    };
    let expected = attested_pg_schema(&fixture).await;
    let admin = sqlx::PgPool::connect(fixture.db.database_url())
        .await
        .expect("candidate installer connection");
    sqlx::raw_sql(include_str!(
        "../../tests/fixtures/ingress_epoch_relation_guard_candidate.sql"
    ))
    .execute(&admin)
    .await
    .expect("install unregistered candidate in disposable schema");

    let substrate = PostgresIngressSubstrate::open(fixture.db.clone()).expect("open substrate");
    let mut writer = substrate
        .begin_with_expected_pg_schema(&expected)
        .await
        .expect("begin schema-bound substrate transaction");
    let key = uuid::Uuid::new_v4();
    writer
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, 1, ?)",
            crate::db_params![key.to_string(), vec![10_u8; 32]],
        )
        .await
        .expect("substrate candidate write uses the bound relation and XID proof");
    writer
        .commit()
        .await
        .expect("commit substrate candidate write");

    let actual_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ingress_messages WHERE message_key = $1")
            .bind(key)
            .fetch_one(&admin)
            .await
            .expect("read substrate candidate write");
    assert_eq!(actual_count, 1);
    admin.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres_schema_bound_candidate_rejects_old_writer_and_wrong_proof() {
    let Some(fixture) = IngressFixture::postgres("schema_bound_candidate_negative").await else {
        return;
    };
    let expected = attested_pg_schema(&fixture).await;
    let admin = sqlx::PgPool::connect(fixture.db.database_url())
        .await
        .expect("candidate installer connection");
    sqlx::raw_sql(include_str!(
        "../../tests/fixtures/ingress_epoch_relation_guard_candidate.sql"
    ))
    .execute(&admin)
    .await
    .expect("install unregistered candidate in disposable schema");

    // The existing writer seam takes only V1009's row lock. The candidate
    // refuses the actual guarded write because no schema relation prelock was
    // acquired before the caller reached application DML.
    let mut old_writer = begin_raw_ingress_writer(&fixture.db)
        .await
        .expect("begin V1009-compatible legacy writer");
    let old_writer_error = old_writer
        .execute(
            "DELETE FROM ingress_messages WHERE message_key = ?::uuid",
            crate::db_params![uuid::Uuid::new_v4().to_string()],
        )
        .await
        .expect_err("candidate must reject a legacy writer without ACCESS SHARE");
    assert!(
        old_writer_error
            .to_string()
            .contains("require an epoch ACCESS SHARE prelock"),
        "{old_writer_error}"
    );
    old_writer
        .rollback()
        .await
        .expect("release rejected writer");

    // The capability-backed writer acquires the schema relation first, but a
    // stale epoch proof still fails closed at the candidate guard.
    let mut writer = begin_raw_ingress_writer_with_expected_pg_schema(&fixture.db, &expected)
        .await
        .expect("begin schema-bound writer");
    writer
        .execute("SELECT set_config('waddle.protocol_epoch', '1', true)", ())
        .await
        .expect("replace proof with wrong epoch");
    let proof_error = writer
        .execute(
            "DELETE FROM ingress_messages WHERE message_key = ?::uuid",
            crate::db_params![uuid::Uuid::new_v4().to_string()],
        )
        .await
        .expect_err("candidate must reject a wrong epoch proof");
    assert!(
        proof_error
            .to_string()
            .contains("transaction-local epoch proof"),
        "{proof_error}"
    );
    writer.rollback().await.expect("release rejected proof");

    admin.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres_schema_bound_raw_writer_ignores_hostile_path_and_rejects_schema_oid_drift() {
    let Some(fixture) = IngressFixture::postgres("schema_bound_hostile_path").await else {
        return;
    };
    let expected = attested_pg_schema(&fixture).await;
    let hostile = format!("waddle_test_hostile_{}", uuid::Uuid::new_v4().simple());
    fixture
        .execute(&format!("CREATE SCHEMA {hostile}"), ())
        .await;
    fixture
        .execute(
            &format!(
                "CREATE TABLE {hostile}.ingress_protocol_epoch (id INTEGER PRIMARY KEY, epoch BIGINT NOT NULL)"
            ),
            (),
        )
        .await;
    fixture
        .execute(
            &format!("INSERT INTO {hostile}.ingress_protocol_epoch (id, epoch) VALUES (1, 1)"),
            (),
        )
        .await;

    let admin = sqlx::PgPool::connect(fixture.db.database_url())
        .await
        .expect("candidate installer connection");
    sqlx::raw_sql(include_str!(
        "../../tests/fixtures/ingress_epoch_relation_guard_candidate.sql"
    ))
    .execute(&admin)
    .await
    .expect("install unregistered candidate in disposable schema");

    let mut url = url::Url::parse(fixture.db.database_url()).expect("Postgres URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair(
            "options",
            &format!("-c search_path={hostile},{}", expected.name.0),
        );
    let db = Database::from_config(
        "schema-bound-hostile-path",
        &DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string()),
    )
    .await
    .expect("hostile-search-path database");
    let key = uuid::Uuid::new_v4();
    let mut writer = begin_raw_ingress_writer_with_expected_pg_schema(&db, &expected)
        .await
        .expect("prelock the independently attested schema");
    let mut proof = writer
        .query(
            "SELECT current_setting('waddle.protocol_epoch'), current_setting('waddle.protocol_epoch_xid') = pg_current_xact_id()::text",
            (),
        )
        .await
        .expect("inspect bound proof");
    let proof_row = proof
        .next()
        .await
        .expect("proof row")
        .expect("proof result");
    assert_eq!(proof_row.get::<String>(0).expect("epoch proof"), "0");
    assert!(proof_row.get::<bool>(1).expect("XID proof"));
    drop(proof);
    writer
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, 1, ?)",
            crate::db_params![key.to_string(), vec![9_u8; 32]],
        )
        .await
        .expect("candidate guard uses the protected table schema, not the hostile epoch shadow");
    writer.commit().await.expect("commit hostile-path writer");

    let actual_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ingress_messages WHERE message_key = $1")
            .bind(key)
            .fetch_one(&admin)
            .await
            .expect("read actual protected schema");
    assert_eq!(actual_count, 1);

    let mut wrong_identity = expected.clone();
    wrong_identity.oid = wrong_identity.oid.saturating_add(1);
    let mismatch = begin_raw_ingress_writer_with_expected_pg_schema(&db, &wrong_identity).await;
    assert!(matches!(
        mismatch,
        Err(crate::ingress_uow::IngressUowError::Substrate(
            IngressSubstrateError::SchemaIdentityMismatch
        ))
    ));
    drop(mismatch);

    drop(db);
    admin.close().await;
    fixture
        .execute(&format!("DROP SCHEMA {hostile} CASCADE"), ())
        .await;
    fixture.close().await;
}
