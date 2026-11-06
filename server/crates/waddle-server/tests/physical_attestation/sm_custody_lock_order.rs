//! Lock-order checks for the unselected PG17 custody candidate.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_server::sm_custody::{
    CandidateSmMemory, CustodyError, CustodyLimits, SmCustodyRepository,
};

use super::{
    multiframe, pg_seed_transaction, uow, wait_for_blocked_application, Candidate,
    CATALOG_GRANT_LOCK,
};

const APPLICATION: &str = "sm-lock-order-candidate";

async fn named_repo(c: &Candidate) -> Arc<SmCustodyRepository> {
    Arc::new(
        SmCustodyRepository::new(
            uow(
                &c.fixture
                    .runtime_options
                    .clone()
                    .application_name(APPLICATION),
                &c.config,
                c.fixture.lineage.deployment_uuid.0,
                &c.capture.owner,
            )
            .await,
            &c.config,
            c.capture.clone(),
            CustodyLimits::new(16, 65536).expect("limits"),
        )
        .expect("repository"),
    )
}

async fn source_before_gate<T, F>(c: &Candidate, source: Uuid, application: &str, work: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let mut conn = PgConnection::connect_with(&c.fixture.admin_options)
        .await
        .expect("source holder");
    let mut tx = conn.begin().await.expect("source transaction");
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await
        .expect("source holder pid");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.ingress_messages WHERE message_key=$1 FOR UPDATE",
        c.fixture.target
    ))
    .bind(source)
    .fetch_one(&mut *tx)
    .await
    .expect("hold source");
    let task = tokio::spawn(work);
    wait_for_blocked_application(&c.fixture.admin, application, pid).await;
    sqlx::query(&format!(
        "SELECT 1 FROM {}.sm_custody_gates WHERE gate_id=$1 FOR UPDATE NOWAIT",
        c.fixture.target
    ))
    .bind(c.capture.gate_id)
    .fetch_one(&mut *tx)
    .await
    .expect("the source holder must be able to acquire the gate");
    tx.commit().await.expect("release source and gate");
    tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .expect("no source/gate deadlock")
        .expect("task")
}

