use super::*;

async fn receipts_terminalization(fixture: IngressFixture) {
    use waddle_server::ingress::{execute::terminalize_if_complete, ExternalEffect};
    use waddle_server::ingress_uow::EffectReceiptRepository;
    use waddle_xmpp::{ingress::FrozenStanzaError, Stanza};
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};
    let mut submission = archive_plan(
        &fixture,
        Some("receipt-origin"),
        "with external intent",
        "receipts-archive",
    );
    let error = StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::NotAcceptable,
        "en",
        "external reply",
    );
    let mut reply = submission.plan.sanitized_message.clone();
    reply.type_ = xmpp_parsers::message::MessageType::Error;
    reply.to = reply.from.take();
    reply.payloads.push(error.clone().into());
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ErrorReply {
            recipient: "romeo@example.com/phone".parse().expect("recipient"),
            error: FrozenStanzaError::from_xmpp(&error).expect("freeze error"),
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(ExternalEffect::Frame(
            Box::new(Stanza::Message(reply)),
        ))));
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit pending effects");
    let key = decision.message_key.expect("canonical");
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("partial receipts"));
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0
    );
    assert_eq!(decision.receipts_pending.len(), 1);
    for receipt in &decision.receipts_pending {
        EffectReceiptRepository::record_receipt_pooled(
            &fixture.db,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("external completion receipt");
    }
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("complete receipts"));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    fixture.close().await;
}
#[tokio::test]
async fn ingress_receipts_terminalization_sqlite() {
    receipts_terminalization(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_receipts_terminalization_postgres() {
    if let Some(fixture) = IngressFixture::postgres("receipts_terminalization").await {
        receipts_terminalization(fixture).await;
    }
}

async fn external_reply_execution(fixture: IngressFixture) {
    use waddle_server::ingress::{execute::execute_effects, Deps, ExternalOutcome, ImmediateSink};
    use waddle_xmpp::{ingress::FrozenStanzaError, registry::ConnectionRegistry, Stanza};
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};
    let mut submission = fixture.submission(None, "denied body");
    let error = StanzaError::new(
        ErrorType::Auth,
        DefinedCondition::NotAuthorized,
        "en",
        "denied",
    );
    let mut reply = submission.plan.sanitized_message.clone();
    reply.type_ = xmpp_parsers::message::MessageType::Error;
    reply.to = reply.from.take();
    reply.payloads.push(error.clone().into());
    submission.plan.error_reply = Some(Stanza::Message(reply));
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ErrorReply {
            recipient: "romeo@example.com/phone".parse().expect("recipient"),
            error: FrozenStanzaError::from_xmpp(&error).expect("frozen error"),
        });
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("committed rejection");
    let registry = ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let timed_out = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        std::time::Duration::ZERO,
    )
    .await;
    assert_eq!(timed_out.outcomes.len(), 1);
    assert_eq!(timed_out.outcomes[0].1, ExternalOutcome::Failed);
    assert!(timed_out.frames.is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0
    );
    assert_eq!(decision.class, IngressDecisionClass::AuthorizationDenied);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes.len(), 1);
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(report.frames.len(), 1);
    assert!(report.receipt_failures.is_empty());
    assert!(report.terminalization_failure.is_none());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    fixture.close().await;
}
#[tokio::test]
async fn ingress_external_reply_execution_sqlite() {
    external_reply_execution(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_external_reply_execution_postgres() {
    if let Some(fixture) = IngressFixture::postgres("external_reply_execution").await {
        external_reply_execution(fixture).await;
    }
}
