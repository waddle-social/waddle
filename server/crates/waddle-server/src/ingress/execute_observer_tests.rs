use super::*;
use crate::ingress::{
    commit::commit_submission,
    test_support::{capture_room_message, IngressFixture},
};
use crate::server::routes::interpret::effects::{room::ExternalRoomEffect, PlanSuppressionPolicy};
use crate::server::routes::interpret::DeliveryExecutionContext;
use waddle_xmpp::{ingress::IngressEffectIntent, registry::ConnectionRegistry};
use xmpp_parsers::message::Message;

fn observer_plugin() -> waddle_extensions::PluginId {
    waddle_extensions::PluginId::new("message-hook-fixture").expect("fixture plugin")
}

fn observer_intent(effect: &ExternalRoomEffect) -> IngressEffectIntent {
    let ExternalRoomEffect::ObserveRoomMessage {
        room,
        plugin,
        requester,
        sender,
        ..
    } = effect
    else {
        panic!("observer effect required");
    };
    IngressEffectIntent::RoomObserver {
        room: room.clone(),
        requester: requester.clone(),
        sender: sender.clone(),
        plugin: plugin.clone(),
        correction_target: None,
        generation: waddle_extensions::ObservationGeneration::new(1).expect("generation"),
        identity: waddle_extensions::Sha256Digest::new("0".repeat(64)).expect("identity"),
    }
}

async fn observer_retry_preserves_payload_without_synchronous_invocation(fixture: IngressFixture) {
    let mut submission = fixture.submission(Some("observer-retry"), "observed message");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let submission_message = submission.plan.sanitized_message.clone();
    let effect = ExternalRoomEffect::ObserveRoomMessage {
        room,
        plugin: observer_plugin(),
        message: Box::new(submission.plan.sanitized_message.clone()),
        requester: submission.sender.to_bare(),
        sender: submission.sender.clone(),
        error_request: Box::new(submission.plan.sanitized_message.clone()),
    };
    capture_room_message(&mut submission.plan, &submission_message);
    submission.plan.intents = vec![observer_intent(&effect)];
    submission.plan.plan = vec![
        PlannedEffect::new(Effect::External(ExternalEffect::Room(effect)))
            .with_suppression(PlanSuppressionPolicy::Always),
    ];
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit observer intent");
    let key = first.message_key.expect("canonical key");
    assert_eq!(fixture.count("ingress_effect_intents").await, 1);
    assert_eq!(first.external_receipts[0].len(), 1);
    let registry = ConnectionRegistry::new();
    let unavailable_deps = Deps::new(&registry, "example.com");
    let deferred = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &unavailable_deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(deferred.outcomes[0].1, ExternalOutcome::AwaitingPredecessor);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("pending observer")
    );

    // Reconciliation retains the recorded invocation even if current enrichment
    // changes. The duplicate must execute the original payload while unresolved.
    let Effect::External(ExternalEffect::Room(ref mut planned)) = submission.plan.plan[0].effect
    else {
        panic!("observer plan");
    };
    let ExternalRoomEffect::ObserveRoomMessage { message, .. } = planned else {
        panic!("observer work");
    };
    message
        .bodies
        .insert(Default::default(), "new enrichment".to_owned());
    submission.plan.intents = vec![observer_intent(planned)];
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry observer");
    assert_eq!(retry.message_key, Some(key));
    assert_eq!(
        retry.external.len(),
        1,
        "duplicate retains unresolved observer"
    );
    assert_eq!(retry.external_receipts, first.external_receipts);
    let ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage { message, .. }) =
        &retry.external[0]
    else {
        panic!("recorded observer");
    };
    assert_eq!(
        message.bodies.values().next().expect("body"),
        "observed message"
    );

    let deferred_retry = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &unavailable_deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        deferred_retry.outcomes[0].1,
        ExternalOutcome::AwaitingPredecessor
    );
    assert!(deferred_retry.receipt_failures.is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("pending observer")
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_observer_retry_preserves_payload_without_synchronous_invocation() {
    observer_retry_preserves_payload_without_synchronous_invocation(IngressFixture::sqlite().await)
        .await;
}

#[tokio::test]
async fn postgres_observer_retry_preserves_payload_without_synchronous_invocation() {
    if let Some(fixture) = IngressFixture::postgres("observer_retry").await {
        observer_retry_preserves_payload_without_synchronous_invocation(fixture).await;
    }
}

#[test]
fn observer_warning_reply_does_not_receipt_failed_invocation() {
    let effect = ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage {
        room: "room@muc.example.com".parse().expect("room"),
        plugin: observer_plugin(),
        message: Box::new(Message::new(None)),
        requester: "romeo@example.com".parse().expect("requester"),
        sender: "romeo@example.com/phone".parse().expect("sender"),
        error_request: Box::new(Message::new(None)),
    });
    let mut frames = Vec::new();
    assert_eq!(
        classify_outcome(
            &effect,
            EffectOutcome::Frames(vec![Stanza::Message(Message::new(None))]),
            &mut frames
        ),
        ExternalOutcome::Failed
    );
    assert_eq!(frames.len(), 1, "failure reply still reaches sender");
    assert_eq!(
        classify_outcome(&effect, EffectOutcome::Frames(Vec::new()), &mut Vec::new()),
        ExternalOutcome::Done
    );
}

