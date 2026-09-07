//! Actual owner fanout replies must only prove complete remote obligations.
use super::*;
use crate::{
    clustering::{
        relay::RelayRemoteUserSideEffectStatus,
        route_bridge::{tests::delivery::remote_carbon_owner_reply, RemoteCarbonFanout},
    },
    ingress::{commit::commit_submission, test_support::IngressFixture},
    server::routes::interpret::carbons::{remote_carbon_delivery, CarbonFanoutFailure},
    sm_persistence::DatabaseSmPersistence,
};
use std::sync::Arc;
use waddle_xmpp::{
    ingress::IngressEffectIntent, protocol::CarbonKind, registry::ConnectionRegistry,
    stream_management::InMemorySmSessionRegistry,
};

async fn owner_fanout_receipts(fixture: IngressFixture, fail_append: bool) {
    let store = Arc::new(
        DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM store"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(store));
    let mut submission = fixture.submission(Some("owner-carbon-storage"), "carbon body");
    let owner = submission.sender.to_bare();
    let exclude = vec![submission.sender.clone()];
    let effect = ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons {
        owner: owner.clone(),
        exclude: exclude.clone(),
        kind: CarbonKind::Sent,
        origin: None,
        message: Box::new(submission.plan.sanitized_message.clone()),
    });
    submission.plan.intents = vec![IngressEffectIntent::RelayCarbons {
        owner: owner.clone(),
        exclude: exclude.clone(),
        kind: CarbonKind::Sent,
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(effect.clone()))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    assert!(decision.class.advances());
    let reply = remote_carbon_owner_reply(submission.sender.clone(), sm, async {
        if fail_append {
            fixture
                .execute(
                    "ALTER TABLE sm_sessions RENAME TO unavailable_sm_sessions",
                    (),
                )
                .await;
        }
    })
    .await;
    assert_eq!(
        reply.status,
        if fail_append {
            RelayRemoteUserSideEffectStatus::Incomplete {
                reason: CarbonFanoutFailure::DetachedAppend,
            }
        } else {
            RelayRemoteUserSideEffectStatus::Applied
        }
    );
    let registry = ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let outcome = EffectOutcome::Delivery(remote_carbon_delivery(
        RemoteCarbonFanout::from_reply(reply).expect("authoritative owner reply"),
        &deps,
        &owner,
        &exclude,
        CarbonKind::Sent,
    ));
    let proven = vec![proven_receipts(
        &effect,
        &outcome,
        &decision.external_receipts[0],
    )];
    let classified = classify_outcome(&effect, outcome, &mut Vec::new());
    assert_eq!(
        classified,
        if fail_append {
            ExternalOutcome::Failed
        } else {
            ExternalOutcome::Done
        }
    );
    let receipts = completed_receipts(&decision, &[(effect, classified)], &proven, 0);
    assert_eq!(receipts.len(), usize::from(!fail_append));
    let key = decision.message_key.expect("message");
    for receipt in receipts {
        EffectReceiptRepository::record_receipt_pooled(
            &fixture.db,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("receipt");
    }
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        i64::from(!fail_append)
    );
    assert_eq!(
        terminalize_if_complete(&fixture.uow, key)
            .await
            .expect("terminalize"),
        !fail_append
    );
    if fail_append {
        fixture
            .execute(
                "ALTER TABLE unavailable_sm_sessions RENAME TO sm_sessions",
                (),
            )
            .await;
        let retry = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("unresolved retry");
        assert!(retry.class.advances());
        assert_eq!(retry.external.len(), 1);
        assert_eq!(retry.external_receipts[0].len(), 1);
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_remote_carbons_owner_append_failure_has_no_receipt() {
    owner_fanout_receipts(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn postgres_remote_carbons_owner_append_failure_has_no_receipt() {
    if let Some(fixture) = IngressFixture::postgres("carbon_owner_failure").await {
        owner_fanout_receipts(fixture, true).await;
    }
}
#[tokio::test]
async fn sqlite_remote_carbons_owner_success_receipts() {
    owner_fanout_receipts(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn postgres_remote_carbons_owner_success_receipts() {
    if let Some(fixture) = IngressFixture::postgres("carbon_owner_success").await {
        owner_fanout_receipts(fixture, false).await;
    }
}
