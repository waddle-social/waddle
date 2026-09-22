//! XEP-0198 §4–§5 local UserActor queue identity and durable replay allocation.
//! The full registered-socket detach and failed-receipt recovery sequence is in
//! websocket::tests::keyed_detach_drain::user_actor_keyed_append.
use std::time::Duration;

use kameo::actor::Spawn;
use waddle_server::ingress::{
    commit::commit_submission,
    effects::{
        delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
        Effect,
    },
    execute::execute_effects,
    Deps, ExternalEffect, ExternalOutcome, ImmediateSink, PlanSuppressionPolicy, PlannedEffect,
};
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent},
    registry::{ConnectionRegistry, DeliveryKind, RegisterUserResource, UserRegistryActor},
    stream_management::{SmKeyedAppendOutcome, SmSessionRegistry, StreamManagementState},
    Stanza,
};

use crate::{detached_progress_support, ingress_support::IngressFixture};

pub async fn local_queue_retains_append_identity(fixture: IngressFixture, kind: PeerDeliveryKind) {
    let connections = ConnectionRegistry::new();
    let users = UserRegistryActor::spawn(UserRegistryActor::new());
    let [target, _, _] = detached_progress_support::resources();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    connections.register(target.clone(), sender);
    users
        .ask(RegisterUserResource {
            jid: target.clone(),
            entry: connections.get_entry(&target).expect("registered resource"),
        })
        .await
        .expect("authoritative local UserActor registration");
    let mut submission = fixture.submission(Some("local-keyed"), "one local replay allocation");
    let identity = EffectMessageIdentity::capture_ordinal(0);
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RouteDirect {
            recipient: target.to_bare(),
            fanout: vec![target.clone()],
            route_identity: identity.clone(),
        });
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer {
                route_identity: Some(identity),
                jid: target.clone(),
                stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
                kind,
                call_setup: None,
            },
        )))
        .with_suppression(PlanSuppressionPolicy::SenderOnly),
    );
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit local delivery");
    let mut deps = Deps::new(&connections, "example.com");
    deps.user_registry = Some(&users);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    let queued = receiver.try_recv().expect("UserActor queued the frame");
    assert!(receiver.try_recv().is_err(), "exactly one local frame");
    assert_eq!(
        queued.kind,
        match kind {
            PeerDeliveryKind::DirectFrame => DeliveryKind::DirectFrame,
            PeerDeliveryKind::PeerStanza => DeliveryKind::PeerStanza,
            PeerDeliveryKind::RegistryFrame => unreachable!("actor delivery variants only"),
        }
    );
    let obligation = queued
        .ingress_append
        .expect("local UserActor retains the committed append obligation");
    assert_eq!(obligation.key.message_key, decision.message_key.unwrap());
    assert_eq!(obligation.key.resource, target);
    assert_eq!(obligation.sender_bare, submission.sender.to_bare());
    assert_eq!(
        obligation.key.semantic_identity_hash,
        decision.external_receipts[0][0].semantic_identity_hash
    );
    assert_eq!(
        obligation.key.kind.to_storage(),
        decision.external_receipts[0][0].kind.to_storage()
    );
    assert_eq!(
        obligation.received_at,
        decision.route_progress[0].received_at
    );
    let actual: minidom::Element = match &queued.stanza {
        Stanza::Message(message) => message.clone().into(),
        _ => panic!("message queue"),
    };
    let expected: minidom::Element = submission.plan.sanitized_message.into();
    assert_eq!(
        actual, expected,
        "append metadata must not alter the XMPP message"
    );

    let mut sm = detached_progress_support::registry(&fixture).await;
    detached_progress_support::attach(&sm, &target).await;
    for attempt in 0..2 {
        let result = sm
            .record_keyed_stanza_for_detached_bound_resource(
                &target,
                &queued.stanza,
                obligation.received_at.expect("canonical receive time"),
                obligation.key.clone(),
            )
            .await
            .expect("durable local append");
        assert!(matches!(
            (attempt, result),
            (0, SmKeyedAppendOutcome::Appended { .. })
                | (1, SmKeyedAppendOutcome::AlreadyAppended { .. })
        ));
        assert_eq!(fixture.count("sm_ingress_appends").await, 1);
        let snapshot = detached_progress_support::queued(&sm, &target).await;
        assert_eq!(snapshot.outbound_count, 1);
        assert_eq!(snapshot.unacked_stanzas.len(), 1);
        if attempt == 0 {
            drop(sm);
            sm = detached_progress_support::registry(&fixture).await;
            assert_eq!(
                sm.restore_from_persistence()
                    .await
                    .expect("restore local replay"),
                1
            );
        }
    }
    let snapshot = detached_progress_support::queued(&sm, &target).await;
    let mut resumed = StreamManagementState::new();
    resumed.restore_from_session(&snapshot);
    assert_eq!(resumed.get_stanzas_to_resend(0).len(), 1);
    resumed.acknowledge(1);
    assert!(resumed.get_stanzas_to_resend(1).is_empty());
    users.kill();
    drop(sm);
    fixture.close().await;
}
