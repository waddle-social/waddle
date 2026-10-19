use super::*;
use crate::ingress::{commit::commit_submission, receipt_key, test_support::IngressFixture};
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};

async fn obligation(fixture: &IngressFixture) -> SendObligation {
    let recipient: FullJid = "juliet@example.com/phone".parse().expect("resource");
    let intent = IngressEffectIntent::RouteDirect {
        prepared: None,
        recipient: recipient.to_bare(),
        fanout: vec![
            recipient.clone(),
            "juliet@example.com/laptop".parse().expect("resource"),
        ],
        route_identity: EffectMessageIdentity::capture_ordinal(1),
    };
    let mut submission = fixture.submission(None, "leased delivery");
    submission.plan.intents = vec![intent.clone()];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("authority");
    SendObligation {
        message: decision.message_key.expect("canonical key"),
        receipt: receipt_key(&intent).expect("receipt identity"),
        recipient,
    }
}

async fn claim(fixture: &IngressFixture, obligation: &SendObligation, epoch: &str) -> SendClaim {
    let mut tx = fixture.uow.begin().await.expect("begin claim");
    let result = SendAttemptRepository::claim(
        &mut tx,
        obligation,
        &NodeIdentity::new("node", epoch),
        Duration::from_secs(60),
    )
    .await
    .expect("claim");
    tx.commit().await.expect("commit claim");
    result
}

fn acquired(claim: SendClaim) -> SendLease {
    match claim {
        SendClaim::Acquired(lease) => lease,
        other => panic!("expected acquired lease, got {other:?}"),
    }
}

async fn expire(fixture: &IngressFixture) {
    let mut tx = fixture.uow.begin().await.expect("begin expiry fixture");
    tx.transaction_mut()
        .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
        .await
        .expect("expire leases");
    tx.commit().await.expect("commit expiry");
}

async fn exclusion(fixture: IngressFixture) {
    let obligation = obligation(&fixture).await;
    let (first, second) = tokio::join!(
        claim(&fixture, &obligation, "first"),
        claim(&fixture, &obligation, "second")
    );
    let lease = match (first, second) {
        (SendClaim::Acquired(lease), SendClaim::Busy)
        | (SendClaim::Busy, SendClaim::Acquired(lease)) => lease,
        other => panic!("exactly one claimant must win: {other:?}"),
    };
    let mut distinct_resource = obligation.clone();
    distinct_resource.recipient = "juliet@example.com/laptop".parse().expect("resource");
    acquired(claim(&fixture, &distinct_resource, "other-resource").await);
    let mut tx = fixture.uow.begin().await.expect("begin start");
    assert!(SendAttemptRepository::start(&mut tx, &lease)
        .await
        .expect("start"));
    tx.commit().await.expect("commit start");
    assert_eq!(
        claim(&fixture, &obligation, "replacement").await,
        SendClaim::Ambiguous
    );
    let mut tx = fixture.uow.begin().await.expect("deadline");
    let delay = SendAttemptRepository::next_retry_delay(&mut tx, obligation.message)
        .await
        .expect("deadline")
        .expect("active lease");
    assert!(delay <= Duration::from_secs(60) && delay > Duration::from_secs(50));
    tx.commit().await.expect("deadline commit");
    expire(&fixture).await;
    let replacement = acquired(claim(&fixture, &obligation, "replacement").await);
    assert_ne!(replacement.token, lease.token);
    let mut tx = fixture.uow.begin().await.expect("begin finish");
    assert!(!SendAttemptRepository::complete(&mut tx, &lease)
        .await
        .expect("old token fenced"));
    assert!(
        !SendAttemptRepository::release_proven_not_enqueued(&mut tx, &lease)
            .await
            .expect("old release fenced")
    );
    assert!(SendAttemptRepository::start(&mut tx, &replacement)
        .await
        .expect("new start"));
    assert!(SendAttemptRepository::complete(&mut tx, &replacement)
        .await
        .expect("new finish"));
    tx.commit().await.expect("commit finish");
    assert_eq!(
        claim(&fixture, &obligation, "replacement").await,
        SendClaim::Completed
    );
    let mut tx = fixture.uow.begin().await.expect("completed release");
    assert!(
        !SendAttemptRepository::release_proven_not_enqueued(&mut tx, &replacement)
            .await
            .expect("completed immutable")
    );
    tx.commit().await.expect("commit");
    fixture.close().await;
}

