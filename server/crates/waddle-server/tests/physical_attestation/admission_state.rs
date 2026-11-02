use super::*;
use waddle_server::db::physical_attestation::{
    AdmissionPin, AdmissionPinLab, AdmissionPinLabError, PinAllocationBinding, PinOutcome,
    PinState, PinTerminalDisposition,
};
use waddle_xmpp::pending_delivery::SmSessionId;

fn sample_binding(pin: &AdmissionPin) -> PinAllocationBinding {
    PinAllocationBinding {
        admission_id: pin.admission_id,
        accepting_stream: SmSessionId::new("stream-one"),
        sequence: u32::MAX,
    }
}

async fn sqlite_ledger(db: &Database, pin: &AdmissionPin, binding: &PinAllocationBinding) {
    let guard = db.guard().await.expect("runtime guard");
    guard.execute(
        "INSERT INTO main.sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, 0)",
        waddle_server::db_params![pin.key.message_key.to_storage().to_string(), pin.key.kind.to_storage(), pin.key.semantic_identity_hash.to_vec(), pin.key.resource.to_string(), binding.accepting_stream.as_str().to_string(), i64::from(binding.sequence), waddle_xmpp::parser::message_to_string(&pin.payload).expect("payload"), pin.received_at.timestamp_millis()],
    ).await.expect("seed ledger");
}

async fn sqlite_assert_retained(db: &Database, retention: &RetentionLab, pin: &AdmissionPin) {
    let guard = db.guard().await.expect("guard");
    guard
        .execute(
            "UPDATE main.ingress_messages SET retention_eligible_at = ? WHERE message_key = ?",
            waddle_server::db_params![
                (now() - ALIAS_RETENTION).to_rfc3339(),
                pin.key.message_key.to_storage().to_string()
            ],
        )
        .await
        .expect("old frontier");
    drop(guard);
    let batch = retention.collect(now(), 1).await.expect("retention");
    assert_eq!(batch.examined, 1);
    assert_eq!(batch.deleted, 0);
    assert_eq!(
        sqlite_count(db, "SELECT COUNT(*) FROM main.ingress_messages").await,
        1
    );
}