async fn observer_maximum_body_envelope(fixture: IngressFixture) {
    let body = "&".repeat(waddle_xmpp::ingress::digest::MAX_TEXT_LEN);
    let mut submission = fixture.submission(Some("observer-max-body"), &body);
    let mut original = submission.plan.sanitized_message.clone();
    original.id = None;
    original.payloads.push(
        minidom::Element::builder("metadata", "urn:test:observer")
            .append("z".repeat(waddle_xmpp::ingress::digest::MAX_TEXT_LEN))
            .build(),
    );
    let mut observed = original.clone();
    observed.id = Some(xmpp_parsers::message::Id("generated-room-id".into()));
    observed.from = Some("room@muc.example.com/nick".parse().expect("occupant"));
    observed.payloads.push(
        minidom::Element::builder("enrichment", "urn:test:observer")
            .append("large metadata".repeat(6000))
            .build(),
    );
    submission.plan.sanitized_message = observed.clone();
    let effect = ExternalRoomEffect::ObserveRoomMessage {
        room: "room@muc.example.com".parse().expect("room"),
        plugin: observer_plugin(),
        message: Box::new(observed.clone()),
        requester: submission.sender.to_bare(),
        sender: submission.sender.clone(),
        error_request: Box::new(original.clone()),
    };
    let intent = observer_intent(&effect);
    assert!(
        intent
            .with_encoded_v1(|_, payload| payload.len())
            .expect("compact intent")
            < 1024
    );
    capture_room_message(&mut submission.plan, &observed);
    submission.plan.intents = vec![intent];
    submission.plan.plan = vec![
        PlannedEffect::new(Effect::External(ExternalEffect::Room(effect)))
            .with_suppression(PlanSuppressionPolicy::Always),
    ];
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("maximum admitted body commits");
    let key = first.message_key.expect("canonical key");
    let mut tx = fixture.uow.begin().await.expect("inspect envelope");
    let envelope = crate::ingress_uow::CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("load envelope")
        .expect("persisted envelope");
    assert_eq!(envelope.message(), &observed);
    assert_eq!(envelope.room_observer_request().as_ref(), Some(&original));
    tx.commit().await.expect("finish inspection");
    let Effect::External(ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage {
        message,
        error_request,
        ..
    })) = &mut submission.plan.plan[0].effect
    else {
        panic!("observer effect")
    };
    message
        .bodies
        .insert(Default::default(), "changed retry".into());
    error_request.id = Some(xmpp_parsers::message::Id("changed-request".into()));
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("replay maximum body");
    assert_eq!(retry.message_key, Some(key));
    assert_eq!(retry.external_receipts, first.external_receipts);
    let ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage {
        message,
        error_request,
        ..
    }) = &retry.external[0]
    else {
        panic!("restored observer")
    };
    assert_eq!(message.as_ref(), &observed);
    assert_eq!(error_request.as_ref(), &original);
    fixture.close().await;
}

