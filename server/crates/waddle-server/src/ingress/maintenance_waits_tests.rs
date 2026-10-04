use super::*;
use crate::ingress_uow::{SendAttemptRepository, SendClaim, SendObligation};
use waddle_xmpp::ownership::NodeIdentity;

async fn waiting_route(
    fixture: &IngressFixture,
    origin: &str,
    uncovered_sibling: bool,
) -> (MessageKey, SendObligation) {
    let recipient: jid::FullJid = "juliet@example.com/phone".parse().expect("recipient");
    let mut fanout = vec![recipient.clone()];
    if uncovered_sibling {
        fanout.push("juliet@example.com/laptop".parse().expect("sibling"));
    }
    let intent = IngressEffectIntent::RouteDirect {
        recipient: recipient.to_bare(),
        fanout,
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    };
    let mut submission = fixture.submission(Some(origin), "waiting delivery");
    submission.plan.intents = vec![intent.clone()];
    let key = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit route")
        .message_key
        .expect("key");
    let obligation = SendObligation {
        message: key,
        receipt: crate::ingress::receipt_key(&intent).expect("receipt"),
        recipient,
    };
    let mut tx = fixture.uow.begin().await.expect("claim transaction");
    let SendClaim::Acquired(lease) = SendAttemptRepository::claim(
        &mut tx,
        &obligation,
        &NodeIdentity::local(),
        Duration::from_secs(60),
    )
    .await
    .expect("claim") else {
        panic!("new resource must be claimable");
    };
    tx.commit().await.expect("commit claim");
    let mut tx = fixture.uow.begin().await.expect("start transaction");
    assert!(SendAttemptRepository::start(&mut tx, &lease)
        .await
        .expect("start"));
    tx.commit().await.expect("commit start");
    (key, obligation)
}

#[tokio::test]
async fn sqlite_lease_wait_backlog_does_not_spend_attempt_budget_after_first_scan() {
    let fixture = IngressFixture::sqlite().await;
    let mut waiting = Vec::new();
    for index in 0..65 {
        let (key, _) = waiting_route(&fixture, &format!("lease-wait-{index}"), false).await;
        backdate_created(&fixture, key, 120).await;
        waiting.push(key);
    }
    let later = stalled_route(&fixture).await;
    backdate_created(&fixture, later, 60).await;
    let environment = EmptyRecoveryEnvironment(ConnectionRegistry::new());
    let cursor = MaintenanceCursor::default();
    let budget = immediate_budget();
    assert_eq!(
        super::super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment)
            .await,
        MaintenanceOutcome::Partial
    );
    assert_eq!(
        super::super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment)
            .await,
        MaintenanceOutcome::Partial,
        "the continuation finishes the original scan"
    );
    assert_eq!(crate::ingress::recovery_executor::attempt_count(later), 1);
    assert_eq!(
        super::super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment)
            .await,
        MaintenanceOutcome::Complete,
        "65 known waits no longer force every scan to exhaust 64 attempts"
    );
    for key in waiting {
        assert_eq!(crate::ingress::recovery_executor::attempt_count(key), 1);
    }
    assert_eq!(crate::ingress::recovery_executor::attempt_count(later), 2);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_lease_wait_does_not_cover_an_unreserved_sibling_or_expired_start() {
    let fixture = IngressFixture::sqlite().await;
    let (mixed, _) = waiting_route(&fixture, "mixed-wait", true).await;
    assert!(super::super::waits::known_wait(&fixture.uow, mixed)
        .await
        .expect("mixed inspection")
        .is_none());
    let (covered, _) = waiting_route(&fixture, "covered-wait", false).await;
    assert!(super::super::waits::known_wait(&fixture.uow, covered)
        .await
        .expect("covered inspection")
        .is_some());
    fixture
        .execute(
            "UPDATE ingress_send_attempts SET expires_at_ms = 0 WHERE message_key = ?",
            crate::db_params![covered.to_storage().to_string()],
        )
        .await;
    assert!(super::super::waits::known_wait(&fixture.uow, covered)
        .await
        .expect("expired inspection")
        .is_none());
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_lease_wait_expiry_retries_even_without_receipt_changes() {
    let fixture = IngressFixture::sqlite().await;
    let (key, _) = waiting_route(&fixture, "expiring-cache", false).await;
    let environment = EmptyRecoveryEnvironment(ConnectionRegistry::new());
    let cursor = MaintenanceCursor::default();
    let budget = immediate_budget();
    for _ in 0..2 {
        super::super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment)
            .await;
    }
    assert_eq!(crate::ingress::recovery_executor::attempt_count(key), 1);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
    super::super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment)
        .await;
    assert_eq!(crate::ingress::recovery_executor::attempt_count(key), 2);
    fixture.close().await;
}

#[tokio::test(start_paused = true)]
async fn waiting_cache_invalidates_evidence_and_resists_older_stall_accounting() {
    let key = MessageKey::new();
    let attempt = accounting_attempt(key, 1);
    let evidence = attempt.observed;
    let until = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut suppressed = super::super::UnsupportedRows::default();
    suppressed.insert(
        key,
        evidence,
        super::super::Suppression::WaitingUntil(until),
    );
    let mut stalled = super::super::StalledRows::default();
    let budget = MaintenanceBudget {
        recovery_stall_attempts: 1,
        ..immediate_budget()
    };
    stalled.account_and_park(&attempt, evidence, budget, &mut suppressed);
    assert_eq!(
        suppressed.get(key, evidence),
        Some(super::super::Suppression::WaitingUntil(until))
    );
    let mut changed = evidence;
    changed.receipts += 1;
    assert_eq!(suppressed.get(key, changed), None);
    // A delayed worker's older snapshot must not erase a newer wait's evidence.
    suppressed.insert(key, changed, super::super::Suppression::WaitingUntil(until));
    stalled.park(&attempt, evidence, budget, &mut suppressed);
    assert_eq!(
        suppressed.get(key, changed),
        Some(super::super::Suppression::WaitingUntil(until))
    );
    suppressed.insert(
        key,
        evidence,
        super::super::Suppression::WaitingUntil(until),
    );
    changed = evidence;
    changed.progress += 1;
    assert_eq!(suppressed.get(key, changed), None);
    suppressed.insert(
        key,
        evidence,
        super::super::Suppression::WaitingUntil(until),
    );
    tokio::time::advance(Duration::from_secs(60)).await;
    assert_eq!(suppressed.get(key, evidence), None);
}