#[tokio::test]
async fn sqlite_observes_exact_ledger_and_never_reverts_terminal() {
    let (db, retention) = sqlite_fixture().await;
    sqlite_exec(
        &db,
        "CREATE TABLE pending_delivery (row_id TEXT PRIMARY KEY)",
    )
    .await;
    let (mut pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    pin.received_at += ChronoDuration::microseconds(123);
    seed_sqlite_direct(&db, &pin, envelope, intent).await;
    let lab = AdmissionPinLab::sqlite(&db, typed_bounds()).expect("lab");
    assert_eq!(lab.admit(&pin).await.expect("admit"), PinOutcome::Created);
    assert!(matches!(
        lab.recover_state(&pin.key)
            .await
            .expect("recover")
            .expect("pin")
            .state,
        PinState::Admitted
    ));
    let guard = db.guard().await.expect("guard");
    guard.execute("INSERT INTO main.ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES (?, ?, ?)", waddle_server::db_params![pin.key.message_key.to_storage().to_string(), pin.key.kind.to_storage(), pin.key.semantic_identity_hash.to_vec()]).await.expect("complete effect");
    guard.execute("CREATE TEMP TABLE ingress_pin_allocations AS SELECT * FROM main.ingress_pin_allocations WHERE 0", ()).await.expect("shadow binding");
    guard.execute("CREATE TEMP TABLE ingress_pin_settlements AS SELECT * FROM main.ingress_pin_settlements WHERE 0", ()).await.expect("shadow settlement");
    guard
        .execute(
            "CREATE TEMP TABLE sm_ingress_appends AS SELECT * FROM main.sm_ingress_appends WHERE 0",
            (),
        )
        .await
        .expect("shadow ledger");
    guard.execute("CREATE TEMP TABLE ingress_protocol_epoch (id INTEGER PRIMARY KEY, epoch INTEGER NOT NULL)", ()).await.expect("shadow epoch");
    guard
        .execute("INSERT INTO temp.ingress_protocol_epoch VALUES (1, 0)", ())
        .await
        .expect("supported shadow");
    guard.execute("UPDATE main.ingress_protocol_epoch SET epoch = 2, activated_at = '2026-11-01T12:00:00Z', lineage_uuid = '00000000-0000-0000-0000-000000000001' WHERE id = 1", ()).await.expect("future main epoch");
    drop(guard);
    assert!(matches!(
        lab.recover_state(&pin.key).await,
        Err(AdmissionPinLabError::Authority(
            DarkIngressAuthorityError::UnsupportedEpoch
        ))
    ));
    sqlite_exec(&db, "UPDATE main.ingress_protocol_epoch SET epoch = 0, activated_at = NULL, lineage_uuid = NULL WHERE id = 1").await;
    sqlite_assert_retained(&db, &retention, &pin).await;

    let binding = sample_binding(&pin);
    assert!(
        matches!(
            lab.bind_existing_allocation(&pin.key, &binding).await,
            Err(AdmissionPinLabError::Rejected)
        ),
        "missing ledger is not proof"
    );
    sqlite_ledger(&db, &pin, &binding).await;
    let guard = db.guard().await.expect("guard");
    guard
        .execute(
            "UPDATE main.sm_ingress_appends SET custody_payload = ?",
            waddle_server::db_params![
                "<message xmlns='jabber:client'><body>forged</body></message>"
            ],
        )
        .await
        .expect("forge payload");
    drop(guard);
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &binding).await,
        Err(AdmissionPinLabError::Rejected)
    ));
    let guard = db.guard().await.expect("guard");
    guard
        .execute(
            "UPDATE main.sm_ingress_appends SET custody_payload = ?, original_receipt_at_ms = ?",
            waddle_server::db_params![
                waddle_xmpp::parser::message_to_string(&pin.payload).expect("payload"),
                pin.received_at.timestamp_millis() + 1
            ],
        )
        .await
        .expect("forge millisecond");
    drop(guard);
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &binding).await,
        Err(AdmissionPinLabError::Rejected)
    ));
    let guard = db.guard().await.expect("guard");
    guard.execute("UPDATE main.sm_ingress_appends SET original_receipt_at_ms = ?, resource = 'bob@example.test/other'", waddle_server::db_params![pin.received_at.timestamp_millis()]).await.expect("forge resource");
    drop(guard);
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &binding).await,
        Err(AdmissionPinLabError::Rejected)
    ));
    let guard = db.guard().await.expect("guard");
    guard
        .execute(
            "UPDATE main.sm_ingress_appends SET resource = ?, semantic_identity_hash = ?",
            waddle_server::db_params![pin.key.resource.to_string(), vec![0_u8; 32]],
        )
        .await
        .expect("forge hash");
    drop(guard);
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &binding).await,
        Err(AdmissionPinLabError::Rejected)
    ));
    let guard = db.guard().await.expect("guard");
    guard.execute("UPDATE main.sm_ingress_appends SET semantic_identity_hash = ?, receipt_kind = receipt_kind + 1", waddle_server::db_params![pin.key.semantic_identity_hash.to_vec()]).await.expect("forge kind");
    drop(guard);
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &binding).await,
        Err(AdmissionPinLabError::Rejected)
    ));
    let guard = db.guard().await.expect("guard");
    guard
        .execute(
            "UPDATE main.sm_ingress_appends SET receipt_kind = ?, accepting_stream_id = 'other'",
            waddle_server::db_params![pin.key.kind.to_storage()],
        )
        .await
        .expect("forge stream");
    drop(guard);
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &binding).await,
        Err(AdmissionPinLabError::Rejected)
    ));
    let guard = db.guard().await.expect("guard");
    guard
        .execute(
            "UPDATE main.sm_ingress_appends SET accepting_stream_id = ?, sequence = 3",
            waddle_server::db_params![binding.accepting_stream.as_str().to_string()],
        )
        .await
        .expect("forge sequence");
    drop(guard);
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &binding).await,
        Err(AdmissionPinLabError::Rejected)
    ));
    let guard = db.guard().await.expect("guard");
    guard
        .execute(
            "UPDATE main.sm_ingress_appends SET sequence = ?",
            waddle_server::db_params![i64::from(binding.sequence)],
        )
        .await
        .expect("restore sequence");
    drop(guard);
    assert_eq!(
        lab.bind_existing_allocation(&pin.key, &binding)
            .await
            .expect("bind sub-ms pin"),
        PinOutcome::Created
    );
    assert_eq!(
        lab.bind_existing_allocation(&pin.key, &binding)
            .await
            .expect("idempotent bind"),
        PinOutcome::Existing
    );
    let recovered = lab
        .recover_state(&pin.key)
        .await
        .expect("recover")
        .expect("pin");
    assert_eq!(recovered.pin.received_at, pin.received_at);
    assert_eq!(recovered.pin.payload, pin.payload);
    assert_eq!(recovered.state, PinState::BoundPending(binding.clone()));
    let mut other = binding.clone();
    other.sequence = 0;
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &other).await,
        Err(AdmissionPinLabError::Conflict)
    ));
    other = binding.clone();
    other.accepting_stream = SmSessionId::new("other-stream");
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &other).await,
        Err(AdmissionPinLabError::Conflict)
    ));
    other = binding.clone();
    other.admission_id = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &other).await,
        Err(AdmissionPinLabError::Conflict)
    ));
    sqlite_assert_retained(&db, &retention, &pin).await;

    assert!(
        matches!(
            lab.settle_existing_allocation(
                &pin.key,
                &binding,
                PinTerminalDisposition::Acknowledged
            )
            .await,
            Err(AdmissionPinLabError::Rejected)
        ),
        "pending and absent session are not terminal evidence"
    );
    sqlite_exec(&db, "DELETE FROM main.sm_ingress_appends").await;
    assert!(
        matches!(
            lab.settle_existing_allocation(
                &pin.key,
                &binding,
                PinTerminalDisposition::Acknowledged
            )
            .await,
            Err(AdmissionPinLabError::Rejected)
        ),
        "missing ledger is not terminal evidence"
    );
    sqlite_ledger(&db, &pin, &binding).await;
    sqlite_exec(&db, "UPDATE main.sm_ingress_appends SET disposition = 1").await;
    assert!(matches!(
        lab.settle_existing_allocation(&pin.key, &binding, PinTerminalDisposition::Promoted)
            .await,
        Err(AdmissionPinLabError::Rejected)
    ));
    assert_eq!(
        lab.settle_existing_allocation(&pin.key, &binding, PinTerminalDisposition::Acknowledged)
            .await
            .expect("settle"),
        PinOutcome::Created
    );
    assert_eq!(
        lab.settle_existing_allocation(&pin.key, &binding, PinTerminalDisposition::Acknowledged)
            .await
            .expect("idempotent settlement"),
        PinOutcome::Existing
    );
    sqlite_exec(&db, "UPDATE main.sm_ingress_appends SET disposition = 2").await;
    assert!(matches!(
        lab.settle_existing_allocation(&pin.key, &binding, PinTerminalDisposition::Promoted)
            .await,
        Err(AdmissionPinLabError::Conflict)
    ));
    let expected = PinState::Terminal {
        binding: binding.clone(),
        disposition: PinTerminalDisposition::Acknowledged,
    };
    assert_eq!(
        lab.recover_state(&pin.key)
            .await
            .expect("recover")
            .expect("pin")
            .state,
        expected
    );
    sqlite_assert_retained(&db, &retention, &pin).await;

    let guard = db.guard().await.expect("guard");
    for sql in [
        "UPDATE main.ingress_pin_allocations SET sequence = 2",
        "DELETE FROM main.ingress_pin_allocations",
        "INSERT OR REPLACE INTO main.ingress_pin_allocations SELECT admission_id, 'other', 2 FROM main.ingress_pin_allocations",
        "INSERT OR REPLACE INTO main.ingress_pin_allocations (rowid, admission_id, accepting_stream_id, sequence) SELECT rowid, 'other', 'other', 2 FROM main.ingress_pin_allocations",
        "UPDATE main.ingress_pin_settlements SET disposition = 2",
        "DELETE FROM main.ingress_pin_settlements",
        "INSERT OR REPLACE INTO main.ingress_pin_settlements SELECT admission_id, 2 FROM main.ingress_pin_settlements",
        "INSERT OR REPLACE INTO main.ingress_pin_settlements (rowid, admission_id, disposition) SELECT rowid, 'other', 2 FROM main.ingress_pin_settlements",
    ] { assert!(guard.execute(sql, ()).await.is_err(), "immutable state: {sql}"); }
    drop(guard);
    assert_eq!(
        lab.recover_state(&pin.key)
            .await
            .expect("recover")
            .expect("pin")
            .state,
        expected
    );
    assert_eq!(
        sqlite_count(&db, "SELECT COUNT(*) FROM temp.ingress_pin_allocations").await,
        0
    );
    assert_eq!(
        sqlite_count(&db, "SELECT COUNT(*) FROM temp.ingress_pin_settlements").await,
        0
    );

    let guard = db.guard().await.expect("guard");
    guard
        .execute(
            "DELETE FROM main.ingress_origin_aliases WHERE message_key = ?",
            waddle_server::db_params![pin.key.message_key.to_storage().to_string()],
        )
        .await
        .expect("delete aliases");
    guard
        .execute(
            "DELETE FROM main.ingress_deliveries WHERE message_key = ?",
            waddle_server::db_params![pin.key.message_key.to_storage().to_string()],
        )
        .await
        .expect("delete deliveries");
    guard
        .execute(
            "DELETE FROM main.ingress_messages WHERE message_key = ?",
            waddle_server::db_params![pin.key.message_key.to_storage().to_string()],
        )
        .await
        .expect("simulate missing canonical");
    drop(guard);
    drop(lab);
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, db.database_url());
    config.pool_size = 1;
    let reopened = Database::from_config("reopened state lab", &config)
        .await
        .expect("reopen shared database with a new pool");
    let restarted = AdmissionPinLab::sqlite(&reopened, typed_bounds()).expect("reopen lab");
    let recovered = restarted
        .recover_state(&pin.key)
        .await
        .expect("recover without canonical")
        .expect("pin");
    assert_eq!(recovered.state, expected);
    assert_eq!(recovered.pin.payload, pin.payload);
    assert_eq!(recovered.pin.received_at, pin.received_at);
    assert!(matches!(
        restarted.bind_existing_allocation(&pin.key, &binding).await,
        Err(AdmissionPinLabError::Rejected)
    ));
    sqlite_exec(
        &reopened,
        "ALTER TABLE main.ingress_pin_allocations RENAME TO hidden_pin_allocations",
    )
    .await;
    assert!(matches!(
        restarted.recover_state(&pin.key).await,
        Err(AdmissionPinLabError::Authority(
            DarkIngressAuthorityError::Database
        ))
    ));
}

