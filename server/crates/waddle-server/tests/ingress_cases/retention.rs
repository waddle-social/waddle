use super::*;
use chrono::{Duration, Utc};
use waddle_server::{
    ingress::execute::terminalize_if_complete,
    ingress_substrate::{gc_expired_aliases, AliasGcBudget, AliasGcProgress, TerminalizeOutcome},
    ingress_uow::{EffectIntentRepository, EffectReceiptRepository},
};
use waddle_xmpp::ingress::{EffectMessageIdentity, MessageKey};

fn omitted_intent() -> IngressEffectIntent {
    IngressEffectIntent::RouteDirect {
        recipient: "juliet@example.com".parse().expect("recipient"),
        fanout: vec!["juliet@example.com/phone".parse().expect("phone")],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    }
}

async fn terminalize_expired(fixture: &IngressFixture, key: MessageKey) {
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("terminalization transaction");
    assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("complete receipts"));
    assert_eq!(
        CanonicalMessageRepository::terminalize(&mut tx, key, Utc::now() - Duration::days(9))
            .await
            .expect("terminalize expired message"),
        TerminalizeOutcome::Terminalized
    );
    tx.commit().await.expect("terminalization commit");
}

async fn collect(fixture: &IngressFixture, now: chrono::DateTime<Utc>) -> usize {
    let bound = std::time::Duration::from_secs(10);
    let outcome = gc_expired_aliases(
        &fixture.db,
        now,
        AliasGcBudget {
            deadline: tokio::time::Instant::now() + bound,
            lock_timeout: bound,
            statement_timeout: bound,
            scan_timeout: bound,
            progress: AliasGcProgress::default(),
        },
    )
    .await
    .expect("GC pass");
    assert!(outcome.completed);
    outcome.deleted_messages
}

async fn terminal_replay_reopens(fixture: IngressFixture) {
    let mut submission = archive_plan(&fixture, Some("terminal-replay"), "retained", "archive");
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("initial commit");
    let key = first.message_key.expect("canonical key");
    terminalize_expired(&fixture, key).await;
    submission.plan.intents.push(omitted_intent());
    let replay = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("repair omitted intent");
    assert_eq!(replay.message_key, Some(key));
    assert_eq!(replay.class, IngressDecisionClass::ExistingRepaired);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NULL")
            .await,
        1
    );
    assert_eq!(collect(&fixture, Utc::now()).await, 0);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    assert_eq!(replay.receipts_pending.len(), 1);
    for receipt in replay.receipts_pending {
        EffectReceiptRepository::record_receipt_pooled(
            &fixture.db,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("receipt omitted intent");
    }
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("re-terminalize"));
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(
        collect(&fixture, Utc::now()).await,
        0,
        "retention starts again"
    );
    assert_eq!(collect(&fixture, Utc::now() + Duration::days(9)).await, 1);
    assert_eq!(fixture.count("ingress_messages").await, 0);
    fixture.close().await;
}

async fn stale_terminal_pending_intent(fixture: IngressFixture) {
    let submission = archive_plan(&fixture, Some("stale-terminal"), "retained", "archive");
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("initial commit");
    let key = first.message_key.expect("canonical key");
    terminalize_expired(&fixture, key).await;
    // Deliberately bypass commit_submission to model a stale terminal proof.
    let mut intents = submission.plan.intents.clone();
    intents.push(omitted_intent());
    let mut tx = fixture.uow.begin().await.expect("insert pending intent");
    // Not a replay: this bypass writes the pending intent directly, so it is
    // reconciled as the row's own first-commit authority.
    EffectIntentRepository::reconcile(&mut tx, key, &intents, false)
        .await
        .expect("insert omitted intent");
    tx.commit().await.expect("commit stale terminal state");
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(collect(&fixture, Utc::now()).await, 0);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn ingress_terminal_replay_reopens_sqlite() {
    terminal_replay_reopens(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_terminal_replay_reopens_postgres() {
    if let Some(fixture) = IngressFixture::postgres("terminal_replay").await {
        terminal_replay_reopens(fixture).await;
    }
}

#[tokio::test]
async fn ingress_gc_stale_terminal_pending_intent_sqlite() {
    stale_terminal_pending_intent(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_gc_stale_terminal_pending_intent_postgres() {
    if let Some(fixture) = IngressFixture::postgres("stale_terminal").await {
        stale_terminal_pending_intent(fixture).await;
    }
}

/// #1756: append proofs are reclaimed with their canonical row, and only then.
///
/// Nothing else deletes them on purpose — the obligation owns their lifetime, not
/// the SM session, because a resume deletes the detached snapshot while the stream
/// continues. That makes retention GC the only place they can be retired, and it
/// must not retire proof while the obligation could still be retried: a missing
/// proof would authorize a second durable allocation.
async fn append_proofs_retire_with_their_message(fixture: IngressFixture) {
    let submission = archive_plan(&fixture, Some("append-proof-gc"), "retained", "archive");
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("initial commit");
    let key = decision.message_key.expect("canonical key");
    let proof_sql = "INSERT INTO sm_ingress_appends \
         (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms) \
         VALUES (?, 1, ?, 'juliet@example.com/phone', 'stream-gc', 7, 0)";
    fixture
        .execute(
            proof_sql,
            waddle_server::db_params![key.to_storage().to_string(), vec![9u8; 32]],
        )
        .await;
    assert_eq!(fixture.count("sm_ingress_appends").await, 1);

    // Still inside the retention window: the obligation remains retryable, so the
    // proof must survive even though the GC pass runs.
    assert_eq!(collect(&fixture, Utc::now()).await, 0);
    assert_eq!(fixture.count("sm_ingress_appends").await, 1);

    // Terminal and past retention: the canonical row goes, and its proofs with it.
    terminalize_expired(&fixture, key).await;
    assert_eq!(collect(&fixture, Utc::now() + Duration::days(9)).await, 1);
    assert_eq!(fixture.count("ingress_messages").await, 0);
    assert_eq!(
        fixture.count("sm_ingress_appends").await,
        0,
        "a reclaimed obligation can never be retried, so its proof is retired with it"
    );
    fixture.close().await;
}

#[tokio::test]
async fn ingress_append_proofs_retire_with_their_message_sqlite() {
    append_proofs_retire_with_their_message(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_append_proofs_retire_with_their_message_postgres() {
    if let Some(fixture) = IngressFixture::postgres("append_proof_gc").await {
        append_proofs_retire_with_their_message(fixture).await;
    }
}