async fn expiry_and_stale_tokens(fixture: IngressFixture) {
    let obligation = obligation(&fixture).await;
    let stale = acquired(claim(&fixture, &obligation, "old").await);
    expire(&fixture).await;
    let mut tx = fixture.uow.begin().await.expect("begin expired start");
    assert_eq!(
        SendAttemptRepository::status(&mut tx, &obligation)
            .await
            .expect("expired initial reservation"),
        Some(SendAttemptStatus::ExpiredLease)
    );

    assert!(!SendAttemptRepository::start(&mut tx, &stale)
        .await
        .expect("expired cannot start"));
    tx.commit().await.expect("commit");
    let replacement = acquired(claim(&fixture, &obligation, "new").await);
    assert_ne!(stale.token, replacement.token);
    let mut tx = fixture.uow.begin().await.expect("begin stale operations");
    assert!(!SendAttemptRepository::start(&mut tx, &stale)
        .await
        .expect("stale cannot start"));
    assert!(!SendAttemptRepository::complete(&mut tx, &stale)
        .await
        .expect("stale cannot finish"));
    assert!(
        !SendAttemptRepository::release_proven_not_enqueued(&mut tx, &stale)
            .await
            .expect("stale cannot delete")
    );
    // Even a valid token cannot cross the persisted process incarnation.
    let mut wrong_owner = replacement.clone();
    wrong_owner.owner = NodeIdentity::new("node", "wrong-incarnation");
    assert!(!SendAttemptRepository::start(&mut tx, &wrong_owner)
        .await
        .expect("incarnation bound"));
    assert!(!SendAttemptRepository::complete(&mut tx, &replacement)
        .await
        .expect("cannot finish before start"));
    assert!(SendAttemptRepository::start(&mut tx, &replacement)
        .await
        .expect("replacement starts"));
    tx.commit().await.expect("commit");
    let mut tx = fixture.uow.begin().await.expect("begin proven failure");
    assert!(
        SendAttemptRepository::release_proven_not_enqueued(&mut tx, &replacement)
            .await
            .expect("release on positive evidence")
    );
    assert_eq!(
        SendAttemptRepository::status(&mut tx, &obligation)
            .await
            .expect("preserved retry lineage"),
        Some(SendAttemptStatus::ExpiredLease)
    );
    tx.commit().await.expect("commit");
    let retry = acquired(claim(&fixture, &obligation, "new").await);
    assert_ne!(replacement.token, retry.token);
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin late old completion");
    assert!(!SendAttemptRepository::complete(&mut tx, &replacement)
        .await
        .expect("old success cannot affect retry"));
    assert!(
        SendAttemptRepository::release_proven_not_enqueued(&mut tx, &retry)
            .await
            .expect("unstarted release")
    );
    tx.commit().await.expect("commit");
    fixture.close().await;
}

