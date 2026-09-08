//! One room stamp receipts the complete occupant fanout, including its sender frame.
use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use crate::server::routes::interpret::effects::delivery::PeerDeliveryKind;
use jid::{BareJid, FullJid};
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, EffectMessageIdentity, EntityGeneration, IngressEffectIntent,
    NormalizedTarget,
};
use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId};
use xmpp_parsers::message::MessageType;

async fn groupchat_decision(fixture: &IngressFixture) -> IngressDecision {
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
    assert!(decision
        .external_receipts
        .iter()
        .all(|keys| keys == receipt));
    decision
}

/// Use the same owner-reply proof seam as `relayed_direct_receipt`: the
/// transport has finished before Phase C classifies its typed delivery result.
fn groupchat_outcomes(
    decision: &IngressDecision,
    relay: FullJidDeliveryOutcome,
) -> (ExecutionReport, Vec<Vec<EffectReceiptKey>>) {
    let mut report = ExecutionReport::default();
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
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("fanout is pending"));
    // The frame is the last covered effect to finish. Local delivery and even
    // a confirmed remote delivery cannot receipt the aggregate on their own.
    report.outcomes[2].1 = ExternalOutcome::Done;
    if relay == FullJidDeliveryOutcome::Delivered {
        for index in 0..report.outcomes.len() {
            report.outcomes[index].1 = ExternalOutcome::Failed;
            assert!(completed_receipts(&decision, &report.outcomes, &proven, 2).is_empty());
            report.outcomes[index].1 = ExternalOutcome::Done;
        }
    }
    let receipts = completed_receipts(&decision, &report.outcomes, &proven, 2);
    let delivered = relay == FullJidDeliveryOutcome::Delivered;
    assert_eq!(receipts.len(), usize::from(delivered));
    for receipt in receipts {
        EffectReceiptRepository::record_receipt_pooled(
            &fixture.db,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("record complete occupant fanout");
    }
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        i64::from(delivered)
    );
    assert_eq!(
        terminalize_if_complete(&fixture.uow, key)
            .await
            .expect("terminalize complete room fanout"),
        delivered
    );
    fixture.close().await;
}

async fn groupchat_frame_completion(fixture: IngressFixture, relay: FullJidDeliveryOutcome) {
    let decision = groupchat_decision(&fixture).await;
    let (mut report, proven) = groupchat_outcomes(&decision, relay);
    // Phase C prepares the keys that a successful socket frame write can
    // discharge; a failed relay must never enter that completion set.
    let mut confirmed = report.outcomes.clone();
    confirmed[2].1 = ExternalOutcome::Done;
    report.frame_completion_receipts = completed_receipts(&decision, &confirmed, &proven, 2);
    let delivered = relay == FullJidDeliveryOutcome::Delivered;
    assert_eq!(
        report.frame_completion_receipts.len(),
        usize::from(delivered)
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(
        !terminalize_if_complete(&fixture.uow, decision.message_key.expect("canonical key"))
            .await
            .expect("frame write still pending")
    );
    assert_eq!(
        report
            .complete_frame_obligations(&fixture.uow, &fixture.db, Duration::from_secs(5))
            .await
            .expect("complete written sender frame"),
        delivered
    );
    assert_eq!(report.outcomes[2].1, ExternalOutcome::Done);
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        i64::from(delivered)
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_groupchat_receipt_requires_every_occupant_and_sender_frame() {
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
async fn postgres_groupchat_receipt_requires_every_occupant_and_sender_frame() {
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
async fn sqlite_groupchat_frame_completion_requires_confirmed_relay() {
    for outcome in [
        FullJidDeliveryOutcome::Delivered,
        FullJidDeliveryOutcome::Dropped,
    ] {
        groupchat_frame_completion(IngressFixture::sqlite().await, outcome).await;
    }
}

#[tokio::test]
async fn postgres_groupchat_frame_completion_requires_confirmed_relay() {
    for outcome in [
        FullJidDeliveryOutcome::Delivered,
        FullJidDeliveryOutcome::Dropped,
    ] {
        if let Some(fixture) = IngressFixture::postgres("groupchat_frame_completion").await {
            groupchat_frame_completion(fixture, outcome).await;
        }
    }
}
