#![cfg(feature = "test-utils")]
//! Unselected PostgreSQL 17 laboratory for exact account/binding transitions.
//!
//! The authority and all receipts here are synthetic. No production claim,
//! endpoint, UserActor, ConnectionRegistry, socket, SM state, or durable P2
//! custody participates. This tests only this fixture's transaction contract.

#[path = "account_claim_binding_lab/db.rs"]
mod db;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use db::{
    Action, Execution, Fixture, Lookup, Outcome, Pause, Rejection, Request, Source, State,
    SyntheticReceipt, SyntheticRetirement,
};
use jid::FullJid;
use sqlx::PgPool;
use tokio::process::{Child, Command};
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindingState, AccountResourceExecution, AccountResourceMutation,
    AccountResourceOperation, AccountResourceOperationId, AccountResourceOperationOutcome,
    AccountResourceRetirementEvidence, AccountResourceSlot, ActorRegistrationReceipt,
    ActorUnregistrationEvidence, BindingEvidence, BindingRetirementDisposition,
    DirectedPresenceTeardownDisposition, DirectedPresenceTeardownReceipt,
    EndpointPublicationReceipt, EndpointRetirementEvidence, PresentationFlushDisposition,
    PresentationFlushReceipt,
};
use waddle_xmpp::ownership::joint_identity::ResourceLifetime;
use waddle_xmpp::ownership::{
    BindingRef, ClaimEpoch, ConnectionRef, NodeIdentity, NodeLeaseIdentity, UserClaimIdentity,
};

fn claim(node: &str, node_epoch: &str, epoch: i64) -> UserClaimIdentity {
    UserClaimIdentity::fresh(
        "alice@example.test".parse().unwrap(),
        NodeLeaseIdentity::from_node_identity(&NodeIdentity::new(node, node_epoch)),
        ClaimEpoch(epoch),
    )
}

fn source(claim: &UserClaimIdentity) -> Source {
    let owner =
        NodeLeaseIdentity::from_node_identity(&NodeIdentity::new("socket-node", "socket-epoch"));
    Source::new(
        claim.clone(),
        BindingRef::fresh(ConnectionRef::fresh(owner)),
    )
}

async fn request(pool: &PgPool, jid: &FullJid, source: &Source, action: Action) -> Request {
    Request::new(jid, source, &db::slot(pool, jid).await.unwrap(), action)
}

async fn applied(pool: &PgPool, request: &Request) -> Outcome {
    match db::execute(pool, request, Pause::None).await.unwrap() {
        Execution::Applied(outcome) => outcome,
        other => panic!("expected new operation, got {other:?}"),
    }
}

async fn rejected(pool: &PgPool, request: &Request, reason: Rejection) {
    assert_eq!(applied(pool, request).await, Outcome::Rejected(reason));
}

fn model_step(model: &mut AccountResourceSlot, mutation: AccountResourceMutation) {
    let result = model
        .execute(AccountResourceOperation::new(
            AccountResourceOperationId::fresh(),
            mutation,
        ))
        .unwrap();
    assert_eq!(
        result,
        AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
            revision: model.revision()
        })
    );
}

async fn compare(pool: &PgPool, jid: &FullJid, model: &AccountResourceSlot, expected: State) {
    let stored = db::slot(pool, jid).await.unwrap();
    assert_eq!(stored.revision as u64, model.revision().value());
    assert_eq!(stored.state, expected);
    assert_eq!(stored.binding.as_ref(), model.binding_ref());
    let model_state = model.binding_receipt().map(|receipt| receipt.state());
    let expected_model = match expected {
        State::Vacant => None,
        State::Prepared => Some(AccountResourceBindingState::Prepared),
        State::Published => Some(AccountResourceBindingState::Published),
        State::Active => Some(AccountResourceBindingState::Active),
        State::Retiring => Some(AccountResourceBindingState::Retiring),
    };
    assert_eq!(model_state, expected_model);
}