#[tokio::test]
async fn sqlite_distinct_admissions_may_share_wrapped_stream_sequence() {
    let (db, _) = sqlite_fixture().await;
    let lab = AdmissionPinLab::sqlite(&db, typed_bounds()).expect("lab");
    for resource in ["bob@example.test/one", "bob@example.test/two"] {
        let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), resource);
        seed_sqlite_direct(&db, &pin, envelope, intent).await;
        lab.admit(&pin).await.expect("admit");
        let binding = sample_binding(&pin);
        sqlite_ledger(&db, &pin, &binding).await;
        assert_eq!(
            lab.bind_existing_allocation(&pin.key, &binding)
                .await
                .expect("bind"),
            PinOutcome::Created
        );
    }
    assert_eq!(sqlite_count(&db, "SELECT COUNT(*) FROM main.ingress_pin_allocations WHERE accepting_stream_id = 'stream-one' AND sequence = 4294967295").await, 2);
}

#[tokio::test]
async fn sqlite_state_cancellation_and_indeterminate_commits_release_one_slot() {
    let (db, _) = sqlite_fixture().await;
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_sqlite_direct(&db, &pin, envelope, intent).await;
    let lab = Arc::new(AdmissionPinLab::sqlite(&db, typed_bounds()).expect("lab"));
    lab.admit(&pin).await.expect("admit");
    let binding = sample_binding(&pin);
    sqlite_ledger(&db, &pin, &binding).await;

    let holder = db.begin_immediate().await.expect("hold single writer slot");
    let (started, observing) = tokio::sync::oneshot::channel();
    let task = {
        let lab = lab.clone();
        let pin = pin.clone();
        let binding = binding.clone();
        tokio::spawn(async move {
            let _ = started.send(());
            lab.bind_existing_allocation(&pin.key, &binding).await
        })
    };
    observing.await.expect("bind task started");
    assert!(!task.is_finished(), "bind waits for the held slot");
    task.abort();
    assert!(task.await.expect_err("cancelled").is_cancelled());
    holder.rollback().await.expect("release slot");

    let mut ddl = db.begin_immediate().await.expect("schema transaction");
    ddl.execute_batch(
        "CREATE TABLE main.commit_probe_parent (id INTEGER PRIMARY KEY); \
         CREATE TABLE main.commit_probe_child (id INTEGER REFERENCES commit_probe_parent(id) DEFERRABLE INITIALLY DEFERRED);",
    ).await.expect("deferred failure tables");
    ddl.commit().await.expect("commit schema");
    for (table, operation) in [
        ("ingress_pin_allocations", "binding"),
        ("ingress_pin_settlements", "settlement"),
    ] {
        sqlite_exec(&db, &format!("CREATE TRIGGER main.reject_state_commit AFTER INSERT ON {table} BEGIN INSERT INTO commit_probe_child (id) VALUES (42); END")).await;
        let result = if operation == "binding" {
            lab.bind_existing_allocation(&pin.key, &binding).await
        } else {
            sqlite_exec(&db, "UPDATE main.sm_ingress_appends SET disposition = 1").await;
            lab.settle_existing_allocation(&pin.key, &binding, PinTerminalDisposition::Acknowledged)
                .await
        };
        assert!(matches!(
            result,
            Err(AdmissionPinLabError::Authority(
                DarkIngressAuthorityError::CommitIndeterminate
            ))
        ));
        let expected = if operation == "binding" {
            PinState::Admitted
        } else {
            PinState::BoundPending(binding.clone())
        };
        assert_eq!(
            lab.recover_state(&pin.key)
                .await
                .expect("recover")
                .expect("pin")
                .state,
            expected
        );
        sqlite_exec(&db, "DROP TRIGGER main.reject_state_commit").await;
        let retry = if operation == "binding" {
            lab.bind_existing_allocation(&pin.key, &binding).await
        } else {
            lab.settle_existing_allocation(&pin.key, &binding, PinTerminalDisposition::Acknowledged)
                .await
        };
        assert_eq!(retry.expect("one-slot retry"), PinOutcome::Created);
    }
}

