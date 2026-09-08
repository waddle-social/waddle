//! Frame obligations are resolved once their receipts are durable, even
//! when the follow-up terminalization cannot acquire the canonical row.
use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::CanonicalMessageRepository;
use waddle_xmpp::ingress::{MessageKey, SemanticDigest};

async fn terminalization_failure_after_receipts_is_not_unresolved(fixture: IngressFixture) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let key = MessageKey::new();
    let mut transaction = fixture.uow.begin().await.expect("begin");
    CanonicalMessageRepository::record_message(
        &mut transaction,
        key,
        &SemanticDigest::from_storage(1, [7; 32]).expect("digest"),
        None,
    )
    .await
    .expect("record canonical message");
    transaction.commit().await.expect("commit");
    let frame = Stanza::Message(xmpp_parsers::message::Message::new(None));
    let mut report = ExecutionReport::default();
    report.message_key = Some(key);
    report.outcomes.push((
        ExternalEffect::Frame(Box::new(frame.clone())),
        ExternalOutcome::AwaitingFrameDelivery,
    ));
    report.frame_obligations.push(FrameObligation {
        frames: vec![frame],
        receipt_keys: Vec::new(),
        effect_index: 0,
    });
    let baseline = metrics
        .counter_sum("ingress.effects.unresolved", &[])
        .unwrap_or(0);
    // A concurrent writer holds the canonical row, so the terminalization
    // that follows receipt persistence times out.
    let mut held = fixture.uow.begin().await.expect("hold writer");
    assert!(CanonicalMessageRepository::lock(&mut held, key)
        .await
        .expect("lock canonical row"));
    report
        .complete_frame_obligations(&fixture.uow, &fixture.db, Duration::from_secs(2))
        .await
        .expect_err("terminalization is blocked");
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    drop(report);
    assert_eq!(
        metrics
            .counter_sum("ingress.effects.unresolved", &[])
            .unwrap_or(0),
        baseline,
        "delivered and receipted frames are not unresolved effects"
    );
    drop(held);
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("terminalize once the row is free"));
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_terminalization_failure_after_receipts_is_not_unresolved() {
    terminalization_failure_after_receipts_is_not_unresolved(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_terminalization_failure_after_receipts_is_not_unresolved() {
    let Some(fixture) = IngressFixture::postgres("frame_completion_terminalization").await else {
        return;
    };
    terminalization_failure_after_receipts_is_not_unresolved(fixture).await;
}
