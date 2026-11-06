//! Required PG17 source-to-custody handoff; no live path is selected.

use std::sync::Arc;
use std::time::Duration;

use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_server::ingress_uow::SendAttemptRepository;
use waddle_server::sm_custody::{
    CandidateHydration, CandidateSmMemory, CustodyCapture, CustodyLimits, CustodyRecoveryAuthority,
    HandoffStage, SessionGeneration, SmCustodyRepository,
};
use waddle_xmpp::ownership::{BindingRef, ClaimEpoch, NodeIdentity};

use super::super::super::super::wait_for_blocked_application;
use super::{child_input, child_repository, uow, Candidate, CATALOG_GRANT_LOCK};

async fn named_repo(c: &Candidate, name: &str) -> SmCustodyRepository {
    let named = uow(
        &c.fixture.runtime_options.clone().application_name(name),
        &c.config,
        c.fixture.lineage.deployment_uuid.0,
        &c.capture.owner,
    )
    .await;
    SmCustodyRepository::new(
        named,
        &c.config,
        c.capture.clone(),
        waddle_server::sm_custody::CustodyLimits::new(16, 65536).expect("limits"),
    )
    .expect("named repository")
}

async fn snapshot(c: &Candidate) -> (i64, i64, i64, i64) {
    let q = &c.fixture.target;
    sqlx::query_as(&format!("SELECT (SELECT state::bigint FROM {q}.ingress_send_attempts WHERE message_key = $1), (SELECT count(*) FROM {q}.sm_custody_operations), (SELECT count(*) FROM {q}.sm_custody_slots), (SELECT count(*) FROM {q}.sm_custody_handoffs)"))
        .bind(c.frame.key.message_key.to_storage()).fetch_one(&c.fixture.admin).await.expect("source/custody snapshot")
}

#[tokio::test]
async fn postgres_preaccept_rejects_unprepared_source_before_effects() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::with_prepared(false).await;
    let repo = c.repository();
    assert!(
        repo.preaccept_local_direct(&c.frame, &c.lease)
            .await
            .is_err(),
        "recipient-pass/unprepared source is unsupported"
    );
    assert_eq!(snapshot(&c).await, (0, 0, 0, 0));
    drop(repo);
    c.fixture.cleanup().await;
}

async fn barrier(c: &Candidate, table: &str, key: i64) -> (PgConnection, i32) {
    let mut blocker = PgConnection::connect_with(&c.fixture.admin_options)
        .await
        .expect("independent commit blocker");
    let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await
        .expect("blocker PID");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(key)
        .execute(&mut blocker)
        .await
        .expect("hold barrier");
    let q = &c.fixture.target;
    sqlx::raw_sql(&format!("CREATE FUNCTION {q}.pause_preaccept_commit() RETURNS trigger LANGUAGE plpgsql SET lock_timeout = '3s' AS $$ BEGIN PERFORM pg_catalog.pg_advisory_xact_lock({key}); RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER pause_preaccept_commit AFTER INSERT ON {q}.{table} DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION {q}.pause_preaccept_commit();"))
        .execute(&c.fixture.admin).await.expect("deferred commit barrier");
    (blocker, pid)
}