async fn pg_ledger(fixture: &Fixture, pin: &AdmissionPin, binding: &PinAllocationBinding) {
    sqlx::query(&format!("INSERT INTO {}.sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES ($1, $2, $3, $4, $5, $6, 1, $7, $8, 0)", fixture.target))
        .bind(pin.key.message_key.to_storage().to_string()).bind(pin.key.kind.to_storage()).bind(pin.key.semantic_identity_hash.as_slice()).bind(pin.key.resource.to_string()).bind(binding.accepting_stream.as_str()).bind(i64::from(binding.sequence)).bind(waddle_xmpp::parser::message_to_string(&pin.payload).expect("payload")).bind(pin.received_at.timestamp_millis())
        .execute(&fixture.admin).await.expect("seed ledger");
}

async fn pg_assert_retained(fixture: &Fixture, retention: &RetentionLab, pin: &AdmissionPin) {
    let mut tx = pg_seed_transaction(fixture).await;
    sqlx::query(&format!(
        "UPDATE {}.ingress_messages SET retention_eligible_at = $1 WHERE message_key = $2",
        fixture.target
    ))
    .bind(now() - ALIAS_RETENTION)
    .bind(pin.key.message_key.to_storage())
    .execute(&mut *tx)
    .await
    .expect("old frontier");
    tx.commit().await.expect("commit");
    let batch = retention.collect(now(), 1).await.expect("retention");
    assert_eq!(batch.examined, 1);
    assert_eq!(batch.deleted, 0);
    assert_eq!(pg_count(fixture, "ingress_messages").await, 1);
}

