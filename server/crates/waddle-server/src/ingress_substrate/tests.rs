use super::substrate::{
    database_timeout_kind, gc_orphan_proof_batch, should_sweep_orphans, OrphanSweepStep,
    GC_BATCH_LIMIT, GC_CANDIDATES_POSTGRES, GC_CANDIDATES_SQLITE,
};
use super::{
    authority_tests, dialect_sql, discard_database_error, gc_expired_aliases, record_message,
    AliasGcBudget, AliasGcError, AliasGcFailure, AliasGcOutcome, AliasGcProgress, BareJid,
    Database, DatabaseDriver, DatabaseError, DatabaseTimeoutKind, DateTime, DbRetryClass,
    DeliveryKey, Digest, Duration, EffectReceiptKind, IngressOrdinal, IngressSubstrateError,
    Instant, MessageKey, MessageWriteOutcome, NormalizedTarget, Ordering, OriginId,
    PostgresIngressSubstrate, SemanticDigest, Sha256, SmIngressAppendKey, SmIngressId,
    SmIngressReceiptKind, StdDuration, TerminalizeOutcome, Uuid, WireHandledCount, ALIAS_RETENTION,
    EPOCH_GUARDED_TABLES, FAIL_ORPHAN_SWEEP,
};
use crate::db::{DatabaseConfig, MigrationRunner};
use chrono::{TimeZone, Utc};
use sqlx::Connection;
use std::borrow::Cow;
use std::sync::Arc;
use tokio::sync::Barrier;
use waddle_xmpp::ingress::{AliasOutcome, AliasResolution};

fn gc_budget() -> AliasGcBudget {
    AliasGcBudget {
        deadline: Instant::now() + StdDuration::from_secs(30),
        lock_timeout: StdDuration::from_secs(5),
        statement_timeout: StdDuration::from_secs(10),
        scan_timeout: StdDuration::from_secs(10),
        progress: AliasGcProgress::default(),
    }
}

async fn seed_gc_proofs(db: &Database, key: MessageKey, count: usize) {
    let mut tx = db.begin_immediate().await.expect("seed transaction");
    for index in 0..count {
        tx.execute(
                "INSERT INTO sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                crate::db_params![key.to_storage().to_string(), 1i64, vec![7u8; 32], format!("gc@example.test/resource-{index:04}"), "retired-stream", 1i64, 0i64, crate::sm_persistence::codec::serialize_stanza(&waddle_xmpp::Stanza::Message(xmpp_parsers::message::Message::new(None))).expect("serialize custody"), 0i64, 1i64],
            ).await.expect("seed proof");
    }
    tx.commit().await.expect("commit proofs");
}

async fn gc_proof_count(db: &Database) -> i64 {
    let connection = db.guard().await.expect("read proofs");
    let mut rows = connection
        .query("SELECT COUNT(*) FROM sm_ingress_appends", ())
        .await
        .expect("count proofs");
    rows.next()
        .await
        .expect("read count")
        .expect("count row")
        .get(0)
        .expect("decode count")
}

async fn orphan_gc_reclaims_bounded_batches(db: &Database) {
    // Multiple proofs for the SAME message catch incorrectly bounding keys
    // instead of proof rows, and force a second sweep batch.
    seed_gc_proofs(db, MessageKey::new(), GC_BATCH_LIMIT + 1).await;
    let budget = gc_budget();
    assert!(matches!(
        gc_orphan_proof_batch(db, &budget).await.expect("batch"),
        OrphanSweepStep::Advanced { deleted } if deleted == GC_BATCH_LIMIT
    ));
    assert_eq!(gc_proof_count(db).await, 1);
    let outcome = gc_expired_aliases(db, timestamp(7), budget.clone())
        .await
        .expect("GC");
    assert_eq!(
        outcome,
        AliasGcOutcome {
            deleted_messages: 0,
            deleted_orphan_proofs: 1,
            completed: true
        }
    );
    assert_eq!(budget.progress.committed_orphan_proofs(), 1);
    assert_eq!(gc_proof_count(db).await, 0);
}

async fn orphan_gc_preserves_live_parent(db: &Database) {
    let key = MessageKey::new();
    let mut tx = db.begin_immediate().await.expect("begin canonical insert");
    record_message(&mut tx, key, &digest(1), None)
        .await
        .expect("live canonical parent");
    tx.commit().await.expect("commit parent");
    seed_gc_proofs(db, key, 1).await;
    seed_gc_proofs(db, MessageKey::new(), 1).await;
    let outcome = gc_expired_aliases(db, timestamp(7), gc_budget())
        .await
        .expect("GC");
    assert_eq!(
        outcome,
        AliasGcOutcome {
            deleted_messages: 0,
            deleted_orphan_proofs: 1,
            completed: true
        }
    );
    assert_eq!(gc_proof_count(db).await, 1);
    let connection = db.guard().await.expect("read retained proof");
    let mut rows = connection
        .query("SELECT message_key FROM sm_ingress_appends", ())
        .await
        .expect("retained proof");
    let retained: String = rows
        .next()
        .await
        .expect("read row")
        .expect("proof row")
        .get(0)
        .expect("message key");
    assert_eq!(retained, key.to_storage().to_string());
}

async fn orphan_gc_expired_budget_preserves_proofs(db: &Database) {
    seed_gc_proofs(db, MessageKey::new(), 1).await;
    let budget = AliasGcBudget {
        deadline: Instant::now(),
        ..gc_budget()
    };
    assert!(matches!(
        gc_orphan_proof_batch(db, &budget)
            .await
            .expect("expired batch"),
        OrphanSweepStep::BudgetExpired { deleted: 0 }
    ));
    let outcome = gc_expired_aliases(db, timestamp(7), budget.clone())
        .await
        .expect("GC");
    assert_eq!(
        outcome,
        AliasGcOutcome {
            deleted_messages: 0,
            deleted_orphan_proofs: 0,
            completed: false
        }
    );
    assert_eq!(budget.progress.committed_orphan_proofs(), 0);
    assert_eq!(gc_proof_count(db).await, 1);
}

async fn orphan_gc_retains_pending_custody(db: &Database) {
    let key = MessageKey::new();
    seed_gc_proofs(db, key, 1).await;
    db.guard()
        .await
        .expect("connection")
        .execute(
            "UPDATE sm_ingress_appends SET disposition = 0 WHERE message_key = ?",
            crate::db_params![key.to_storage().to_string()],
        )
        .await
        .expect("pending custody");
    let outcome = gc_expired_aliases(db, timestamp(7), gc_budget())
        .await
        .expect("GC");
    assert_eq!(outcome.deleted_orphan_proofs, 0);
    assert_eq!(
        gc_proof_count(db).await,
        1,
        "orphaned payload remains recoverable"
    );
    db.guard()
        .await
        .expect("connection")
        .execute(
            "UPDATE sm_ingress_appends SET disposition = 1 WHERE message_key = ?",
            crate::db_params![key.to_storage().to_string()],
        )
        .await
        .expect("ack custody");
    let outcome = gc_expired_aliases(db, timestamp(7), gc_budget())
        .await
        .expect("GC after ack");
    assert_eq!(outcome.deleted_orphan_proofs, 1);
    assert_eq!(gc_proof_count(db).await, 0);
}