#[tokio::test]
async fn postgres_preaccept_waits_for_payload_slot_and_source_commits_before_hydration() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    for (table, expected) in [
        ("sm_custody_operations", (0, 0, 0, 0)),
        ("sm_custody_slots", (1, 1, 0, 0)),
        ("sm_custody_handoffs", (1, 1, 1, 0)),
    ] {
        let c = Candidate::new().await;
        let repo = Arc::new(named_repo(&c, "sm-preaccept-barrier").await);
        let mut receiver = CandidateSmMemory::new(c.capture.clone());
        let (mut blocker, blocker_pid) = barrier(&c, table, 2220001).await;
        let writer = Arc::clone(&repo);
        let frame = c.frame.clone();
        let lease = c.lease.clone();
        let task = tokio::spawn(async move { writer.preaccept_local_direct(&frame, &lease).await });
        let pid =
            wait_for_blocked_application(&c.fixture.admin, "sm-preaccept-barrier", blocker_pid)
                .await;
        let committing: bool =
            sqlx::query_scalar("SELECT query = 'COMMIT' FROM pg_stat_activity WHERE pid = $1")
                .bind(pid)
                .fetch_one(&c.fixture.admin)
                .await
                .expect("COMMIT observation");
        assert!(committing);
        assert!(!task.is_finished());
        assert_eq!(snapshot(&c).await, expected);
        assert_eq!(
            (receiver.queued_count(), receiver.accepted_ordinal()),
            (0, None)
        );
        sqlx::query("SELECT pg_advisory_unlock(2220001)")
            .execute(&mut blocker)
            .await
            .expect("release original");
        let handoff = task
            .await
            .expect("source task")
            .expect("known committed handoff");
        assert_eq!(snapshot(&c).await, (2, 1, 1, 1));
        assert_eq!(
            receiver.queued_count(),
            0,
            "source completion is durable custody, not memory or exposure"
        );
        assert_eq!(
            repo.hydrate_preaccepted(&mut receiver, &handoff)
                .await
                .expect("hydrate"),
            CandidateHydration::Inserted
        );
        assert_eq!(
            (receiver.queued_count(), receiver.accepted_ordinal()),
            (1, Some(1))
        );
        assert_eq!(receiver.queued_frame(), Some(&c.frame));
        let repeated = repo
            .preaccept_local_direct(&c.frame, &c.lease)
            .await
            .expect("same source retry");
        assert_eq!(
            repo.hydrate_preaccepted(&mut receiver, &repeated)
                .await
                .expect("idempotent hydrate"),
            CandidateHydration::AlreadyHydrated
        );
        assert_eq!(
            (receiver.queued_count(), receiver.accepted_ordinal()),
            (1, Some(1))
        );
        let restored = repo
            .resume_preaccept(c.frame.operation_id)
            .await
            .expect("exact-ID positive handoff");
        assert_eq!(restored.ordinal(), 1);
        let mut second = c.frame.clone();
        second.operation_id = Uuid::new_v4();
        assert!(repo
            .preaccept_local_direct(&second, &c.lease)
            .await
            .is_err());
        assert_eq!(snapshot(&c).await, (2, 1, 1, 1));
        drop(blocker);
        drop(repo);
        c.fixture.cleanup().await;
    }
}