#[tokio::test]
async fn postgres_repository_source_operations_never_hold_gate_while_waiting_for_source() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = named_repo(&c).await;
    let source = c.frame.key.message_key.to_storage();
    let frame = c.frame.clone();
    let lease = c.lease.clone();
    let r = Arc::clone(&repo);
    let admission = source_before_gate(&c, source, APPLICATION, async move {
        r.admit(&frame, &lease).await
    })
    .await
    .expect("admission");
    let mut route = PgConnection::connect_with(&c.fixture.admin_options)
        .await
        .expect("recipient guard holder");
    let mut route_tx = route.begin().await.expect("recipient guard transaction");
    let route_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *route_tx)
        .await
        .expect("recipient guard holder pid");
    sqlx::query(&format!("SELECT 1 FROM {}.sm_custody_route_guards WHERE message_key=$1 AND recipient='bob@example.test' FOR UPDATE", c.fixture.target))
        .bind(source).fetch_one(&mut *route_tx).await.expect("hold bare-recipient guard");
    let r = Arc::clone(&repo);
    let guarded_recovery = repo.handoff_recovery(&c.frame);
    let blocked = tokio::spawn(async move { r.committed_admission(&guarded_recovery).await });
    wait_for_blocked_application(&c.fixture.admin, APPLICATION, route_pid).await;
    sqlx::query(&format!(
        "SELECT 1 FROM {}.sm_custody_gates WHERE gate_id=$1 FOR UPDATE NOWAIT",
        c.fixture.target
    ))
    .bind(c.capture.gate_id)
    .fetch_one(&mut *route_tx)
    .await
    .expect("recipient holder can acquire gate");
    route_tx.commit().await.expect("release route and gate");
    tokio::time::timeout(Duration::from_secs(8), blocked)
        .await
        .expect("recipient/gate deadlock")
        .expect("task")
        .expect("readback");
    let recovery = repo.handoff_recovery(&c.frame);
    let r = Arc::clone(&repo);
    source_before_gate(&c, source, APPLICATION, async move {
        r.committed_admission(&recovery).await
    })
    .await
    .expect("positive readback");
    let r = Arc::clone(&repo);
    let a = admission.clone();
    let slot = source_before_gate(&c, source, APPLICATION, async move { r.bind(&a).await })
        .await
        .expect("slot");
    let handoff = repo
        .resume_preaccept(&repo.handoff_recovery(&c.frame))
        .await
        .expect("source handoff");
    let capture = c.capture.clone();
    let r = Arc::clone(&repo);
    let mut memory = source_before_gate(&c, source, APPLICATION, async move {
        let mut memory = CandidateSmMemory::new(capture);
        r.hydrate_preaccepted(&mut memory, &handoff)
            .await
            .expect("hydrate");
        memory
    })
    .await;
    let r = Arc::clone(&repo);
    let s = slot.clone();
    memory = source_before_gate(&c, source, APPLICATION, async move {
        r.expose(&memory, &s).await.expect("expose");
        memory
    })
    .await;
    drop(memory);
    let r = Arc::clone(&repo);
    source_before_gate(&c, source, APPLICATION, async move {
        r.acknowledge(slot.h()).await
    })
    .await
    .expect("acknowledge");

    let second = multiframe::source(&c, "transfer source").await;
    let second_source = second.0.key.message_key.to_storage();
    let handoff = repo
        .preaccept_local_direct(&second.0, &second.1)
        .await
        .expect("second source handoff");
    let admission = repo
        .committed_admission(&handoff.recovery())
        .await
        .expect("second admission");
    let r = Arc::clone(&repo);
    source_before_gate(&c, second_source, APPLICATION, async move {
        r.transfer(&admission).await
    })
    .await
    .expect("transfer");
    let seal = repo.seal().await.expect("seal");
    let r = Arc::clone(&repo);
    let key = c.frame.key.message_key;
    assert!(!source_before_gate(&c, source, APPLICATION, async move {
        r.collect_terminal(&seal, key).await
    })
    .await
    .expect("fresh retention tail"));
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_ack_retries_when_the_provisional_frontier_changes_before_the_gate() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = named_repo(&c).await;
    let second = multiframe::source(&c, "next exposed slot").await;
    let mut memory = CandidateSmMemory::new(c.capture.clone());
    let mut slots = Vec::new();
    for (frame, lease) in [(&c.frame, &c.lease), (&second.0, &second.1)] {
        let handoff = repo
            .preaccept_local_direct(frame, lease)
            .await
            .expect("handoff");
        let admission = repo
            .committed_admission(&handoff.recovery())
            .await
            .expect("admission");
        let slot = repo.bind(&admission).await.expect("slot");
        repo.hydrate_preaccepted(&mut memory, &handoff)
            .await
            .expect("hydrate");
        slots.push(slot);
    }
    repo.expose(&memory, &slots[0])
        .await
        .expect("first exposed");
    let q = &c.fixture.target;
    sqlx::raw_sql(&format!("CREATE SEQUENCE {q}.sm_ack_retry_probe; CREATE FUNCTION {q}.sm_ack_retry_probe_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM nextval('{q}.sm_ack_retry_probe'); RETURN NEW; END $$; CREATE TRIGGER sm_ack_retry_probe BEFORE INSERT ON {q}.sm_custody_route_guards FOR EACH ROW EXECUTE FUNCTION {q}.sm_ack_retry_probe_fn(); GRANT USAGE, SELECT ON SEQUENCE {q}.sm_ack_retry_probe TO {}", c.fixture.runtime))
        .execute(&c.fixture.admin).await.expect("nontransactional retry witness");
    let mut gate = PgConnection::connect_with(&c.fixture.admin_options)
        .await
        .expect("gate holder");
    let mut tx = gate.begin().await.expect("gate transaction");
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await
        .expect("gate holder pid");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.sm_custody_gates WHERE gate_id=$1 FOR UPDATE",
        c.fixture.target
    ))
    .bind(c.capture.gate_id)
    .fetch_one(&mut *tx)
    .await
    .expect("hold gate");
    let r = Arc::clone(&repo);
    let task = tokio::spawn(async move { r.acknowledge(1).await });
    wait_for_blocked_application(&c.fixture.admin, APPLICATION, pid).await;
    let mut probe = c.fixture.admin.begin().await.expect("probe");
    assert!(
        sqlx::query(&format!(
            "SELECT 1 FROM {}.ingress_messages WHERE message_key=$1 FOR UPDATE NOWAIT",
            c.fixture.target
        ))
        .bind(c.frame.key.message_key.to_storage())
        .fetch_one(&mut *probe)
        .await
        .is_err(),
        "ACK must hold its provisional source before waiting for the gate"
    );
    probe.rollback().await.expect("release failed probe");
    // The second slot was already durably hydrated. Simulate the concurrent
    // exposure's gate mutation, without taking any source lock after the gate.
    sqlx::query(&format!(
        "UPDATE {}.sm_custody_gates SET exposed=2 WHERE gate_id=$1",
        c.fixture.target
    ))
    .bind(c.capture.gate_id)
    .execute(&mut *tx)
    .await
    .expect("advance exposed frontier");
    tx.commit().await.expect("release gate");
    tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .expect("bounded prefix retry")
        .expect("task")
        .expect("revalidated ACK");
    let state: (i64, i64, i64) = sqlx::query_as(&format!(
        "SELECT peer_acked, exposed, (SELECT count(*) FROM {}.sm_custody_terminals) FROM {}.sm_custody_gates WHERE gate_id=$1",
        c.fixture.target, c.fixture.target
    ))
    .bind(c.capture.gate_id)
    .fetch_one(&c.fixture.admin)
    .await
    .expect("settled state");
    assert_eq!(state, (1, 2, 1));
    let attempts: i64 =
        sqlx::query_scalar(&format!("SELECT last_value FROM {q}.sm_ack_retry_probe"))
            .fetch_one(&c.fixture.admin)
            .await
            .expect("retry witness");
    // Initial acquisition, retry acquisition, and the terminal trigger's
    // reentrant acquisition each attempt the guard insert.
    assert_eq!(
        attempts, 3,
        "the changed frontier caused a fresh transaction"
    );
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_ack_rechecks_the_entire_prefix_after_source_wait() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = named_repo(&c).await;
    let handoff = repo
        .preaccept_local_direct(&c.frame, &c.lease)
        .await
        .expect("handoff");
    let admission = repo
        .committed_admission(&handoff.recovery())
        .await
        .expect("admission");
    let slot = repo.bind(&admission).await.expect("slot");
    let mut memory = CandidateSmMemory::new(c.capture.clone());
    repo.hydrate_preaccepted(&mut memory, &handoff)
        .await
        .expect("hydrate");
    repo.expose(&memory, &slot).await.expect("expose");
    let mut holder = PgConnection::connect_with(&c.fixture.admin_options)
        .await
        .expect("source holder");
    let mut tx = holder.begin().await.expect("source transaction");
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await
        .expect("pid");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.ingress_messages WHERE message_key=$1 FOR UPDATE",
        c.fixture.target
    ))
    .bind(c.frame.key.message_key.to_storage())
    .fetch_one(&mut *tx)
    .await
    .expect("hold source");
    let r = Arc::clone(&repo);
    let pending = tokio::spawn(async move { r.acknowledge(1).await });
    wait_for_blocked_application(&c.fixture.admin, APPLICATION, pid).await;
    // A raw candidate terminal can change without moving the frontier. The
    // ACK must notice the changed row, not insert from its stale snapshot.
    sqlx::query(&format!(
        "INSERT INTO {}.sm_custody_terminals (operation_id,disposition) VALUES ($1,1)",
        c.fixture.target
    ))
    .bind(c.frame.operation_id)
    .execute(&mut *tx)
    .await
    .expect("concurrent terminal");
    tx.commit().await.expect("commit terminal");
    let result = tokio::time::timeout(Duration::from_secs(8), pending)
        .await
        .expect("bounded retry")
        .expect("task");
    assert!(
        matches!(result, Err(CustodyError::Conflict)),
        "changed prefix must be revalidated: {result:?}"
    );
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_exact_transfer_retry_survives_canonical_collection() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let handoff = repo
        .preaccept_local_direct(&c.frame, &c.lease)
        .await
        .expect("handoff");
    let admission = repo
        .committed_admission(&handoff.recovery())
        .await
        .expect("admission");
    repo.transfer(&admission).await.expect("transfer");
    let seal = repo.seal().await.expect("seal");
    c.settle_executor().await;
    let mut tx = pg_seed_transaction(&c.fixture).await;
    sqlx::query(&format!("UPDATE {}.ingress_messages SET retention_eligible_at=clock_timestamp()-interval '9 days' WHERE message_key=$1", c.fixture.target))
        .bind(c.frame.key.message_key.to_storage()).execute(&mut *tx).await.expect("age retention tail");
    tx.commit().await.expect("commit retention tail");
    assert!(repo
        .collect_terminal(&seal, c.frame.key.message_key)
        .await
        .expect("collect canonical"));
    repo.transfer(&admission)
        .await
        .expect("exact original retry after collection");
    repo.transfer_recovered(&seal, &c.authority(), &admission)
        .await
        .expect("exact recovery retry after collection");
    let source_exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM {}.ingress_messages WHERE message_key=$1)",
        c.fixture.target
    ))
    .bind(c.frame.key.message_key.to_storage())
    .fetch_one(&c.fixture.admin)
    .await
    .expect("source lookup");
    assert!(!source_exists, "readback does not recreate the source");
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_ack_rejects_a_prefix_larger_than_the_candidate_quota() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    sqlx::query(&format!(
        "UPDATE {}.sm_custody_gates SET appended=1025,exposed=1025 WHERE gate_id=$1",
        c.fixture.target
    ))
    .bind(c.capture.gate_id)
    .execute(&c.fixture.admin)
    .await
    .expect("simulate invalid frontier");
    assert!(matches!(
        repo.acknowledge(1025).await,
        Err(CustodyError::Conflict)
    ));
    let state: (i64, i64) = sqlx::query_as(&format!("SELECT peer_acked,(SELECT count(*) FROM {}.sm_custody_terminals) FROM {}.sm_custody_gates WHERE gate_id=$1", c.fixture.target, c.fixture.target))
        .bind(c.capture.gate_id).fetch_one(&c.fixture.admin).await.expect("unchanged frontier and terminals");
    assert_eq!(state, (0, 0));
    drop(repo);
    c.fixture.cleanup().await;
}