#[tokio::test]
async fn sqlite_orphan_gc_retains_pending_custody() {
    let db = Database::in_memory("custody-gc").await.expect("SQLite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    orphan_gc_retains_pending_custody(&db).await;
}

#[tokio::test]
async fn postgres_orphan_gc_retains_pending_custody() {
    let Some(fixture) = Fixture::open("custody_gc").await else {
        return;
    };
    orphan_gc_retains_pending_custody(&fixture.db).await;
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_orphan_gc_reclaims_bounded_batches() {
    let db = Database::in_memory("orphan-gc").await.expect("SQLite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    orphan_gc_reclaims_bounded_batches(&db).await;
}

#[tokio::test]
async fn postgres_orphan_gc_reclaims_bounded_batches() {
    let Some(fixture) = Fixture::open("orphan").await else {
        return;
    };
    orphan_gc_reclaims_bounded_batches(&fixture.db).await;
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_orphan_gc_preserves_live_parent() {
    let db = Database::in_memory("orphan-gc").await.expect("SQLite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    orphan_gc_preserves_live_parent(&db).await;
}

#[tokio::test]
async fn postgres_orphan_gc_preserves_live_parent() {
    let Some(fixture) = Fixture::open("orphan").await else {
        return;
    };
    orphan_gc_preserves_live_parent(&fixture.db).await;
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_orphan_gc_expired_budget_preserves_proofs() {
    let db = Database::in_memory("orphan-gc").await.expect("SQLite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    orphan_gc_expired_budget_preserves_proofs(&db).await;
}

#[tokio::test]
async fn postgres_orphan_gc_expired_budget_preserves_proofs() {
    let Some(fixture) = Fixture::open("orphan").await else {
        return;
    };
    orphan_gc_expired_budget_preserves_proofs(&fixture.db).await;
    fixture.close().await;
}

async fn seed_healthy_gc_proofs(db: &Database, count: usize) -> MessageKey {
    let key = MessageKey::from_storage(Uuid::from_u128(1));
    let mut tx = db.begin_immediate().await.expect("begin healthy parent");
    record_message(&mut tx, key, &digest(1), None)
        .await
        .expect("healthy parent");
    tx.commit().await.expect("commit healthy parent");
    seed_gc_proofs(db, key, count).await;
    key
}

async fn orphan_gc_advances_across_healthy_windows(db: &Database) {
    let healthy_count = GC_BATCH_LIMIT * 3;
    seed_healthy_gc_proofs(db, healthy_count).await;
    let late = MessageKey::from_storage(Uuid::from_u128(u128::MAX));
    seed_gc_proofs(db, late, 1).await;
    let budget = gc_budget();
    // Each window examines at most `GC_BATCH_LIMIT` rows and advances the
    // cursor by exactly that much, whether or not any of them were orphans.
    // A filter-limited scan would instead examine the whole table looking
    // for matches, which is what could never finish under the scan timeout.
    for page in 0..3 {
        let step = gc_orphan_proof_batch(db, &budget)
            .await
            .expect("healthy window");
        assert!(matches!(step, OrphanSweepStep::Advanced { deleted: 0 }));
        assert_eq!(gc_proof_count(db).await, healthy_count as i64 + 1);
        let position = budget.progress.orphan_cursor.lock().await.clone();
        let key = position.as_ref().expect("full window retains cursor");
        assert_eq!(
            key.resource.to_string(),
            format!(
                "gc@example.test/resource-{:04}",
                (page + 1) * GC_BATCH_LIMIT - 1
            )
        );
    }
    let step = gc_orphan_proof_batch(db, &budget)
        .await
        .expect("late orphan window");
    assert!(matches!(step, OrphanSweepStep::Finished { deleted: 1 }));
    assert_eq!(gc_proof_count(db).await, healthy_count as i64);

    // One pass drains the whole traversal within its budget rather than a
    // single window. Windowing at one-per-pass would traverse at roughly
    // `GC_BATCH_LIMIT` rows per coordinator backoff, which time-ordered
    // UUIDv7 keys can outrun, leaving an orphan behind an ever-growing tail.
    let late = MessageKey::from_storage(Uuid::from_u128(u128::MAX - 1));
    seed_gc_proofs(db, late, 1).await;
    let drained = gc_budget();
    let outcome = gc_expired_aliases(db, timestamp(7), drained.clone())
        .await
        .expect("single draining pass");
    assert_eq!(outcome.deleted_orphan_proofs, 1);
    assert_eq!(outcome.deleted_messages, 0);
    assert!(outcome.completed);
    assert_eq!(drained.progress.committed_orphan_proofs(), 1);
    assert_eq!(gc_proof_count(db).await, healthy_count as i64);
}

async fn orphan_gc_expired_first_window_is_not_traversal_completion(db: &Database) {
    seed_healthy_gc_proofs(db, GC_BATCH_LIMIT).await;
    let budget = AliasGcBudget {
        deadline: Instant::now(),
        ..gc_budget()
    };
    // The cursor is `None` here both because nothing ran and because a short
    // window would have reset it. Only the step distinguishes them, and
    // reporting completion would cost the coordinator its prompt partial
    // continuation, leaving reclamation to the next periodic tick.
    let step = gc_orphan_proof_batch(db, &budget)
        .await
        .expect("expired budget is not an error");
    assert!(matches!(
        step,
        OrphanSweepStep::BudgetExpired { deleted: 0 }
    ));
    assert!(budget.progress.orphan_cursor.lock().await.is_none());
}

async fn orphan_sweep_is_skipped_when_it_cannot_find_anything(db: &Database) {
    // First pass sweeps: a fresh process has never swept, so it is due.
    let budget = gc_budget();
    let first = gc_expired_aliases(db, timestamp(7), budget.clone())
        .await
        .expect("first pass sweeps");
    assert!(first.completed);

    // An orphan appears after that sweep, with no canonical row reclaimed.
    seed_gc_proofs(db, MessageKey::new(), 1).await;
    let quiet = gc_expired_aliases(db, timestamp(7), budget.fresh_progress_pass())
        .await
        .expect("quiet pass");
    assert!(quiet.completed);
    assert_eq!(quiet.deleted_orphan_proofs, 0);
    // Deliberate: nothing waits on an orphan, so an idle pass does not pay a
    // transaction to look for one. The interval reclaims it later.
    assert_eq!(gc_proof_count(db).await, 1);
}

#[tokio::test]
async fn orphan_sweep_gate_admits_only_passes_that_can_find_something() {
    let budget = gc_budget();
    // A fresh process has never swept, so the first pass is due even idle.
    assert!(should_sweep_orphans(&budget).await);
    // Being admitted ARMS the sweep; it stays owed until a traversal
    // finishes, so a retry after an interrupted pass is still admitted.
    assert!(should_sweep_orphans(&budget).await);
    // Finishing a traversal is what clears it and restarts the interval.
    budget
        .progress
        .orphan_sweep_pending
        .store(false, Ordering::Release);
    *budget.progress.orphan_swept_at.lock().await = Some(Instant::now());
    assert!(!should_sweep_orphans(&budget).await);
    // A reclaimed canonical row re-arms it regardless of the interval.
    budget.progress.mark_orphan_sweep_pending();
    assert!(should_sweep_orphans(&budget).await);

    // Mid-traversal always continues: parking a cursor would defeat the
    // paging that keeps each window bounded.
    let mid = gc_budget();
    *mid.progress.orphan_swept_at.lock().await = Some(Instant::now());
    *mid.progress.orphan_cursor.lock().await = Some(SmIngressAppendKey {
        message_key: MessageKey::new(),
        kind: SmIngressReceiptKind::from_storage(3),
        semantic_identity_hash: [7; 32],
        resource: "gc@example.test/resource".parse().expect("resource"),
    });
    assert!(should_sweep_orphans(&mid).await);
}

#[tokio::test]
async fn an_owed_sweep_survives_a_pass_that_never_reached_the_ledger() {
    let budget = gc_budget();
    // Admit a pass, then end it before the traversal finished.
    assert!(should_sweep_orphans(&budget).await);
    // A production retry carries fresh pass-local counters over the same
    // shared state, so nothing in the retry itself says a sweep is owed.
    let retry = budget.fresh_progress_pass();
    assert_eq!(retry.progress.committed_orphan_proofs(), 0);
    assert!(retry.progress.orphan_cursor.lock().await.is_none());
    // It is still owed, so the retry sweeps rather than waiting out the
    // interval with orphans already on the floor.
    assert!(should_sweep_orphans(&retry).await);
}

#[tokio::test]
async fn a_deletion_arms_the_sweep_even_when_its_pass_runs_out_of_budget() {
    let budget = gc_budget();
    // Consume the startup allowance so only the deletion can arm it.
    assert!(should_sweep_orphans(&budget).await);
    budget
        .progress
        .orphan_sweep_pending
        .store(false, Ordering::Release);
    *budget.progress.orphan_swept_at.lock().await = Some(Instant::now());
    assert!(!should_sweep_orphans(&budget).await);

    // A canonical phase that reclaims rows and then runs out of budget marks
    // the sweep owed before returning, because its retry cannot see the
    // deletion in its own fresh counters.
    budget.progress.mark_orphan_sweep_pending();
    let retry = budget.fresh_progress_pass();
    assert!(should_sweep_orphans(&retry).await);
}

async fn orphan_gc_failure_does_not_fail_the_pass(db: &Database) {
    let healthy = GC_BATCH_LIMIT * 2;
    seed_healthy_gc_proofs(db, healthy).await;
    let budget = gc_budget();
    // The pass must report incomplete, never `Err`: the maintenance pass that
    // wraps it maps a failed GC phase onto `MaintenanceOutcome::Failed`, which
    // also governs the unrelated recovery phase's accounting and backoff. A
    // sweep of rows nothing is waiting on must not drag recovery down.
    let outcome = FAIL_ORPHAN_SWEEP
        .scope(true, gc_expired_aliases(db, timestamp(7), budget.clone()))
        .await
        .expect("sweep failure must not fail the pass");
    assert!(!outcome.completed);
    assert_eq!(outcome.deleted_orphan_proofs, 0);
    assert_eq!(gc_proof_count(db).await, healthy as i64);
    // The next pass is unaffected and completes the traversal normally.
    let recovered = gc_expired_aliases(db, timestamp(7), gc_budget())
        .await
        .expect("later pass runs normally");
    assert!(recovered.completed);
}

async fn orphan_gc_wraps_after_healthy_traversal(db: &Database) {
    let healthy_count = GC_BATCH_LIMIT * 2;
    seed_healthy_gc_proofs(db, healthy_count).await;
    let budget = gc_budget();
    // A draining pass walks every window and resets the traversal at the end.
    let outcome = gc_expired_aliases(db, timestamp(7), budget.clone())
        .await
        .expect("full healthy traversal");
    assert!(outcome.completed);
    assert_eq!(outcome.deleted_orphan_proofs, 0);
    assert!(budget.progress.orphan_cursor.lock().await.is_none());
    // A proof landing before the traversal's old position is still reclaimed,
    // because the reset sends the next pass back to the beginning.
    let early = MessageKey::from_storage(Uuid::nil());
    seed_gc_proofs(db, early, 1).await;
    let wrapped = gc_budget();
    let outcome = gc_expired_aliases(db, timestamp(7), wrapped.clone())
        .await
        .expect("wrapped traversal");
    assert_eq!(outcome.deleted_orphan_proofs, 1);
    assert_eq!(outcome.deleted_messages, 0);
    assert!(outcome.completed);
    assert_eq!(gc_proof_count(db).await, healthy_count as i64);
}

#[tokio::test]
async fn sqlite_orphan_gc_advances_across_healthy_windows() {
    let db = Database::in_memory("orphan-gc").await.expect("SQLite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    orphan_gc_advances_across_healthy_windows(&db).await;
}

#[tokio::test]
async fn postgres_orphan_gc_advances_across_healthy_windows() {
    let Some(fixture) = Fixture::open("orphan").await else {
        return;
    };
    orphan_gc_advances_across_healthy_windows(&fixture.db).await;
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_orphan_gc_expired_first_window_is_not_traversal_completion() {
    let db = Database::in_memory("orphan-gc-expired-first")
        .await
        .expect("SQLite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    orphan_gc_expired_first_window_is_not_traversal_completion(&db).await;
}

#[tokio::test]
async fn postgres_orphan_gc_expired_first_window_is_not_traversal_completion() {
    let Some(fixture) = Fixture::open("orphan_expired_first").await else {
        return;
    };
    orphan_gc_expired_first_window_is_not_traversal_completion(&fixture.db).await;
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_orphan_sweep_is_skipped_when_it_cannot_find_anything() {
    let db = Database::in_memory("orphan-gate-skip")
        .await
        .expect("SQLite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    orphan_sweep_is_skipped_when_it_cannot_find_anything(&db).await;
}

#[tokio::test]
async fn sqlite_orphan_gc_failure_does_not_fail_the_pass() {
    let db = Database::in_memory("orphan-gc-failure")
        .await
        .expect("SQLite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    orphan_gc_failure_does_not_fail_the_pass(&db).await;
}

#[tokio::test]
async fn postgres_orphan_gc_failure_does_not_fail_the_pass() {
    let Some(fixture) = Fixture::open("orphan_sweep_failure").await else {
        return;
    };
    orphan_gc_failure_does_not_fail_the_pass(&fixture.db).await;
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_orphan_gc_wraps_after_healthy_traversal() {
    let db = Database::in_memory("orphan-gc").await.expect("SQLite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    orphan_gc_wraps_after_healthy_traversal(&db).await;
}

#[tokio::test]
async fn postgres_orphan_gc_wraps_after_healthy_traversal() {
    let Some(fixture) = Fixture::open("orphan").await else {
        return;
    };
    orphan_gc_wraps_after_healthy_traversal(&fixture.db).await;
    fixture.close().await;
}

async fn descendant_test_uow(db: &Database) -> crate::ingress_uow::IngressUnitOfWork {
    let config = crate::config::LineageConfig {
        deployment_uuid: Some(
            "018f47b2-4b2e-7a3a-9a4c-52a5a6a90001"
                .parse()
                .expect("deployment UUID"),
        ),
        action: None,
    };
    crate::db::lineage::enroll(db, &config)
        .await
        .expect("enroll fixture");
    crate::ingress_uow::IngressUnitOfWork::open(db.clone(), config).expect("open UoW")
}

async fn descendants_hold_canonical_evidence_until_every_provider_settles(db: &Database) {
    use crate::ingress_uow::{
        CanonicalMessageRepository, DeliveryEffectRepository, EffectDeliveryBinding,
        EffectDescendantRepository, EffectIntentRepository, EffectReceiptRepository,
    };
    use waddle_xmpp::ingress::{IngressEffectIntent, IngressEffectKey};
    let uow = descendant_test_uow(db).await;
    let store = PostgresIngressSubstrate::open(db.clone()).expect("store");
    let key = MessageKey::new();
    let recipient: BareJid = "target@example.com".parse().expect("recipient");
    let effect = IngressEffectIntent::Extension {
        recipient: recipient.clone(),
        stanza_id: waddle_xmpp_core::xep0359::StanzaId::new("canonical", recipient.clone().into()),
    };
    let semantic = effect.semantic_key();
    let candidate = Uuid::new_v4();
    let first_provider = Uuid::new_v4();
    let last_provider = Uuid::new_v4();
    let terminal_at = timestamp(1);
    let mut tx = uow.begin().await.expect("begin");
    let alias = OriginId::new("descendant-dedup-origin");
    assert_eq!(
        CanonicalMessageRepository::resolve_and_record_alias(
            &mut tx,
            &sender(),
            &target(),
            &alias,
            &digest(1),
            || key,
        )
        .await
        .expect("canonical alias"),
        inserted(key)
    );
    EffectIntentRepository::reconcile(&mut tx, key, &[effect], false)
        .await
        .expect("intent");
    let binding = DeliveryEffectRepository::bind_effect(&mut tx, key, &semantic)
        .await
        .expect("bind");
    let EffectDeliveryBinding::Bound(delivery) = binding else {
        panic!("extension is supported");
    };
    // A different target cannot borrow the accepted message's authority.
    assert!(matches!(
        DeliveryEffectRepository::bind_effect(
            &mut tx,
            key,
            &IngressEffectKey::Extension("foreign@example.com".parse().expect("foreign"))
        )
        .await,
        Err(crate::ingress_uow::IngressUowError::EffectIntentConflict)
    ));
    EffectDescendantRepository::attach(&mut tx, key, &semantic, candidate)
        .await
        .expect("candidate custody");
    let hash: [u8; 32] = Sha256::digest(semantic.storage_identity().as_bytes()).into();
    EffectReceiptRepository::record_receipt(
        &mut tx,
        key,
        EffectReceiptKind::from_storage(semantic.storage_kind()),
        &hash,
    )
    .await
    .expect("canonical receipt");
    CanonicalMessageRepository::terminalize(&mut tx, key, terminal_at)
        .await
        .expect("terminal");
    tx.commit().await.expect("commit");
    assert_eq!(
        store
            .gc_expired_aliases(terminal_at + Duration::days(20), gc_budget())
            .await
            .expect("pending candidate GC")
            .deleted_messages,
        0
    );
    let mut tx = uow.begin().await.expect("fanout");
    EffectDescendantRepository::copy(&mut tx, candidate, first_provider)
        .await
        .expect("first provider ancestry");
    EffectDescendantRepository::copy(&mut tx, candidate, last_provider)
        .await
        .expect("last provider ancestry");
    EffectDescendantRepository::settle_all(&mut tx, candidate, terminal_at + Duration::days(21))
        .await
        .expect("candidate accepted");
    EffectDescendantRepository::settle_all(
        &mut tx,
        first_provider,
        terminal_at + Duration::days(21),
    )
    .await
    .expect("first provider settles");
    tx.commit().await.expect("commit fanout");
    assert_eq!(
        store
            .gc_expired_aliases(terminal_at + Duration::days(40), gc_budget())
            .await
            .expect("pending final provider GC")
            .deleted_messages,
        0
    );
    let mut tx = uow.begin().await.expect("binding replay");
    assert_eq!(
        CanonicalMessageRepository::resolve_and_record_alias(
            &mut tx,
            &sender(),
            &target(),
            &alias,
            &digest(1),
            MessageKey::new,
        )
        .await
        .expect("alias still deduplicates with pending descendant"),
        existing(key)
    );
    assert_eq!(
        DeliveryEffectRepository::lookup(&mut tx, delivery)
            .await
            .expect("lookup retained binding"),
        Some(key)
    );
    assert_eq!(
        DeliveryEffectRepository::bind_effect(&mut tx, key, &semantic)
            .await
            .expect("same-key replay"),
        binding
    );
    let settled = terminal_at + Duration::days(41);
    EffectDescendantRepository::settle_all(&mut tx, last_provider, settled)
        .await
        .expect("last provider settles");
    tx.commit().await.expect("settled");
    assert_eq!(
        store
            .gc_expired_aliases(
                settled + ALIAS_RETENTION - Duration::microseconds(1),
                gc_budget()
            )
            .await
            .expect("full tail")
            .deleted_messages,
        0
    );
    assert_eq!(
        store
            .gc_expired_aliases(settled + ALIAS_RETENTION, gc_budget())
            .await
            .expect("exact tail boundary")
            .deleted_messages,
        1
    );
    let mut tx = uow.begin().await.expect("reclaimed");
    assert_eq!(
        DeliveryEffectRepository::lookup(&mut tx, delivery)
            .await
            .expect("lookup gone binding"),
        None
    );
    tx.commit().await.expect("commit read");
}

#[tokio::test]
async fn sqlite_descendants_hold_canonical_evidence_until_every_provider_settles() {
    let db = Database::in_memory("descendant-retention")
        .await
        .expect("SQLite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    descendants_hold_canonical_evidence_until_every_provider_settles(&db).await;
}

#[tokio::test]
async fn postgres_descendants_hold_canonical_evidence_until_every_provider_settles() {
    let Some(fixture) = Fixture::open("descendant_retention").await else {
        return;
    };
    descendants_hold_canonical_evidence_until_every_provider_settles(&fixture.db).await;
    fixture.close().await;
}

async fn last_sm_reference_starts_a_fresh_full_retention_tail(db: &Database) {
    use crate::ingress_uow::{CanonicalMessageRepository, SmIngressRepository};
    let uow = descendant_test_uow(db).await;
    let store = PostgresIngressSubstrate::open(db.clone()).expect("store");
    let key = MessageKey::new();
    let sm = SmIngressId::new();
    let now = Utc::now();
    let mut stream_tx = store.begin().await.expect("stream fixture");
    authority_tests::insert_stream(&mut stream_tx, sm).await;
    stream_tx.commit().await.expect("stream fixture commit");
    let mut tx = uow.begin().await.expect("begin");
    CanonicalMessageRepository::record_message(&mut tx, key, &digest(1), None)
        .await
        .expect("message");
    SmIngressRepository::insert_sm_ref(
        &mut tx,
        sm,
        IngressOrdinal::FIRST,
        WireHandledCount::from_storage(1),
        key,
    )
    .await
    .expect("SM custody");
    CanonicalMessageRepository::terminalize(&mut tx, key, now - Duration::days(20))
        .await
        .expect("terminal");
    tx.commit().await.expect("commit");
    assert_eq!(
        store
            .gc_expired_aliases(now, gc_budget())
            .await
            .expect("live SM ref GC")
            .deleted_messages,
        0
    );
    let mut tx = uow.begin().await.expect("retire stream");
    SmIngressRepository::delete_stream_ref(&mut tx, sm, IngressOrdinal::FIRST)
        .await
        .expect("last reference settles");
    tx.commit().await.expect("retire");
    assert_eq!(
        store
            .gc_expired_aliases(now + ALIAS_RETENTION - Duration::seconds(1), gc_budget())
            .await
            .expect("fresh full tail")
            .deleted_messages,
        0
    );
    assert_eq!(
        store
            .gc_expired_aliases(
                Utc::now() + ALIAS_RETENTION + Duration::seconds(1),
                gc_budget()
            )
            .await
            .expect("after fresh tail")
            .deleted_messages,
        1
    );
}

#[tokio::test]
async fn sqlite_last_sm_reference_starts_a_fresh_full_retention_tail() {
    let db = Database::in_memory("sm-retention").await.expect("SQLite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    last_sm_reference_starts_a_fresh_full_retention_tail(&db).await;
}

#[tokio::test]
async fn postgres_last_sm_reference_starts_a_fresh_full_retention_tail() {
    let Some(fixture) = Fixture::open("last_sm_tail").await else {
        return;
    };
    last_sm_reference_starts_a_fresh_full_retention_tail(&fixture.db).await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres_canonical_nowait_contention_is_retryable_and_preserves_authority() {
    use crate::ingress_uow::{
        CanonicalMessageRepository, DbRetryClass, EffectDescendantRepository, IngressUowError,
    };
    let Some(fixture) = Fixture::open("canonical_nowait").await else {
        return;
    };
    let uow = descendant_test_uow(&fixture.db).await;
    let key = fixture.record_message().await;
    let mut holder = uow.begin().await.expect("holder");
    assert!(CanonicalMessageRepository::lock(&mut holder, key)
        .await
        .expect("lock parent"));
    let mut contender = uow.begin().await.expect("contender");
    let refused = tokio::time::timeout(
        StdDuration::from_secs(1),
        EffectDescendantRepository::lock_nowait(&mut contender, key),
    )
    .await
    .expect("canonical acquisition must not wait");
    assert!(matches!(
        refused,
        Err(IngressUowError::Database {
            retry_class: DbRetryClass::CanonicalLockContention
        })
    ));
    drop(contender);
    holder.commit().await.expect("release holder");
    let mut retry = uow.begin().await.expect("retry whole transaction");
    EffectDescendantRepository::lock_nowait(&mut retry, key)
        .await
        .expect("acquire on fresh attempt");
    retry.commit().await.expect("retry commit");
    fixture.close().await;
}

async fn legacy_terminal_rows_start_the_tail_at_adoption_without_fake_proof(db: &Database) {
    let uow = descendant_test_uow(db).await;
    let store = PostgresIngressSubstrate::open(db.clone()).expect("store");
    let key = MessageKey::new();
    let now = timestamp(9);
    let mut tx = uow.begin().await.expect("legacy fixture");
    crate::ingress_uow::CanonicalMessageRepository::record_message(&mut tx, key, &digest(1), None)
        .await
        .expect("legacy message");
    tx.commit().await.expect("canonical fixture commit");
    let mut tx = store.begin().await.expect("legacy fixture mutation");
    // Simulate a pre-V1025 row: terminal means receipts, never retention.
    tx.execute(
        dialect_sql(
            db.driver(),
            "UPDATE ingress_messages SET terminal_at = ?::timestamptz WHERE message_key = ?::uuid",
            "UPDATE ingress_messages SET terminal_at = ? WHERE message_key = ?",
        ),
        crate::db_params![
            (now - Duration::days(20)).to_rfc3339(),
            key.to_storage().to_string()
        ],
    )
    .await
    .expect("legacy terminal");
    tx.commit().await.expect("legacy commit");
    assert_eq!(
        store
            .gc_expired_aliases(now, gc_budget())
            .await
            .expect("adopt without deletion")
            .deleted_messages,
        0
    );
    assert_eq!(
        store
            .gc_expired_aliases(
                now + ALIAS_RETENTION - Duration::microseconds(1),
                gc_budget()
            )
            .await
            .expect("full adoption tail")
            .deleted_messages,
        0
    );
    assert_eq!(
        store
            .gc_expired_aliases(now + ALIAS_RETENTION, gc_budget())
            .await
            .expect("exact adoption tail")
            .deleted_messages,
        1
    );
}

#[tokio::test]
async fn sqlite_legacy_terminal_rows_start_the_tail_at_adoption_without_fake_proof() {
    let db = Database::in_memory("legacy-retention")
        .await
        .expect("SQLite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    legacy_terminal_rows_start_the_tail_at_adoption_without_fake_proof(&db).await;
}

#[tokio::test]
async fn postgres_legacy_terminal_rows_start_the_tail_at_adoption_without_fake_proof() {
    let Some(fixture) = Fixture::open("legacy_tail").await else {
        return;
    };
    legacy_terminal_rows_start_the_tail_at_adoption_without_fake_proof(&fixture.db).await;
    fixture.close().await;
}
#[test]
fn gc_monitoring_predicate_matches_collector() {
    fn normalized(sql: &str) -> String {
        sql.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    let monitoring = std::fs::read_to_string(
            std::path::Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").expect("test runner sets CARGO_MANIFEST_DIR")).join(
                "../../../infrastructure/waddle.cloud/gitops/waddle-server/postgresql-monitoring-ingress.yaml",
            ),
        )
        .expect("read deployed ingress monitoring SQL");
    let monitoring = normalized(&monitoring);
    let eligibility = "receipts_complete AND NOT has_ref AND NOT has_descendant";
    let receipt_predicate = "NOT EXISTS ( SELECT 1 FROM ingress_effect_intents intent WHERE intent.message_key = message.message_key AND NOT EXISTS ( SELECT 1 FROM ingress_effect_receipts receipt WHERE receipt.message_key = intent.message_key AND receipt.kind = intent.kind AND receipt.semantic_identity_hash = intent.semantic_identity_hash ) )";
    assert!(monitoring.contains(&format!("{receipt_predicate} AS receipts_complete")));

    assert!(monitoring.contains("SELECT now() - interval '8 days' AS value"));
    assert!(monitoring.contains(
            "WHERE message.terminal_at IS NOT NULL AND (message.retention_eligible_at IS NULL OR message.retention_eligible_at <= cutoff.value)"
        ));
    assert_eq!(
        monitoring
            .matches(&format!("FILTER (WHERE {eligibility})"))
            .count(),
        2,
        "backlog and oldest-age must use the same collector eligibility"
    );
    assert!(
            monitoring.contains("count(*) FILTER (WHERE has_ref OR has_descendant OR NOT receipts_complete) AS retained_referenced_messages")
        );
    for (sql, cutoff) in [
        (GC_CANDIDATES_POSTGRES, "?::timestamptz"),
        (GC_CANDIDATES_SQLITE, "?"),
    ] {
        let sql = normalized(sql)
                .replace(
                    "NOT EXISTS ( SELECT 1 FROM ingress_effect_intents i WHERE i.message_key = m.message_key AND NOT EXISTS ( SELECT 1 FROM ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash ) )",
                    "receipts_complete",
                )
                .replace(
                    "EXISTS ( SELECT 1 FROM ingress_origin_aliases a WHERE a.message_key = m.message_key )",
                    "has_alias",
                )
                .replace(
                    "EXISTS ( SELECT 1 FROM ingress_deliveries d WHERE d.message_key = m.message_key )",
                    "has_delivery",
                )
                .replace(
                    "EXISTS ( SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = m.message_key )",
                    "has_ref",
                )
                .replace(
                    "EXISTS ( SELECT 1 FROM ingress_effect_descendants d WHERE d.message_key = m.message_key AND d.settled_at IS NULL )",
                    "has_descendant",
                );
        let predicate = sql
            .split_once(" WHERE ")
            .expect("candidate WHERE")
            .1
            .split_once(" ORDER BY ")
            .expect("candidate ordering")
            .0;
        assert_eq!(
                predicate,
                format!(
                    "m.terminal_at IS NOT NULL AND (m.retention_eligible_at IS NULL OR m.retention_eligible_at <= {cutoff}) AND receipts_complete AND NOT has_ref AND NOT has_descendant"
                )
            );
    }
}

#[test]
fn storage_codecs_round_trip_full_range_values_and_uuid_keys() {
    let ordinal = IngressOrdinal::from_storage(u64::MAX)
        .expect("u64::MAX is a valid persisted ingress ordinal");
    assert_eq!(ordinal.to_storage(), u64::MAX);
    assert!(IngressOrdinal::from_storage(0).is_err());

    let uuid =
        Uuid::parse_str("018e68e7-6a5f-7d4d-a0bc-64dc70a9ce10").expect("fixture UUID is valid");
    assert_eq!(MessageKey::from_storage(uuid).to_storage(), uuid);
    assert_eq!(DeliveryKey::from_storage(uuid).to_storage(), uuid);
    assert_eq!(SmIngressId::from_storage(uuid).to_storage(), uuid);

    for target in [
        NormalizedTarget::Absent,
        NormalizedTarget::Bare(
            "romeo@example.com"
                .parse()
                .expect("fixture is a valid bare JID"),
        ),
        NormalizedTarget::Full(
            "romeo@example.com/phone"
                .parse()
                .expect("fixture is a valid full JID"),
        ),
    ] {
        let storage = target.to_storage();
        assert_eq!(
            NormalizedTarget::from_storage(storage.kind(), storage.jid()),
            Ok(target),
            "target codec must round trip every variant"
        );
    }
}

#[derive(Debug)]
struct FakePgError(&'static str);

impl std::fmt::Display for FakePgError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "fake pg error {}", self.0)
    }
}

impl std::error::Error for FakePgError {}

impl sqlx::error::DatabaseError for FakePgError {
    fn message(&self) -> &str {
        "fake pg error"
    }

    fn code(&self) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(self.0))
    }

    fn kind(&self) -> sqlx::error::ErrorKind {
        sqlx::error::ErrorKind::Other
    }

    fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        self
    }

    fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
        self
    }

    fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
        self
    }
}

fn fake_database_error(code: &'static str) -> DatabaseError {
    DatabaseError::Internal(sqlx::Error::Database(Box::new(FakePgError(code))))
}

#[test]
fn alias_gc_maps_postgres_timeout_sqlstates_to_typed_kinds() {
    assert_eq!(
        database_timeout_kind(&fake_database_error("55P03")),
        Some(DatabaseTimeoutKind::Lock)
    );
    assert_eq!(
        database_timeout_kind(&fake_database_error("57014")),
        Some(DatabaseTimeoutKind::Statement)
    );
    assert_eq!(database_timeout_kind(&fake_database_error("42P01")), None);
}

#[tokio::test]
async fn alias_key_is_unique_and_resolves_existing_or_conflict_from_joined_digest() {
    let Some(fixture) = Fixture::open("alias_key").await else {
        return;
    };
    let sender = sender();
    let target = target();
    let origin = OriginId::new("opaque-client-origin");
    let first_digest = digest(7);
    let first_key = MessageKey::new();

    let mut tx = fixture.store.begin().await.expect("begin alias insert");
    let first = fixture
        .store
        .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &first_digest, || {
            first_key
        })
        .await
        .expect("insert alias");
    tx.commit().await.expect("commit alias insert");
    assert_eq!(first, inserted(first_key));

    let mut tx = fixture.store.begin().await.expect("begin existing read");
    let resolved = fixture
        .store
        .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &first_digest, || {
            MessageKey::new()
        })
        .await
        .expect("resolve existing alias");
    tx.commit().await.expect("commit existing read");
    assert_eq!(resolved, existing(first_key));

    let mut tx = fixture.store.begin().await.expect("begin conflict read");
    let conflict = fixture
        .store
        .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &digest(8), || {
            MessageKey::new()
        })
        .await
        .expect("resolve conflicting alias");
    tx.commit().await.expect("commit conflict read");
    assert!(matches!(
        conflict,
        AliasResolution::Aliased(AliasOutcome::Conflict(ref value))
            if value.existing == first_key && value.stored == first_digest && value.offered == digest(8)
    ));
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn concurrent_first_alias_insert_same_digest_leaves_one_message_and_existing_result() {
    let Some(fixture) = Fixture::open("alias_race_same").await else {
        return;
    };
    let sender = sender();
    let target = target();
    let origin = OriginId::new("race-same-digest");
    let start = Arc::new(Barrier::new(2));
    let first = race_alias(
        fixture.store.clone(),
        sender.clone(),
        target.clone(),
        origin.clone(),
        digest(9),
        Arc::clone(&start),
    );
    let second = race_alias(
        fixture.store.clone(),
        sender,
        target,
        origin,
        digest(9),
        start,
    );
    let (first, second) = tokio::join!(first, second);
    let outcomes = [
        first.expect("first race result"),
        second.expect("second race result"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                AliasResolution::Aliased(AliasOutcome::Inserted(_))
            ))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                AliasResolution::Aliased(AliasOutcome::Existing(_))
            ))
            .count(),
        1
    );
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn concurrent_first_alias_insert_different_digest_leaves_one_message_and_conflict_result() {
    let Some(fixture) = Fixture::open("alias_race_conflict").await else {
        return;
    };
    let sender = sender();
    let target = target();
    let origin = OriginId::new("race-different-digest");
    let start = Arc::new(Barrier::new(2));
    let first = race_alias(
        fixture.store.clone(),
        sender.clone(),
        target.clone(),
        origin.clone(),
        digest(10),
        Arc::clone(&start),
    );
    let second = race_alias(
        fixture.store.clone(),
        sender,
        target,
        origin,
        digest(11),
        start,
    );
    let (first, second) = tokio::join!(first, second);
    let outcomes = [
        first.expect("first race result"),
        second.expect("second race result"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                AliasResolution::Aliased(AliasOutcome::Inserted(_))
            ))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                AliasResolution::Aliased(AliasOutcome::Conflict(_))
            ))
            .count(),
        1
    );
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn terminalize_keeps_the_first_proven_terminal_time() {
    let Some(fixture) = Fixture::open("terminalize").await else {
        return;
    };
    let key = fixture.record_message().await;
    let first_time = timestamp(1);
    let second_time = timestamp(2);
    let mut tx = fixture
        .store
        .begin()
        .await
        .expect("begin first terminalize");
    assert_eq!(
        fixture
            .store
            .terminalize_message(&mut tx, key, first_time)
            .await
            .expect("first terminalize"),
        TerminalizeOutcome::Terminalized
    );
    tx.commit().await.expect("commit first terminalize");
    let mut tx = fixture
        .store
        .begin()
        .await
        .expect("begin repeated terminalize");
    assert_eq!(
        fixture
            .store
            .terminalize_message(&mut tx, key, second_time)
            .await
            .expect("repeat terminalize"),
        TerminalizeOutcome::AlreadyTerminal
    );
    tx.commit().await.expect("commit repeated terminalize");
    assert!(fixture.terminal_is(key, first_time).await);
    fixture.close().await;
}

#[tokio::test]
async fn gc_deletes_aliasless_terminal_messages_and_makes_child_writes_vanish() {
    let Some(fixture) = Fixture::open("gc_aliasless").await else {
        return;
    };
    let key = fixture.record_message().await;
    let terminal_at = timestamp(3);
    fixture.terminalize(key, terminal_at).await;
    let result = fixture
        .store
        .gc_expired_aliases(terminal_at + ALIAS_RETENTION, gc_budget())
        .await
        .expect("garbage collect terminal message");
    assert_eq!(result.deleted_messages, 1);
    let mut tx = fixture
        .store
        .begin()
        .await
        .expect("begin missing child insert");
    assert_eq!(
        fixture
            .store
            .insert_sm_ref(
                &mut tx,
                SmIngressId::new(),
                IngressOrdinal::FIRST,
                WireHandledCount::from_storage(1),
                key
            )
            .await
            .expect("record vanished child"),
        MessageWriteOutcome::MessageVanished
    );
    tx.commit().await.expect("commit vanished child result");
    fixture.close().await;
}

#[tokio::test]
async fn gc_respects_the_exact_terminal_retention_cutoff() {
    let Some(fixture) = Fixture::open("gc_cutoff").await else {
        return;
    };
    let key = fixture.record_message().await;
    let terminal_at = timestamp(4);
    fixture.terminalize(key, terminal_at).await;
    assert_eq!(
        fixture
            .store
            .gc_expired_aliases(
                terminal_at + ALIAS_RETENTION - Duration::microseconds(1),
                gc_budget(),
            )
            .await
            .expect("collect before exact cutoff")
            .deleted_messages,
        0
    );
    assert_eq!(
        fixture
            .store
            .gc_expired_aliases(terminal_at + ALIAS_RETENTION, gc_budget())
            .await
            .expect("collect at exact cutoff")
            .deleted_messages,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn gc_budget_stops_without_work_then_preserves_partial_progress() {
    let Some(fixture) = Fixture::open("gc_budget_progress").await else {
        return;
    };
    let terminal_at = timestamp(10);
    let total = GC_BATCH_LIMIT + 2;
    fixture.record_terminal_messages(total, terminal_at).await;
    let now = terminal_at + ALIAS_RETENTION + Duration::days(1);

    let no_work = fixture
        .store
        .gc_expired_aliases(
            now,
            AliasGcBudget {
                deadline: tokio::time::Instant::now() - StdDuration::from_millis(1),
                ..gc_budget()
            },
        )
        .await
        .expect("an expired budget is a cooperative stop");
    assert_eq!(
        no_work,
        AliasGcOutcome {
            deleted_orphan_proofs: 0,
            deleted_messages: 0,
            completed: false,
        }
    );

    fixture
            .db
            .execute(
                "CREATE FUNCTION waddle_test_slow_gc_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.05); RETURN OLD; END $$",
            )
            .await
            .expect("create deterministic GC pacing function");
    fixture
            .db
            .execute(
                "CREATE TRIGGER waddle_test_slow_gc_delete BEFORE DELETE ON ingress_messages FOR EACH ROW EXECUTE FUNCTION waddle_test_slow_gc_delete()",
            )
            .await
            .expect("install deterministic GC pacing trigger");
    let partial = fixture
        .store
        .gc_expired_aliases(
            now,
            AliasGcBudget {
                deadline: tokio::time::Instant::now() + StdDuration::from_secs(1),
                ..gc_budget()
            },
        )
        .await
        .expect("budgeted GC returns committed progress");
    assert!(!partial.completed);
    assert!(partial.deleted_messages > 0);
    assert!(partial.deleted_messages < total);
    let remaining = usize::try_from(fixture.count("ingress_messages").await)
        .expect("remaining count fits usize");
    assert_eq!(partial.deleted_messages + remaining, total);

    fixture
        .db
        .execute("DROP TRIGGER waddle_test_slow_gc_delete ON ingress_messages")
        .await
        .expect("remove deterministic GC pacing trigger");
    fixture
        .db
        .execute("DROP FUNCTION waddle_test_slow_gc_delete()")
        .await
        .expect("remove deterministic GC pacing function");
    let completed = fixture
        .store
        .gc_expired_aliases(now, gc_budget())
        .await
        .expect("finish remaining GC backlog");
    assert!(completed.completed);
    assert_eq!(completed.deleted_messages, remaining);
    assert_eq!(partial.deleted_messages + completed.deleted_messages, total);
    assert_eq!(fixture.count("ingress_messages").await, 0);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn gc_exact_full_batch_completes_after_empty_rescan() {
    let Some(fixture) = Fixture::open("gc_exact_batch").await else {
        return;
    };
    let terminal_at = timestamp(11);
    fixture
        .record_terminal_messages(GC_BATCH_LIMIT, terminal_at)
        .await;
    let outcome = fixture
        .store
        .gc_expired_aliases(
            terminal_at + ALIAS_RETENTION + Duration::days(1),
            gc_budget(),
        )
        .await
        .expect("collect exact full batch and confirm empty rescan");
    assert_eq!(
        outcome,
        AliasGcOutcome {
            deleted_orphan_proofs: 0,
            deleted_messages: GC_BATCH_LIMIT,
            completed: true,
        }
    );
    assert_eq!(fixture.count("ingress_messages").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn gc_skips_a_candidate_locked_by_another_writer_and_keeps_going() {
    let Some(fixture) = Fixture::open("gc_lock_progress").await else {
        return;
    };
    let terminal_at = timestamp(12);
    let keys = fixture.record_terminal_messages(2, terminal_at).await;
    let mut blocker = sqlx::PgConnection::connect(&fixture.schema_url)
        .await
        .expect("open candidate blocker");
    let mut blocker_tx = blocker.begin().await.expect("begin candidate blocker");
    sqlx::query("SELECT 1 FROM ingress_messages WHERE message_key = $1 FOR UPDATE")
        .bind(keys[1].to_storage())
        .execute(&mut *blocker_tx)
        .await
        .expect("lock second GC candidate");

    let outcome = fixture
        .store
        .gc_expired_aliases(
            terminal_at + ALIAS_RETENTION + Duration::days(1),
            AliasGcBudget {
                lock_timeout: StdDuration::from_millis(100),
                ..gc_budget()
            },
        )
        .await
        .expect("a locked candidate is skipped, not waited on");
    assert_eq!(
        outcome,
        AliasGcOutcome {
            deleted_orphan_proofs: 0,
            deleted_messages: 1,
            completed: false,
        },
        "a skipped candidate is still eligible, so the run is not complete"
    );
    assert_eq!(fixture.count("ingress_messages").await, 1);
    blocker_tx.rollback().await.expect("release candidate lock");
    drop(blocker);

    let outcome = fixture
        .store
        .gc_expired_aliases(
            terminal_at + ALIAS_RETENTION + Duration::days(1),
            gc_budget(),
        )
        .await
        .expect("the skipped candidate is reclaimed by the next run");
    assert_eq!(
        outcome,
        AliasGcOutcome {
            deleted_orphan_proofs: 0,
            deleted_messages: 1,
            completed: true,
        }
    );
    assert_eq!(fixture.count("ingress_messages").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn gc_keeps_draining_later_batches_after_skipping_a_locked_candidate() {
    let Some(fixture) = Fixture::open("gc_skip_then_drain").await else {
        return;
    };
    let terminal_at = timestamp(17);
    let total = GC_BATCH_LIMIT + 8;
    let keys = fixture.record_terminal_messages(total, terminal_at).await;
    let mut blocker = sqlx::PgConnection::connect(&fixture.schema_url)
        .await
        .expect("open candidate blocker");
    let mut blocker_tx = blocker.begin().await.expect("begin candidate blocker");
    sqlx::query("SELECT 1 FROM ingress_messages WHERE message_key = $1 FOR UPDATE")
        .bind(keys[0].to_storage())
        .execute(&mut *blocker_tx)
        .await
        .expect("lock the oldest GC candidate");

    let outcome = fixture
        .store
        .gc_expired_aliases(
            terminal_at + ALIAS_RETENTION + Duration::days(1),
            gc_budget(),
        )
        .await
        .expect("a locked head candidate must not stop the run");
    assert_eq!(
        outcome,
        AliasGcOutcome {
            deleted_orphan_proofs: 0,
            deleted_messages: total - 1,
            completed: false,
        },
        "every unlocked row across both batches is reclaimed; the locked one keeps the run partial"
    );
    assert_eq!(fixture.count("ingress_messages").await, 1);
    blocker_tx.rollback().await.expect("release candidate lock");
    drop(blocker);
    fixture.close().await;
}

#[tokio::test]
async fn gc_epoch_lock_timeout_has_zero_progress() {
    let Some(fixture) = Fixture::open("gc_epoch_lock_timeout").await else {
        return;
    };
    let terminal_at = timestamp(13);
    fixture.record_terminal_messages(1, terminal_at).await;
    let mut blocker = sqlx::PgConnection::connect(&fixture.schema_url)
        .await
        .expect("open epoch blocker");
    let mut blocker_tx = blocker.begin().await.expect("begin epoch blocker");
    sqlx::query("SELECT 1 FROM ingress_protocol_epoch WHERE id = 1 FOR UPDATE")
        .execute(&mut *blocker_tx)
        .await
        .expect("lock protocol epoch row");

    // Production-relative bounds: the statement timer covers the lock
    // wait, so the lock bound must be the one that fires first.
    let failure = fixture
        .store
        .gc_expired_aliases(
            terminal_at + ALIAS_RETENTION + Duration::days(1),
            AliasGcBudget {
                lock_timeout: StdDuration::from_millis(100),
                statement_timeout: StdDuration::from_millis(250),
                ..gc_budget()
            },
        )
        .await
        .expect_err("epoch lock must time out");
    assert_eq!(failure.deleted_messages, 0);
    assert!(matches!(
        failure.error,
        AliasGcError::DatabaseTimeout {
            kind: DatabaseTimeoutKind::Lock
        }
    ));
    assert_eq!(fixture.count("ingress_messages").await, 1);
    blocker_tx.rollback().await.expect("release epoch lock");
    drop(blocker);
    fixture.close().await;
}

#[tokio::test]
async fn gc_non_timeout_failure_after_progress_preserves_committed_count() {
    let Some(fixture) = Fixture::open("gc_failure_progress").await else {
        return;
    };
    let terminal_at = timestamp(14);
    let keys = fixture.record_terminal_messages(2, terminal_at).await;
    fixture
            .db
            .execute(&format!(
                "CREATE FUNCTION waddle_test_fail_gc_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF OLD.message_key = '{}'::uuid THEN RAISE EXCEPTION 'forced GC failure' USING ERRCODE = 'P0001'; END IF; RETURN OLD; END $$",
                keys[1].to_storage()
            ))
            .await
            .expect("create deterministic GC failure function");
    fixture
            .db
            .execute(
                "CREATE TRIGGER waddle_test_fail_gc_delete AFTER DELETE ON ingress_messages FOR EACH ROW EXECUTE FUNCTION waddle_test_fail_gc_delete()",
            )
            .await
            .expect("install deterministic GC failure trigger");

    let progress = AliasGcProgress::default();
    let failure = fixture
        .store
        .gc_expired_aliases(
            terminal_at + ALIAS_RETENTION + Duration::days(1),
            AliasGcBudget {
                progress: progress.clone(),
                ..gc_budget()
            },
        )
        .await
        .expect_err("second candidate trigger must fail");
    assert_eq!(failure.deleted_messages, 1);
    assert!(matches!(failure.error, AliasGcError::Substrate(_)));
    assert_eq!(fixture.count("ingress_messages").await, 1);
    // The committed deletion is exactly when a cross-node append can be
    // orphaned, and this pass exits through `?` before the sweep would run.
    // Arming at the commit is what keeps the retry from skipping it.
    assert!(progress.orphan_sweep_pending.load(Ordering::Acquire));
    fixture.close().await;
}

#[tokio::test]
async fn gc_progress_handle_reports_committed_deletions_after_external_cancellation() {
    let Some(fixture) = Fixture::open("gc_progress_cancel").await else {
        return;
    };
    let terminal_at = timestamp(16);
    let total = 8usize;
    fixture.record_terminal_messages(total, terminal_at).await;
    fixture
            .db
            .execute(
                "CREATE FUNCTION waddle_test_slow_gc_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.05); RETURN OLD; END $$",
            )
            .await
            .expect("create deterministic GC pacing function");
    fixture
            .db
            .execute(
                "CREATE TRIGGER waddle_test_slow_gc_delete BEFORE DELETE ON ingress_messages FOR EACH ROW EXECUTE FUNCTION waddle_test_slow_gc_delete()",
            )
            .await
            .expect("install deterministic GC pacing trigger");
    let progress = AliasGcProgress::default();
    let cancelled = tokio::time::timeout(
        StdDuration::from_millis(150),
        fixture.store.gc_expired_aliases(
            terminal_at + ALIAS_RETENTION + Duration::days(1),
            AliasGcBudget {
                progress: progress.clone(),
                ..gc_budget()
            },
        ),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the paced run must be cancelled mid-way"
    );
    let remaining = usize::try_from(fixture.count("ingress_messages").await)
        .expect("remaining count fits usize");
    assert!(remaining > 0 && remaining < total);
    assert_eq!(
        progress.committed(),
        total - remaining,
        "the progress handle must match the rows actually committed"
    );
    // Cancellation runs no code after the await, so the sweep can only have
    // been armed at the commit itself.
    assert!(progress.orphan_sweep_pending.load(Ordering::Acquire));
    fixture.close().await;
}

#[tokio::test]
async fn concurrent_gc_runs_never_double_count_deletions() {
    let Some(fixture) = Fixture::open("gc_concurrent").await else {
        return;
    };
    let terminal_at = timestamp(15);
    let total = 32usize;
    fixture.record_terminal_messages(total, terminal_at).await;
    let barrier = Arc::new(Barrier::new(3));
    let now = terminal_at + ALIAS_RETENTION + Duration::days(1);
    let spawn_gc = |store: PostgresIngressSubstrate, barrier: Arc<Barrier>| {
        tokio::spawn(async move {
            barrier.wait().await;
            store.gc_expired_aliases(now, gc_budget()).await
        })
    };
    let first = spawn_gc(fixture.store.clone(), barrier.clone());
    let second = spawn_gc(fixture.store.clone(), barrier.clone());
    barrier.wait().await;
    let results = [
        first.await.expect("join first GC"),
        second.await.expect("join second GC"),
    ];
    let mut deleted = 0usize;
    for result in results {
        let outcome = result.expect("concurrent GC runs skip each other's locked rows");
        deleted += outcome.deleted_messages;
    }
    assert_eq!(deleted, total);
    assert_eq!(fixture.count("ingress_messages").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn gc_preserves_aliases_and_deliveries_with_live_sm_refs() {
    let Some(fixture) = Fixture::open("gc_live_children").await else {
        return;
    };
    // Create the message through the alias path so the retention pass
    // has an expired alias to remove.
    let key = MessageKey::new();
    let mut tx = fixture.store.begin().await.expect("begin alias insert");
    fixture
        .store
        .resolve_and_record_alias(
            &mut tx,
            &sender(),
            &target(),
            &OriginId::new("expired-with-live-children"),
            &digest(5),
            || key,
        )
        .await
        .expect("insert alias");
    tx.commit().await.expect("commit alias insert");

    let terminal_at = timestamp(5);
    let mut tx = fixture.store.begin().await.expect("begin child writes");
    let sm_id = SmIngressId::new();
    authority_tests::insert_stream(&mut tx, sm_id).await;
    assert_eq!(
        fixture
            .store
            .insert_sm_ref(
                &mut tx,
                sm_id,
                IngressOrdinal::FIRST,
                WireHandledCount::from_storage(1),
                key
            )
            .await
            .expect("insert sm ref"),
        MessageWriteOutcome::Recorded
    );
    assert_eq!(
        fixture
            .store
            .record_delivery(&mut tx, DeliveryKey::new(), key)
            .await
            .expect("record delivery"),
        MessageWriteOutcome::Recorded
    );
    tx.commit().await.expect("commit child writes");
    fixture.terminalize(key, terminal_at).await;
    assert_eq!(
        fixture
            .store
            .gc_expired_aliases(terminal_at + ALIAS_RETENTION, gc_budget())
            .await
            .expect("collect terminal message with children")
            .deleted_messages,
        0
    );
    // Live SM custody retains the entire canonical dedup authority.
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_sm_refs").await, 1);
    assert_eq!(fixture.count("ingress_deliveries").await, 1);
    // Referenced rows remain outside the candidate scan: a second
    // pass finds no work and touches nothing.
    assert_eq!(
        fixture
            .store
            .gc_expired_aliases(terminal_at + ALIAS_RETENTION, gc_budget())
            .await
            .expect("re-run GC over retained history")
            .deleted_messages,
        0
    );
    assert_eq!(fixture.count("ingress_messages").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sm_refs_are_keyed_by_sm_ingress_identity_not_transport_stream() {
    let Some(fixture) = Fixture::open("sm_ref_identity").await else {
        return;
    };
    let key = fixture.record_message().await;
    let sm_id = SmIngressId::new();
    let mut tx = fixture.store.begin().await.expect("begin first ref");
    authority_tests::insert_stream(&mut tx, sm_id).await;
    assert_eq!(
        fixture
            .store
            .insert_sm_ref(
                &mut tx,
                sm_id,
                IngressOrdinal::FIRST,
                WireHandledCount::from_storage(1),
                key
            )
            .await
            .expect("record the first handled-stanza reference"),
        MessageWriteOutcome::Recorded
    );
    tx.commit().await.expect("commit first ref");

    // An XEP-0198 resume continues the SAME SM ingress identity on a new
    // transport stream: replaying the identical durable reference is an
    // idempotent AlreadyRecorded, so an ambiguous-commit retry can finish
    // its unit of work.
    let mut tx = fixture.store.begin().await.expect("begin replayed ref");
    assert_eq!(
        fixture
            .store
            .insert_sm_ref(
                &mut tx,
                sm_id,
                IngressOrdinal::FIRST,
                WireHandledCount::from_storage(1),
                key
            )
            .await
            .expect("identical replay is idempotent"),
        MessageWriteOutcome::AlreadyRecorded
    );
    tx.commit().await.expect("commit replayed ref");

    // The same ordinal bound to a DIFFERENT message is a typed identity
    // conflict, distinguishable from outages and from AlreadyRecorded.
    let other = fixture.record_message().await;
    let mut tx = fixture.store.begin().await.expect("begin conflicting ref");
    assert!(
        matches!(
            fixture
                .store
                .insert_sm_ref(
                    &mut tx,
                    sm_id,
                    IngressOrdinal::FIRST,
                    WireHandledCount::from_storage(1),
                    other
                )
                .await,
            Err(IngressSubstrateError::SmOrdinalConflict)
        ),
        "a different message under the same (SmIngressId, ordinal) must conflict"
    );
    drop(tx);
    assert_eq!(fixture.count("ingress_sm_refs").await, 1);

    // record_delivery mirrors the same idempotency contract.
    let delivery = DeliveryKey::new();
    let mut tx = fixture.store.begin().await.expect("begin delivery");
    assert_eq!(
        fixture
            .store
            .record_delivery(&mut tx, delivery, key)
            .await
            .expect("record delivery"),
        MessageWriteOutcome::Recorded
    );
    assert_eq!(
        fixture
            .store
            .record_delivery(&mut tx, delivery, key)
            .await
            .expect("identical delivery replay is idempotent"),
        MessageWriteOutcome::AlreadyRecorded
    );
    assert!(matches!(
        fixture
            .store
            .record_delivery(&mut tx, delivery, other)
            .await,
        Err(IngressSubstrateError::DeliveryKeyConflict)
    ));
    drop(tx);
    fixture.close().await;
}

#[tokio::test]
async fn epoch_advances_at_most_once_per_transaction_and_future_epochs_fence_gc() {
    let Some(fixture) = Fixture::open("epoch_single_advance").await else {
        return;
    };
    let key = fixture.record_message().await;
    let terminal_at = timestamp(8);
    fixture.terminalize(key, terminal_at).await;

    // A duplicated activation statement inside ONE transaction must not
    // commit a two-epoch jump no deployment ever observed.
    let mut conn = sqlx::PgConnection::connect(&fixture.schema_url)
        .await
        .expect("open activation connection");
    let mut tx = conn.begin().await.expect("begin double-advance tx");
    sqlx::query(
        "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
             lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
    )
    .execute(&mut *tx)
    .await
    .expect("first advance in the transaction");
    assert!(
        sqlx::query(
            "UPDATE ingress_protocol_epoch SET epoch = 2, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
        )
        .execute(&mut *tx)
        .await
        .is_err(),
        "a second epoch advance in the same transaction must be rejected"
    );
    drop(tx);

    // Advance to epoch 2 through two sanctioned transactions; the live
    // epoch now exceeds what this binary supports, so its GC fails
    // closed instead of auto-claiming the future epoch.
    for statement in [
        "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
             lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
        "UPDATE ingress_protocol_epoch SET epoch = 2, activated_at = now(), \
             lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
    ] {
        sqlx::query(statement)
            .execute(&mut conn)
            .await
            .expect("sanctioned single-step advance");
    }
    assert!(matches!(
        fixture
            .store
            .gc_expired_aliases(terminal_at + ALIAS_RETENTION, gc_budget())
            .await,
        Err(AliasGcFailure {
            deleted_messages: 0,
            error: AliasGcError::Substrate(IngressSubstrateError::UnsupportedLiveEpoch),
            deleted_orphan_proofs: 0,
        })
    ));
    drop(conn);
    fixture.close().await;
}

#[tokio::test]
async fn alias_insert_succeeds_at_combined_maximum_key_lengths() {
    let Some(fixture) = Fixture::open("alias_max_len").await else {
        return;
    };
    // Maximum-length localparts/domains (1023 bytes each per RFC 7622)
    // with a maximum-length origin-id: the fixed-width hash key must
    // absorb what a composite B-tree tuple could not.
    let long_domain = |label: &str| {
        std::iter::repeat_n(label.repeat(62), 4)
            .collect::<Vec<_>>()
            .join(".")
    };
    let long_sender: BareJid = format!("{}@{}", "a".repeat(1023), long_domain("b"))
        .parse()
        .expect("long sender JID is valid");
    let long_target: BareJid = format!("{}@{}", "c".repeat(1023), long_domain("d"))
        .parse()
        .expect("long target JID is valid");
    let origin = OriginId::new("o".repeat(1024));
    let key = MessageKey::new();
    let mut tx = fixture.store.begin().await.expect("begin max-length alias");
    assert_eq!(
        fixture
            .store
            .resolve_and_record_alias(
                &mut tx,
                &long_sender,
                &NormalizedTarget::Bare(long_target),
                &origin,
                &digest(6),
                || key,
            )
            .await
            .expect("maximum-length alias key inserts"),
        inserted(key)
    );
    tx.commit().await.expect("commit max-length alias");
    fixture.close().await;
}

#[tokio::test]
async fn epoch_one_rejects_unproven_writes_and_accepts_transaction_bound_proof() {
    let Some(fixture) = Fixture::open("epoch_proof").await else {
        return;
    };
    let key = fixture.record_message().await;
    fixture
        .db
        .execute(
            "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
        )
        .await
        .expect("activate epoch one");

    for statement in [
        format!(
            "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
            Uuid::new_v4(),
            key.to_storage()
        ),
        format!(
            "UPDATE ingress_messages SET terminal_at = now() WHERE message_key = '{}'",
            key.to_storage()
        ),
        format!(
            "DELETE FROM ingress_messages WHERE message_key = '{}'",
            key.to_storage()
        ),
        "TRUNCATE ingress_deliveries".to_string(),
    ] {
        assert!(
            fixture.db.execute(&statement).await.is_err(),
            "epoch-one protected operation must require proof: {statement}"
        );
    }

    let mut tx = fixture
        .store
        .begin()
        .await
        .expect("begin proof transaction");
    tx.execute("SET LOCAL waddle.protocol_epoch = '1'", ())
        .await
        .expect("set epoch proof");
    tx.execute(
        "SELECT set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)",
        (),
    )
    .await
    .expect("set xid proof");
    fixture
        .store
        .record_delivery(&mut tx, DeliveryKey::new(), key)
        .await
        .expect("proof authorizes protected write");
    tx.commit().await.expect("commit proof transaction");

    // A proof with a wrong epoch or no xid is intentionally incomplete.
    let mut wrong_epoch = fixture
        .store
        .begin()
        .await
        .expect("begin wrong epoch proof");
    wrong_epoch
        .execute("SET LOCAL waddle.protocol_epoch = '0'", ())
        .await
        .expect("set wrong epoch proof");
    assert!(fixture
        .store
        .record_delivery(&mut wrong_epoch, DeliveryKey::new(), key)
        .await
        .is_err());
    drop(wrong_epoch); // dropping an uncommitted transaction rolls it back
    fixture.close().await;
}

#[tokio::test]
async fn epoch_and_manifest_tables_enforce_their_singleton_and_append_only_rules() {
    let Some(fixture) = Fixture::open("epoch_invariants").await else {
        return;
    };
    for statement in [
            "UPDATE ingress_protocol_epoch SET epoch = 2, activated_at = now(), lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
            "UPDATE ingress_protocol_epoch SET epoch = 1 WHERE id = 1",
            "DELETE FROM ingress_protocol_epoch WHERE id = 1",
            "TRUNCATE ingress_protocol_epoch",
            "INSERT INTO ingress_protocol_epoch (id, epoch) VALUES (2, 0)",
            "UPDATE ingress_epoch_guard_manifest SET table_name = 'bad' WHERE table_name = 'ingress_messages'",
            "DELETE FROM ingress_epoch_guard_manifest WHERE table_name = 'ingress_messages'",
            "TRUNCATE ingress_epoch_guard_manifest",
            // TRUNCATE on protected tables is rejected at EVERY epoch by the
            // lock-free truncate guard (its trigger must never request the
            // epoch row: TRUNCATE's ACCESS EXCLUSIVE is taken first, which
            // would invert the epoch-first lock order).
            "TRUNCATE ingress_messages",
            "TRUNCATE ingress_sm_refs",
        ] {
            assert!(fixture.db.execute(statement).await.is_err(), "must reject: {statement}");
        }
    let mut tx = fixture.store.begin().await.expect("begin manifest probe");
    tx.execute(
        "INSERT INTO ingress_epoch_guard_manifest (table_name) VALUES ('ingress_future_probe')",
        (),
    )
    .await
    .expect("manifest permits future enrollment");
    drop(tx); // probe must leave the append-only manifest unchanged
    fixture.close().await;
}

#[tokio::test]
async fn epoch_guard_manifest_matches_rust_and_live_trigger_catalog() {
    let Some(fixture) = Fixture::open("guard_manifest").await else {
        return;
    };
    let conn = fixture.db.guard().await.expect("guard manifest database");
    let mut rows = conn
        .query(
            "SELECT table_name FROM ingress_epoch_guard_manifest ORDER BY table_name",
            (),
        )
        .await
        .expect("read guard manifest");
    let mut manifest = Vec::new();
    while let Some(row) = rows.next().await.expect("read manifest row") {
        manifest.push(row.get::<String>(0).expect("decode manifest table"));
    }
    let mut expected = EPOCH_GUARDED_TABLES.map(str::to_owned).to_vec();
    expected.sort();
    assert_eq!(manifest, expected, "migration manifest and Rust list agree");

    // Every live ingress_* table must be enrolled: an unlisted table
    // would silently sit outside the activation boundary.
    let mut live_rows = conn
        .query(
            "SELECT c.relname FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = current_schema() AND c.relkind = 'r' \
                   AND c.relname LIKE 'ingress_%' ORDER BY c.relname",
            (),
        )
        .await
        .expect("enumerate live ingress tables");
    let mut live = Vec::new();
    while let Some(row) = live_rows.next().await.expect("read live table row") {
        live.push(row.get::<String>(0).expect("decode live table name"));
    }
    let mut expected_live = expected.clone();
    expected_live.push("ingress_epoch_guard_manifest".to_string());
    expected_live.push("ingress_protocol_epoch".to_string());
    expected_live.sort();
    assert_eq!(
        live, expected_live,
        "every live ingress_* table is either guarded or the epoch/manifest table"
    );

    // Trigger shape is pinned, not just presence: BEFORE statement-level
    // I/U/D (tgtype 30) plus BEFORE statement-level TRUNCATE (tgtype 34),
    // both ENABLE ALWAYS and bound to the guard function.
    for table in EPOCH_GUARDED_TABLES {
        let mut trigger_rows = conn
            .query(
                "SELECT tg.tgname, tg.tgtype::int, tg.tgenabled::text, p.proname \
                     FROM pg_trigger tg \
                     JOIN pg_class c ON c.oid = tg.tgrelid \
                     JOIN pg_namespace n ON n.oid = c.relnamespace \
                     JOIN pg_proc p ON p.oid = tg.tgfoid \
                     WHERE n.nspname = current_schema() AND c.relname = ? \
                       AND NOT tg.tgisinternal ORDER BY tg.tgname",
                crate::db_params![table],
            )
            .await
            .expect("read guard triggers");
        let mut triggers = Vec::new();
        while let Some(row) = trigger_rows.next().await.expect("read trigger row") {
            triggers.push((
                row.get::<String>(0).expect("decode trigger name"),
                row.get::<i64>(1).expect("decode trigger type"),
                row.get::<String>(2).expect("decode trigger mode"),
                row.get::<String>(3).expect("decode trigger function"),
            ));
        }
        assert!(
            triggers.contains(&(
                format!("{table}_epoch_guard_dml"),
                30,
                "A".to_string(),
                "waddle_ingress_epoch_guard".to_string(),
            )),
            "{table} must carry the BEFORE-statement I/U/D guard: {triggers:?}"
        );
        assert!(
            triggers.contains(&(
                format!("{table}_epoch_guard_truncate"),
                34,
                "A".to_string(),
                "waddle_ingress_truncate_guard".to_string(),
            )),
            "{table} must carry the lock-free BEFORE-statement TRUNCATE reject: {triggers:?}"
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn gc_installs_its_own_epoch_proof_and_collects_at_epoch_one() {
    let Some(fixture) = Fixture::open("gc_epoch_one").await else {
        return;
    };
    let key = fixture.record_message().await;
    let terminal_at = timestamp(7);
    fixture.terminalize(key, terminal_at).await;
    fixture
        .db
        .execute(
            "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
        )
        .await
        .expect("activate epoch one");
    let outcome = fixture
        .store
        .gc_expired_aliases(terminal_at + ALIAS_RETENTION, gc_budget())
        .await
        .expect("GC proves its own transactions at epoch one");
    assert_eq!(
        outcome.deleted_messages, 1,
        "GC collects normally after activation"
    );
    fixture.close().await;
}

#[tokio::test]
async fn guard_uses_its_table_schema_not_the_callers_search_path() {
    let Some(fixture) = Fixture::open("hostile_search_path").await else {
        return;
    };
    let key = fixture.record_message().await;
    // Independent short name: suffixing the fixture schema would exceed
    // PostgreSQL's 63-byte identifier cap and silently truncate into a
    // collision with the fixture schema itself.
    let hostile = format!("waddle_test_hostile_{}", Uuid::new_v4().simple());
    let mut conn = sqlx::PgConnection::connect(&fixture.schema_url)
        .await
        .expect("open one hostile-search-path connection");
    sqlx::query(&format!("CREATE SCHEMA {hostile}"))
        .execute(&mut conn)
        .await
        .expect("create hostile schema");
    sqlx::query(&format!(
            "CREATE TABLE {hostile}.ingress_protocol_epoch (id INTEGER PRIMARY KEY, epoch BIGINT NOT NULL)"
        ))
        .execute(&mut conn)
        .await
        .expect("create hostile epoch shadow");
    sqlx::query(&format!(
        "INSERT INTO {hostile}.ingress_protocol_epoch (id, epoch) VALUES (1, 1)"
    ))
    .execute(&mut conn)
    .await
    .expect("seed hostile epoch shadow");
    sqlx::query(&format!("SET search_path = {hostile}, {}", fixture.schema))
        .execute(&mut conn)
        .await
        .expect("place hostile schema first");
    sqlx::query(&format!(
        "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
        Uuid::new_v4(),
        key.to_storage()
    ))
    .execute(&mut conn)
    .await
    .expect("epoch-zero guard must ignore hostile epoch-one shadow");
    drop(conn);
    fixture.close().await;
}

/// Poll `pg_stat_activity` until a backend is blocked on a heavyweight
/// lock while running a query containing `fragment`.  The fragment is a
/// bound parameter, so this poll's own query text never matches itself.
async fn wait_for_lock_waiter(admin: &sqlx::PgPool, fragment: &str) {
    for _ in 0..400 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_stat_activity \
                 WHERE wait_event_type = 'Lock' AND query LIKE $1",
        )
        .bind(format!("%{fragment}%"))
        .fetch_one(admin)
        .await
        .expect("poll pg_stat_activity for a lock waiter");
        if waiting > 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("no blocked backend appeared for query fragment {fragment:?}");
}

#[tokio::test]
async fn session_level_guc_and_stale_xid_proofs_do_not_authorize_writes() {
    let Some(fixture) = Fixture::open("epoch_proof_matrix").await else {
        return;
    };
    let key = fixture.record_message().await;
    fixture
        .db
        .execute(
            "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
        )
        .await
        .expect("activate epoch one");

    let mut conn = sqlx::PgConnection::connect(&fixture.schema_url)
        .await
        .expect("open one dedicated connection");
    let insert = format!(
        "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
        Uuid::new_v4(),
        key.to_storage()
    );

    // Session-level SET of BOTH GUCs: the xid captured now belongs to
    // this transaction, so it cannot prove any later transaction.
    sqlx::query("SET waddle.protocol_epoch = '1'")
        .execute(&mut conn)
        .await
        .expect("session-level epoch setting");
    let stale_xid: String = sqlx::query_scalar(
        "SELECT set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, false)",
    )
    .fetch_one(&mut conn)
    .await
    .expect("session-level xid setting");
    assert!(
        sqlx::query(&insert).execute(&mut conn).await.is_err(),
        "a session-retained proof must not authorize a later transaction"
    );

    // Missing xid half of the proof.
    let mut tx = conn.begin().await.expect("begin missing-xid transaction");
    sqlx::query("SET LOCAL waddle.protocol_epoch = '1'")
        .execute(&mut *tx)
        .await
        .expect("set epoch half only");
    assert!(
        sqlx::query(&insert).execute(&mut *tx).await.is_err(),
        "the epoch GUC alone must not authorize a write"
    );
    drop(tx);

    // Stale xid replayed as a literal from an earlier transaction.
    let mut tx = conn.begin().await.expect("begin stale-xid transaction");
    sqlx::query("SET LOCAL waddle.protocol_epoch = '1'")
        .execute(&mut *tx)
        .await
        .expect("set epoch for stale proof");
    sqlx::query("SELECT set_config('waddle.protocol_epoch_xid', $1, true)")
        .bind(&stale_xid)
        .execute(&mut *tx)
        .await
        .expect("replay stale xid literal");
    assert!(
        sqlx::query(&insert).execute(&mut *tx).await.is_err(),
        "a stale xid proof must not authorize a write"
    );
    drop(tx);

    // A correct transaction-local proof works — and does not survive
    // into the next transaction on the SAME pooled connection.
    let mut tx = conn.begin().await.expect("begin proven transaction");
    sqlx::query("SET LOCAL waddle.protocol_epoch = '1'")
        .execute(&mut *tx)
        .await
        .expect("set local epoch");
    sqlx::query("SELECT set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)")
        .execute(&mut *tx)
        .await
        .expect("set local xid");
    sqlx::query(&insert)
        .execute(&mut *tx)
        .await
        .expect("transaction-bound proof authorizes the write");
    tx.commit().await.expect("commit proven transaction");
    assert!(
        sqlx::query(&format!(
            "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
            Uuid::new_v4(),
            key.to_storage()
        ))
        .execute(&mut conn)
        .await
        .is_err(),
        "SET LOCAL must not be retained past commit on the same connection"
    );
    drop(conn);
    fixture.close().await;
}

#[tokio::test]
async fn epoch_activation_waits_behind_in_flight_epoch_zero_writes() {
    let Some(fixture) = Fixture::open("activation_race").await else {
        return;
    };
    let key = fixture.record_message().await;

    // Transaction A: an epoch-0 protected write whose statement trigger
    // took FOR SHARE on the epoch row, held until commit.
    let mut writer = sqlx::PgConnection::connect(&fixture.schema_url)
        .await
        .expect("open epoch-zero writer connection");
    let mut tx = writer.begin().await.expect("begin epoch-zero write");
    sqlx::query(&format!(
        "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
        Uuid::new_v4(),
        key.to_storage()
    ))
    .execute(&mut *tx)
    .await
    .expect("epoch-zero write starts before activation");

    // Concurrent activation must block behind the in-flight write.
    let flip_url = fixture.schema_url.clone();
    let flip = tokio::spawn(async move {
        let mut conn = sqlx::PgConnection::connect(&flip_url)
            .await
            .expect("open activation connection");
        sqlx::query(
            "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
        )
        .execute(&mut conn)
        .await
        .expect("activation update completes after the writer commits");
    });
    wait_for_lock_waiter(&fixture.admin, "UPDATE ingress_protocol_epoch").await;
    assert!(!flip.is_finished(), "activation must still be blocked");
    tx.commit().await.expect("commit the epoch-zero write");
    flip.await.expect("join activation task");

    // First post-activation write without a proof is rejected.
    assert!(
        sqlx::query(&format!(
            "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
            Uuid::new_v4(),
            key.to_storage()
        ))
        .execute(&mut writer)
        .await
        .is_err(),
        "the first post-activation write requires the transaction proof"
    );
    drop(writer);
    fixture.close().await;
}

#[tokio::test]
async fn gc_skips_a_message_whose_row_is_locked_by_an_in_flight_child_insert() {
    let Some(fixture) = Fixture::open("gc_ref_race").await else {
        return;
    };
    let key = fixture.record_message().await;
    let terminal_at = timestamp(6);
    fixture.terminalize(key, terminal_at).await;

    let mut tx = fixture.store.begin().await.expect("begin child insert");
    let sm_id = SmIngressId::new();
    authority_tests::insert_stream(&mut tx, sm_id).await;
    assert_eq!(
        fixture
            .store
            .insert_sm_ref(
                &mut tx,
                sm_id,
                IngressOrdinal::FIRST,
                WireHandledCount::from_storage(1),
                key
            )
            .await
            .expect("insert sm ref before GC"),
        MessageWriteOutcome::Recorded
    );

    let outcome = fixture
        .store
        .gc_expired_aliases(
            terminal_at + ALIAS_RETENTION,
            AliasGcBudget {
                lock_timeout: StdDuration::from_millis(50),
                ..gc_budget()
            },
        )
        .await
        .expect("a row held by an in-flight child write is skipped");
    assert_eq!(
        outcome,
        AliasGcOutcome {
            deleted_orphan_proofs: 0,
            deleted_messages: 0,
            completed: false,
        }
    );
    tx.commit()
        .await
        .expect("commit child insert after GC skipped the row");
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_sm_refs").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn child_insert_blocked_behind_gc_deletion_observes_message_vanished() {
    let Some(fixture) = Fixture::open("ref_gc_race").await else {
        return;
    };
    let key = fixture.record_message().await;

    // Simulate GC's deletion transaction: FOR UPDATE on the message row,
    // held while a child insert arrives.
    let mut gc_conn = sqlx::PgConnection::connect(&fixture.schema_url)
        .await
        .expect("open GC connection");
    let mut gc_tx = gc_conn.begin().await.expect("begin GC transaction");
    sqlx::query(&format!(
        "SELECT 1 FROM ingress_messages WHERE message_key = '{}' FOR UPDATE",
        key.to_storage()
    ))
    .execute(&mut *gc_tx)
    .await
    .expect("GC locks the message row");

    let insert_store = fixture.store.clone();
    let insert = tokio::spawn(async move {
        let mut tx = insert_store.begin().await?;
        let outcome = insert_store
            .insert_sm_ref(
                &mut tx,
                SmIngressId::new(),
                IngressOrdinal::FIRST,
                WireHandledCount::from_storage(1),
                key,
            )
            .await?;
        tx.commit()
            .await
            .map_err(|_| IngressSubstrateError::Database {
                retry_class: DbRetryClass::NotRetryable,
            })?;
        Ok::<_, IngressSubstrateError>(outcome)
    });
    wait_for_lock_waiter(&fixture.admin, "FOR UPDATE").await;
    assert!(!insert.is_finished(), "child insert must wait behind GC");
    sqlx::query(&format!(
        "DELETE FROM ingress_messages WHERE message_key = '{}'",
        key.to_storage()
    ))
    .execute(&mut *gc_tx)
    .await
    .expect("GC deletes the childless message");
    gc_tx.commit().await.expect("commit GC deletion");
    assert_eq!(
        insert
            .await
            .expect("join child insert task")
            .expect("child insert completes after GC commits"),
        MessageWriteOutcome::MessageVanished,
        "a child insert serialized behind GC observes the deletion"
    );
    drop(gc_conn);
    fixture.close().await;
}

#[tokio::test]
async fn restricted_role_cannot_disable_or_replace_the_epoch_guard() {
    let Some(fixture) = Fixture::open("restricted_role").await else {
        return;
    };
    let role = format!("waddle_test_dml_{}", Uuid::new_v4().simple());
    let mut conn = sqlx::PgConnection::connect(&fixture.schema_url)
        .await
        .expect("open role-test connection");
    sqlx::query(&format!("CREATE ROLE {role} NOLOGIN"))
        .execute(&mut conn)
        .await
        .expect("create restricted DML role");
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA {} TO {role}",
        fixture.schema
    ))
    .execute(&mut conn)
    .await
    .expect("grant schema usage");
    for table in EPOCH_GUARDED_TABLES {
        sqlx::query(&format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.{table} TO {role}",
            fixture.schema
        ))
        .execute(&mut conn)
        .await
        .expect("grant DML on protected table");
    }
    sqlx::query(&format!(
        "GRANT SELECT ON {}.ingress_protocol_epoch TO {role}",
        fixture.schema
    ))
    .execute(&mut conn)
    .await
    .expect("grant epoch read");

    sqlx::query(&format!("SET ROLE {role}"))
        .execute(&mut conn)
        .await
        .expect("assume restricted role");
    for statement in [
        "ALTER TABLE ingress_messages DISABLE TRIGGER ingress_messages_epoch_guard_dml".to_string(),
        "CREATE OR REPLACE FUNCTION waddle_ingress_epoch_guard() RETURNS trigger \
             LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$"
            .to_string(),
        "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
             lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1"
            .to_string(),
        "INSERT INTO ingress_epoch_guard_manifest (table_name) VALUES ('rogue')".to_string(),
        "DROP TRIGGER ingress_messages_epoch_guard_dml ON ingress_messages".to_string(),
    ] {
        assert!(
            sqlx::query(&statement).execute(&mut conn).await.is_err(),
            "the restricted role must not bypass the guard: {statement}"
        );
    }
    sqlx::query("RESET ROLE")
        .execute(&mut conn)
        .await
        .expect("reset role");
    sqlx::query(&format!("DROP OWNED BY {role}"))
        .execute(&mut conn)
        .await
        .expect("drop role grants");
    sqlx::query(&format!("DROP ROLE {role}"))
        .execute(&mut conn)
        .await
        .expect("drop restricted role");
    drop(conn);
    fixture.close().await;
}

async fn race_alias(
    store: PostgresIngressSubstrate,
    sender: BareJid,
    target: NormalizedTarget,
    origin: OriginId,
    digest: SemanticDigest,
    start: Arc<Barrier>,
) -> Result<AliasResolution, IngressSubstrateError> {
    let mut tx = store.begin().await?;
    start.wait().await;
    let result = store
        .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &digest, MessageKey::new)
        .await?;
    tx.commit().await.map_err(discard_database_error)?;
    Ok(result)
}

fn inserted(key: MessageKey) -> AliasResolution {
    AliasResolution::Aliased(AliasOutcome::Inserted(key))
}

fn existing(key: MessageKey) -> AliasResolution {
    AliasResolution::Aliased(AliasOutcome::Existing(key))
}

fn digest(byte: u8) -> SemanticDigest {
    SemanticDigest::from_storage(1, [byte; 32]).expect("valid semantic digest fixture")
}

fn sender() -> BareJid {
    "romeo@example.com"
        .parse()
        .expect("fixture is a valid bare JID")
}

fn target() -> NormalizedTarget {
    NormalizedTarget::Full(
        "juliet@example.com/laptop"
            .parse()
            .expect("fixture is a valid full JID"),
    )
}

fn timestamp(second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, second)
        .single()
        .expect("fixture timestamp is valid")
}

struct Fixture {
    store: PostgresIngressSubstrate,
    db: Database,
    admin: sqlx::PgPool,
    schema: String,
    schema_url: String,
}

impl Fixture {
    async fn open(test_name: &str) -> Option<Self> {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (ingress substrate)");
            return None;
        };
        let schema = format!(
            "waddle_test_ingress_{test_name}_{}",
            Uuid::new_v4().simple()
        );
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect postgres admin pool");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated postgres schema");
        let schema_url = postgres_url_with_search_path(&database_url, &schema);
        let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, schema_url.clone());
        config.pool_size = 10;
        let db = Database::from_config("ingress-substrate-test", &config)
            .await
            .expect("open isolated postgres database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("apply migrations to isolated schema");
        let store = PostgresIngressSubstrate::open(db.clone()).expect("open Postgres substrate");
        Some(Self {
            store,
            db,
            admin,
            schema,
            schema_url,
        })
    }

    async fn close(self) {
        drop(self.store);
        drop(self.db);
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .expect("drop isolated postgres schema");
    }

    async fn record_message(&self) -> MessageKey {
        let key = MessageKey::new();
        let mut tx = self.store.begin().await.expect("begin message insert");
        self.store
            .record_message(&mut tx, key, &digest(1), None)
            .await
            .expect("record message");
        tx.commit().await.expect("commit message insert");
        key
    }

    async fn terminalize(&self, key: MessageKey, terminal_at: DateTime<Utc>) {
        let mut tx = self.store.begin().await.expect("begin terminalize");
        assert_eq!(
            self.store
                .terminalize_message(&mut tx, key, terminal_at)
                .await
                .expect("terminalize message"),
            TerminalizeOutcome::Terminalized
        );
        tx.commit().await.expect("commit terminalize");
    }

    async fn record_terminal_messages(
        &self,
        count: usize,
        first_terminal_at: DateTime<Utc>,
    ) -> Vec<MessageKey> {
        let mut tx = self.store.begin().await.expect("begin message seed");
        let mut keys = Vec::with_capacity(count);
        for index in 0..count {
            let key = MessageKey::new();
            self.store
                .record_message(&mut tx, key, &digest(1), None)
                .await
                .expect("record seeded message");
            assert_eq!(
                self.store
                    .terminalize_message(
                        &mut tx,
                        key,
                        first_terminal_at + Duration::milliseconds(index as i64),
                    )
                    .await
                    .expect("terminalize seeded message"),
                TerminalizeOutcome::Terminalized
            );
            keys.push(key);
        }
        tx.commit().await.expect("commit message seed");
        keys
    }

    async fn count(&self, table: &str) -> i64 {
        let conn = self.db.guard().await.expect("database guard");
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .expect("count ingress rows");
        let row = rows
            .next()
            .await
            .expect("read count row")
            .expect("count row exists");
        row.get(0).expect("decode count")
    }

    async fn terminal_is(&self, key: MessageKey, expected: DateTime<Utc>) -> bool {
        let conn = self.db.guard().await.expect("database guard");
        let mut rows = conn
                .query(
                    "SELECT terminal_at = ?::timestamptz FROM ingress_messages WHERE message_key = ?::uuid",
                    crate::db_params![expected.to_rfc3339(), key.to_storage().to_string()],
                )
                .await
                .expect("read terminal time");
        let row = rows
            .next()
            .await
            .expect("read terminal row")
            .expect("message row exists");
        row.get(0).expect("decode terminal comparison")
    }
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse postgres URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}