#[tokio::test]
async fn postgres_preaccept_wrong_repository_retains_handoff_recovery_identity() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let handoff = repo
        .preaccept_local_direct(&c.frame, &c.lease)
        .await
        .expect("committed A handoff");
    let mut other_capture = c.capture.clone();
    other_capture.gate_id = Uuid::new_v4();
    other_capture.session = waddle_xmpp::pending_delivery::SmSessionId::new("session-b");
    other_capture.resource = "bob@example.test/other".parse().expect("resource B");
    other_capture.generation = SessionGeneration::new(8).expect("generation B");
    let other = SmCustodyRepository::new(
        c.uow.clone(),
        &c.config,
        other_capture,
        CustodyLimits::new(16, 65536).expect("limits"),
    )
    .expect("repository B");
    let mut receiver = CandidateSmMemory::new(c.capture.clone());
    let failed = other
        .hydrate_preaccepted(&mut receiver, &handoff)
        .await
        .expect_err("repository B cannot hydrate A");
    assert_eq!(failed.stage(), HandoffStage::Hydration);
    assert_eq!(failed.recovery(), &handoff.recovery());
    assert_eq!(receiver.queued_count(), 0);
    assert_eq!(snapshot(&c).await, (2, 1, 1, 1));
    drop(other);
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_preaccept_rejects_stale_receiver_and_retains_committed_custody() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let handoff = repo
        .preaccept_local_direct(&c.frame, &c.lease)
        .await
        .expect("source handoff");
    let mut variants: Vec<CustodyCapture> = Vec::new();
    let mut binding = c.capture.clone();
    binding.binding = BindingRef::fresh(c.capture.binding.connection_ref().clone());
    variants.push(binding);
    let mut generation = c.capture.clone();
    generation.generation = SessionGeneration::new(8).expect("replacement generation");
    variants.push(generation);
    let mut gate = c.capture.clone();
    gate.gate_id = Uuid::new_v4();
    variants.push(gate);
    let mut session = c.capture.clone();
    session.session = waddle_xmpp::pending_delivery::SmSessionId::new("replacement-session");
    variants.push(session);
    for capture in variants {
        let mut receiver = CandidateSmMemory::new(capture);
        let failed = repo
            .hydrate_preaccepted(&mut receiver, &handoff)
            .await
            .expect_err("mismatched receiver");
        assert_eq!(failed.stage(), HandoffStage::Hydration);
        assert_eq!(failed.recovery().operation_id(), c.frame.operation_id);
        assert_eq!(receiver.queued_count(), 0);
    }
    let mut retired = CandidateSmMemory::new(c.capture.clone());
    retired.retire();
    assert!(repo
        .hydrate_preaccepted(&mut retired, &handoff)
        .await
        .is_err());
    let next = NodeIdentity::new("preaccept-successor", "incarnation-b");
    sqlx::query(&format!(
        "UPDATE {}.clustering_claims SET node_id = $1, node_epoch = $2, claim_epoch = 42",
        c.fixture.target
    ))
    .bind(&next.node_id)
    .bind(&next.node_epoch)
    .execute(&c.fixture.admin)
    .await
    .expect("post-commit claim revocation");
    sqlx::query(&format!("DELETE FROM {}.sessions", c.fixture.target))
        .execute(&c.fixture.admin)
        .await
        .expect("old principal retired");
    let mut receiver = CandidateSmMemory::new(c.capture.clone());
    assert!(repo
        .hydrate_preaccepted(&mut receiver, &handoff)
        .await
        .is_err());
    assert_eq!(receiver.queued_count(), 0);
    let next_uow = uow(
        &c.fixture.runtime_options,
        &c.config,
        c.fixture.lineage.deployment_uuid.0,
        &next,
    )
    .await;
    let authority = CustodyRecoveryAuthority {
        owner: next,
        claim: ClaimEpoch(42),
    };
    let successor = SmCustodyRepository::recover_session(
        next_uow,
        &c.config,
        &c.capture.session,
        &c.capture.resource,
        &authority,
        CustodyLimits::new(16, 65536).expect("limits"),
    )
    .await
    .expect("successor discovers source custody");
    let seal = successor
        .seal_for_recovery(&authority)
        .await
        .expect("successor seals old endpoint");
    let page = successor
        .inventory(&seal, &authority, None)
        .await
        .expect("journal survives revoke");
    assert_eq!(page.operations[0].frame(), &c.frame);
    successor
        .transfer_recovered(
            &seal,
            &authority,
            &page.operations[0]
                .admission(&seal)
                .expect("exact operation"),
        )
        .await
        .expect("retained recovery payload");
    drop(successor);
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_preaccept_rejects_already_started_source_and_missing_marker() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let mut tx = c.uow.begin().await.expect("production-like start");
    assert!(SendAttemptRepository::start(&mut tx, &c.lease)
        .await
        .expect("start"));
    tx.commit().await.expect("prior source start");
    assert!(
        repo.preaccept_local_direct(&c.frame, &c.lease)
            .await
            .is_err(),
        "adapter must branch before production start"
    );
    assert_eq!(snapshot(&c).await, (1, 0, 0, 0));
    drop(repo);
    c.fixture.cleanup().await;

    let c = Candidate::new().await;
    let repo = c.repository();
    let admission = repo.admit(&c.frame, &c.lease).await.expect("payload");
    repo.bind(&admission).await.expect("slot");
    let mut tx = c.uow.begin().await.expect("legacy completion");
    assert!(SendAttemptRepository::complete(&mut tx, &c.lease)
        .await
        .expect("complete"));
    tx.commit().await.expect("completed row alone");
    assert!(
        repo.resume_preaccept(c.frame.operation_id).await.is_err(),
        "Completed row is not a handoff marker"
    );
    assert_eq!(snapshot(&c).await, (2, 1, 1, 0));
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_preaccept_hydration_commit_failure_or_cancellation_discards_only_new_memory() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = Arc::new(named_repo(&c, "sm-preaccept-hydration").await);
    let handoff = repo
        .preaccept_local_direct(&c.frame, &c.lease)
        .await
        .expect("committed source");
    let mut existing = CandidateSmMemory::new(c.capture.clone());
    repo.hydrate_preaccepted(&mut existing, &handoff)
        .await
        .expect("prior committed hydration");
    let q = &c.fixture.target;
    // Disposable read fault: the marker view preserves its rows/shape but its
    // volatile predicate queues a deferred error in the authority transaction.
    // This is test-owned DDL, not a schema or runtime policy selected by code.
    sqlx::raw_sql(&format!("CREATE TABLE {q}.preaccept_hydration_fault (operation_id UUID NOT NULL); CREATE FUNCTION {q}.touch_preaccept_hydration() RETURNS boolean LANGUAGE plpgsql VOLATILE AS $$ BEGIN INSERT INTO {q}.preaccept_hydration_fault VALUES ('{}'::uuid); RETURN TRUE; END $$; ALTER TABLE {q}.sm_custody_handoffs RENAME TO sm_custody_handoffs_storage; CREATE VIEW {q}.sm_custody_handoffs WITH (security_invoker = true) AS SELECT operation_id FROM {q}.sm_custody_handoffs_storage WHERE {q}.touch_preaccept_hydration(); GRANT SELECT ON {q}.sm_custody_handoffs TO {}; GRANT INSERT ON {q}.preaccept_hydration_fault TO {}; CREATE FUNCTION {q}.finish_preaccept_hydration() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'controlled hydration commit failure'; END $$; CREATE CONSTRAINT TRIGGER fail_hydration_commit AFTER INSERT ON {q}.preaccept_hydration_fault DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION {q}.finish_preaccept_hydration();",c.frame.operation_id,c.fixture.runtime,c.fixture.runtime))
        .execute(&c.fixture.admin).await.expect("commit fault injection");
    let mut fresh = CandidateSmMemory::new(c.capture.clone());
    let failed = repo
        .hydrate_preaccepted(&mut fresh, &handoff)
        .await
        .expect_err("commit failed");
    assert_eq!(failed.stage(), HandoffStage::Hydration);
    assert_eq!(failed.recovery().operation_id(), c.frame.operation_id);
    assert_eq!((fresh.queued_count(), fresh.accepted_ordinal()), (0, None));
    assert!(repo
        .hydrate_preaccepted(&mut existing, &handoff)
        .await
        .is_err());
    assert_eq!(
        (existing.queued_count(), existing.accepted_ordinal()),
        (1, Some(1)),
        "prior known committed entry survives failed duplicate hydration"
    );

    sqlx::raw_sql(&format!("CREATE OR REPLACE FUNCTION {q}.finish_preaccept_hydration() RETURNS trigger LANGUAGE plpgsql SET lock_timeout = '3s' AS $$ BEGIN PERFORM pg_catalog.pg_advisory_xact_lock(2220002); RETURN NEW; END $$;"))
        .execute(&c.fixture.admin).await.expect("cancellation barrier function");
    let mut blocker = PgConnection::connect_with(&c.fixture.admin_options)
        .await
        .expect("blocker");
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await
        .expect("blocker PID");
    sqlx::query("SELECT pg_advisory_lock(2220002)")
        .execute(&mut blocker)
        .await
        .expect("hold hydration COMMIT");
    let memory = Arc::new(tokio::sync::Mutex::new(CandidateSmMemory::new(
        c.capture.clone(),
    )));
    let writer = Arc::clone(&repo);
    let target = Arc::clone(&memory);
    let pending = handoff.clone();
    let task = tokio::spawn(async move {
        let mut receiver = target.lock().await;
        writer.hydrate_preaccepted(&mut receiver, &pending).await
    });
    wait_for_blocked_application(&c.fixture.admin, "sm-preaccept-hydration", blocker_pid).await;
    assert!(
        memory.try_lock().is_err(),
        "provisional memory cannot be observed before terminal result"
    );
    task.abort();
    assert!(task.await.expect_err("cancelled hydration").is_cancelled());
    assert_eq!(
        memory.lock().await.queued_count(),
        0,
        "cancelled COMMIT drops new provisional memory"
    );
    sqlx::query("SELECT pg_advisory_unlock(2220002)")
        .execute(&mut blocker)
        .await
        .expect("release backend");
    let retained: i64 =
        sqlx::query_scalar(&format!("SELECT count(*) FROM {q}.sm_custody_operations"))
            .fetch_one(&c.fixture.admin)
            .await
            .expect("durable operation survives");
    assert_eq!(retained, 1);
    drop(blocker);
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "native subprocess entry explicitly invoked by the handoff COMMIT test"]
async fn sm_preaccept_process_child() {
    let (repo, operation) = child_repository().await;
    repo.resume_preaccept(operation)
        .await
        .expect("committed source handoff");
}

