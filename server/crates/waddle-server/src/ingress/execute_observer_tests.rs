use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use crate::server::routes::interpret::effects::{room::ExternalRoomEffect, PlanSuppressionPolicy};
use waddle_xmpp::{ingress::IngressEffectIntent, registry::ConnectionRegistry};
use xmpp_parsers::message::Message;

fn observer_intent(effect: &ExternalRoomEffect) -> IngressEffectIntent {
    let ExternalRoomEffect::ObserveRoomMessage {
        room,
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
    }
}

async fn observer_failure_retry_and_receipt(fixture: IngressFixture) {
    let mut submission = fixture.submission(Some("observer-retry"), "observed message");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let effect = ExternalRoomEffect::ObserveRoomMessage {
        room,
        message: Box::new(submission.plan.sanitized_message.clone()),
        requester: submission.sender.to_bare(),
        sender: submission.sender.clone(),
        error_request: Box::new(submission.plan.sanitized_message.clone()),
    };
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
    let failed = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &unavailable_deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(failed.outcomes[0].1, ExternalOutcome::Failed);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("pending observer"));

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

    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(state.as_ref());
    let completed = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(completed.outcomes[0].1, ExternalOutcome::Done);
    assert!(completed.receipt_failures.is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("complete observer"));

    let duplicate = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("receipted observer retry");
    let skipped = execute_effects(
        &fixture.uow,
        &fixture.db,
        &duplicate,
        &ImmediateSink,
        &unavailable_deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        skipped.outcomes[0].1,
        ExternalOutcome::Done,
        "receipted invocation is not repeated against unavailable observer"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_observer_failure_retries_recorded_payload_and_receipts_success_once() {
    observer_failure_retry_and_receipt(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_observer_failure_retries_recorded_payload_and_receipts_success_once() {
    if let Some(fixture) = IngressFixture::postgres("observer_retry").await {
        observer_failure_retry_and_receipt(fixture).await;
    }
}

#[test]
fn observer_warning_reply_does_not_receipt_failed_invocation() {
    let effect = ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage {
        room: "room@muc.example.com".parse().expect("room"),
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
    submission.plan.intents = vec![intent];
    submission.plan.plan = vec![
        PlannedEffect::new(Effect::External(ExternalEffect::Room(effect)))
            .with_suppression(PlanSuppressionPolicy::Always),
    ];
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("maximum admitted body commits");
    let key = first.message_key.expect("canonical key");
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