async fn raw_connection(c: &Candidate, application: &str) -> PgConnection {
    let mut conn = PgConnection::connect_with(
        &c.fixture
            .admin_options
            .clone()
            .application_name(application),
    )
    .await
    .expect("raw writer");
    sqlx::query("BEGIN")
        .execute(&mut conn)
        .await
        .expect("begin");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS SHARE MODE",
        c.fixture.target
    ))
    .execute(&mut conn)
    .await
    .expect("epoch prelock");
    sqlx::query("SELECT pg_catalog.set_config('waddle.protocol_epoch', '0', true), pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)")
        .execute(&mut conn).await.expect("epoch proof");
    conn
}

#[tokio::test]
async fn postgres_single_row_raw_triggers_take_source_before_gate() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let source = c.frame.key.message_key.to_storage();
    let q = c.fixture.target.clone();
    let gate = c.capture.gate_id;
    let application = "sm-raw-admission-order";
    let mut conn = raw_connection(&c, application).await;
    let query = format!("INSERT INTO {q}.sm_custody_operations (operation_id,gate_id,message_key,receipt_kind,semantic_identity_hash,resource,attempt_token,payload,received_at) VALUES ($1,$2,$3,99,$4,$5,$6,'raw',clock_timestamp())");
    source_before_gate(&c, source, application, async move {
        sqlx::query(&query)
            .bind(Uuid::new_v4())
            .bind(gate)
            .bind(source)
            .bind([9_u8; 32].as_slice())
            .bind("bob@example.test/laptop")
            .bind(Uuid::new_v4())
            .execute(&mut conn)
            .await
            .expect("raw admission");
        sqlx::query("COMMIT")
            .execute(&mut conn)
            .await
            .expect("commit");
    })
    .await;

    let repo = c.repository();
    repo.admit(&c.frame, &c.lease).await.expect("admit");
    let operation = c.frame.operation_id;
    let application = "sm-raw-slot-order";
    let mut conn = raw_connection(&c, application).await;
    let query =
        format!("INSERT INTO {q}.sm_custody_slots (operation_id,gate_id,ordinal) VALUES ($1,$2,1)");
    source_before_gate(&c, source, application, async move {
        sqlx::query(&query)
            .bind(operation)
            .bind(gate)
            .execute(&mut conn)
            .await
            .expect("raw slot");
        sqlx::query("COMMIT")
            .execute(&mut conn)
            .await
            .expect("commit");
    })
    .await;

    let application = "sm-raw-handoff-order";
    let mut conn = raw_connection(&c, application).await;
    sqlx::query(&format!("UPDATE {q}.ingress_send_attempts SET state=2 WHERE message_key=$1 AND lease_token=(SELECT attempt_token::text FROM {q}.sm_custody_operations WHERE operation_id=$2) AND state=1"))
        .bind(source).bind(operation).execute(&mut conn).await.expect("complete exact attempt in raw transaction");
    let query = format!("INSERT INTO {q}.sm_custody_handoffs (operation_id) VALUES ($1)");
    source_before_gate(&c, source, application, async move {
        sqlx::query(&query)
            .bind(operation)
            .execute(&mut conn)
            .await
            .expect("raw handoff");
        sqlx::query("COMMIT")
            .execute(&mut conn)
            .await
            .expect("commit");
    })
    .await;

    let application = "sm-raw-transfer-order";
    let mut conn = raw_connection(&c, application).await;
    let query = format!("INSERT INTO {q}.sm_custody_transfers (operation_id,resource,payload,received_at) SELECT operation_id,resource,payload,received_at FROM {q}.sm_custody_operations WHERE operation_id=$1");
    source_before_gate(&c, source, application, async move {
        sqlx::query(&query)
            .bind(operation)
            .execute(&mut conn)
            .await
            .expect("raw transfer");
        sqlx::query("COMMIT")
            .execute(&mut conn)
            .await
            .expect("commit");
    })
    .await;

    let application = "sm-raw-terminal-order";
    let mut conn = raw_connection(&c, application).await;
    let query =
        format!("INSERT INTO {q}.sm_custody_terminals (operation_id,disposition) VALUES ($1,2)");
    source_before_gate(&c, source, application, async move {
        sqlx::query(&query)
            .bind(operation)
            .execute(&mut conn)
            .await
            .expect("raw terminal");
        sqlx::query("COMMIT")
            .execute(&mut conn)
            .await
            .expect("commit");
    })
    .await;
    drop(repo);
    c.fixture.cleanup().await;
}
