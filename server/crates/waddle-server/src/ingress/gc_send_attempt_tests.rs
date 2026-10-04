use super::{run_retention_gc_with_budget, AliasGcProgress, RetentionGcBudget};
use crate::{
    ingress::{commit::commit_submission, receipt_key, test_support::IngressFixture},
    ingress_uow::{
        settle_recorded, CanonicalMessageRepository, SendAttemptRepository, SendClaim,
        SendObligation,
    },
};
use chrono::{Duration as ChronoDuration, Utc};
use std::time::Duration;
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent},
    ownership::NodeIdentity,
    telemetry::attributes::IngressGcOutcome,
};

async fn retention_preserves_unresolved_and_collects_settled_attempts(fixture: IngressFixture) {
    let recipient: jid::FullJid = "juliet@example.com/phone".parse().expect("recipient");
    let intent = IngressEffectIntent::RouteDirect {
        recipient: recipient.to_bare(),
        fanout: vec![recipient.clone()],
        route_identity: EffectMessageIdentity::capture_ordinal(1),
    };
    let mut submission = fixture.submission(None, "send retention");
    submission.plan.intents = vec![intent.clone()];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("record canonical obligation");
    let key = decision.message_key.expect("message key");
    let obligation = SendObligation {
        message: key,
        receipt: receipt_key(&intent).expect("receipt"),
        recipient,
    };
    let mut tx = fixture.uow.begin().await.expect("claim transaction");
    let SendClaim::Acquired(lease) = SendAttemptRepository::claim(
        &mut tx,
        &obligation,
        &NodeIdentity::local(),
        Duration::from_secs(5),
    )
    .await
    .expect("claim") else {
        panic!("fresh claim");
    };
    assert!(SendAttemptRepository::start(&mut tx, &lease)
        .await
        .expect("start"));
    tx.commit().await.expect("commit start");
    fixture
        .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
        .await;

    assert_eq!(
        run_retention_gc_with_budget(
            &fixture.db,
            RetentionGcBudget::DEFAULT,
            AliasGcProgress::default(),
        )
        .await,
        IngressGcOutcome::Completed
    );
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(
        fixture.count("ingress_send_attempts").await,
        1,
        "expiry permits recovery, never blind deletion of unresolved evidence"
    );

    let mut tx = fixture.uow.begin().await.expect("settlement transaction");
    assert!(SendAttemptRepository::complete(&mut tx, &lease)
        .await
        .expect("observed enqueue completion"));
    let settled = settle_recorded(&mut tx, key, std::slice::from_ref(&intent))
        .await
        .expect("settle proven obligation");
    assert!(settled.contains(&intent));
    CanonicalMessageRepository::terminalize(&mut tx, key, Utc::now() - ChronoDuration::days(9))
        .await
        .expect("age terminal canonical row");
    tx.commit().await.expect("commit settled retention fixture");

    assert_eq!(
        run_retention_gc_with_budget(
            &fixture.db,
            RetentionGcBudget::DEFAULT,
            AliasGcProgress::default(),
        )
        .await,
        IngressGcOutcome::Completed
    );
    assert_eq!(fixture.count("ingress_messages").await, 0);
    assert_eq!(fixture.count("ingress_effect_intents").await, 0);
    assert_eq!(
        fixture.count("ingress_send_attempts").await,
        0,
        "canonical retention must cascade the send ledger after resolution"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_retention_collects_only_settled_send_attempts() {
    retention_preserves_unresolved_and_collects_settled_attempts(IngressFixture::sqlite().await)
        .await;
}

#[tokio::test]
async fn postgres_retention_collects_only_settled_send_attempts() {
    if let Some(fixture) = IngressFixture::postgres("send_attempt_retention").await {
        retention_preserves_unresolved_and_collects_settled_attempts(fixture).await;
    }
}
