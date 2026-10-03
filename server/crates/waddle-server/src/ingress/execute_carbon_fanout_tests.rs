//! Actual owner fanout replies must only prove complete remote obligations.
use super::*;
use crate::server::routes::interpret::DeliveryExecutionContext;
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
        terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
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
            ingress_delivery: None,
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
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("still pending")
    );
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
            ingress_delivery: None,
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
    assert!(
        terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("terminal")
    );
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

async fn remote_carbon_lost_reply_does_not_repeat_resource_send(
    fixture: IngressFixture,
    kind: CarbonKind,
    detached: bool,
    disappear: bool,
    ambiguous: bool,
) {
    use crate::server::routes::interpret::carbons::{
        send_carbons_to_registry_with_capture, CarbonRegistryDeps,
    };
    let mut submission = fixture.submission(Some("carbon-lost-reply"), "carbon body");
    let (owner, source) = match kind {
        CarbonKind::Sent => (submission.sender.to_bare(), submission.sender.clone()),
        CarbonKind::Received => {
            let owner = submission
                .plan
                .sanitized_message
                .to
                .as_ref()
                .expect("recipient")
                .to_bare();
            let source = owner
                .with_resource_str("primary")
                .expect("original recipient");
            (owner, source)
        }
    };
    let target = owner.with_resource_str("sibling").expect("target");
    let intent = IngressEffectIntent::RelayCarbons {
        owner: owner.clone(),
        exclude: vec![source.clone()],
        kind,
    };
    let effect = ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons {
        owner: owner.clone(),
        exclude: vec![source.clone()],
        kind,
        origin: None,
        message: Box::new(submission.plan.sanitized_message.clone()),
    });
    submission.plan.intents = vec![intent];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(effect.clone()))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let registry = ConnectionRegistry::new();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    let sm = Arc::new(
        InMemorySmSessionRegistry::new().with_persistence(Arc::new(
            DatabaseSmPersistence::open(Some(fixture.db.database_url()))
                .await
                .expect("SM store"),
        )),
    );
    if detached {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
        sm.store_session(DetachedSession {
            stream_id: target.to_string(),
            user_id: owner.to_string(),
            jid: target.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("detached carbon");
    } else {
        registry.register(target.clone(), sender);
        assert!(registry.set_carbons_enabled(&target, true));
    }
    let mut delivery = Deps::new(&registry, "example.com");
    delivery.ingress_delivery_uow = Some(fixture.uow.clone());
    delivery.sm_session_registry = Some(&sm);
    delivery.ingress_append_context = carbon_progress::append_context(
        &fixture.uow,
        decision.message_key.expect("key"),
        &effect,
        &decision.external_receipts[0],
    )
    .await
    .expect("original relay context");
    for attempt in 0..2 {
        let outcome = send_carbons_to_registry_with_capture(
            &registry,
            CarbonRegistryDeps {
                ingress_delivery: Some(&delivery),
                ingress_effect_capture: None,
                sm_session_registry: Some(&sm),
                web_socket_state: None,
            },
            owner.clone(),
            Box::new(submission.plan.sanitized_message.clone()),
            kind,
            vec![source.clone()],
        )
        .await;
        if ambiguous && attempt == 1 {
            let incomplete = outcome.expect_err("vanished started resource remains unresolved");
            assert!(incomplete.completed.carbon_recipients.is_empty());
            // An inventory sampled before a competing start can claim empty
            // success. Final settlement rechecks under the canonical lock.
            let empty_success = EffectOutcome::CarbonFanout {
                outcome: crate::server::routes::interpret::FullJidDeliveryOutcome::Delivered,
                recipients: Vec::new(),
            };
            assert!(matches!(
                carbon_progress::persist(
                    &fixture.uow,
                    decision.message_key.expect("key"),
                    &effect,
                    &empty_success
                )
                .await,
                Err(crate::ingress_uow::IngressUowError::UnresolvedCarbonSend)
            ));
        } else {
            assert_eq!(
                outcome.expect("fanout").carbon_recipients,
                vec![target.clone()]
            );
        }
        if attempt == 0 && disappear {
            registry.unregister(&target);
            if ambiguous {
                fixture
                    .execute("UPDATE ingress_send_attempts SET state = 1", ())
                    .await;
            }
        }
        // The owner reply and its per-target progress never reach the origin.
        assert_eq!(fixture.count("ingress_carbon_receipts").await, 0);
        assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    }
    if detached {
        use waddle_xmpp::stream_management::SmSessionRegistry;
        let session = sm
            .peek_session(&target.to_string())
            .await
            .expect("peek")
            .expect("session");
        assert_eq!(session.unacked_stanzas.len(), 1, "one detached allocation");
        assert_eq!(fixture.count("sm_ingress_appends").await, 1);
        assert!(receiver.try_recv().is_err());
    } else {
        assert!(receiver.try_recv().is_ok(), "one accepted carbon");
        assert!(
            receiver.try_recv().is_err(),
            "lost reply cannot replay carbon"
        );
        assert_eq!(
            fixture.count("ingress_send_attempts WHERE state = 2").await,
            i64::from(!ambiguous)
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_remote_carbon_lost_reply_does_not_repeat_resource_send() {
    remote_carbon_lost_reply_does_not_repeat_resource_send(
        IngressFixture::sqlite().await,
        CarbonKind::Sent,
        false,
        false,
        false,
    )
    .await;
}

#[tokio::test]
async fn sqlite_remote_carbon_lost_reply_does_not_repeat_detached_append() {
    remote_carbon_lost_reply_does_not_repeat_resource_send(
        IngressFixture::sqlite().await,
        CarbonKind::Sent,
        true,
        false,
        false,
    )
    .await;
}

#[tokio::test]
async fn sqlite_remote_carbon_lost_reply_repairs_disconnected_resource() {
    remote_carbon_lost_reply_does_not_repeat_resource_send(
        IngressFixture::sqlite().await,
        CarbonKind::Sent,
        false,
        true,
        false,
    )
    .await;
}

#[tokio::test]
async fn sqlite_remote_carbon_disconnected_started_resource_blocks_aggregate_receipt() {
    remote_carbon_lost_reply_does_not_repeat_resource_send(
        IngressFixture::sqlite().await,
        CarbonKind::Sent,
        false,
        true,
        true,
    )
    .await;
}

#[tokio::test]
async fn postgres_remote_carbon_lost_reply_does_not_repeat_resource_send() {
    if let Some(fixture) = IngressFixture::postgres("remote_carbon_lost_reply").await {
        remote_carbon_lost_reply_does_not_repeat_resource_send(
            fixture,
            CarbonKind::Sent,
            false,
            false,
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn postgres_remote_carbon_disconnected_started_resource_blocks_aggregate_receipt() {
    if let Some(fixture) = IngressFixture::postgres("remote_carbon_started").await {
        remote_carbon_lost_reply_does_not_repeat_resource_send(
            fixture,
            CarbonKind::Sent,
            false,
            true,
            true,
        )
        .await;
    }
}

#[tokio::test]
async fn sqlite_received_remote_carbon_lost_reply_preserves_correspondent_sender() {
    remote_carbon_lost_reply_does_not_repeat_resource_send(
        IngressFixture::sqlite().await,
        CarbonKind::Received,
        false,
        false,
        false,
    )
    .await;
}

#[tokio::test]
async fn sqlite_remote_carbon_owner_receiver_retains_keyed_custody() {
    use crate::clustering::route_bridge::tests::delivery::{
        remote_carbon_owner_reply_with_ingress, RemoteCarbonIngressFixture,
    };
    let fixture = IngressFixture::sqlite().await;
    let mut submission = fixture.submission(Some("keyed-carbon-owner"), "carbon body");
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
        owner,
        exclude: vec![source.clone()],
        kind: CarbonKind::Sent,
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(effect.clone()))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("canonical commit");
    let context = carbon_progress::append_context(
        &fixture.uow,
        decision.message_key.expect("key"),
        &effect,
        &decision.external_receipts[0],
    )
    .await
    .expect("context")
    .expect("relay context");
    let obligation = crate::ingress::identity::IngressAppendObligationRef::from_context(
        &context,
        source.to_bare(),
    );
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared database");
    let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool), Arc::new(fixture.authority().await),
    ).await;
    let sm = Arc::new(
        InMemorySmSessionRegistry::new().with_persistence(Arc::new(
            DatabaseSmPersistence::open(Some(fixture.db.database_url()))
                .await
                .expect("SM store"),
        )),
    );
    let reply = remote_carbon_owner_reply_with_ingress(
        source,
        sm,
        Some(RemoteCarbonIngressFixture {
            state: state.clone(),
            message: submission.plan.sanitized_message.clone(),
            obligation,
        }),
        async {},
    )
    .await;
    assert_eq!(reply.status, RelayRemoteUserSideEffectStatus::Applied);
    assert_eq!(reply.carbon_recipients.len(), 1);
    assert_eq!(
        fixture.count("sm_ingress_appends").await,
        1,
        "owner receiver must retain the transported key at the detached sink"
    );
    state
        .deps
        .protocol
        .ingress
        .drain_and_join(Duration::from_secs(1))
        .await;
    drop(state);
    fixture.close().await;
}