async fn step(
    pool: &PgPool,
    jid: &FullJid,
    source: &Source,
    action: Action,
    model: &mut AccountResourceSlot,
    mutation: AccountResourceMutation,
    expected: State,
) -> Request {
    let request = request(pool, jid, source, action).await;
    assert!(matches!(
        applied(pool, &request).await,
        Outcome::Committed { .. }
    ));
    model_step(model, mutation);
    compare(pool, jid, model, expected).await;
    request
}

#[tokio::test]
async fn exact_identities_replacement_and_retirement_match_model_trace() {
    let jid: FullJid = "alice@example.test/device".parse().unwrap();
    let first_claim = claim("owner", "epoch-1", 7);
    let replacement_claim = claim("owner", "epoch-1", 8);
    let first = source(&first_claim);
    let Some(fixture) = Fixture::new(&jid, &first_claim).await.unwrap() else {
        return;
    };
    db::check_failed_setup_cleanup(&jid, &first_claim)
        .await
        .unwrap();
    fixture.check_marker_fence().await.unwrap();
    let pool = &fixture.pool;
    let mut model = AccountResourceSlot::new(jid.clone(), first_claim.clone()).unwrap();
    let prepared = AccountResourceMutation::Bind {
        claim_identity: first_claim.clone(),
        expected_revision: model.revision(),
        binding_ref: first.binding.clone(),
        lifetime: ResourceLifetime::fresh(),
    };
    step(
        pool,
        &jid,
        &first,
        Action::Prepare,
        &mut model,
        prepared,
        State::Prepared,
    )
    .await;

    let actor_only_changed = Source::new(claim("owner", "epoch-1", 7), first.binding.clone());
    let wrong_claim = request(
        pool,
        &jid,
        &actor_only_changed,
        Action::Publish(SyntheticReceipt::new(&actor_only_changed, 1)),
    )
    .await;
    rejected(pool, &wrong_claim, Rejection::Claim).await;
    for (field, replacement) in [
        ("/claim_epoch", serde_json::json!(8)),
        ("/owner/Clustered/node_id", serde_json::json!("other-owner")),
        ("/owner/Clustered/node_epoch", serde_json::json!("epoch-2")),
    ] {
        let mut encoded = serde_json::to_value(&first_claim).unwrap();
        *encoded.pointer_mut(field).expect("claim identity field") = replacement;
        let changed: UserClaimIdentity = serde_json::from_value(encoded).unwrap();
        assert_eq!(changed.actor_incarnation(), first_claim.actor_incarnation());
        let stale = Source::new(changed, first.binding.clone());
        let stale_claim = request(
            pool,
            &jid,
            &stale,
            Action::Publish(SyntheticReceipt::new(&stale, 1)),
        )
        .await;
        rejected(pool, &stale_claim, Rejection::Claim).await;
    }
    let mut wrong_connection = first.clone();
    wrong_connection.connection = source(&first_claim).connection;
    let inconsistent = request(
        pool,
        &jid,
        &wrong_connection,
        Action::Publish(SyntheticReceipt::new(&wrong_connection, 1)),
    )
    .await;
    rejected(pool, &inconsistent, Rejection::InconsistentSource).await;
    let wrong_attempt = Source::new(
        first_claim.clone(),
        BindingRef::fresh(first.connection.clone()),
    );
    let wrong_binding = request(
        pool,
        &jid,
        &wrong_attempt,
        Action::Publish(SyntheticReceipt::new(&wrong_attempt, 1)),
    )
    .await;
    rejected(pool, &wrong_binding, Rejection::Binding).await;
    let other_connection = source(&first_claim);
    let wrong_transport = request(
        pool,
        &jid,
        &other_connection,
        Action::Publish(SyntheticReceipt::new(&other_connection, 1)),
    )
    .await;
    rejected(pool, &wrong_transport, Rejection::Binding).await;
    let incorrect_receipt = request(
        pool,
        &jid,
        &first,
        Action::Publish(SyntheticReceipt::new(&wrong_attempt, 1)),
    )
    .await;
    rejected(pool, &incorrect_receipt, Rejection::Receipt).await;
    compare(pool, &jid, &model, State::Prepared).await;

    let model_source = BindingEvidence::for_binding(&first.binding);
    let publication = EndpointPublicationReceipt::observed(model_source.clone());
    let publish = AccountResourceMutation::PublishEndpoint {
        claim_identity: first_claim.clone(),
        expected_revision: model.revision(),
        binding_ref: first.binding.clone(),
        publication: publication.clone(),
    };
    step(
        pool,
        &jid,
        &first,
        Action::Publish(SyntheticReceipt::new(&first, 1)),
        &mut model,
        publish,
        State::Published,
    )
    .await;
    let registration =
        ActorRegistrationReceipt::observed(first_claim.clone(), model_source.clone());
    let register = AccountResourceMutation::RegisterActor {
        claim_identity: first_claim.clone(),
        expected_revision: model.revision(),
        binding_ref: first.binding.clone(),
        registration,
    };
    let historic_activation = step(
        pool,
        &jid,
        &first,
        Action::Register(SyntheticReceipt::new(&first, 1)),
        &mut model,
        register,
        State::Active,
    )
    .await;
    assert!(db::current(pool, &jid, &first, 1).await.unwrap());
    let mut other_slot = historic_activation.clone();
    other_slot.full_jid = "alice@example.test/other-device".parse().unwrap();
    assert_eq!(
        db::lookup(pool, &other_slot).await.unwrap(),
        Lookup::PayloadConflict
    );
    assert_eq!(
        db::execute(pool, &other_slot, Pause::None).await.unwrap(),
        Execution::PayloadConflict
    );
    other_slot.id = uuid::Uuid::new_v4();
    rejected(pool, &other_slot, Rejection::UnsupportedSlot).await;

    let replace = AccountResourceMutation::ReplaceClaim {
        claim_identity: first_claim.clone(),
        expected_revision: model.revision(),
        binding_ref: first.binding.clone(),
        replacement_claim_identity: replacement_claim.clone(),
    };
    step(
        pool,
        &jid,
        &first,
        Action::ReplaceClaim(replacement_claim.clone()),
        &mut model,
        replace,
        State::Active,
    )
    .await;
    assert!(!db::current(pool, &jid, &first, 1).await.unwrap());
    let stale_register = request(
        pool,
        &jid,
        &first,
        Action::BeginRetirement {
            custody_known: true,
        },
    )
    .await;
    rejected(pool, &stale_register, Rejection::Claim).await;
    let adopted = Source::new(replacement_claim.clone(), first.binding.clone());
    let premature = request(
        pool,
        &jid,
        &adopted,
        Action::BeginRetirement {
            custody_known: true,
        },
    )
    .await;
    rejected(pool, &premature, Rejection::BindingClaim).await;
    let new_registration =
        ActorRegistrationReceipt::observed(replacement_claim.clone(), model_source.clone());
    let adopt = AccountResourceMutation::AdoptBinding {
        claim_identity: replacement_claim.clone(),
        expected_revision: model.revision(),
        binding_ref: first.binding.clone(),
        expected_binding_claim_identity: first_claim.clone(),
        actor_registration: Some(new_registration.clone()),
    };
    step(
        pool,
        &jid,
        &adopted,
        Action::Adopt {
            previous: first_claim.clone(),
            registration: SyntheticReceipt::new(&adopted, 1),
        },
        &mut model,
        adopt,
        State::Active,
    )
    .await;
    assert!(db::current(pool, &jid, &adopted, 1).await.unwrap());
    assert!(matches!(
        db::execute(pool, &historic_activation, Pause::None)
            .await
            .unwrap(),
        Execution::Replayed(Outcome::Committed { .. })
    ));
    assert!(
        !db::current(pool, &jid, &first, 1).await.unwrap(),
        "replayed historical activation is not current authority"
    );

    let unknown_custody = request(
        pool,
        &jid,
        &adopted,
        Action::BeginRetirement {
            custody_known: false,
        },
    )
    .await;
    rejected(pool, &unknown_custody, Rejection::CustodyUnknown).await;
    let snapshot_revision = model.binding_receipt().unwrap().snapshot_revision();
    let begin = AccountResourceMutation::BeginRetirement {
        claim_identity: replacement_claim.clone(),
        expected_revision: model.revision(),
        binding_ref: first.binding.clone(),
        custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
    };
    step(
        pool,
        &jid,
        &adopted,
        Action::BeginRetirement {
            custody_known: true,
        },
        &mut model,
        begin,
        State::Retiring,
    )
    .await;
    let mut invalid_retirement = SyntheticRetirement::complete(&adopted, 1);
    invalid_retirement.endpoint = SyntheticReceipt::new(&wrong_attempt, 1);
    let invalid = request(
        pool,
        &jid,
        &adopted,
        Action::CompleteRetirement(invalid_retirement),
    )
    .await;
    rejected(pool, &invalid, Rejection::Receipt).await;
    let evidence = AccountResourceRetirementEvidence {
        endpoint: EndpointRetirementEvidence::Unpublished {
            publication,
            removed: model_source.clone(),
        },
        actor: ActorUnregistrationEvidence::Unregistered {
            registration: new_registration,
            removed: model_source.clone(),
        },
        directed_presence: DirectedPresenceTeardownReceipt::observed(
            model_source.clone(),
            snapshot_revision,
            DirectedPresenceTeardownDisposition::NoDirectedRecipients,
        ),
        presentation_flush: PresentationFlushReceipt::observed(
            model_source.clone(),
            snapshot_revision,
            PresentationFlushDisposition::NoPendingPresentation,
        ),
        custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
    };
    let complete = AccountResourceMutation::CompleteRetirement {
        claim_identity: replacement_claim.clone(),
        expected_revision: model.revision(),
        binding_ref: first.binding.clone(),
        evidence: Box::new(evidence),
    };
    let old_cleanup = step(
        pool,
        &jid,
        &adopted,
        Action::CompleteRetirement(SyntheticRetirement::complete(&adopted, 1)),
        &mut model,
        complete,
        State::Vacant,
    )
    .await;
    let reused = request(pool, &jid, &adopted, Action::Prepare).await;
    rejected(pool, &reused, Rejection::ReusedBinding).await;

    // Capture two repeatable-read snapshots before the next installation.
    // One tests the slot lock/version fence; the other tests the unique
    // generation constraint even when the conflicting row is not visible in
    // that transaction's earlier snapshot.
    let mut stale_slot = pool.begin().await.unwrap();
    let mut stale_history = pool.begin().await.unwrap();
    for tx in [&mut stale_slot, &mut stale_history] {
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut **tx)
            .await
            .unwrap();
        let generation: i64 =
            sqlx::query_scalar("SELECT generation FROM lab_slot WHERE full_jid = $1")
                .bind(jid.to_string())
                .fetch_one(&mut **tx)
                .await
                .unwrap();
        assert_eq!(generation, 1);
    }

    // A second attempt on the same transport still has a different BindingRef.
    let successor = Source::new(
        replacement_claim.clone(),
        BindingRef::fresh(first.connection.clone()),
    );
    let prepared = AccountResourceMutation::Bind {
        claim_identity: replacement_claim.clone(),
        expected_revision: model.revision(),
        binding_ref: successor.binding.clone(),
        lifetime: ResourceLifetime::fresh(),
    };
    step(
        pool,
        &jid,
        &successor,
        Action::Prepare,
        &mut model,
        prepared,
        State::Prepared,
    )
    .await;
    let stale_lock = sqlx::query("SELECT generation FROM lab_slot WHERE full_jid = $1 FOR UPDATE")
        .bind(jid.to_string())
        .fetch_one(&mut *stale_slot)
        .await
        .unwrap_err();
    assert_eq!(
        stale_lock
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("40001")
    );
    stale_slot.rollback().await.unwrap();
    let duplicate_hidden_generation =
        sqlx::query("INSERT INTO lab_used_binding VALUES ($1, 2, $2)")
            .bind(jid.to_string())
            .bind(serde_json::to_value(BindingRef::fresh(first.connection.clone())).unwrap())
            .execute(&mut *stale_history)
            .await
            .unwrap_err();
    assert_eq!(
        duplicate_hidden_generation
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23505")
    );
    stale_history.rollback().await.unwrap();
    let successor_source = BindingEvidence::for_binding(&successor.binding);
    let pub2 = AccountResourceMutation::PublishEndpoint {
        claim_identity: replacement_claim.clone(),
        expected_revision: model.revision(),
        binding_ref: successor.binding.clone(),
        publication: EndpointPublicationReceipt::observed(successor_source.clone()),
    };
    step(
        pool,
        &jid,
        &successor,
        Action::Publish(SyntheticReceipt::new(&successor, 2)),
        &mut model,
        pub2,
        State::Published,
    )
    .await;
    let reg2 = AccountResourceMutation::RegisterActor {
        claim_identity: replacement_claim.clone(),
        expected_revision: model.revision(),
        binding_ref: successor.binding.clone(),
        registration: ActorRegistrationReceipt::observed(
            replacement_claim.clone(),
            successor_source,
        ),
    };
    step(
        pool,
        &jid,
        &successor,
        Action::Register(SyntheticReceipt::new(&successor, 2)),
        &mut model,
        reg2,
        State::Active,
    )
    .await;
    let mut delayed = request(
        pool,
        &jid,
        &adopted,
        Action::CompleteRetirement(SyntheticRetirement::complete(&adopted, 1)),
    )
    .await;
    delayed.generation = 1;
    rejected(pool, &delayed, Rejection::RevisionOrGeneration).await;
    let stale_exact = request(
        pool,
        &jid,
        &adopted,
        Action::CompleteRetirement(SyntheticRetirement::complete(&adopted, 1)),
    )
    .await;
    rejected(pool, &stale_exact, Rejection::Binding).await;
    assert!(matches!(
        db::execute(pool, &old_cleanup, Pause::None).await.unwrap(),
        Execution::Replayed(Outcome::Committed { .. })
    ));
    assert!(db::current(pool, &jid, &successor, 2).await.unwrap());

    // Direct database checks ensure non-reuse does not depend on the Rust check.
    let duplicate_generation = sqlx::query("INSERT INTO lab_used_binding VALUES ($1, 1, $2)")
        .bind(jid.to_string())
        .bind(serde_json::to_value(&BindingRef::fresh(first.connection.clone())).unwrap())
        .execute(pool)
        .await;
    assert!(duplicate_generation.is_err());
    let duplicate_binding = sqlx::query("INSERT INTO lab_used_binding VALUES ($1, 3, $2)")
        .bind(jid.to_string())
        .bind(serde_json::to_value(&first.binding).unwrap())
        .execute(pool)
        .await;
    assert!(duplicate_binding.is_err());
    fixture.close().await.unwrap();
}

