use super::*;
use crate::server::routes::interpret::{effects::PlanSink, interpret};
use waddle_xmpp::{muc::RoomSubjectTexts, protocol::OutboundEvent, registry::ConnectionRegistry};

async fn rejected_subject_receipt(fixture: test_support::IngressFixture) {
    let registry = ConnectionRegistry::new();
    let capture = IngressEffectCapture::new();
    let sink = PlanSink::new();
    let mut deps = Deps::registry_only(&registry);
    deps.ingress_effect_capture = Some(capture.clone());
    deps.effects = &sink;
    let mut submission = fixture.submission(Some("subject-bounce-receipt"), "");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let mut message = submission.plan.sanitized_message.clone();
    message.to = Some(room.clone().into());
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.bodies.clear();
    message
        .subjects
        .insert(xmpp_parsers::message::Lang::new(), "subject".to_owned());
    let outcome = interpret(
        vec![OutboundEvent::PersistRoomSubject {
            room,
            claim_fence: None,
            texts: RoomSubjectTexts::from_iter([(String::new(), "subject".to_owned())]),
            setter: submission.sender.to_bare(),
            sender: submission.sender.clone(),
            message: Box::new(message),
            setter_nick: "romeo".to_owned(),
            set_at: chrono::Utc::now(),
        }],
        &deps,
    )
    .await;
    assert!(outcome.frames.is_empty(), "planning defers the bounce");
    submission.plan.intents = capture.snapshot().intents;
    submission.plan.plan = sink.take().0;
    assert!(matches!(
        submission.plan.intents.as_slice(),
        [waddle_xmpp::ingress::IngressEffectIntent::ErrorReply { .. }]
    ));
    let decision = commit::commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit subject rejection");
    assert_eq!(decision.external_receipts[0].len(), 1);
    deps.effects = &ImmediateSink;
    let mut report = execute::execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::AwaitingFrameDelivery);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(report.frame_obligations.len(), 1);
    for frame in &report.frame_obligations[0].frames {
        let waddle_xmpp::Stanza::Message(message) = frame else {
            panic!("subject bounce must be a message");
        };
        let element = minidom::Element::from(message.clone());
        let mut wire = Vec::new();
        element
            .write_to(&mut wire)
            .expect("write bounce to transport");
        assert!(!wire.is_empty());
    }
    assert!(report
        .complete_frame_obligations(&fixture.uow, &fixture.db, Duration::from_secs(5))
        .await
        .expect("receipt written bounce and terminalize"));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_subject_failure_bounce_receipts_only_after_frame_delivery() {
    rejected_subject_receipt(test_support::IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_subject_failure_bounce_receipts_only_after_frame_delivery() {
    if let Some(fixture) = test_support::IngressFixture::postgres("subject_bounce_receipt").await {
        rejected_subject_receipt(fixture).await;
    }
}