#[tokio::test]
async fn postgres_preaccept_origin_death_before_commit_reply_retains_exact_operation() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = Arc::new(c.repository());
    let admission = repo
        .admit(&c.frame, &c.lease)
        .await
        .expect("original payload");
    repo.bind(&admission).await.expect("original slot");
    let receiver = CandidateSmMemory::new(c.capture.clone());
    let (mut blocker, blocker_pid) = barrier(&c, "sm_custody_handoffs", 2220003).await;
    let mut child = tokio::process::Command::new(std::env::current_exe().expect("test binary"))
        .arg("--ignored").arg("--exact")
        .arg("retention_lab_tests::direct_admission_tests::sm_custody_tests::preaccept_handoff::sm_preaccept_process_child")
        .arg("--nocapture").env("WADDLE_SM_CUSTODY_CHILD",child_input(&c).to_string())
        .kill_on_drop(true).spawn().expect("origin process");
    let pid = wait_for_blocked_application(&c.fixture.admin, "sm-custody-child", blocker_pid).await;
    let committing: bool =
        sqlx::query_scalar("SELECT query = 'COMMIT' FROM pg_stat_activity WHERE pid = $1")
            .bind(pid)
            .fetch_one(&c.fixture.admin)
            .await
            .expect("source COMMIT");
    assert!(committing);
    assert_eq!(snapshot(&c).await, (1, 1, 1, 0));
    let origin = child.id().expect("origin PID").to_string();
    assert!(tokio::process::Command::new("/bin/kill")
        .arg("-STOP")
        .arg(&origin)
        .status()
        .await
        .expect("stop origin")
        .success());
    let recovery = Arc::clone(&repo);
    let authority = c.authority();
    let seal_task = tokio::spawn(async move { recovery.seal_for_recovery(&authority).await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        !seal_task.is_finished(),
        "seal cannot infer absence during source COMMIT"
    );
    sqlx::query("SELECT pg_advisory_unlock(2220003)")
        .execute(&mut blocker)
        .await
        .expect("release original");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if snapshot(&c).await == (2, 1, 1, 1) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("source marker and exact attempt completion become visible together");
    child
        .start_kill()
        .expect("origin dies before reading committed reply");
    child.wait().await.expect("origin reaped");
    let seal = seal_task.await.expect("seal task").expect("known seal");
    assert_eq!(
        (receiver.queued_count(), receiver.accepted_ordinal()),
        (0, None)
    );
    let inventory = repo
        .inventory(&seal, &c.authority(), None)
        .await
        .expect("post-origin journal");
    assert_eq!(inventory.operations[0].frame(), &c.frame);
    assert_eq!(
        inventory.operations[0].slot().expect("same slot").ordinal(),
        1
    );
    repo.transfer_recovered(
        &seal,
        &c.authority(),
        &inventory.operations[0]
            .admission(&seal)
            .expect("exact custody"),
    )
    .await
    .expect("recovery payload survives source death");
    drop(blocker);
    drop(repo);
    c.fixture.cleanup().await;
}