const CHILD_SCHEMA: &str = "WADDLE_CLAIM_LAB_SCHEMA";
const CHILD_REQUEST: &str = "WADDLE_CLAIM_LAB_REQUEST";
const CHILD_MODE: &str = "WADDLE_CLAIM_LAB_MODE";
const CHILD_READY: &str = "WADDLE_CLAIM_LAB_READY";
const CHILD_RELEASE: &str = "WADDLE_CLAIM_LAB_RELEASE";

#[tokio::test]
#[ignore = "spawned by the parent test with an isolated schema"]
async fn child_worker() {
    let url = waddle_ws_test_support::require_test_postgres_url().expect("child PostgreSQL URL");
    let schema = std::env::var(CHILD_SCHEMA).unwrap();
    let request: Request = serde_json::from_str(&std::env::var(CHILD_REQUEST).unwrap()).unwrap();
    let ready = std::env::var_os(CHILD_READY).unwrap().into();
    let pause = match std::env::var(CHILD_MODE).unwrap().as_str() {
        "before" => Pause::BeforeCommit(ready),
        "after" => Pause::AfterCommit(ready),
        "release" => Pause::BeforeCommitUntilRelease {
            ready,
            release: std::env::var_os(CHILD_RELEASE).unwrap().into(),
        },
        mode => panic!("unknown child mode {mode}"),
    };
    let pool = db::pool_in_schema(&url, &schema).await.unwrap();
    let _ = db::execute(&pool, &request, pause).await.unwrap();
    pool.close().await;
}

