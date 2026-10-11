use crate::ingress::{
    commit::commit_submission, execute::terminalize_if_complete, test_support::IngressFixture,
    IngressDecisionClass,
};
use crate::server::routes::interpret::effects::{
    delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
    room::ExternalRoomEffect,
    Effect, ExternalEffect, PlanSuppressionPolicy, PlannedEffect,
};
use crate::server::routes::interpret::DeliveryExecutionContext;
use waddle_xmpp::ingress::{
    EffectMessageIdentity, GroupDmHistoryVisibility, GroupDmMembershipGrant, IngressEffectIntent,
};

async fn excluded_effects_never_gain_durable_owner_from_operational_receipts(
    fixture: IngressFixture,
) {
    use crate::ingress_uow::{
        CanonicalMessageRepository, DeliveryEffectRepository, EffectDeliveryBinding,
        EffectDescendantRepository, EffectIntentRepository, EffectReceiptRepository,
        IngressUowError,
    };
    use waddle_xmpp::ingress::{DeliveryKey, IngressEffectKey, MessageKey, SemanticDigest};

    let excluded: Vec<_> = IngressEffectIntent::storage_round_trip_samples()
        .into_iter()
        .filter(|intent| {
            matches!(
                intent.semantic_key(),
                IngressEffectKey::CallSignal(..)
                    | IngressEffectKey::Pin(..)
                    | IngressEffectKey::DmPinMutation(..)
                    | IngressEffectKey::DmCallThreadState(..)
            )
        })
        .collect();
    assert_eq!(excluded.len(), 4, "exercise every excluded variant");
    let key = MessageKey::new();
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("record excluded authority");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [51; 32]).expect("digest"),
        None,
    )
    .await
    .expect("canonical message");
    EffectIntentRepository::reconcile(&mut tx, key, &excluded, false)
        .await
        .expect("record excluded intents");
    tx.commit().await.expect("authority commit");

    for intent in &excluded {
        let effect = intent.semantic_key();
        let receipt = crate::ingress::receipt_key(intent).expect("operational receipt");
        for received in [false, true] {
            if received {
                let mut tx = fixture.uow.begin().await.expect("operational completion");
                EffectReceiptRepository::record_receipt(
                    &mut tx,
                    key,
                    receipt.kind,
                    &receipt.semantic_identity_hash,
                )
                .await
                .expect("record operational completion");
                tx.commit().await.expect("operational completion commit");
            }
            let mut tx = fixture.uow.begin().await.expect("test owner boundary");
            assert_eq!(
                EffectReceiptRepository::contains(
                    &mut tx,
                    key,
                    receipt.kind,
                    &receipt.semantic_identity_hash,
                )
                .await
                .expect("receipt state"),
                received,
            );
            assert_eq!(
                DeliveryEffectRepository::bind_effect(&mut tx, key, &effect)
                    .await
                    .expect("excluded binding decision"),
                EffectDeliveryBinding::AwaitingDurableOwner,
                "{effect:?}: an operational receipt does not supply a durable owner",
            );
            assert!(matches!(
                EffectDescendantRepository::attach(&mut tx, key, &effect, uuid::Uuid::new_v4())
                    .await,
                Err(IngressUowError::EffectIntentConflict)
            ));
            assert_eq!(
                DeliveryEffectRepository::lookup(&mut tx, DeliveryKey::effect(key, &effect))
                    .await
                    .expect("excluded delivery lookup"),
                None,
                "excluded effects cannot mint a host delivery identity",
            );
            tx.commit().await.expect("boundary commit");
            assert_eq!(fixture.count("ingress_deliveries").await, 0);
            assert_eq!(fixture.count("ingress_effect_descendants").await, 0);
        }
    }
    assert_eq!(fixture.count("ingress_effect_receipts").await, 4);

    // A supported effect must still bind and own a descendant in this fixture.
    let supported = IngressEffectIntent::RouteDirect {
        recipient: fixture.principal.bare_jid().clone(),
        fanout: vec![],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
        prepared: None,
    };
    let effect = supported.semantic_key();
    let mut tx = fixture.uow.begin().await.expect("supported control");
    let control = MessageKey::new();
    CanonicalMessageRepository::record_message(
        &mut tx,
        control,
        &SemanticDigest::from_storage(1, [52; 32]).expect("control digest"),
        None,
    )
    .await
    .expect("control canonical message");
    EffectIntentRepository::reconcile(&mut tx, control, &[supported], false)
        .await
        .expect("record supported authority");
    assert_eq!(
        DeliveryEffectRepository::bind_effect(&mut tx, control, &effect)
            .await
            .expect("supported binding"),
        EffectDeliveryBinding::Bound(DeliveryKey::effect(control, &effect)),
    );
    EffectDescendantRepository::attach(&mut tx, control, &effect, uuid::Uuid::new_v4())
        .await
        .expect("supported descendant custody");
    tx.commit().await.expect("control commit");
    assert_eq!(fixture.count("ingress_deliveries").await, 1);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_excluded_effects_never_gain_durable_owner_from_operational_receipts() {
    excluded_effects_never_gain_durable_owner_from_operational_receipts(
        IngressFixture::sqlite().await,
    )
    .await;
}

