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
    let outcome = remote_carbon_delivery(
        RemoteCarbonFanout::from_reply(reply).expect("authoritative owner reply"),
        &deps,
        &owner,
        &exclude,
        CarbonKind::Sent,
    );
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

async fn remote_carbons_partial_retry(fixture: IngressFixture) {
    use crate::clustering::relay::RelayRemoteUserSideEffectReply;
    use crate::server::routes::interpret::carbons::{
        send_carbons_to_registry_with_capture, CarbonRegistryDeps,
    };
    use waddle_xmpp::registry::ConnectionEntry;

    let mut submission = fixture.submission(Some("partial-remote-carbon"), "carbon body");
    let owner = submission.sender.to_bare();
    let source = submission.sender.clone();
    let effect = ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons {
        owner: owner.clone(),
        exclude: vec![source.clone()],
        kind: CarbonKind::Sent,
        origin: None,
        message: Box::new(submission.plan.sanitized_message.clone()),
    });
    submission.plan.intents = vec![IngressEffectIntent::RelayCarbons {
        owner: owner.clone(),
        exclude: vec![source.clone()],
        kind: CarbonKind::Sent,
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(effect.clone()))];
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("first commit");
    let key = first.message_key.expect("message");
    let registry = ConnectionRegistry::new();
    let targets = ["a-first", "b-middle", "c-last"]
        .map(|resource| owner.with_resource_str(resource).expect("target"));
    let mut receivers = Vec::new();
    for target in &targets {
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        registry.register_entry(target.clone(), ConnectionEntry::new(sender));
        assert!(registry.set_carbons_enabled(target, true));
        receivers.push(Some(receiver));
    }
    drop(receivers[1].take());
    let outcome = send_carbons_to_registry_with_capture(
        &registry,
        CarbonRegistryDeps {
            ingress_effect_capture: None,
            sm_session_registry: None,
            web_socket_state: None,
        },
        owner.clone(),
        Box::new(submission.plan.sanitized_message.clone()),
        CarbonKind::Sent,
        vec![source.clone()],
    )
    .await
    .expect_err("middle destination fails");
    assert_eq!(
        outcome.completed.carbon_recipients,
        vec![targets[0].clone(), targets[2].clone()]
    );
    let reply = RelayRemoteUserSideEffectReply {
        status: RelayRemoteUserSideEffectStatus::Incomplete {
            reason: outcome.reason,
        },
        carbon_recipients: outcome.completed.carbon_recipients,
    };
    let deps = Deps::new(&registry, "example.com");
    let partial = remote_carbon_delivery(
        RemoteCarbonFanout::from_reply(reply).expect("reply"),
        &deps,
        &owner,
        std::slice::from_ref(&source),
        CarbonKind::Sent,
    );
    carbon_progress::persist(&fixture.uow, key, &effect, &partial)
        .await
        .expect("persist partial proof");
    assert_eq!(fixture.count("ingress_carbon_receipts").await, 2);
    assert_eq!(
        classify_outcome(&effect, partial, &mut Vec::new()),
        ExternalOutcome::Failed
    );
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("still pending"));
    for index in [0, 2] {
        assert!(receivers[index]
            .as_mut()
            .expect("healthy receiver")
            .try_recv()
            .is_ok());
    }

    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("same-origin retry");
    assert_eq!(retry.message_key, Some(key));
    assert_eq!(retry.external.len(), 1);
    let prepared = carbon_progress::prepare(&fixture.uow, key, &retry.external[0])
        .await
        .expect("load progress");
    let ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons {
        exclude, message, ..
    }) = prepared
    else {
        panic!("relay effect");
    };
    assert!(exclude.contains(&targets[0]) && exclude.contains(&targets[2]));
    assert!(!exclude.contains(&targets[1]));
    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    registry.register_entry(targets[1].clone(), ConnectionEntry::new(sender));
    assert!(registry.set_carbons_enabled(&targets[1], true));
    receivers[1] = Some(receiver);
    let outcome = send_carbons_to_registry_with_capture(
        &registry,
        CarbonRegistryDeps {
            ingress_effect_capture: None,
            sm_session_registry: None,
            web_socket_state: None,
        },
        owner.clone(),
        message,
        CarbonKind::Sent,
        exclude,
    )
    .await
    .expect("only unfinished destination");
    assert_eq!(outcome.carbon_recipients, vec![targets[1].clone()]);
    let final_result = remote_carbon_delivery(
        RemoteCarbonFanout::from_reply(RelayRemoteUserSideEffectReply {
            status: RelayRemoteUserSideEffectStatus::Applied,
            carbon_recipients: outcome.carbon_recipients,
        })
        .expect("reply"),
        &deps,
        &owner,
        std::slice::from_ref(&source),
        CarbonKind::Sent,
    );
    carbon_progress::persist(&fixture.uow, key, &effect, &final_result)
        .await
        .expect("final target proof");
    let proven = vec![proven_receipts(
        &effect,
        &final_result,
        &retry.external_receipts[0],
    )];
    let classified = classify_outcome(&effect, final_result, &mut Vec::new());
    assert_eq!(classified, ExternalOutcome::Done);
    let receipts = completed_receipts(&retry, &[(effect, classified)], &proven, 0);
    assert_eq!(receipts.len(), 1);
    for receipt in receipts {
        EffectReceiptRepository::record_receipt_pooled(
            &fixture.db,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("complete relay intent");
    }
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("terminal"));
    assert_eq!(fixture.count("ingress_carbon_receipts").await, 3);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(receivers[1]
        .as_mut()
        .expect("repaired receiver")
        .try_recv()
        .is_ok());
    for receiver in receivers.iter_mut().flatten() {
        assert!(receiver.try_recv().is_err(), "no duplicate carbon");
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_remote_carbons_partial_progress_retries_only_unfinished_target() {
    remote_carbons_partial_retry(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_remote_carbons_partial_progress_retries_only_unfinished_target() {
    if let Some(fixture) = IngressFixture::postgres("carbon_partial_retry").await {
        remote_carbons_partial_retry(fixture).await;
    }
}
