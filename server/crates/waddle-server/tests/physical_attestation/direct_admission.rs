use super::super::{
    proxy_runtime_options, test_url, typed_bounds, wait_for_blocked_application, Fixture,
    CATALOG_GRANT_LOCK,
};
use super::{
    direct_pin, now, pg_seed_transaction, postgres_fixture, seed_pg_direct, seed_sqlite_direct,
    sqlite_count, sqlite_exec, sqlite_fixture,
};
use std::sync::Arc;
use std::time::Duration;

use chrono::Duration as ChronoDuration;
use waddle_server::config::LineageConfig;
use waddle_server::db::physical_attestation::{
    attested_pool, AdmissionPin, DarkIngressAuthorityError, DirectAdmission, DirectAdmissionLab,
    DirectAdmissionLabError, DirectAdmissionLimits, DirectAdmissionOutcome, FixtureConnection,
    PhysicalAttestationConfig, RetentionLabBatch, TransactionBounds,
};
use waddle_server::db::{Database, DatabasePool};
use waddle_server::ingress::EffectReceiptKey;
use waddle_server::ingress_substrate::{EffectReceiptKind, ALIAS_RETENTION};
use waddle_server::ingress_uow::{
    IngressUnitOfWork, SendAttemptRepository, SendClaim, SendLease, SendObligation,
};
use waddle_xmpp::ownership::NodeIdentity;
use waddle_xmpp::pending_delivery::SmSessionId;

fn request(pin: AdmissionPin) -> DirectAdmission {
    DirectAdmission {
        connection: FixtureConnection {
            resource: pin.key.resource.clone(),
            generation: uuid::Uuid::new_v4(),
            sm_session: SmSessionId::new("fixture-stream"),
        },
        pin,
        owner: NodeIdentity::new("fixture-node", "fixture-incarnation"),
        attempt_token: uuid::Uuid::new_v4(),
    }
}

fn obligation(a: &DirectAdmission) -> SendObligation {
    SendObligation {
        message: a.pin.key.message_key,
        receipt: EffectReceiptKey {
            kind: EffectReceiptKind::from_storage(a.pin.key.kind.to_storage()),
            semantic_identity_hash: a.pin.key.semantic_identity_hash,
        },
        recipient: a.pin.key.resource.clone(),
    }
}

async fn claim(uow: &IngressUnitOfWork, a: &DirectAdmission) -> SendLease {
    let mut tx = uow.begin().await.expect("claim transaction");
    let result =
        SendAttemptRepository::claim(&mut tx, &obligation(a), &a.owner, Duration::from_secs(300))
            .await
            .expect("claim");
    let SendClaim::Acquired(lease) = result else {
        panic!("unexpected claim: {result:?}")
    };
    tx.commit().await.expect("commit claim");
    lease
}

async fn production_start(uow: &IngressUnitOfWork, lease: &SendLease) -> bool {
    let mut tx = uow.begin().await.expect("start transaction");
    let started = SendAttemptRepository::start(&mut tx, lease)
        .await
        .expect("production start");
    tx.commit().await.expect("commit start");
    started
}

fn use_lease(a: &mut DirectAdmission, lease: &SendLease) {
    *a = DirectAdmission::for_lease(a.pin.clone(), lease, a.connection.clone())
        .expect("matching lease");
}

async fn sqlite_attempt(db: &Database, a: &mut DirectAdmission) -> SendLease {
    let uow = IngressUnitOfWork::open(db.clone(), LineageConfig::default()).expect("SQLite UOW");
    let lease = claim(&uow, a).await;
    use_lease(a, &lease);
    lease
}

async fn pg_attempt(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    a: &mut DirectAdmission,
) -> SendLease {
    let pool = attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        2,
        Duration::from_secs(2),
    )
    .expect("attested pool");
    let pool = DatabasePool::from_attested_postgres_pool("direct claim", pool, 1, 1)
        .await
        .expect("database pool");
    let uow = IngressUnitOfWork::open(
        pool.ingress(),
        LineageConfig {
            deployment_uuid: Some(fixture.lineage.deployment_uuid),
            action: None,
        },
    )
    .expect("PG UOW");
    let lease = claim(&uow, a).await;
    use_lease(a, &lease);
    lease
}

async fn pg_production_start(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    lease: &SendLease,
) -> bool {
    let pool = attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        2,
        Duration::from_secs(2),
    )
    .expect("attested pool");
    let pool = DatabasePool::from_attested_postgres_pool("direct production start", pool, 1, 1)
        .await
        .expect("database pool");
    let uow = IngressUnitOfWork::open(
        pool.ingress(),
        LineageConfig {
            deployment_uuid: Some(fixture.lineage.deployment_uuid),
            action: None,
        },
    )
    .expect("PG UOW");
    production_start(&uow, lease).await
}