async fn two_observer_plugins_record_distinct_obligations(fixture: IngressFixture) {
    let mut submission = fixture.submission(Some("two-observer-plugins"), "observed message");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let make_effect = |plugin: &str| ExternalRoomEffect::ObserveRoomMessage {
        room: room.clone(),
        plugin: waddle_extensions::PluginId::new(plugin).expect("plugin"),
        message: Box::new(submission.plan.sanitized_message.clone()),
        requester: submission.sender.to_bare(),
        sender: submission.sender.clone(),
        error_request: Box::new(submission.plan.sanitized_message.clone()),
    };
    let effects = vec![make_effect("observer-one"), make_effect("observer-two")];
    let message = submission.plan.sanitized_message.clone();
    capture_room_message(&mut submission.plan, &message);
    submission.plan.intents = effects.iter().map(observer_intent).collect();
    submission.plan.plan = effects
        .into_iter()
        .map(|effect| {
            PlannedEffect::new(Effect::External(ExternalEffect::Room(effect)))
                .with_suppression(PlanSuppressionPolicy::Always)
        })
        .collect();

    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit per-plugin observer obligations");
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    assert_eq!(decision.external.len(), 2);
    assert_eq!(decision.external_receipts.len(), 2);
    assert!(decision
        .external_receipts
        .iter()
        .all(|receipts| receipts.len() == 1));
    assert_ne!(decision.external_receipts[0], decision.external_receipts[1]);
    let registry = ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let report = tokio::time::timeout(
        Duration::from_millis(500),
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        ),
    )
    .await
    .expect("observer ingress wake stays nonblocking");
    assert_eq!(report.outcomes.len(), 2);
    assert!(report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::AwaitingPredecessor));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_two_observer_plugins_record_distinct_obligations() {
    two_observer_plugins_record_distinct_obligations(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_two_observer_plugins_record_distinct_obligations() {
    if let Some(fixture) = IngressFixture::postgres("two_observer_plugins").await {
        two_observer_plugins_record_distinct_obligations(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_room_observer_maximum_body_commits_and_replays_envelope() {
    observer_maximum_body_envelope(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_room_observer_maximum_body_commits_and_replays_envelope() {
    if let Some(fixture) = IngressFixture::postgres("observer_max_body").await {
        observer_maximum_body_envelope(fixture).await;
    }
}

#[path = "execute_observer_membership_tests.rs"]
mod membership;

async fn non_room_envelope(fixture: IngressFixture) {
    let submission = fixture.submission(Some("no-room-capture"), "sanitized content");
    assert!(submission.plan.room_canonical_message.is_none());
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let mut tx = fixture.uow.begin().await.expect("inspect envelope");
    let envelope = crate::ingress_uow::CanonicalMessageRepository::load_envelope(
        &mut tx,
        decision.message_key.expect("key"),
    )
    .await
    .expect("load envelope")
    .expect("first commit persists envelope");
    assert_eq!(envelope.message(), &submission.plan.sanitized_message);
    assert!(envelope.room_observer_request().is_none());
    tx.commit().await.expect("finish inspection");
    fixture.close().await;
}
#[tokio::test]
async fn sqlite_observer_non_room_first_commit_persists_sanitized_envelope() {
    non_room_envelope(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_observer_non_room_first_commit_persists_sanitized_envelope() {
    if let Some(fixture) = IngressFixture::postgres("observer_non_room").await {
        non_room_envelope(fixture).await;
    }
}