#[tokio::test]
async fn postgres_observes_exact_ledger_under_hostile_path_and_recovers() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, retention) = postgres_fixture(&fixture).await;
    let (mut pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    pin.received_at += ChronoDuration::microseconds(123);
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    let lab = AdmissionPinLab::postgres(
        fixture.runtime_options.clone(),
        config.clone(),
        1,
        typed_bounds(),
    )
    .expect("lab");
    assert_eq!(lab.admit(&pin).await.expect("admit"), PinOutcome::Created);
    assert!(matches!(
        lab.recover_state(&pin.key)
            .await
            .expect("recover")
            .expect("pin")
            .state,
        PinState::Admitted
    ));
    let schema = &fixture.target;
    let mut receipt = pg_seed_transaction(&fixture).await;
    sqlx::query(&format!("INSERT INTO {schema}.ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES ($1, $2, $3)"))
        .bind(pin.key.message_key.to_storage()).bind(pin.key.kind.to_storage()).bind(pin.key.semantic_identity_hash.as_slice()).execute(&mut *receipt).await.expect("complete effect");
    receipt.commit().await.expect("commit effect");
    pg_assert_retained(&fixture, &retention, &pin).await;
    let binding = sample_binding(&pin);
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &binding).await,
        Err(AdmissionPinLabError::Rejected)
    ));
    pg_ledger(&fixture, &pin, &binding).await;
    sqlx::query(&format!(
        "UPDATE {schema}.sm_ingress_appends SET original_receipt_at_ms = $1"
    ))
    .bind(pin.received_at.timestamp_millis() + 1)
    .execute(&fixture.admin)
    .await
    .expect("forge time");
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &binding).await,
        Err(AdmissionPinLabError::Rejected)
    ));
    sqlx::query(&format!("UPDATE {schema}.sm_ingress_appends SET original_receipt_at_ms = $1, custody_payload = '<message xmlns=''jabber:client''><body>forged</body></message>'"))
        .bind(pin.received_at.timestamp_millis()).execute(&fixture.admin).await.expect("forge payload");
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &binding).await,
        Err(AdmissionPinLabError::Rejected)
    ));
    sqlx::query(&format!(
        "UPDATE {schema}.sm_ingress_appends SET custody_payload = $1"
    ))
    .bind(waddle_xmpp::parser::message_to_string(&pin.payload).expect("payload"))
    .execute(&fixture.admin)
    .await
    .expect("restore payload");

    let shadow = format!("state_shadow_{}", uuid::Uuid::new_v4().simple());
    for statement in [
        format!("CREATE SCHEMA {shadow}"),
        format!("CREATE TABLE {shadow}.ingress_pin_allocations (LIKE {schema}.ingress_pin_allocations INCLUDING ALL)"),
        format!("CREATE TABLE {shadow}.ingress_pin_settlements (LIKE {schema}.ingress_pin_settlements INCLUDING ALL)"),
        format!("GRANT USAGE ON SCHEMA {shadow} TO {}", fixture.runtime),
        format!("GRANT ALL ON ALL TABLES IN SCHEMA {shadow} TO {}", fixture.runtime),
        format!("ALTER TABLE {schema}.sm_ingress_appends ENABLE ROW LEVEL SECURITY"),
        format!("CREATE POLICY poison_path ON {schema}.sm_ingress_appends FOR ALL TO {} USING (pg_catalog.set_config('search_path', '{shadow}, {schema}', true) IS NOT NULL)", fixture.runtime),
    ] { sqlx::query(&statement).execute(&fixture.admin).await.expect("hostile path"); }
    assert_eq!(
        lab.bind_existing_allocation(&pin.key, &binding)
            .await
            .expect("bind sub-ms pin"),
        PinOutcome::Created
    );
    assert_eq!(
        lab.bind_existing_allocation(&pin.key, &binding)
            .await
            .expect("same bind"),
        PinOutcome::Existing
    );
    let mut different = binding.clone();
    different.sequence = 0;
    assert!(matches!(
        lab.bind_existing_allocation(&pin.key, &different).await,
        Err(AdmissionPinLabError::Conflict)
    ));
    assert_eq!(
        lab.recover_state(&pin.key)
            .await
            .expect("recover")
            .expect("pin")
            .state,
        PinState::BoundPending(binding.clone())
    );
    pg_assert_retained(&fixture, &retention, &pin).await;
    assert!(matches!(
        lab.settle_existing_allocation(&pin.key, &binding, PinTerminalDisposition::Acknowledged)
            .await,
        Err(AdmissionPinLabError::Rejected)
    ));
    sqlx::query(&format!(
        "UPDATE {schema}.sm_ingress_appends SET disposition = 1"
    ))
    .execute(&fixture.admin)
    .await
    .expect("positive acknowledgement");
    assert_eq!(
        lab.settle_existing_allocation(&pin.key, &binding, PinTerminalDisposition::Acknowledged)
            .await
            .expect("settle"),
        PinOutcome::Created
    );
    assert_eq!(
        lab.settle_existing_allocation(&pin.key, &binding, PinTerminalDisposition::Acknowledged)
            .await
            .expect("same settle"),
        PinOutcome::Existing
    );
    sqlx::query(&format!(
        "UPDATE {schema}.sm_ingress_appends SET disposition = 2"
    ))
    .execute(&fixture.admin)
    .await
    .expect("conflicting ledger");
    assert!(matches!(
        lab.settle_existing_allocation(&pin.key, &binding, PinTerminalDisposition::Promoted)
            .await,
        Err(AdmissionPinLabError::Conflict)
    ));
    let expected = PinState::Terminal {
        binding: binding.clone(),
        disposition: PinTerminalDisposition::Acknowledged,
    };
    assert_eq!(
        lab.recover_state(&pin.key)
            .await
            .expect("recover")
            .expect("pin")
            .state,
        expected
    );
    pg_assert_retained(&fixture, &retention, &pin).await;
    for table in ["ingress_pin_allocations", "ingress_pin_settlements"] {
        for operation in ["DELETE FROM", "TRUNCATE"] {
            assert!(sqlx::query(&format!("{operation} {schema}.{table}"))
                .execute(&fixture.admin)
                .await
                .is_err());
        }
        let shadow_rows: i64 =
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {shadow}.{table}"))
                .fetch_one(&fixture.admin)
                .await
                .expect("shadow count");
        assert_eq!(shadow_rows, 0);
    }
    assert!(sqlx::query(&format!(
        "UPDATE {schema}.ingress_pin_allocations SET sequence = 2"
    ))
    .execute(&fixture.admin)
    .await
    .is_err());
    assert!(sqlx::query(&format!(
        "UPDATE {schema}.ingress_pin_settlements SET disposition = 2"
    ))
    .execute(&fixture.admin)
    .await
    .is_err());

    let (second, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/tablet");
    seed_pg_direct(&fixture, &second, envelope, intent).await;
    lab.admit(&second).await.expect("second admission");
    let same_sequence = sample_binding(&second);
    pg_ledger(&fixture, &second, &same_sequence).await;
    assert_eq!(
        lab.bind_existing_allocation(&second.key, &same_sequence)
            .await
            .expect("reuse wrapping sequence"),
        PinOutcome::Created
    );

    let mut deletion = pg_seed_transaction(&fixture).await;
    for table in [
        "ingress_origin_aliases",
        "ingress_deliveries",
        "ingress_messages",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {schema}.{table} WHERE message_key = $1"
        ))
        .bind(pin.key.message_key.to_storage())
        .execute(&mut *deletion)
        .await
        .expect("simulate canonical removal");
    }
    deletion.commit().await.expect("commit deletion");
    sqlx::query(&format!(
        "DELETE FROM {schema}.sm_ingress_appends WHERE message_key = $1"
    ))
    .bind(pin.key.message_key.to_storage().to_string())
    .execute(&fixture.admin)
    .await
    .expect("simulate ledger loss");
    drop(lab);
    let restarted =
        AdmissionPinLab::postgres(fixture.runtime_options.clone(), config, 1, typed_bounds())
            .expect("reopen lab");
    let recovered = restarted
        .recover_state(&pin.key)
        .await
        .expect("recover without canonical")
        .expect("pin");
    assert_eq!(recovered.state, expected);
    assert_eq!(recovered.pin.payload, pin.payload);
    assert_eq!(recovered.pin.received_at, pin.received_at);
    assert!(matches!(
        restarted.bind_existing_allocation(&pin.key, &binding).await,
        Err(AdmissionPinLabError::Rejected)
    ));
    for epoch in [1_i64, 2] {
        let mut transition = fixture.admin.begin().await.expect("epoch transition");
        sqlx::query(&format!(
            "LOCK TABLE ONLY {schema}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE"
        ))
        .execute(&mut *transition)
        .await
        .expect("epoch lock");
        sqlx::query(&format!("UPDATE {schema}.ingress_protocol_epoch SET epoch = $1, activated_at = now(), lineage_uuid = $2 WHERE id = 1"))
            .bind(epoch).bind(fixture.lineage.lineage_uuid.0).execute(&mut *transition).await.expect("advance epoch");
        transition.commit().await.expect("commit epoch");
    }
    assert!(matches!(
        restarted.recover_state(&pin.key).await,
        Err(AdmissionPinLabError::Authority(
            DarkIngressAuthorityError::UnsupportedEpoch
        ))
    ));
    drop(restarted);
    drop(retention);
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_state_cancellation_and_indeterminate_commits_release_one_slot() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, retention) = postgres_fixture(&fixture).await;
    drop(retention);
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    let app = format!("state_cancel_{}", uuid::Uuid::new_v4().simple());
    let lab = Arc::new(
        AdmissionPinLab::postgres(
            fixture.runtime_options.clone().application_name(&app),
            config,
            1,
            typed_bounds(),
        )
        .expect("lab"),
    );
    lab.admit(&pin).await.expect("admit");
    let binding = sample_binding(&pin);
    pg_ledger(&fixture, &pin, &binding).await;
    let mut holder = fixture.admin.begin().await.expect("holder");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.ingress_messages WHERE message_key = $1 FOR UPDATE",
        fixture.target
    ))
    .bind(pin.key.message_key.to_storage())
    .fetch_one(&mut *holder)
    .await
    .expect("hold canonical");
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .expect("pid");
    let task = {
        let lab = lab.clone();
        let pin = pin.clone();
        let binding = binding.clone();
        tokio::spawn(async move { lab.bind_existing_allocation(&pin.key, &binding).await })
    };
    wait_for_blocked_application(&fixture.admin, &app, holder_pid).await;
    task.abort();
    assert!(task.await.expect_err("cancelled").is_cancelled());
    holder.rollback().await.expect("release");
    let schema = &fixture.target;
    sqlx::query(&format!("CREATE FUNCTION {schema}.reject_state_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected'; END $$"))
        .execute(&fixture.admin).await.expect("injection function");
    for (table, operation) in [
        ("ingress_pin_allocations", "binding"),
        ("ingress_pin_settlements", "settlement"),
    ] {
        sqlx::query(&format!("CREATE CONSTRAINT TRIGGER reject_state_commit AFTER INSERT ON {schema}.{table} DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION {schema}.reject_state_commit()"))
            .execute(&fixture.admin).await.expect("inject commit failure");
        let result = if operation == "binding" {
            lab.bind_existing_allocation(&pin.key, &binding).await
        } else {
            sqlx::query(&format!(
                "UPDATE {schema}.sm_ingress_appends SET disposition = 1"
            ))
            .execute(&fixture.admin)
            .await
            .expect("acknowledge");
            lab.settle_existing_allocation(&pin.key, &binding, PinTerminalDisposition::Acknowledged)
                .await
        };
        assert!(matches!(
            result,
            Err(AdmissionPinLabError::Authority(
                DarkIngressAuthorityError::CommitIndeterminate
            ))
        ));
        let expected = if operation == "binding" {
            PinState::Admitted
        } else {
            PinState::BoundPending(binding.clone())
        };
        assert_eq!(
            lab.recover_state(&pin.key)
                .await
                .expect("recover")
                .expect("pin")
                .state,
            expected
        );
        sqlx::query(&format!(
            "DROP TRIGGER reject_state_commit ON {schema}.{table}"
        ))
        .execute(&fixture.admin)
        .await
        .expect("remove injection");
        let retry = if operation == "binding" {
            lab.bind_existing_allocation(&pin.key, &binding).await
        } else {
            lab.settle_existing_allocation(&pin.key, &binding, PinTerminalDisposition::Acknowledged)
                .await
        };
        assert_eq!(retry.expect("one-slot retry"), PinOutcome::Created);
    }
    drop(lab);
    fixture.cleanup().await;
}
