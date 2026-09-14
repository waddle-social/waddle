use super::*;
use crate::{
    ingress::{commit::commit_submission, receipt_key, test_support::IngressFixture},
    ingress_uow::{
        CanonicalMessageRepository, DeliveryProgressRepository, EffectReceiptRepository,
    },
    server::routes::interpret::effects::delivery::PeerDeliveryKind,
};
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, EffectMessageIdentity, EntityGeneration, IngressEffectIntent,
    NormalizedTarget,
};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::MessageType;

async fn receipted_aggregate_sender_copies(fixture: IngressFixture) {
    let mut submission = fixture.submission(Some("sender-copy-suppression"), "room content");
    let sender = submission.sender.clone();
    let sibling = sender
        .to_bare()
        .with_resource_str("sibling")
        .expect("sibling");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.type_ = MessageType::Groupchat;
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("groupchat digest");
    submission.plan.intents = vec![IngressEffectIntent::RouteMucGroupchat {
        room: room.clone(),
        occupants: vec![sender.clone(), sibling.clone()],
        reflection: sender.clone(),
        room_generation: EntityGeneration::INITIAL,
        route_identity: EffectMessageIdentity::stanza(StanzaId::new("room-copy", room.into())),
    }];
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("first commit");
    let key = first.message_key.expect("canonical key");
    let receipt = receipt_key(&submission.plan.intents[0]).expect("aggregate receipt");
    let mut tx = fixture.uow.begin().await.expect("receipt transaction");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("canonical lock"));
    EffectReceiptRepository::record_receipt(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await
    .expect("completed aggregate");
    assert!(DeliveryProgressRepository::load_all(&mut tx, key)
        .await
        .expect("progress")
        .is_empty());
    tx.commit().await.expect("receipt commit");
    let duplicate = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("duplicate");
    assert!(duplicate.receipts_pending.is_empty());
    assert!(duplicate.route_progress.is_empty());
    let verdict = duplicate.verdict.as_ref().expect("reconciled duplicate");
    assert!(!matches!(verdict, ReconcileVerdict::FirstCommit));

    // Exercise the suppression boundary with SenderOnly effects explicitly:
    // earlier restoration can otherwise remove or promote these copies.
    for message_type in [MessageType::Chat, MessageType::Groupchat] {
        let mut plan = submission.plan.clone();
        plan.sanitized_message.type_ = message_type.clone();
        plan.plan = [&sender, &sibling]
            .into_iter()
            .map(|target| {
                let mut message = plan.sanitized_message.clone();
                message.to = Some(target.clone().into());
                PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
                    ExternalDeliveryEffect::RouteToPeer {
                        route_identity: None,
                        jid: target.clone(),
                        stanza: Box::new(Stanza::Message(message)),
                        kind: PeerDeliveryKind::RegistryFrame,
                        call_setup: None,
                    },
                )))
                .with_suppression(PlanSuppressionPolicy::SenderOnly)
            })
            .collect();
        let effects = filter_external_effects(
            &plan,
            Some(&sender),
            verdict,
            &[],
            &[],
            &duplicate.route_progress,
        );
        let targets: Vec<_> = effects
            .iter()
            .filter_map(super::super::recorded::single_target)
            .cloned()
            .collect();
        let expected = if message_type == MessageType::Groupchat {
            vec![sender.clone()]
        } else {
            vec![sender.clone(), sibling.clone()]
        };
        assert_eq!(targets, expected, "sender copies for {message_type:?}");
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_receipted_aggregate_sender_copies() {
    receipted_aggregate_sender_copies(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_receipted_aggregate_sender_copies() {
    if let Some(fixture) = IngressFixture::postgres("sender_copy_suppression").await {
        receipted_aggregate_sender_copies(fixture).await;
    }
}