async fn rollback_and_foreign_key(fixture: IngressFixture) {
    let obligation = obligation(&fixture).await;
    let mut tx = fixture.uow.begin().await.expect("begin rollback");
    let abandoned = acquired(
        SendAttemptRepository::claim(
            &mut tx,
            &obligation,
            &NodeIdentity::new("node", "abandoned"),
            Duration::from_secs(60),
        )
        .await
        .expect("claim"),
    );
    drop(tx);
    let winner = acquired(claim(&fixture, &obligation, "winner").await);
    assert_ne!(abandoned.token, winner.token);
    let mut tx = fixture.uow.begin().await.expect("begin invalid duration");
    assert!(matches!(
        SendAttemptRepository::claim(&mut tx, &obligation, &NodeIdentity::local(), Duration::ZERO)
            .await,
        Err(IngressUowError::InvalidSendLeaseDuration)
    ));
    tx.commit().await.expect("commit");
    let mut unrelated = obligation.clone();
    unrelated.receipt.semantic_identity_hash = [42; 32];
    let mut tx = fixture.uow.begin().await.expect("begin unrelated intent");
    assert!(
        SendAttemptRepository::claim(
            &mut tx,
            &unrelated,
            &NodeIdentity::local(),
            Duration::from_secs(60)
        )
        .await
        .is_err(),
        "a lease cannot invent a new effect obligation"
    );
    drop(tx);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_send_attempt_exclusion() {
    exclusion(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_send_attempt_exclusion() {
    if let Some(fixture) = IngressFixture::postgres("send_exclusion").await {
        exclusion(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_send_attempt_expiry_and_stale_tokens() {
    expiry_and_stale_tokens(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_send_attempt_expiry_and_stale_tokens() {
    if let Some(fixture) = IngressFixture::postgres("send_expiry").await {
        expiry_and_stale_tokens(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_send_attempt_rollback_and_foreign_key() {
    rollback_and_foreign_key(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_send_attempt_rollback_and_foreign_key() {
    if let Some(fixture) = IngressFixture::postgres("send_rollback").await {
        rollback_and_foreign_key(fixture).await;
    }
}

async fn identity_independence(fixture: IngressFixture) {
    let recipient: FullJid = "juliet@example.com/phone".parse().expect("resource");
    let intents: Vec<_> = [1, 2]
        .into_iter()
        .map(|ordinal| IngressEffectIntent::RouteDirect {
            prepared: None,
            recipient: recipient.to_bare(),
            fanout: vec![recipient.clone()],
            route_identity: EffectMessageIdentity::capture_ordinal(ordinal),
        })
        .collect();
    let mut submission = fixture.submission(None, "first canonical message");
    submission.plan.intents = intents.clone();
    let first_key = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("first authority")
        .message_key
        .expect("first canonical key");
    let mut submission = fixture.submission(None, "second canonical message");
    submission.plan.intents = vec![intents[0].clone()];
    let second_key = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("second authority")
        .message_key
        .expect("second canonical key");
    assert_ne!(first_key, second_key);
    let first = SendObligation {
        message: first_key,
        receipt: receipt_key(&intents[0]).expect("first receipt"),
        recipient,
    };
    let distinct_receipt = SendObligation {
        receipt: receipt_key(&intents[1]).expect("second receipt"),
        ..first.clone()
    };
    assert_ne!(first.receipt, distinct_receipt.receipt);
    let distinct_message = SendObligation {
        message: second_key,
        ..first.clone()
    };
    let completed = acquired(claim(&fixture, &first, "owner").await);
    let ambiguous = acquired(claim(&fixture, &distinct_receipt, "owner").await);
    let leased = acquired(claim(&fixture, &distinct_message, "owner").await);
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin independent transitions");
    assert!(SendAttemptRepository::start(&mut tx, &completed)
        .await
        .expect("first start"));
    assert!(SendAttemptRepository::complete(&mut tx, &completed)
        .await
        .expect("first complete"));
    assert!(SendAttemptRepository::start(&mut tx, &ambiguous)
        .await
        .expect("distinct receipt start"));
    tx.commit().await.expect("commit independent transitions");
    assert_eq!(claim(&fixture, &first, "retry").await, SendClaim::Completed);
    assert_eq!(
        claim(&fixture, &distinct_receipt, "retry").await,
        SendClaim::Ambiguous
    );
    assert_eq!(
        claim(&fixture, &distinct_message, "retry").await,
        SendClaim::Busy
    );
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin independent release");
    assert!(
        SendAttemptRepository::release_proven_not_enqueued(&mut tx, &leased)
            .await
            .expect("release distinct message")
    );
    tx.commit().await.expect("commit release");
    acquired(claim(&fixture, &distinct_message, "retry").await);
    assert_eq!(claim(&fixture, &first, "retry").await, SendClaim::Completed);
    assert_eq!(
        claim(&fixture, &distinct_receipt, "retry").await,
        SendClaim::Ambiguous
    );
    fixture.close().await;
}

async fn deletion_cascades(fixture: IngressFixture) {
    for delete_canonical in [false, true] {
        let first = obligation(&fixture).await;
        let second = SendObligation {
            recipient: "juliet@example.com/laptop".parse().expect("resource"),
            ..first.clone()
        };
        let completed = acquired(claim(&fixture, &first, "owner").await);
        let ambiguous = acquired(claim(&fixture, &second, "owner").await);
        let mut tx = fixture.uow.begin().await.expect("begin attempts");
        assert!(SendAttemptRepository::start(&mut tx, &completed)
            .await
            .expect("start completed"));
        assert!(SendAttemptRepository::complete(&mut tx, &completed)
            .await
            .expect("complete"));
        assert!(SendAttemptRepository::start(&mut tx, &ambiguous)
            .await
            .expect("start ambiguous"));
        tx.commit().await.expect("commit attempts");
        assert_eq!(fixture.count("ingress_send_attempts").await, 2);
        assert_eq!(claim(&fixture, &first, "retry").await, SendClaim::Completed);
        assert_eq!(
            claim(&fixture, &second, "retry").await,
            SendClaim::Ambiguous
        );
        let mut tx = fixture.uow.begin().await.expect("begin authority deletion");
        assert!(CanonicalMessageRepository::lock(&mut tx, first.message)
            .await
            .expect("lock canonical authority"));
        let (key, _) = dialect(&mut tx);
        let sql = if delete_canonical {
            format!("DELETE FROM ingress_messages WHERE message_key = {key}")
        } else {
            format!("DELETE FROM ingress_effect_intents WHERE message_key = {key} AND kind = ? AND semantic_identity_hash = ?")
        };
        let deleted = if delete_canonical {
            tx.transaction_mut()
                .execute(
                    &sql,
                    crate::db_params![first.message.to_storage().to_string()],
                )
                .await
        } else {
            tx.transaction_mut()
                .execute(
                    &sql,
                    crate::db_params![
                        first.message.to_storage().to_string(),
                        first.receipt.kind.to_storage(),
                        first.receipt.semantic_identity_hash.to_vec()
                    ],
                )
                .await
        }
        .expect("delete authority");
        assert_eq!(deleted, 1);
        tx.commit().await.expect("commit authority deletion");
        assert_eq!(
            fixture.count("ingress_send_attempts").await,
            0,
            "deleting either authority parent cascades both completed and ambiguous attempts"
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_send_attempt_identity_independence() {
    identity_independence(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_send_attempt_identity_independence() {
    if let Some(fixture) = IngressFixture::postgres("send_identity").await {
        identity_independence(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_send_attempt_deletion_cascades() {
    deletion_cascades(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_send_attempt_deletion_cascades() {
    if let Some(fixture) = IngressFixture::postgres("send_cascade").await {
        deletion_cascades(fixture).await;
    }
}