#[tokio::test]
async fn postgres_excluded_effects_never_gain_durable_owner_from_operational_receipts() {
    if let Some(fixture) = IngressFixture::postgres("excluded_owner_boundary").await {
        excluded_effects_never_gain_durable_owner_from_operational_receipts(fixture).await;
    }
}

async fn empty_accepted_authority(fixture: IngressFixture) {
    // A suppressed invitation commits successfully without any obligations.
    let mut submission = fixture.submission(Some("empty-invite-authority"), "invitation");
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("empty acceptance");
    assert_eq!(first.class, IngressDecisionClass::Accepted);
    let key = first.message_key.expect("canonical key");
    assert!(
        terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("empty authority terminal")
    );
    let repeat = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("unchanged replay");
    assert_eq!(repeat.class, IngressDecisionClass::ExistingConsistent);

    // Current policy now permits the invitee's membership and delivery. These
    // are proposals from the same handler path; they are not historical work.
    let invitee = "juliet@example.com/phone"
        .parse::<jid::FullJid>()
        .expect("invitee");
    let identity = EffectMessageIdentity::capture_ordinal(0);
    submission.plan.intents = vec![
        IngressEffectIntent::GroupDmMembershipGrant {
            grant: GroupDmMembershipGrant {
                room: "private@muc.example.com".parse().expect("room"),
                invitee: invitee.to_bare(),
                inviter: submission.sender.to_bare(),
                history_visibility: GroupDmHistoryVisibility::Full,
            },
        },
        IngressEffectIntent::RouteDirect {
            prepared: None,
            recipient: invitee.to_bare(),
            fanout: vec![invitee.clone()],
            route_identity: identity.clone(),
        },
    ];
    submission.plan.plan = vec![
        PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer {
                route_identity: Some(identity),
                jid: invitee,
                stanza: Box::new(waddle_xmpp::Stanza::Message(
                    submission.plan.sanitized_message.clone(),
                )),
                kind: PeerDeliveryKind::PeerStanza,
                call_setup: None,
            },
        )))
        .with_suppression(PlanSuppressionPolicy::Always),
    ];
    let replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("policy drift replay");
    assert_eq!(replay.class, IngressDecisionClass::ExistingDivergent);
    assert_eq!(replay.message_key, Some(key));
    assert!(
        replay.external.is_empty(),
        "historically suppressed invitation must stay suppressed"
    );
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("ingress_effect_intents").await, 0);
    assert!(
        terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("still terminal")
    );
    fixture.close().await;
}

