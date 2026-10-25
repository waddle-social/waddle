//! Generic delivery and sender-frame proof never settle per-occupant MUC progress.
use super::{
    classify_outcome, completed_receipts, proven_receipts, terminalize_if_complete, Duration,
    Effect, EffectOutcome, EffectReceiptKey, EffectReceiptRepository, ExecutionReport,
    ExternalDeliveryEffect, ExternalEffect, ExternalOutcome, FrameObligation,
    FullJidDeliveryOutcome, IngressDecision, PlannedEffect, Stanza,
};
use crate::ingress::commit::commit_submission;
use crate::ingress::test_support::IngressFixture;
use crate::server::routes::interpret::effects::delivery::PeerDeliveryKind;
use crate::server::routes::interpret::DeliveryExecutionContext;
use jid::{BareJid, FullJid};
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, EffectMessageIdentity, EntityGeneration, IngressEffectIntent,
    NormalizedTarget,
};
use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId};
use xmpp_parsers::message::MessageType;

pub(super) async fn groupchat_decision(fixture: &IngressFixture) -> IngressDecision {
    let mut submission = fixture.submission(Some("groupchat-receipt"), "room fanout");
    let room: BareJid = "room@muc.example.com".parse().expect("room");
    let local: FullJid = "juliet@example.com/phone".parse().expect("local occupant");
    let remote: FullJid = "mercutio@example.com/phone"
        .parse()
        .expect("remote occupant");
    let sender = submission.sender.clone();
    let stamp = StanzaId::new("room-accepted", room.clone().into());
    let mut message = submission.plan.sanitized_message.clone();
    message.type_ = MessageType::Groupchat;
    message.to = Some(room.clone().into());
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("groupchat digest");
    submission.plan.sanitized_message = message.clone();
    message.from = Some(room.with_resource_str("romeo").expect("room nick").into());
    add_stanza_id(&mut message, &stamp);
    crate::ingress::test_support::capture_room_message(&mut submission.plan, &message);
    let copy = |recipient: &FullJid| {
        let mut copy = message.clone();
        copy.to = Some(recipient.clone().into());
        Box::new(Stanza::Message(copy))
    };
    let intent = IngressEffectIntent::RouteMucGroupchat {
        room,
        occupants: vec![local.clone(), remote.clone(), sender.clone()],
        reflection: sender.clone(),
        room_generation: EntityGeneration::INITIAL,
        route_identity: EffectMessageIdentity::stanza(stamp),
    };
    let effects = vec![
        ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
            route_identity: None,
            jid: local.clone(),
            stanza: copy(&local),
            kind: PeerDeliveryKind::PeerStanza,
            call_setup: None,
        }),
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid {
            route_identity: None,
            origin: None,
            target: remote.clone(),
            stanza: copy(&remote),
            call_setup: None,
        }),
        ExternalEffect::Frame(copy(&sender)),
    ];
    submission.plan.intents = vec![intent];
    submission.plan.plan = effects
        .into_iter()
        .map(|effect| PlannedEffect::new(Effect::External(effect)))
        .collect();
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit room fanout");
    assert_eq!(decision.external.len(), 3);
    let receipt = &decision.external_receipts[0];
    assert_eq!(receipt.len(), 1);
    assert_eq!(&decision.external_receipts[1], receipt);
    assert_eq!(decision.external_receipts[2].len(), 1);
    assert_ne!(
        &decision.external_receipts[2], receipt,
        "sender reflection owns a distinct child receipt, never the aggregate MUC key"
    );
    assert!(decision.arm_owned_receipts.contains(&receipt[0]));
    decision
}

/// Use the same owner-reply proof seam as `relayed_direct_receipt`: the
/// transport has finished before Phase C classifies its typed delivery result.
fn groupchat_outcomes(
    decision: &IngressDecision,
    relay: FullJidDeliveryOutcome,
) -> (ExecutionReport, Vec<Vec<EffectReceiptKey>>) {
    let mut report = ExecutionReport::new(DeliveryExecutionContext::Live.into());
    report.message_key = decision.message_key;
    let mut proven = Vec::new();
    for (index, effect) in decision.external.iter().enumerate() {
        let result = match effect {
            ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { .. }) => {
                EffectOutcome::Delivery(FullJidDeliveryOutcome::Delivered)
            }
            ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid { .. }) => {
                EffectOutcome::Delivery(relay)
            }
            ExternalEffect::Frame(stanza) => EffectOutcome::Frames(vec![*stanza.clone()]),
            _ => panic!("unexpected room fanout effect"),
        };
        proven.push(proven_receipts(
            effect,
            &result,
            &decision.external_receipts[index],
        ));
        let mut frames = Vec::new();
        let mut outcome = classify_outcome(effect, result, &mut frames);
        if !frames.is_empty() {
            assert_eq!(outcome, ExternalOutcome::Done);
            report.frame_obligations.push(FrameObligation {
                frames,
                receipt_keys: proven[index].clone(),
                effect_index: index,
            });
            outcome = ExternalOutcome::AwaitingFrameDelivery;
        }
        report.outcomes.push((effect.clone(), outcome));
        assert!(completed_receipts(decision, &report.outcomes, &proven, index).is_empty());
    }
    (report, proven)
}

