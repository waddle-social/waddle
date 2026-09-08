use super::*;
use crate::{
    config::{IngressConfig, LineageConfig},
    ingress::{
        commit::commit_submission, execute::terminalize_if_complete, test_support::IngressFixture,
    },
    ingress_uow::IngressLineage,
    server::routes::interpret::{
        effects::{Effect, ExternalEffect, ImmediateSink, PlannedEffect},
        Deps,
    },
};
use waddle_xmpp::{
    ingress::{FrozenStanzaError, FrozenStanzaErrorType, IngressEffectIntent},
    registry::ConnectionRegistry,
    Stanza, StanzaErrorCondition,
};

async fn cached_reply_receipt(fixture: IngressFixture, fail_storage: bool) {
    let tx = fixture.uow.begin().await.expect("attest fixture");
    let IngressLineage::Attested(lineage) = tx.lineage() else {
        panic!("durable fixture required")
    };
    let config = LineageConfig {
        deployment_uuid: Some(lineage.deployment_uuid),
        action: None,
    };
    tx.commit().await.expect("release attestation");
    let authority = Arc::new(
        crate::ingress::IngressAuthority::new(
            IngressConfig::default(),
            fixture.db.clone(),
            config,
            None,
        )
        .await
        .expect("authority"),
    );
    let mut submission = fixture.submission(Some("relay-frame-receipt"), "reply");
    let error = FrozenStanzaError::new(
        FrozenStanzaErrorType::Cancel,
        StanzaErrorCondition::Conflict,
    );
    let mut message = xmpp_parsers::message::Message::new(Some(submission.sender.clone().into()));
    message.type_ = xmpp_parsers::message::MessageType::Error;
    message.payloads.push(error.to_xmpp().into());
    submission.plan.intents = vec![IngressEffectIntent::ErrorReply {
        recipient: submission.sender.clone(),
        error,
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(ExternalEffect::Frame(
        Box::new(Stanza::Message(message)),
    )))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit reply");
    let key = decision.message_key.expect("canonical key");
    assert_eq!(decision.external_receipts[0].len(), 1);
    let registry = ConnectionRegistry::new();
    let report = authority
        .execute(&decision, &ImmediateSink, &Deps::registry_only(&registry))
        .await;
    assert_eq!(report.frame_obligations.len(), 1);
    let frames = report
        .frame_obligations
        .iter()
        .flat_map(|obligation| obligation.frames.iter().cloned())
        .map(RemoteStanza)
        .collect::<Vec<_>>();
    let receipts = Arc::new(Mutex::new(PendingReplyReceipts::default()));
    let token = receipts
        .lock()
        .await
        .register(RelayFrameReceiptCompletion::new(
            crate::clustering::route_bridge::RelayFrameCompletion {
                authority: Arc::clone(&authority),
                report,
            },
        ))
        .expect("receipt token");
    let receiver = Arc::new(Mutex::new(OrderedRelayReceiverState::default()));
    let envelope = super::super::tests::timeout_envelope();
    let OrderedRelayReservation::Reserved(reserved) =
        receiver.lock().await.reserve(envelope.clone())
    else {
        panic!("first relay reservation")
    };
    let first = receiver.lock().await.commit_reserved_with_reply_receipt(
        *reserved,
        frames.clone(),
        Some(token),
        Vec::new(),
    );
    assert!(matches!(first, OrderedRelayReply::Ack(_)));
    // The first ACK is lost. Only the matching duplicate reaches the origin.
    let duplicate = receiver.lock().await.reserve(envelope);
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    let OrderedRelayReply::Ack(ack) =
        finish_ordered_reservation(receiver, bridge, duplicate, Arc::clone(&receipts)).await
    else {
        panic!("matching duplicate must return its original proof")
    };
    assert!(ack.duplicate);
    assert_eq!(ack.client_replies, frames);
    assert_eq!(ack.reply_receipt, Some(token));
    let token = ack.reply_receipt.expect("received proof");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("awaiting duplicate frame write"));
    // Only frames from the received duplicate authorize confirmation.
    for frame in &ack.client_replies {
        let Stanza::Message(message) = &frame.0 else {
            panic!("message reply")
        };
        let mut bytes = Vec::new();
        minidom::Element::from(message.clone())
            .write_to(&mut bytes)
            .expect("transport write");
        assert!(!bytes.is_empty());
    }
    if fail_storage {
        fixture
            .execute(
                "ALTER TABLE ingress_effect_receipts RENAME TO unavailable_effect_receipts",
                (),
            )
            .await;
        assert!(
            !confirm(&receipts, token).await,
            "owner receipt write fails"
        );
        assert!(
            receipts.lock().await.get(token).is_some(),
            "original proof survives failure"
        );
        fixture
            .execute(
                "ALTER TABLE unavailable_effect_receipts RENAME TO ingress_effect_receipts",
                (),
            )
            .await;
    }
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("unreceipted reply"));
    assert!(
        confirm(&receipts, token).await,
        "same proof retries successfully"
    );
    assert!(
        confirm(&receipts, token).await,
        "lost confirmation reply can be retried"
    );
    assert_eq!(
        receipts.lock().await.pending_count(),
        0,
        "success releases heavy completion capacity"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("terminal reply"));
    drop(receipts);
    assert!(authority.drain_and_join(Duration::from_secs(5)).await);
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_relay_frame_receipt_survives_owner_storage_failure() {
    cached_reply_receipt(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn postgres_relay_frame_receipt_survives_owner_storage_failure() {
    if let Some(fixture) = IngressFixture::postgres("relay_receipt_retry").await {
        cached_reply_receipt(fixture, true).await;
    }
}

#[tokio::test]
async fn sqlite_relay_lost_ack_duplicate_confirms_owner_receipts() {
    cached_reply_receipt(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn postgres_relay_lost_ack_duplicate_confirms_owner_receipts() {
    if let Some(fixture) = IngressFixture::postgres("relay_lost_ack").await {
        cached_reply_receipt(fixture, false).await;
    }
}

async fn owner_reflection_survives_replay(fixture: IngressFixture) {
    let tx = fixture.uow.begin().await.expect("attest fixture");
    let IngressLineage::Attested(lineage) = tx.lineage() else {
        panic!("durable fixture required")
    };
    let config = LineageConfig {
        deployment_uuid: Some(lineage.deployment_uuid),
        action: None,
    };
    tx.commit().await.expect("release attestation");
    let authority = Arc::new(
        crate::ingress::IngressAuthority::new(
            IngressConfig::default(),
            fixture.db.clone(),
            config,
            None,
        )
        .await
        .expect("authority"),
    );

    let mut submission = fixture.submission(Some("relay-reflection-resume"), "reflection");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let stamp = waddle_xmpp_core::xep0359::StanzaId::new("reflection-stamp", room.clone().into());
    let mut message = xmpp_parsers::message::Message::new(Some(submission.sender.clone().into()));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    waddle_xmpp_core::xep0359::add_stanza_id(&mut message, &stamp);
    submission.plan.intents = vec![IngressEffectIntent::RouteMucGroupchat {
        room,
        occupants: vec![],
        reflection: submission.sender.clone(),
        room_generation: waddle_xmpp::ingress::EntityGeneration::INITIAL,
        route_identity: waddle_xmpp::ingress::EffectMessageIdentity::stanza(stamp),
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(ExternalEffect::Frame(
        Box::new(Stanza::Message(message)),
    )))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit reflection");
    let key = decision.message_key.expect("owner key");
    let registry = ConnectionRegistry::new();
    let report = authority
        .execute(&decision, &ImmediateSink, &Deps::registry_only(&registry))
        .await;
    assert_eq!(report.frame_receipts().len(), 1);
    let frames = report
        .frame_obligations
        .iter()
        .flat_map(|obligation| obligation.frames.iter().cloned())
        .map(RemoteStanza)
        .collect();
    let completion =
        RelayFrameReceiptCompletion::new(crate::clustering::route_bridge::RelayFrameCompletion {
            authority: Arc::clone(&authority),
            report,
        });
    let owner_receipts = completion.frame_receipts();
    let mut pending = PendingReplyReceipts::default();
    let token = pending.register(completion).expect("reply token");
    let mut receiver = OrderedRelayReceiverState::default();
    let envelope = super::super::tests::timeout_envelope();
    let OrderedRelayReservation::Reserved(reserved) = receiver.reserve(envelope) else {
        panic!("reserved")
    };
    let reply = receiver.commit_reserved_with_reply_receipt(
        *reserved,
        frames,
        Some(token),
        owner_receipts.clone(),
    );
    let encoded = serde_json::to_vec(&reply).expect("relay wire encode");
    let OrderedRelayReply::Ack(ack) = serde_json::from_slice(&encoded).expect("relay wire decode")
    else {
        panic!("ack")
    };
    let (frames, completion) =
        ack.into_frame_delivery(NodeId::generate(), CancellationToken::new());
    let mut origin = crate::ingress::ExecutionReport::default();
    origin.retain_relay_frame_completion(completion.expect("owner completion"));
    let retained = origin.frame_receipts();
    assert_eq!(retained, owner_receipts);
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("owner pending"));
    // Disconnect before writing. Neither the origin report nor the owner's
    // expiring token table survives the reconnect.
    drop(origin);
    drop(pending);
    drop(receiver);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    for frame in frames {
        let Stanza::Message(message) = frame else {
            panic!("reflection")
        };
        assert_eq!(message.type_, xmpp_parsers::message::MessageType::Groupchat);
        let mut transport = Vec::new();
        minidom::Element::from(message)
            .write_to(&mut transport)
            .expect("resume transport write");
        assert!(!transport.is_empty());
    }
    for mut report in crate::ingress::ExecutionReport::replay_frame_completions(&retained) {
        assert!(authority
            .complete_frame_obligations(&mut report)
            .await
            .expect("owner confirmation"));
    }
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("owner terminal"));
    assert!(authority.drain_and_join(Duration::from_secs(5)).await);
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_relay_reflection_resume_terminalizes_owner_without_token() {
    owner_reflection_survives_replay(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_relay_reflection_resume_terminalizes_owner_without_token() {
    if let Some(fixture) = IngressFixture::postgres("relay_reflection_resume").await {
        owner_reflection_survives_replay(fixture).await;
    }
}