fn spawn_child(
    fixture: &Fixture,
    request: &Request,
    mode: &str,
    ready: &Path,
    release: &Path,
) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "child_worker", "--nocapture"])
        .env(CHILD_SCHEMA, &fixture.schema)
        .env(CHILD_REQUEST, serde_json::to_string(request).unwrap())
        .env(CHILD_MODE, mode)
        .env(CHILD_READY, ready)
        .env(CHILD_RELEASE, release)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

async fn wait_for_signal(path: &Path) -> i32 {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child reached its transaction barrier");
    std::fs::read_to_string(path)
        .expect("read backend PID")
        .parse()
        .expect("valid backend PID")
}

async fn wait_for_blocked_backend(pool: &PgPool, contender: i32, holder: i32) {
    assert_ne!(contender, holder, "separate PostgreSQL backends");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1 AND wait_event_type = 'Lock' AND $2 = ANY(pg_catalog.pg_blocking_pids(pid)))",
            )
            .bind(contender)
            .bind(holder)
            .fetch_one(pool)
            .await
            .expect("inspect PostgreSQL lock wait");
            if blocked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("contender is blocked by the exact child backend");
}

#[tokio::test]
async fn multiprocess_cancellation_lost_reply_and_generation_fenced_recovery() {
    let jid: FullJid = "alice@example.test/device".parse().unwrap();
    let owner = claim("owner", "epoch", 1);
    let source = source(&owner);
    let Some(fixture) = Fixture::new(&jid, &owner).await.unwrap() else {
        return;
    };
    let signals = tempfile::tempdir().unwrap();
    let ready = signals.path().join("before");
    let release = signals.path().join("release");

    let prepare = request(&fixture.pool, &jid, &source, Action::Prepare).await;
    let mut child = spawn_child(&fixture, &prepare, "before", &ready, &release);
    let holder_pid = wait_for_signal(&ready).await;
    assert_eq!(
        db::lookup(&fixture.pool, &prepare).await.unwrap(),
        Lookup::Indeterminate,
        "an uncommitted journal row is not evidence of rollback"
    );
    let pool = fixture.pool.clone();
    let retry = prepare.clone();
    let contender_ready = signals.path().join("before-contender");
    let contender_signal = contender_ready.clone();
    let pending = tokio::spawn(async move {
        db::execute(&pool, &retry, Pause::ReportBackend(contender_signal))
            .await
            .unwrap()
    });
    let contender_pid = wait_for_signal(&contender_ready).await;
    wait_for_blocked_backend(&fixture.pool, contender_pid, holder_pid).await;
    child.kill().await.unwrap();
    let _ = child.wait().await.unwrap();
    assert_eq!(
        pending.await.unwrap(),
        Execution::Applied(Outcome::Committed {
            revision: 1,
            generation: 1
        })
    );
    assert_eq!(
        db::lookup(&fixture.pool, &prepare).await.unwrap(),
        Lookup::Recorded(Outcome::Committed {
            revision: 1,
            generation: 1
        })
    );

    // A real COMMIT succeeds in the child; withholding its result and killing
    // it simulates response loss. The marker schedules the test only and is
    // not used as the recovery result.
    let publish = request(
        &fixture.pool,
        &jid,
        &source,
        Action::Publish(SyntheticReceipt::new(&source, 1)),
    )
    .await;
    let ready_after = signals.path().join("after");
    let mut child = spawn_child(&fixture, &publish, "after", &ready_after, &release);
    wait_for_signal(&ready_after).await;
    child.kill().await.unwrap();
    let _ = child.wait().await.unwrap();
    let committed = Outcome::Committed {
        revision: 2,
        generation: 1,
    };
    assert_eq!(
        db::lookup(&fixture.pool, &publish).await.unwrap(),
        Lookup::Recorded(committed.clone())
    );
    assert_eq!(
        db::execute(&fixture.pool, &publish, Pause::None)
            .await
            .unwrap(),
        Execution::Replayed(committed)
    );
    let mut conflict = publish.clone();
    conflict.action = Action::Register(SyntheticReceipt::new(&source, 1));
    assert_eq!(
        db::lookup(&fixture.pool, &conflict).await.unwrap(),
        Lookup::PayloadConflict
    );
    assert_eq!(
        db::execute(&fixture.pool, &conflict, Pause::None)
            .await
            .unwrap(),
        Execution::PayloadConflict
    );
    let unknown = request(
        &fixture.pool,
        &jid,
        &source,
        Action::Register(SyntheticReceipt::new(&source, 1)),
    )
    .await;
    assert_eq!(
        db::lookup(&fixture.pool, &unknown).await.unwrap(),
        Lookup::Indeterminate
    );

    // A separate process wins while a same-revision competitor is blocked.
    let ready_race = signals.path().join("race");
    let register = unknown;
    let mut child = spawn_child(&fixture, &register, "release", &ready_race, &release);
    let holder_pid = wait_for_signal(&ready_race).await;
    let competitor = request(
        &fixture.pool,
        &jid,
        &source,
        Action::Register(SyntheticReceipt::new(&source, 1)),
    )
    .await;
    let pool = fixture.pool.clone();
    let contender_ready = signals.path().join("race-contender");
    let contender_signal = contender_ready.clone();
    let pending = tokio::spawn(async move {
        db::execute(&pool, &competitor, Pause::ReportBackend(contender_signal))
            .await
            .unwrap()
    });
    let contender_pid = wait_for_signal(&contender_ready).await;
    wait_for_blocked_backend(&fixture.pool, contender_pid, holder_pid).await;
    std::fs::write(&release, b"commit").unwrap();
    assert!(child.wait().await.unwrap().success());
    assert_eq!(
        pending.await.unwrap(),
        Execution::Applied(Outcome::Rejected(Rejection::RevisionOrGeneration))
    );
    assert!(db::current(&fixture.pool, &jid, &source, 1).await.unwrap());

    // Lose the retirement reply as well, install a successor on the same JID,
    // then recover the predecessor's exact result without touching generation 2.
    let begin = request(
        &fixture.pool,
        &jid,
        &source,
        Action::BeginRetirement {
            custody_known: true,
        },
    )
    .await;
    assert!(matches!(
        applied(&fixture.pool, &begin).await,
        Outcome::Committed { .. }
    ));
    let retire = request(
        &fixture.pool,
        &jid,
        &source,
        Action::CompleteRetirement(SyntheticRetirement::complete(&source, 1)),
    )
    .await;
    let ready_retire = signals.path().join("retire-after");
    let mut child = spawn_child(&fixture, &retire, "after", &ready_retire, &release);
    wait_for_signal(&ready_retire).await;
    child.kill().await.unwrap();
    let _ = child.wait().await.unwrap();

    let successor = Source::new(owner, BindingRef::fresh(source.connection.clone()));
    for action in [
        Action::Prepare,
        Action::Publish(SyntheticReceipt::new(&successor, 2)),
        Action::Register(SyntheticReceipt::new(&successor, 2)),
    ] {
        let next = request(&fixture.pool, &jid, &successor, action).await;
        assert!(matches!(
            applied(&fixture.pool, &next).await,
            Outcome::Committed { .. }
        ));
    }
    let recorded = db::lookup(&fixture.pool, &retire).await.unwrap();
    let Lookup::Recorded(outcome) = recorded else {
        panic!("predecessor result must be journaled")
    };
    assert!(matches!(outcome, Outcome::Committed { generation: 1, .. }));
    assert_eq!(
        db::execute(&fixture.pool, &retire, Pause::None)
            .await
            .unwrap(),
        Execution::Replayed(outcome)
    );
    let mut delayed = request(
        &fixture.pool,
        &jid,
        &source,
        Action::CompleteRetirement(SyntheticRetirement::complete(&source, 1)),
    )
    .await;
    delayed.generation = 1;
    rejected(&fixture.pool, &delayed, Rejection::RevisionOrGeneration).await;
    assert!(db::current(&fixture.pool, &jid, &successor, 2)
        .await
        .unwrap());
    fixture.close().await.unwrap();
}