async fn newly_enabled_observer(fixture: IngressFixture) {
    let mut submission = fixture.submission(Some("new-observer-policy"), "room message");
    // Keep a nonempty historical authority to exercise the omission-repair
    // branch independently of empty accepted authorities.
    submission.plan.intents = vec![IngressEffectIntent::RouteDirect {
        prepared: None,
        recipient: submission.sender.to_bare(),
        fanout: vec![submission.sender.clone()],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    }];
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("original acceptance");
    let key = first.message_key.expect("canonical key");
    let room = "room@muc.example.com"
        .parse::<jid::BareJid>()
        .expect("room");
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RoomObserver {
            room: room.clone(),
            requester: submission.sender.to_bare(),
            sender: submission.sender.clone(),
            plugin: waddle_extensions::PluginId::new("message-hook-fixture")
                .expect("fixture plugin"),
            generation: waddle_extensions::ObservationGeneration::new(1).expect("generation"),
            identity: waddle_extensions::Sha256Digest::new("0".repeat(64)).expect("identity"),
            correction_target: None,
        });
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Room(
            ExternalRoomEffect::ObserveRoomMessage {
                room,
                plugin: waddle_extensions::PluginId::new("message-hook-fixture")
                    .expect("fixture plugin"),
                requester: submission.sender.to_bare(),
                sender: submission.sender.clone(),
                message: Box::new(submission.plan.sanitized_message.clone()),
                error_request: Box::new(submission.plan.sanitized_message.clone()),
            },
        )))
        .with_suppression(PlanSuppressionPolicy::Always),
    );
    let replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("observer policy replay");
    assert_eq!(replay.class, IngressDecisionClass::ExistingDivergent);
    assert_eq!(replay.message_key, Some(key));
    assert!(
        replay.external.is_empty(),
        "newly enabled host mutation must not execute"
    );
    assert_eq!(fixture.count("ingress_effect_intents").await, 1);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_empty_accepted_authority_rejects_new_invite_obligations() {
    empty_accepted_authority(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_empty_accepted_authority_rejects_new_invite_obligations() {
    if let Some(fixture) = IngressFixture::postgres("empty_authority").await {
        empty_accepted_authority(fixture).await;
    }
}
#[tokio::test]
async fn sqlite_replay_rejects_newly_enabled_room_observer() {
    newly_enabled_observer(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_replay_rejects_newly_enabled_room_observer() {
    if let Some(fixture) = IngressFixture::postgres("observer_policy").await {
        newly_enabled_observer(fixture).await;
    }
}

#[test]
fn room_observer_first_owner_acceptance_is_not_historical_policy_drift() {
    let dispatch = IngressEffectIntent::storage_round_trip_samples()
        .into_iter()
        .find(|intent| matches!(intent, IngressEffectIntent::DispatchToRoomRemote { .. }))
        .expect("remote dispatch sample");
    let IngressEffectIntent::DispatchToRoomRemote { room, .. } = &dispatch else {
        unreachable!("selected remote dispatch")
    };
    let room = room.clone();
    let observer = IngressEffectIntent::RoomObserver {
        room: room.clone(),
        requester: "romeo@example.com".parse().expect("requester"),
        sender: "romeo@example.com/phone".parse().expect("sender"),
        plugin: waddle_extensions::PluginId::new("message-hook-fixture").expect("fixture plugin"),
        correction_target: None,
        generation: waddle_extensions::ObservationGeneration::new(1).expect("generation"),
        identity: waddle_extensions::Sha256Digest::new("0".repeat(64)).expect("identity"),
    };
    let mut recorded = vec![super::RecordedEffect {
        ordinal: 0,
        intent: dispatch.clone(),
    }];
    let planned = vec![dispatch, observer.clone()];
    let (verdict, omissions) = super::compare_effects(&recorded, &planned, true);
    assert!(matches!(verdict, super::ReconcileVerdict::Repaired { .. }));
    assert_eq!(omissions, vec![&observer]);

    // Once this room's authority exists, enabling its observer is policy drift.
    let archive = IngressEffectIntent::ArchiveAuthoritative {
        ordinal: None,
        archive: room.clone(),
        by: room.clone(),
        stanza_id: waddle_xmpp_core::xep0359::StanzaId::new("canonical", room.into()),
        archived_at: chrono::Utc::now(),
    };
    recorded.push(super::RecordedEffect {
        ordinal: 1,
        intent: archive.clone(),
    });
    let mut planned = planned;
    planned.push(archive);
    let (verdict, omissions) = super::compare_effects(&recorded, &planned, true);
    assert!(matches!(verdict, super::ReconcileVerdict::Divergent { .. }));
    assert!(omissions.is_empty());
}

#[test]
fn room_observer_plugins_have_distinct_semantic_hashes() {
    let intent = |plugin: &str| IngressEffectIntent::RoomObserver {
        room: "room@muc.example.com".parse().expect("room"),
        requester: "romeo@example.com".parse().expect("requester"),
        sender: "romeo@example.com/phone".parse().expect("sender"),
        plugin: waddle_extensions::PluginId::new(plugin).expect("plugin"),
        correction_target: None,
        generation: waddle_extensions::ObservationGeneration::new(1).expect("generation"),
        identity: waddle_extensions::Sha256Digest::new("0".repeat(64)).expect("identity"),
    };
    let first = intent("observer-one");
    let second = intent("observer-two");
    assert_ne!(first.semantic_key(), second.semantic_key());
    assert_ne!(
        super::semantic_identity_hash(&first),
        super::semantic_identity_hash(&second)
    );
}
