use super::*;
use waddle_server::db::physical_attestation::{
    AdmissionPin, DirectAdmission, DirectAdmissionLab, DirectAdmissionLabError,
    DirectAdmissionLimits, DirectAdmissionOutcome, FixtureConnection,
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

async fn sqlite_attempt(db: &Database, a: &DirectAdmission) {
    let guard = db.guard().await.expect("guard");
    guard.execute("INSERT INTO main.ingress_send_attempts (message_key, kind, semantic_identity_hash, recipient, node_id, node_incarnation, lease_token, expires_at_ms, state, recovered) VALUES (?, ?, ?, ?, ?, ?, ?, 4102444800000, 0, 0)", waddle_server::db_params![a.pin.key.message_key.to_storage().to_string(), a.pin.key.kind.to_storage(), a.pin.key.semantic_identity_hash.to_vec(), a.pin.key.resource.to_string(), a.owner.node_id.clone(), a.owner.node_epoch.clone(), a.attempt_token.to_string()]).await.expect("seed lease");
}

async fn pg_attempt(fixture: &Fixture, a: &DirectAdmission) {
    let mut tx = pg_seed_transaction(fixture).await;
    sqlx::query(&format!("INSERT INTO {}.ingress_send_attempts (message_key, kind, semantic_identity_hash, recipient, node_id, node_incarnation, lease_token, expires_at_ms, state, recovered) VALUES ($1, $2, $3, $4, $5, $6, $7, 4102444800000, 0, 0)", fixture.target))
        .bind(a.pin.key.message_key.to_storage()).bind(a.pin.key.kind.to_storage()).bind(a.pin.key.semantic_identity_hash.as_slice()).bind(a.pin.key.resource.to_string()).bind(&a.owner.node_id).bind(&a.owner.node_epoch).bind(a.attempt_token.to_string()).execute(&mut *tx).await.expect("seed lease");
    tx.commit().await.expect("commit lease");
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
    let admission = request(pin);
    sqlite_attempt(&db, &admission).await;
    let limits = DirectAdmissionLimits::new(1, 4096).expect("limits");
    let lab = DirectAdmissionLab::sqlite(&db, typed_bounds(), admission.connection.clone(), limits)
        .expect("lab");
    let mut wrong = admission.clone();
    wrong.connection.generation = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.start(&wrong).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    wrong = admission.clone();
    wrong.connection.sm_session = SmSessionId::new("wrong-stream");
    assert!(matches!(
        lab.start(&wrong).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    wrong = admission.clone();
    wrong.attempt_token = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.start(&wrong).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    wrong = admission.clone();
    wrong.owner = NodeIdentity::new("wrong-node", "wrong-incarnation");
    assert!(matches!(
        lab.start(&wrong).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    wrong = admission.clone();
    wrong.pin.received_at += ChronoDuration::microseconds(1);
    assert!(matches!(
        lab.start(&wrong).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    wrong = admission.clone();
    wrong.pin.payload = waddle_xmpp::parser::message_from_string("<message xmlns='jabber:client' from='alice@example.test/phone' to='bob@example.test' type='chat'><body>forged</body></message>").expect("message");
    assert!(matches!(
        lab.start(&wrong).await,
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
        lab.start(&admission).await,
        Err(DirectAdmissionLabError::Authority(
            DarkIngressAuthorityError::UnsupportedEpoch
        ))
    ));
    sqlite_exec(&db, "UPDATE main.ingress_protocol_epoch SET epoch = 0, activated_at = NULL, lineage_uuid = NULL WHERE id = 1").await;
    let guard = db.guard().await.expect("guard");
    guard.execute("INSERT INTO main.ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES (?, ?, ?)", waddle_server::db_params![admission.pin.key.message_key.to_storage().to_string(), admission.pin.key.kind.to_storage(), admission.pin.key.semantic_identity_hash.to_vec()]).await.expect("completed effect");
    drop(guard);
    assert!(
        matches!(
            lab.start(&admission).await,
            Err(DirectAdmissionLabError::Rejected)
        ),
        "completed obligation cannot start"
    );
    sqlite_exec(&db, "DELETE FROM main.ingress_effect_receipts").await;
    sqlite_exec(&db, "UPDATE main.ingress_send_attempts SET recovered = 1").await;
    assert!(
        matches!(
            lab.start(&admission).await,
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
            lab.start(&admission).await,
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
        lab.start(&admission).await.expect("record"),
        DirectAdmissionOutcome::Recorded
    );
    assert_eq!(
        lab.start(&admission).await.expect("recover exact"),
        DirectAdmissionOutcome::Recovered
    );
    let recovered = lab
        .lookup(admission.pin.admission_id, admission.attempt_token)
        .await
        .expect("lookup")
        .expect("admission");
    assert_eq!(recovered.pin.payload, admission.pin.payload);
    assert_eq!(recovered.pin.received_at, admission.pin.received_at);
    assert_eq!(recovered.pin.key, admission.pin.key);
    assert_eq!(recovered.connection, admission.connection);
    assert!(matches!(
        lab.lookup(admission.pin.admission_id, uuid::Uuid::new_v4())
            .await,
        Err(DirectAdmissionLabError::Conflict)
    ));
    wrong = admission.clone();
    wrong.attempt_token = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.start(&wrong).await,
        Err(DirectAdmissionLabError::Conflict)
    ));
    let (other, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_sqlite_direct(&db, &other, envelope, intent).await;
    let mut other = request(other);
    other.connection = admission.connection.clone();
    sqlite_attempt(&db, &other).await;
    assert!(matches!(
        lab.start(&other).await,
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
        byte_limited.start(&other).await,
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
    let admission = request(pin);
    sqlite_attempt(&db, &admission).await;
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
        tokio::spawn(async move {
            let _ = started.send(());
            lab.start(&admission).await
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
        lab.start(&admission).await,
        Err(DirectAdmissionLabError::Authority(
            DarkIngressAuthorityError::CommitIndeterminate
        ))
    ));
    assert!(lab
        .lookup(admission.pin.admission_id, admission.attempt_token)
        .await
        .expect("lookup")
        .is_none());
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
        lab.start(&admission).await.expect("retry"),
        DirectAdmissionOutcome::Recorded
    );
}

#[tokio::test]
async fn postgres_direct_admission_serializes_with_gc_and_uses_attested_schema() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, retention) = postgres_fixture(&fixture).await;
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    let admission = request(pin);
    pg_attempt(&fixture, &admission).await;
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
            config,
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
        lab.start(&wrong).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    wrong = admission.clone();
    wrong.attempt_token = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.start(&wrong).await,
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
        tokio::spawn(async move { lab.start(&admission).await })
    };
    wait_for_blocked_application(&fixture.admin, &app, pid).await;
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
    assert_eq!(
        lab.start(&admission).await.expect("recover"),
        DirectAdmissionOutcome::Recovered
    );
    let recovered = lab
        .lookup(admission.pin.admission_id, admission.attempt_token)
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
    pg_attempt(&fixture, &late).await;
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
        lab.start(&late).await,
        Err(DirectAdmissionLabError::Rejected)
    ));
    assert!(lab
        .lookup(late.pin.admission_id, late.attempt_token)
        .await
        .expect("lookup")
        .is_none());
    drop(lab);
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
    let first = request(first);
    pg_attempt(&fixture, &first).await;
    let (second, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &second, envelope, intent).await;
    let mut second = request(second);
    second.connection = first.connection.clone();
    pg_attempt(&fixture, &second).await;
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
        byte_limited.start(&first).await,
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
    let task_a = tokio::spawn(async move { lab_a.start(&first).await });
    let task_b = tokio::spawn(async move { lab_b.start(&second).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE application_name = ANY($1) AND wait_event_type = 'Lock'")
                .bind(vec![app_a.clone(), app_b.clone()]).fetch_one(&fixture.admin).await.expect("observe quota waits");
            if waiting == 2 { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("both writers wait for the quota row");
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
    let admission = request(pin);
    pg_attempt(&fixture, &admission).await;
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
        tokio::spawn(async move { lab.start(&admission).await })
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
        lab.start(&admission).await,
        Err(DirectAdmissionLabError::Authority(
            DarkIngressAuthorityError::CommitIndeterminate
        ))
    ));
    assert!(lab
        .lookup(admission.pin.admission_id, admission.attempt_token)
        .await
        .expect("lookup")
        .is_none());
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
        lab.start(&admission).await.expect("retry"),
        DirectAdmissionOutcome::Recorded
    );
    drop(lab);
    fixture.cleanup().await;
}
