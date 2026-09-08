use crate::ingress::{
    commit::commit_submission, execute::terminalize_if_complete, test_support::IngressFixture,
    IngressDecisionClass,
};
use crate::server::routes::interpret::effects::{
    delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
    room::ExternalRoomEffect,
    Effect, ExternalEffect, PlanSuppressionPolicy, PlannedEffect,
};
use waddle_xmpp::ingress::{
    EffectMessageIdentity, GroupDmHistoryVisibility, GroupDmMembershipGrant, IngressEffectIntent,
};

async fn empty_accepted_authority(fixture: IngressFixture) {
    // A suppressed invitation commits successfully without any obligations.
    let mut submission = fixture.submission(Some("empty-invite-authority"), "invitation");
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("empty acceptance");
    assert_eq!(first.class, IngressDecisionClass::Accepted);
    let key = first.message_key.expect("canonical key");
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("empty authority terminal"));
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
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("still terminal"));
    fixture.close().await;
}

async fn newly_enabled_observer(fixture: IngressFixture) {
    let mut submission = fixture.submission(Some("new-observer-policy"), "room message");
    // Keep a nonempty historical authority to exercise the omission-repair
    // branch independently of empty accepted authorities.
    submission.plan.intents = vec![IngressEffectIntent::RouteDirect {
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
        });
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Room(
            ExternalRoomEffect::ObserveRoomMessage {
                room,
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