async fn installed_v1009(fixture: &Fixture) {
    let source = include_str!("../../src/db/migrations/waddle.rs");
    let sql = source
        .split("pub const V1009_INERT_INGRESS_EPOCH_GUARDS_POSTGRES: &str = r#\"")
        .nth(1)
        .expect("V1009 source")
        .split("CREATE FUNCTION waddle_ingress_truncate_guard")
        .next()
        .expect("epoch guard source");
    let sql = sql.replace(
        "CREATE FUNCTION waddle_ingress_epoch_guard",
        &format!(
            "CREATE OR REPLACE FUNCTION {}.waddle_ingress_epoch_guard",
            fixture.target
        ),
    );
    sqlx::raw_sql(&sql)
        .execute(&fixture.admin)
        .await
        .expect("restore installed V1009 guard");
    sqlx::query(&format!(
        "GRANT UPDATE (id) ON {}.ingress_protocol_epoch TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("epoch row lock permission");
}

#[tokio::test]
async fn sqlite_direct_admission_is_exact_bounded_and_recoverable() {
    let (db, retention) = sqlite_fixture().await;
    sqlite_exec(
        &db,
        "CREATE TABLE pending_delivery (row_id TEXT PRIMARY KEY)",
    )
    .await;
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_sqlite_direct(&db, &pin, envelope, intent).await;
    let mut admission = request(pin);
    let lease = sqlite_attempt(&db, &mut admission).await;
    let limits = DirectAdmissionLimits::new(1, 4096).expect("limits");
    let lab = DirectAdmissionLab::sqlite(&db, typed_bounds(), admission.connection.clone(), limits)
        .expect("lab");
    let mut wrong = admission.clone();
    wrong.connection.generation = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.start(&wrong, &lease).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    wrong = admission.clone();
    wrong.connection.sm_session = SmSessionId::new("wrong-stream");
    assert!(matches!(
        lab.start(&wrong, &lease).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    wrong = admission.clone();
    wrong.attempt_token = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.start(&wrong, &lease).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    wrong = admission.clone();
    wrong.owner = NodeIdentity::new("wrong-node", "wrong-incarnation");
    assert!(matches!(
        lab.start(&wrong, &lease).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    wrong = admission.clone();
    wrong.pin.received_at += ChronoDuration::microseconds(1);
    assert!(matches!(
        lab.start(&wrong, &lease).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    wrong = admission.clone();
    wrong.pin.payload = waddle_xmpp::parser::message_from_string("<message xmlns='jabber:client' from='alice@example.test/phone' to='bob@example.test' type='chat'><body>forged</body></message>").expect("message");
    assert!(matches!(
        lab.start(&wrong, &lease).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    assert_eq!(
        sqlite_count(&db, "SELECT COUNT(*) FROM main.direct_admission_lab").await,
        0
    );
    assert_eq!(
        sqlite_count(
            &db,
            "SELECT COUNT(*) FROM main.ingress_send_attempts WHERE state = 0"
        )
        .await,
        1
    );
    let guard = db.guard().await.expect("guard");
    guard.execute("CREATE TEMP TABLE direct_admission_lab AS SELECT * FROM main.direct_admission_lab WHERE 0", ()).await.expect("shadow admission");
    guard.execute("CREATE TEMP TABLE ingress_send_attempts AS SELECT * FROM main.ingress_send_attempts WHERE 0", ()).await.expect("shadow attempt");
    guard
        .execute(
            "CREATE TEMP TABLE ingress_messages AS SELECT * FROM main.ingress_messages WHERE 0",
            (),
        )
        .await
        .expect("shadow canonical");
    guard
        .execute(
            "CREATE TEMP TABLE ingress_protocol_epoch (id INTEGER PRIMARY KEY, epoch INTEGER)",
            (),
        )
        .await
        .expect("shadow epoch");
    guard
        .execute("INSERT INTO temp.ingress_protocol_epoch VALUES (1, 0)", ())
        .await
        .expect("shadow epoch row");
    guard.execute("UPDATE main.ingress_protocol_epoch SET epoch = 2, activated_at = '2026-11-01T12:00:00Z', lineage_uuid = '00000000-0000-0000-0000-000000000001' WHERE id = 1", ()).await.expect("future epoch");
    drop(guard);
    assert!(matches!(
        lab.start(&admission, &lease).await,
        Err(DirectAdmissionLabError::Authority(
            DarkIngressAuthorityError::UnsupportedEpoch
        ))
    ));
    sqlite_exec(&db, "UPDATE main.ingress_protocol_epoch SET epoch = 0, activated_at = NULL, lineage_uuid = NULL WHERE id = 1").await;
    sqlite_exec(&db, "DELETE FROM main.ingress_protocol_epoch WHERE id = 1").await;
    assert!(matches!(
        lab.start(&admission, &lease).await,
        Err(DirectAdmissionLabError::Authority(
            DarkIngressAuthorityError::MissingEpoch
        ))
    ));
    sqlite_exec(
        &db,
        "INSERT INTO main.ingress_protocol_epoch (id, epoch) VALUES (1, 0)",
    )
    .await;
    let guard = db.guard().await.expect("guard");
    guard.execute("INSERT INTO main.ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES (?, ?, ?)", waddle_server::db_params![admission.pin.key.message_key.to_storage().to_string(), admission.pin.key.kind.to_storage(), admission.pin.key.semantic_identity_hash.to_vec()]).await.expect("completed effect");
    drop(guard);
    assert!(
        matches!(
            lab.start(&admission, &lease).await,
            Err(DirectAdmissionLabError::Rejected)
        ),
        "completed obligation cannot start"
    );
    sqlite_exec(&db, "DELETE FROM main.ingress_effect_receipts").await;
    sqlite_exec(&db, "UPDATE main.ingress_send_attempts SET recovered = 1").await;
    assert!(
        matches!(
            lab.start(&admission, &lease).await,
            Err(DirectAdmissionLabError::Rejected)
        ),
        "a prior unknown start cannot mint a new admission"
    );
    sqlite_exec(
        &db,
        "UPDATE main.ingress_send_attempts SET recovered = 0, expires_at_ms = 0",
    )
    .await;
    assert!(
        matches!(
            lab.start(&admission, &lease).await,
            Err(DirectAdmissionLabError::Rejected)
        ),
        "an expired lease cannot start"
    );
    sqlite_exec(
        &db,
        "UPDATE main.ingress_send_attempts SET expires_at_ms = 4102444800000",
    )
    .await;
    assert_eq!(
        lab.start(&admission, &lease).await.expect("record"),
        DirectAdmissionOutcome::Recorded
    );
    assert_eq!(
        lab.start(&admission, &lease).await.expect("recover exact"),
        DirectAdmissionOutcome::Recovered
    );
    let recovered = lab
        .lookup(&admission)
        .await
        .expect("lookup")
        .expect("admission");
    assert_eq!(recovered.pin.payload, admission.pin.payload);
    assert_eq!(recovered.pin.received_at, admission.pin.received_at);
    assert_eq!(recovered.pin.key, admission.pin.key);
    assert_eq!(recovered.connection, admission.connection);
    wrong = admission.clone();
    wrong.attempt_token = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.lookup(&wrong).await,
        Err(DirectAdmissionLabError::Conflict)
    ));
    wrong = admission.clone();
    wrong.attempt_token = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.start(&wrong, &lease).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    sqlite_exec(&db, "DROP TABLE temp.ingress_send_attempts").await;
    sqlite_exec(&db, "DROP TABLE temp.ingress_protocol_epoch").await;
    sqlite_exec(&db, "DROP TABLE temp.ingress_messages").await;
    let (other, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_sqlite_direct(&db, &other, envelope, intent).await;
    let mut other = request(other);
    other.connection = admission.connection.clone();
    let other_lease = sqlite_attempt(&db, &mut other).await;
    assert!(matches!(
        lab.start(&other, &other_lease).await,
        Err(DirectAdmissionLabError::Capacity)
    ));
    let byte_limited = DirectAdmissionLab::sqlite(
        &db,
        typed_bounds(),
        other.connection.clone(),
        DirectAdmissionLimits::new(10, 1).expect("byte limit"),
    )
    .expect("lab");
    assert!(matches!(
        byte_limited.start(&other, &other_lease).await,
        Err(DirectAdmissionLabError::Capacity)
    ));
    assert_eq!(
        sqlite_count(
            &db,
            "SELECT COUNT(*) FROM main.ingress_send_attempts WHERE state = 1"
        )
        .await,
        1
    );
    let guard = db.guard().await.expect("guard");
    for sql in ["UPDATE main.direct_admission_lab SET payload = 'forged'", "DELETE FROM main.direct_admission_lab", "INSERT OR REPLACE INTO main.direct_admission_lab SELECT * FROM main.direct_admission_lab", "INSERT OR REPLACE INTO main.direct_admission_lab (rowid, admission_id, message_key, receipt_kind, semantic_identity_hash, resource, attempt_token, node_id, node_incarnation, connection_generation, sm_session_id, payload, received_at) SELECT rowid, admission_id, message_key, receipt_kind, semantic_identity_hash, resource, attempt_token, node_id, node_incarnation, connection_generation, sm_session_id, payload, received_at FROM main.direct_admission_lab"] {
        assert!(guard.execute(sql, ()).await.is_err(), "immutable: {sql}");
    }
    assert!(guard.execute("INSERT OR REPLACE INTO main.direct_admission_lab SELECT ?, message_key, receipt_kind, semantic_identity_hash, resource, ?, node_id, node_incarnation, connection_generation, sm_session_id, 'forged', received_at FROM main.direct_admission_lab", waddle_server::db_params![uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string()]).await.is_err(), "exact allocation-key collision cannot replace the pin");
    assert!(guard.execute("INSERT OR REPLACE INTO main.direct_admission_lab SELECT ?, ?, receipt_kind, semantic_identity_hash, resource, attempt_token, node_id, node_incarnation, connection_generation, sm_session_id, 'forged', received_at FROM main.direct_admission_lab", waddle_server::db_params![uuid::Uuid::new_v4().to_string(), other.pin.key.message_key.to_storage().to_string()]).await.is_err(), "attempt-token collision cannot move the pin");
    guard.execute("INSERT INTO main.ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES (?, ?, ?)", waddle_server::db_params![admission.pin.key.message_key.to_storage().to_string(), admission.pin.key.kind.to_storage(), admission.pin.key.semantic_identity_hash.to_vec()]).await.expect("settled effect");
    guard
        .execute(
            "UPDATE main.ingress_messages SET retention_eligible_at = ? WHERE message_key = ?",
            waddle_server::db_params![
                (now() - ALIAS_RETENTION).to_rfc3339(),
                admission.pin.key.message_key.to_storage().to_string()
            ],
        )
        .await
        .expect("old frontier");
    drop(guard);
    assert_eq!(
        retention.collect(now(), 5).await.expect("collect").deleted,
        0
    );
    assert_eq!(
        sqlite_count(&db, "SELECT COUNT(*) FROM main.direct_admission_lab").await,
        1
    );
    assert_eq!(
        sqlite_count(&db, "SELECT COUNT(*) FROM temp.direct_admission_lab").await,
        0
    );
}

#[tokio::test]
async fn sqlite_direct_unknown_commit_rolls_back_and_releases_one_slot() {
    let (db, _) = sqlite_fixture().await;
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_sqlite_direct(&db, &pin, envelope, intent).await;
    let mut admission = request(pin);
    let lease = sqlite_attempt(&db, &mut admission).await;
    let lab = Arc::new(
        DirectAdmissionLab::sqlite(
            &db,
            typed_bounds(),
            admission.connection.clone(),
            DirectAdmissionLimits::new(10, 4096).expect("limits"),
        )
        .expect("lab"),
    );
    let holder = db.begin_immediate().await.expect("hold slot");
    let (started, observing) = tokio::sync::oneshot::channel();
    let task = {
        let lab = lab.clone();
        let admission = admission.clone();
        let lease = lease.clone();
        tokio::spawn(async move {
            let _ = started.send(());
            lab.start(&admission, &lease).await
        })
    };
    observing.await.expect("start task");
    assert!(!task.is_finished());
    task.abort();
    assert!(task.await.expect_err("cancelled").is_cancelled());
    holder.rollback().await.expect("release slot");
    let mut tx = db.begin_immediate().await.expect("DDL");
    tx.execute_batch("CREATE TABLE main.direct_probe_parent (id INTEGER PRIMARY KEY); CREATE TABLE main.direct_probe_child (id INTEGER REFERENCES direct_probe_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER main.direct_reject_commit AFTER INSERT ON direct_admission_lab BEGIN INSERT INTO direct_probe_child VALUES (1); END;").await.expect("deferred failure");
    tx.commit().await.expect("commit DDL");
    assert!(matches!(
        lab.start(&admission, &lease).await,
        Err(DirectAdmissionLabError::Authority(
            DarkIngressAuthorityError::CommitIndeterminate
        ))
    ));
    assert!(lab.lookup(&admission).await.expect("lookup").is_none());
    assert_eq!(
        sqlite_count(
            &db,
            "SELECT COUNT(*) FROM main.ingress_send_attempts WHERE state = 0"
        )
        .await,
        1
    );
    sqlite_exec(&db, "DROP TRIGGER main.direct_reject_commit").await;
    assert_eq!(
        lab.start(&admission, &lease).await.expect("retry"),
        DirectAdmissionOutcome::Recorded
    );
}

#[tokio::test]
async fn sqlite_real_claim_reclaims_only_never_started_and_rolls_back_insert_failure() {
    let (db, _) = sqlite_fixture().await;
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_sqlite_direct(&db, &pin, envelope, intent).await;
    let mut a = request(pin);
    let stale = sqlite_attempt(&db, &mut a).await;
    let stale_admission = a.clone();
    sqlite_exec(
        &db,
        "UPDATE main.ingress_send_attempts SET expires_at_ms = 0",
    )
    .await;
    let current = sqlite_attempt(&db, &mut a).await;
    let lab = DirectAdmissionLab::sqlite(
        &db,
        typed_bounds(),
        a.connection.clone(),
        DirectAdmissionLimits::new(10, 4096).expect("limits"),
    )
    .expect("lab");
    assert!(matches!(
        lab.start(&a, &stale).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    assert!(matches!(
        lab.start(&stale_admission, &stale).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    sqlite_exec(&db, "CREATE TRIGGER main.direct_reject_insert BEFORE INSERT ON direct_admission_lab BEGIN SELECT RAISE(ABORT, 'injected'); END").await;
    assert!(matches!(
        lab.start(&a, &current).await,
        Err(DirectAdmissionLabError::Authority(
            DarkIngressAuthorityError::Database
        ))
    ));
    assert_eq!(
        sqlite_count(
            &db,
            "SELECT count(*) FROM main.ingress_send_attempts WHERE state = 0 AND recovered = 0"
        )
        .await,
        1
    );
    assert!(lab.lookup(&a).await.expect("lookup").is_none());
    sqlite_exec(&db, "DROP TRIGGER main.direct_reject_insert").await;
    assert_eq!(
        lab.start(&a, &current)
            .await
            .expect("reclaimed never-started claim"),
        DirectAdmissionOutcome::Recorded
    );

    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_sqlite_direct(&db, &pin, envelope, intent).await;
    let mut b = request(pin);
    b.connection = a.connection.clone();
    let first = sqlite_attempt(&db, &mut b).await;
    let uow = IngressUnitOfWork::open(db.clone(), LineageConfig::default()).expect("SQLite UOW");
    assert!(production_start(&uow, &first).await);
    let guard = db.guard().await.expect("guard");
    guard
        .execute(
            "UPDATE main.ingress_send_attempts SET expires_at_ms = 0 WHERE message_key = ?",
            waddle_server::db_params![b.pin.key.message_key.to_storage().to_string()],
        )
        .await
        .expect("expire started attempt");
    drop(guard);
    let recovered_started = sqlite_attempt(&db, &mut b).await;
    assert!(matches!(
        lab.start(&b, &recovered_started).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    assert_eq!(
        sqlite_count(
            &db,
            "SELECT count(*) FROM main.ingress_send_attempts WHERE state = 0 AND recovered = 1"
        )
        .await,
        1
    );
    assert!(
        production_start(&uow, &recovered_started).await,
        "the normal start still permits recovered starts"
    );
}

#[tokio::test]
async fn sqlite_missing_main_parent_stays_unresolved_despite_temp_shadow() {
    let (db, retention) = sqlite_fixture().await;
    sqlite_exec(
        &db,
        "CREATE TABLE main.pending_delivery (row_id TEXT PRIMARY KEY)",
    )
    .await;
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_sqlite_direct(&db, &pin, envelope, intent).await;
    let mut a = request(pin);
    let lease = sqlite_attempt(&db, &mut a).await;
    let lab = DirectAdmissionLab::sqlite(
        &db,
        typed_bounds(),
        a.connection.clone(),
        DirectAdmissionLimits::new(10, 4096).expect("limits"),
    )
    .expect("lab");
    let guard = db.guard().await.expect("guard");
    guard
        .execute(
            "CREATE TEMP TABLE ingress_messages AS SELECT * FROM main.ingress_messages",
            (),
        )
        .await
        .expect("shadow parent");
    guard.execute("INSERT INTO main.ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES (?, ?, ?)", waddle_server::db_params![a.pin.key.message_key.to_storage().to_string(), a.pin.key.kind.to_storage(), a.pin.key.semantic_identity_hash.to_vec()]).await.expect("settle effect");
    drop(guard);
    assert_eq!(
        retention
            .collect(now(), 1)
            .await
            .expect("collector first")
            .deleted,
        1
    );
    assert_eq!(
        sqlite_count(&db, "SELECT count(*) FROM temp.ingress_messages").await,
        1
    );
    assert!(matches!(
        lab.start(&a, &lease).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    assert!(matches!(
        lab.lookup(&a).await,
        Err(DirectAdmissionLabError::ReconciliationUnresolved)
    ));
}

#[tokio::test]
async fn postgres_direct_admission_serializes_with_gc_and_uses_attested_schema() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, retention) = postgres_fixture(&fixture).await;
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    installed_v1009(&fixture).await;
    let mut admission = request(pin);
    let lease = pg_attempt(&fixture, &config, &mut admission).await;
    let schema = &fixture.target;
    let shadow = format!("direct_shadow_{}", uuid::Uuid::new_v4().simple());
    for statement in [
        format!("CREATE SCHEMA {shadow}"),
        format!("CREATE TABLE {shadow}.direct_admission_lab (LIKE {schema}.direct_admission_lab INCLUDING ALL)"),
        format!("GRANT USAGE ON SCHEMA {shadow} TO {}", fixture.runtime),
        format!("GRANT ALL ON ALL TABLES IN SCHEMA {shadow} TO {}", fixture.runtime),
        format!("CREATE FUNCTION {schema}.poison_direct_path() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_catalog.set_config('search_path', '{shadow}, {schema}', true); RETURN NEW; END $$"),
        format!("CREATE TRIGGER poison_direct_path BEFORE UPDATE ON {schema}.ingress_send_attempts FOR EACH ROW EXECUTE FUNCTION {schema}.poison_direct_path()"),
        format!("CREATE FUNCTION {schema}.pause_direct() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_catalog.pg_advisory_xact_lock(7199333); RETURN NEW; END $$"),
        format!("CREATE TRIGGER pause_direct BEFORE INSERT ON {schema}.direct_admission_lab FOR EACH ROW EXECUTE FUNCTION {schema}.pause_direct()"),
    ] { sqlx::query(&statement).execute(&fixture.admin).await.expect("hostile path and barrier"); }
    let app = format!("direct_lab_{}", uuid::Uuid::new_v4().simple());
    let lab = Arc::new(
        DirectAdmissionLab::postgres(
            fixture.runtime_options.clone().application_name(&app),
            config.clone(),
            1,
            typed_bounds(),
            admission.connection.clone(),
            DirectAdmissionLimits::new(10, 4096).expect("limits"),
        )
        .expect("lab"),
    );
    let mut wrong = admission.clone();
    wrong.connection.generation = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.start(&wrong, &lease).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    wrong = admission.clone();
    wrong.attempt_token = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.start(&wrong, &lease).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    let mut blocker = fixture.admin.begin().await.expect("blocker");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock(7199333)")
        .execute(&mut *blocker)
        .await
        .expect("hold barrier");
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .expect("pid");
    let task = {
        let lab = lab.clone();
        let admission = admission.clone();
        let lease = lease.clone();
        tokio::spawn(async move { lab.start(&admission, &lease).await })
    };
    let writer_pid = wait_for_blocked_application(&fixture.admin, &app, pid).await;
    let lookup_app = format!("direct_lookup_{}", uuid::Uuid::new_v4().simple());
    let lookup_lab = Arc::new(
        DirectAdmissionLab::postgres(
            fixture
                .runtime_options
                .clone()
                .application_name(&lookup_app),
            config.clone(),
            1,
            typed_bounds(),
            admission.connection.clone(),
            DirectAdmissionLimits::new(10, 4096).expect("limits"),
        )
        .expect("lookup lab"),
    );
    let lookup_task = {
        let lab = lookup_lab.clone();
        let admission = admission.clone();
        tokio::spawn(async move { lab.lookup(&admission).await })
    };
    wait_for_blocked_application(&fixture.admin, &lookup_app, writer_pid).await;
    assert!(
        !lookup_task.is_finished(),
        "reconciliation waits for the old transaction"
    );
    assert_eq!(
        retention
            .collect(now(), 1)
            .await
            .expect("skip locked canonical"),
        RetentionLabBatch::default()
    );
    blocker.commit().await.expect("release writer");
    assert_eq!(
        task.await.expect("task").expect("record"),
        DirectAdmissionOutcome::Recorded
    );
    assert!(lookup_task
        .await
        .expect("lookup task")
        .expect("lookup")
        .is_some());
    assert_eq!(
        lab.start(&admission, &lease).await.expect("recover"),
        DirectAdmissionOutcome::Recovered
    );
    let recovered = lab
        .lookup(&admission)
        .await
        .expect("lookup")
        .expect("record");
    assert_eq!(recovered.pin.payload, admission.pin.payload);
    assert_eq!(recovered.pin.received_at, admission.pin.received_at);
    assert_eq!(recovered.connection, admission.connection);
    let shadow_count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {shadow}.direct_admission_lab"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("shadow count");
    assert_eq!(shadow_count, 0);
    let mut tx = pg_seed_transaction(&fixture).await;
    sqlx::query(&format!("INSERT INTO {schema}.ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES ($1, $2, $3)"))
        .bind(admission.pin.key.message_key.to_storage()).bind(admission.pin.key.kind.to_storage()).bind(admission.pin.key.semantic_identity_hash.as_slice()).execute(&mut *tx).await.expect("effect completion");
    sqlx::query(&format!(
        "UPDATE {schema}.ingress_messages SET retention_eligible_at = $1 WHERE message_key = $2"
    ))
    .bind(now() - ALIAS_RETENTION)
    .bind(admission.pin.key.message_key.to_storage())
    .execute(&mut *tx)
    .await
    .expect("old frontier");
    tx.commit().await.expect("commit");
    assert_eq!(
        retention
            .collect(now(), 1)
            .await
            .expect("admission blocks GC")
            .deleted,
        0
    );
    for statement in [
        format!("UPDATE {schema}.direct_admission_lab SET payload = 'forged'"),
        format!("DELETE FROM {schema}.direct_admission_lab"),
        format!("TRUNCATE {schema}.direct_admission_lab"),
    ] {
        assert!(sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .is_err());
    }
    let (late, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &late, envelope, intent).await;
    let mut late = request(late);
    late.connection = admission.connection.clone();
    let late_lease = pg_attempt(&fixture, &config, &mut late).await;
    let mut tx = pg_seed_transaction(&fixture).await;
    sqlx::query(&format!("INSERT INTO {schema}.ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES ($1, $2, $3)"))
        .bind(late.pin.key.message_key.to_storage()).bind(late.pin.key.kind.to_storage()).bind(late.pin.key.semantic_identity_hash.as_slice()).execute(&mut *tx).await.expect("settle late effect");
    tx.commit().await.expect("commit");
    assert_eq!(
        retention
            .collect(now(), 2)
            .await
            .expect("collector first")
            .deleted,
        1
    );
    assert!(matches!(
        lab.start(&late, &late_lease).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    assert!(matches!(
        lab.lookup(&late).await,
        Err(DirectAdmissionLabError::ReconciliationUnresolved)
    ));
    drop(lab);
    drop(lookup_lab);
    drop(retention);
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_direct_capacity_serializes_concurrent_distinct_messages() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, retention) = postgres_fixture(&fixture).await;
    drop(retention);
    let (first, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &first, envelope, intent).await;
    installed_v1009(&fixture).await;
    let mut first = request(first);
    let first_lease = pg_attempt(&fixture, &config, &mut first).await;
    let (second, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &second, envelope, intent).await;
    let mut second = request(second);
    second.connection = first.connection.clone();
    let second_lease = pg_attempt(&fixture, &config, &mut second).await;
    let byte_limited = DirectAdmissionLab::postgres(
        fixture.runtime_options.clone(),
        config.clone(),
        1,
        typed_bounds(),
        first.connection.clone(),
        DirectAdmissionLimits::new(10, 1).expect("byte limit"),
    )
    .expect("byte-limited lab");
    assert!(matches!(
        byte_limited.start(&first, &first_lease).await,
        Err(DirectAdmissionLabError::Capacity)
    ));
    drop(byte_limited);
    let suffix = uuid::Uuid::new_v4().simple();
    let app_a = format!("direct_quota_a_{suffix}");
    let app_b = format!("direct_quota_b_{suffix}");
    let limits = DirectAdmissionLimits::new(1, 4096).expect("limits");
    let wait_bounds = TransactionBounds::new(
        Duration::from_secs(10),
        Duration::from_secs(8),
        Duration::from_secs(9),
    )
    .expect("race bounds");
    let lab_a = Arc::new(
        DirectAdmissionLab::postgres(
            fixture.runtime_options.clone().application_name(&app_a),
            config.clone(),
            1,
            wait_bounds,
            first.connection.clone(),
            limits,
        )
        .expect("first lab"),
    );
    let lab_b = Arc::new(
        DirectAdmissionLab::postgres(
            fixture.runtime_options.clone().application_name(&app_b),
            config,
            1,
            wait_bounds,
            second.connection.clone(),
            limits,
        )
        .expect("second lab"),
    );
    let mut holder = fixture.admin.begin().await.expect("holder");
    sqlx::query(&format!(
        "SELECT id FROM {}.direct_admission_lab_quota WHERE id = 1 FOR UPDATE",
        fixture.target
    ))
    .fetch_one(&mut *holder)
    .await
    .expect("hold quota");
    let keys = [first.pin.key.message_key, second.pin.key.message_key];
    let task_a = tokio::spawn(async move { lab_a.start(&first, &first_lease).await });
    let task_b = tokio::spawn(async move { lab_b.start(&second, &second_lease).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE application_name = ANY($1) AND wait_event_type = 'Lock'")
                .bind(vec![app_a.clone(), app_b.clone()]).fetch_one(&fixture.admin).await.expect("observe quota waits");
            if waiting == 2 { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("both writers wait for the quota row");
    for key in keys {
        let mut check = fixture.admin.begin().await.expect("canonical check");
        assert!(
            sqlx::query(&format!(
                "SELECT 1 FROM {}.ingress_messages WHERE message_key = $1 FOR UPDATE NOWAIT",
                fixture.target
            ))
            .bind(key.to_storage())
            .fetch_one(&mut *check)
            .await
            .is_err(),
            "both independent admissions reach canonical locks"
        );
        check.rollback().await.expect("release check");
    }
    holder.commit().await.expect("release quota");
    let outcomes = [
        task_a.await.expect("first task"),
        task_b.await.expect("second task"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|r| matches!(r, Ok(DirectAdmissionOutcome::Recorded)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|r| matches!(r, Err(DirectAdmissionLabError::Capacity)))
            .count(),
        1
    );
    let recorded: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.direct_admission_lab",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("count");
    let started: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.ingress_send_attempts WHERE state = 1",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("started");
    assert_eq!((recorded, started), (1, 1));
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_direct_unknown_commit_is_reconciled_on_one_slot() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, retention) = postgres_fixture(&fixture).await;
    drop(retention);
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    installed_v1009(&fixture).await;
    let mut admission = request(pin);
    let lease = pg_attempt(&fixture, &config, &mut admission).await;
    let app = format!("direct_cancel_{}", uuid::Uuid::new_v4().simple());
    let lab = Arc::new(
        DirectAdmissionLab::postgres(
            fixture.runtime_options.clone().application_name(&app),
            config,
            1,
            typed_bounds(),
            admission.connection.clone(),
            DirectAdmissionLimits::new(10, 4096).expect("limits"),
        )
        .expect("lab"),
    );
    let mut holder = fixture.admin.begin().await.expect("holder");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.ingress_messages WHERE message_key = $1 FOR UPDATE",
        fixture.target
    ))
    .bind(admission.pin.key.message_key.to_storage())
    .fetch_one(&mut *holder)
    .await
    .expect("hold canonical");
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .expect("pid");
    let task = {
        let lab = lab.clone();
        let admission = admission.clone();
        let lease = lease.clone();
        tokio::spawn(async move { lab.start(&admission, &lease).await })
    };
    wait_for_blocked_application(&fixture.admin, &app, pid).await;
    task.abort();
    assert!(task.await.expect_err("cancelled").is_cancelled());
    holder.rollback().await.expect("release canonical");
    let schema = &fixture.target;
    for statement in [
        format!("CREATE FUNCTION {schema}.reject_direct_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected'; END $$"),
        format!("CREATE CONSTRAINT TRIGGER reject_direct_commit AFTER INSERT ON {schema}.direct_admission_lab DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION {schema}.reject_direct_commit()"),
    ] { sqlx::query(&statement).execute(&fixture.admin).await.expect("deferred failure"); }
    assert!(matches!(
        lab.start(&admission, &lease).await,
        Err(DirectAdmissionLabError::Authority(
            DarkIngressAuthorityError::CommitIndeterminate
        ))
    ));
    assert!(lab.lookup(&admission).await.expect("lookup").is_none());
    let state: i32 = sqlx::query_scalar(&format!(
        "SELECT state FROM {schema}.ingress_send_attempts WHERE lease_token = $1"
    ))
    .bind(admission.attempt_token.to_string())
    .fetch_one(&fixture.admin)
    .await
    .expect("attempt state");
    assert_eq!(state, 0);
    sqlx::query(&format!(
        "DROP TRIGGER reject_direct_commit ON {schema}.direct_admission_lab"
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove failure");
    assert_eq!(
        lab.start(&admission, &lease).await.expect("retry"),
        DirectAdmissionOutcome::Recorded
    );
    drop(lab);
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_real_claim_recovery_requires_never_started_and_epoch_first() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, retention) = postgres_fixture(&fixture).await;
    drop(retention);
    installed_v1009(&fixture).await;
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    let mut a = request(pin);
    let stale = pg_attempt(&fixture, &config, &mut a).await;
    let stale_admission = a.clone();
    sqlx::query(&format!(
        "UPDATE {}.ingress_send_attempts SET expires_at_ms = 0",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("expire unstarted");
    let current = pg_attempt(&fixture, &config, &mut a).await;
    let app = format!("direct_epoch_{}", uuid::Uuid::new_v4().simple());
    let lab = Arc::new(
        DirectAdmissionLab::postgres(
            fixture.runtime_options.clone().application_name(&app),
            config.clone(),
            1,
            typed_bounds(),
            a.connection.clone(),
            DirectAdmissionLimits::new(10, 4096).expect("limits"),
        )
        .expect("lab"),
    );
    assert!(matches!(
        lab.start(&stale_admission, &stale).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    assert!(matches!(
        lab.start(&a, &stale).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    let mut holder = fixture.admin.begin().await.expect("epoch holder");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.ingress_protocol_epoch WHERE id = 1 FOR UPDATE",
        fixture.target
    ))
    .fetch_one(&mut *holder)
    .await
    .expect("epoch lock");
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .expect("pid");
    let task = {
        let lab = lab.clone();
        let a = a.clone();
        let current = current.clone();
        tokio::spawn(async move { lab.start(&a, &current).await })
    };
    wait_for_blocked_application(&fixture.admin, &app, pid).await;
    let mut check = fixture.admin.begin().await.expect("canonical check");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.ingress_messages WHERE message_key = $1 FOR UPDATE NOWAIT",
        fixture.target
    ))
    .bind(a.pin.key.message_key.to_storage())
    .fetch_one(&mut *check)
    .await
    .expect("canonical is not locked before epoch");
    check.rollback().await.expect("release canonical");
    holder.commit().await.expect("release epoch");
    assert_eq!(
        task.await
            .expect("task")
            .expect("reclaimed never-started claim"),
        DirectAdmissionOutcome::Recorded
    );

    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    let mut b = request(pin);
    b.connection = a.connection.clone();
    let first = pg_attempt(&fixture, &config, &mut b).await;
    assert!(pg_production_start(&fixture, &config, &first).await);
    sqlx::query(&format!(
        "UPDATE {}.ingress_send_attempts SET expires_at_ms = 0 WHERE message_key = $1",
        fixture.target
    ))
    .bind(b.pin.key.message_key.to_storage())
    .execute(&fixture.admin)
    .await
    .expect("prior started attempt");
    let recovered_started = pg_attempt(&fixture, &config, &mut b).await;
    assert!(matches!(
        lab.start(&b, &recovered_started).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    let state: (i32, i32) = sqlx::query_as(&format!(
        "SELECT state, recovered FROM {}.ingress_send_attempts WHERE message_key = $1",
        fixture.target
    ))
    .bind(b.pin.key.message_key.to_storage())
    .fetch_one(&fixture.admin)
    .await
    .expect("attempt state");
    assert_eq!(state, (0, 1));
    assert!(
        pg_production_start(&fixture, &config, &recovered_started).await,
        "the normal start still permits recovered starts"
    );

    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    let mut c = request(pin);
    c.connection = a.connection.clone();
    let fresh = pg_attempt(&fixture, &config, &mut c).await;
    assert!(matches!(
        lab.start(&c, &current).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    sqlx::query(&format!("CREATE FUNCTION {}.reject_direct_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected'; END $$", fixture.target)).execute(&fixture.admin).await.expect("injection function");
    sqlx::query(&format!("CREATE TRIGGER reject_direct_insert BEFORE INSERT ON {}.direct_admission_lab FOR EACH ROW EXECUTE FUNCTION {}.reject_direct_insert()", fixture.target, fixture.target)).execute(&fixture.admin).await.expect("injection trigger");
    assert!(matches!(
        lab.start(&c, &fresh).await,
        Err(DirectAdmissionLabError::Authority(
            DarkIngressAuthorityError::Database
        ))
    ));
    let state: i32 = sqlx::query_scalar(&format!(
        "SELECT state FROM {}.ingress_send_attempts WHERE message_key = $1",
        fixture.target
    ))
    .bind(c.pin.key.message_key.to_storage())
    .fetch_one(&fixture.admin)
    .await
    .expect("rolled back attempt");
    assert_eq!(state, 0);
    assert!(lab
        .lookup(&c)
        .await
        .expect("lookup after rollback")
        .is_none());
    sqlx::query(&format!(
        "DROP TRIGGER reject_direct_insert ON {}.direct_admission_lab",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove injection");
    // Deliberately disable the transition guard to force a row change between
    // the attested epoch read and its row lock; normal activation cannot do
    // this because it takes ACCESS EXCLUSIVE on the relation first.
    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_protocol_epoch DISABLE TRIGGER ingress_protocol_epoch_guard_row",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("disable guard for race");
    let mut changed = fixture.admin.begin().await.expect("epoch racer");
    sqlx::query(&format!("UPDATE {}.ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = $1 WHERE id = 1", fixture.target)).bind(fixture.lineage.lineage_uuid.0).execute(&mut *changed).await.expect("uncommitted epoch");
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *changed)
        .await
        .expect("pid");
    let raced = {
        let lab = lab.clone();
        let c = c.clone();
        let fresh = fresh.clone();
        tokio::spawn(async move { lab.start(&c, &fresh).await })
    };
    wait_for_blocked_application(&fixture.admin, &app, pid).await;
    changed.commit().await.expect("commit raced epoch");
    assert!(matches!(
        raced.await.expect("race task"),
        Err(DirectAdmissionLabError::Authority(
            DarkIngressAuthorityError::Database
        ))
    ));
    sqlx::query(&format!("ALTER TABLE {}.ingress_protocol_epoch ENABLE ALWAYS TRIGGER ingress_protocol_epoch_guard_row", fixture.target)).execute(&fixture.admin).await.expect("restore guard");
    let mut transition = fixture.admin.begin().await.expect("epoch transition");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *transition)
    .await
    .expect("epoch relation lock");
    sqlx::query(&format!("UPDATE {}.ingress_protocol_epoch SET epoch = 2, activated_at = now(), lineage_uuid = $1 WHERE id = 1", fixture.target)).bind(fixture.lineage.lineage_uuid.0).execute(&mut *transition).await.expect("advance epoch");
    transition.commit().await.expect("commit epoch transition");
    assert!(matches!(
        lab.start(&c, &fresh).await,
        Err(DirectAdmissionLabError::Authority(
            DarkIngressAuthorityError::UnsupportedEpoch
        ))
    ));
    drop(lab);
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_committed_lost_reply_requires_fresh_authoritative_lookup() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, retention) = postgres_fixture(&fixture).await;
    drop(retention);
    installed_v1009(&fixture).await;
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    let mut a = request(pin);
    let lease = pg_attempt(&fixture, &config, &mut a).await;

    // Forward one plaintext PostgreSQL connection, but drop its COMMIT result
    // only after the server emits CommandComplete("COMMIT"). This makes the
    // server-side outcome committed while the client sees a lost response.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("proxy listener");
    let proxy_port = listener.local_addr().expect("proxy address").port();
    let upstream = fixture.admin_options.clone();
    let proxy = tokio::spawn(async move {
        let (client, _) = listener.accept().await.expect("accept client");
        let server = crate::postgres_proxy::connect_local_postgres(&upstream)
            .await
            .expect("connect upstream");
        let (mut client_read, mut client_write) = client.into_split();
        let (mut server_read, mut server_write) = tokio::io::split(server);
        let forward = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
        });
        loop {
            let tag = server_read.read_u8().await.expect("backend message tag");
            let length = server_read
                .read_u32()
                .await
                .expect("backend message length");
            assert!((4..=1_048_576).contains(&length));
            let mut body = vec![0; usize::try_from(length - 4).expect("length")];
            server_read
                .read_exact(&mut body)
                .await
                .expect("backend message body");
            if tag == b'C' && body == b"COMMIT\0" {
                break;
            }
            client_write.write_u8(tag).await.expect("forward tag");
            client_write
                .write_u32(length)
                .await
                .expect("forward length");
            client_write.write_all(&body).await.expect("forward body");
        }
        forward.abort();
        let _ = forward.await;
        client_write.shutdown().await.expect("drop commit response");
    });
    let options = proxy_runtime_options(&fixture, proxy_port);
    let lab = DirectAdmissionLab::postgres(
        options,
        config.clone(),
        1,
        typed_bounds(),
        a.connection.clone(),
        DirectAdmissionLimits::new(10, 4096).expect("limits"),
    )
    .expect("proxied lab");
    assert!(matches!(
        lab.start(&a, &lease).await,
        Err(DirectAdmissionLabError::Authority(
            DarkIngressAuthorityError::CommitIndeterminate
        ))
    ));
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .expect("proxy finishes")
        .expect("proxy task");
    drop(lab);
    let recovered_lab = DirectAdmissionLab::postgres(
        fixture.runtime_options.clone(),
        config,
        1,
        typed_bounds(),
        a.connection.clone(),
        DirectAdmissionLimits::new(10, 4096).expect("limits"),
    )
    .expect("fresh lab");
    let recovered = recovered_lab
        .lookup(&a)
        .await
        .expect("reconcile")
        .expect("committed admission");
    assert_eq!(recovered.pin.payload, a.pin.payload);
    assert_eq!(recovered.attempt_token, a.attempt_token);
    let state: i32 = sqlx::query_scalar(&format!(
        "SELECT state FROM {}.ingress_send_attempts WHERE message_key = $1",
        fixture.target
    ))
    .bind(a.pin.key.message_key.to_storage())
    .fetch_one(&fixture.admin)
    .await
    .expect("committed state");
    assert_eq!(state, 1);
    drop(recovered_lab);
    fixture.cleanup().await;
}

#[path = "allocation.rs"]
mod allocation_tests;