async fn groupchat_aggregate_receipt(fixture: IngressFixture, relay: FullJidDeliveryOutcome) {
    let decision = groupchat_decision(&fixture).await;
    let (mut report, proven) = groupchat_outcomes(&decision, relay);
    let key = decision.message_key.expect("canonical room message");
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("fanout is pending")
    );
    // Even generic Delivered proof for both occupants plus a written sender
    // frame cannot replace the transactional per-resource progress arm.
    report.outcomes[2].1 = ExternalOutcome::Done;
    for index in 0..report.outcomes.len() {
        let completed = completed_receipts(&decision, &report.outcomes, &proven, index);
        assert_eq!(
            completed,
            if index == 2 {
                decision.external_receipts[2].clone()
            } else {
                Vec::new()
            },
            "generic delivery and sender proof cannot complete aggregate occupant progress"
        );
    }
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("MUC progress remains pending")
    );
    fixture.close().await;
}

async fn groupchat_frame_completion(fixture: IngressFixture, relay: FullJidDeliveryOutcome) {
    let decision = groupchat_decision(&fixture).await;
    let (mut report, proven) = groupchat_outcomes(&decision, relay);
    // Reflection is governed by its sender stream, independent of the
    // non-sender occupant obligation, including confirmed relay outcomes.
    let mut confirmed = report.outcomes.clone();
    confirmed[2].1 = ExternalOutcome::Done;
    report.frame_completion_receipts = completed_receipts(&decision, &confirmed, &proven, 2);
    assert_eq!(
        report.frame_completion_receipts,
        decision.external_receipts[2]
    );
    assert_eq!(
        report.frame_obligations[0].receipt_keys,
        decision.external_receipts[2]
    );
    let frame_receipts = report.frame_receipts();
    assert_eq!(frame_receipts.len(), 1);
    assert_eq!(
        frame_receipts[0].semantic_identity_hash,
        decision.external_receipts[2][0].semantic_identity_hash,
        "retained frames export only the independent original sender receipt"
    );
    assert_ne!(
        frame_receipts[0].semantic_identity_hash,
        decision.external_receipts[0][0].semantic_identity_hash,
        "frame confirmation cannot export aggregate occupant proof"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(!terminalize_if_complete(
        &fixture.uow,
        decision.message_key.expect("canonical key"),
        DeliveryExecutionContext::Live.into()
    )
    .await
    .expect("frame write still pending"));
    assert!(!report
        .complete_frame_obligations(&fixture.uow, &fixture.db, Duration::from_secs(5))
        .await
        .expect("complete written sender frame"));
    assert_eq!(report.outcomes[2].1, ExternalOutcome::Done);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    let mut tx = fixture.uow.begin().await.expect("inspect frame proof");
    let aggregate = &decision.external_receipts[0][0];
    assert!(!EffectReceiptRepository::contains(
        &mut tx,
        decision.message_key.expect("canonical key"),
        aggregate.kind,
        &aggregate.semantic_identity_hash,
    )
    .await
    .expect("aggregate remains pending"));
    tx.commit().await.expect("read frame proof");
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_groupchat_generic_delivery_cannot_settle_occupant_progress() {
    for outcome in [
        FullJidDeliveryOutcome::Delivered,
        FullJidDeliveryOutcome::Unavailable,
        FullJidDeliveryOutcome::Dropped,
        FullJidDeliveryOutcome::MaybeCommitted,
    ] {
        groupchat_aggregate_receipt(IngressFixture::sqlite().await, outcome).await;
    }
}

#[tokio::test]
async fn postgres_groupchat_generic_delivery_cannot_settle_occupant_progress() {
    for outcome in [
        FullJidDeliveryOutcome::Delivered,
        FullJidDeliveryOutcome::Unavailable,
        FullJidDeliveryOutcome::Dropped,
        FullJidDeliveryOutcome::MaybeCommitted,
    ] {
        if let Some(fixture) = IngressFixture::postgres("groupchat_aggregate_receipt").await {
            groupchat_aggregate_receipt(fixture, outcome).await;
        }
    }
}

#[tokio::test]
async fn sqlite_groupchat_sender_frame_never_settles_occupant_progress() {
    for outcome in [
        FullJidDeliveryOutcome::Delivered,
        FullJidDeliveryOutcome::Dropped,
    ] {
        groupchat_frame_completion(IngressFixture::sqlite().await, outcome).await;
    }
}

#[tokio::test]
async fn postgres_groupchat_sender_frame_never_settles_occupant_progress() {
    for outcome in [
        FullJidDeliveryOutcome::Delivered,
        FullJidDeliveryOutcome::Dropped,
    ] {
        if let Some(fixture) = IngressFixture::postgres("groupchat_frame_completion").await {
            groupchat_frame_completion(fixture, outcome).await;
        }
    }
}
