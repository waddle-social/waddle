use super::*;
use crate::server::routes::interpret::effects::{PlannedEffect, RoomExecutionPath};
use waddle_xmpp::{
    inbox::{ConversationKind, InboxEntry},
    ingress::NotificationActivityMutation,
};

fn empty_plan() -> IngressPlan {
    IngressPlan {
        plan: Vec::new(),
        intents: Vec::new(),
        sanitized_message: xmpp_parsers::message::Message::new(None),
        rejection: None,
        error_reply: None,
        room_execution: RoomExecutionPath::None,
    }
}

#[test]
fn recorded_inbox_payload_and_receipt_identity_win_over_policy_drift() {
    let owner = "alice@example.test".parse::<jid::BareJid>().expect("owner");
    let partner = "bob@example.test".parse().expect("partner");
    let offered = InboxEntry::new(partner, ConversationKind::Direct, "archive-id", 300)
        .with_preview("new policy");
    let mut saved = offered.clone();
    saved.preview = None;
    saved.last_updated = 100;
    let original = IngressEffectIntent::InboxProject {
        owner: owner.clone(),
        mutation: InboxProjectionMutation::Direct {
            entry: offered.clone(),
            increment_unread: true,
        },
    };
    let recorded = IngressEffectIntent::InboxProject {
        owner: owner.clone(),
        mutation: InboxProjectionMutation::Direct {
            entry: saved.clone(),
            increment_unread: false,
        },
    };
    let mut plan = empty_plan();
    plan.intents.push(original.clone());
    plan.plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ProjectInbox {
                owner: owner.clone(),
                entry: Box::new(offered),
                increment_unread: true,
            },
        ))));
    plan.plan.push(PlannedEffect::new(Effect::External(
        ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
            owner,
            projection: crate::server::routes::interpret::effects::ProjectionRef(0),
        }),
    )));
    let result = apply_recorded_intents(&plan, std::slice::from_ref(&recorded));
    assert!(matches!(
        result.plan[1].effect,
        Effect::External(ExternalEffect::Direct(
            ExternalDirectEffect::PushInboxUpdate {
                projection: crate::server::routes::interpret::effects::ProjectionRef(0),
                ..
            }
        ))
    ));
    assert_eq!(result.intents, vec![recorded]);
    assert_eq!(plan.intents, vec![original]);
    let Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ProjectInbox {
        entry,
        increment_unread,
        ..
    })) = &result.plan[0].effect
    else {
        panic!("inbox");
    };
    assert_eq!(entry.as_ref(), &saved);
    assert!(!increment_unread);
}

#[test]
fn recorded_notification_policy_uses_matching_mutation_in_shared_conversation() {
    let owner = "alice@example.test".parse::<jid::BareJid>().expect("owner");
    let conversation = "bob@example.test"
        .parse::<jid::BareJid>()
        .expect("conversation");
    let old = NotificationActivityMutation::OutboundMessage {
        conversation: conversation.clone(),
        committed_at_ms: 100,
    };
    let offered = NotificationActivityMutation::OutboundMessage {
        conversation: conversation.clone(),
        committed_at_ms: 200,
    };
    let unrelated = NotificationActivityMutation::ReadMarker {
        conversation,
        committed_at_ms: 90,
    };
    let recorded = vec![
        IngressEffectIntent::NotificationActivityPreview {
            owner: owner.clone(),
            mutation: unrelated,
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: owner.clone(),
            mutation: old.clone(),
        },
    ];
    let mut plan = empty_plan();
    plan.intents
        .push(IngressEffectIntent::NotificationActivityPreview {
            owner: owner.clone(),
            mutation: offered.clone(),
        });
    plan.plan.push(PlannedEffect::new(Effect::External(
        ExternalEffect::Direct(ExternalDirectEffect::NotificationActivity {
            owner,
            mutation: offered,
        }),
    )));
    let result = apply_recorded_intents(&plan, &recorded);
    assert_eq!(result.intents, vec![recorded[1].clone()]);
    let Effect::External(ExternalEffect::Direct(ExternalDirectEffect::NotificationActivity {
        mutation,
        ..
    })) = &result.plan[0].effect
    else {
        panic!("notification");
    };
    assert_eq!(mutation, &old);
}

#[test]
fn recorded_matching_prefers_exact_payload_under_shared_authority() {
    let owner = "alice@example.test".parse::<jid::BareJid>().expect("owner");
    let conversation = "bob@example.test"
        .parse::<jid::BareJid>()
        .expect("conversation");
    let mut plan = empty_plan();
    for committed_at_ms in [100, 200] {
        plan.intents
            .push(IngressEffectIntent::NotificationActivityPreview {
                owner: owner.clone(),
                mutation: NotificationActivityMutation::OutboundMessage {
                    conversation: conversation.clone(),
                    committed_at_ms,
                },
            });
    }
    assert_eq!(
        apply_recorded_intents(&plan, &plan.intents).intents,
        plan.intents
    );
}

#[test]
fn recorded_recovery_preserves_action_when_both_actions_share_authority() {
    use waddle_xmpp::ingress::GroupchatNotificationRecoveryAction;
    let mut saved = IngressEffectIntent::storage_round_trip_samples()
        .into_iter()
        .find_map(|intent| match intent {
            IngressEffectIntent::GroupchatNotificationRecovery { mutation } => Some(mutation),
            _ => None,
        })
        .expect("recovery fixture");
    saved.action = GroupchatNotificationRecoveryAction::Recorded;
    let mut completed = saved.clone();
    completed.action = GroupchatNotificationRecoveryAction::Completed;
    let recorded = vec![
        IngressEffectIntent::GroupchatNotificationRecovery {
            mutation: saved.clone(),
        },
        IngressEffectIntent::GroupchatNotificationRecovery {
            mutation: completed.clone(),
        },
    ];
    for (mut offered, expected) in [(saved, &recorded[0]), (completed, &recorded[1])] {
        offered.created_at_ms += 100;
        let mut plan = empty_plan();
        plan.intents
            .push(IngressEffectIntent::GroupchatNotificationRecovery { mutation: offered });
        assert_eq!(
            apply_recorded_intents(&plan, &recorded).intents,
            vec![expected.clone()]
        );
    }
}

#[test]
fn recorded_route_prefers_semantic_identity_when_audience_drifts() {
    use waddle_xmpp::ingress::EffectMessageIdentity;
    let recipient = "alice@example.test"
        .parse::<jid::BareJid>()
        .expect("recipient");
    let recorded = [1, 2].map(|ordinal| IngressEffectIntent::RouteDirect {
        recipient: recipient.clone(),
        fanout: Vec::new(),
        route_identity: EffectMessageIdentity::CaptureOrdinal(ordinal),
    });
    let mut offered = recorded[1].clone();
    let IngressEffectIntent::RouteDirect { fanout, .. } = &mut offered else {
        panic!("route");
    };
    fanout.push(
        "alice@example.test/new-device"
            .parse()
            .expect("new audience"),
    );
    let mut plan = empty_plan();
    plan.intents.push(offered);
    assert_eq!(
        apply_recorded_intents(&plan, &recorded).intents,
        vec![recorded[1].clone()]
    );
}

#[test]
fn recorded_media_reference_keeps_current_and_unreferenced_actions_separate() {
    use waddle_xmpp::ingress::LinkPreviewMediaRefState;
    let mut current = IngressEffectIntent::storage_round_trip_samples()
        .into_iter()
        .find_map(|intent| match intent {
            IngressEffectIntent::LinkPreviewMediaRef { mutation } => Some(mutation),
            _ => None,
        })
        .expect("media reference fixture");
    current.state = LinkPreviewMediaRefState::Current;
    let mut unreferenced = current.clone();
    unreferenced.state = LinkPreviewMediaRefState::Unreferenced;
    let recorded = vec![
        IngressEffectIntent::LinkPreviewMediaRef {
            mutation: unreferenced,
        },
        IngressEffectIntent::LinkPreviewMediaRef {
            mutation: current.clone(),
        },
    ];
    current.current_archive_stanza_id.id.push_str("-retry");
    let mut plan = empty_plan();
    plan.intents
        .push(IngressEffectIntent::LinkPreviewMediaRef { mutation: current });
    assert_eq!(
        apply_recorded_intents(&plan, &recorded).intents,
        vec![recorded[1].clone()]
    );
}

#[test]
fn recorded_recovery_payload_respects_thread_and_execution_phase() {
    use waddle_xmpp::inbox::storage::GroupchatNotificationRecoveryKey;
    use waddle_xmpp_core::{mam::ThreadId, xep0359::StanzaId};
    let room: jid::BareJid = "room@example.test".parse().expect("room");
    let owner: jid::BareJid = "alice@example.test".parse().expect("owner");
    let recovery = GroupchatNotificationRecovery {
        key: GroupchatNotificationRecoveryKey {
            recipient: owner.clone(),
            room: room.clone(),
            thread_id: Some("thread-one".into()),
            archive_stanza_id: StanzaId::new("archive", room.clone().into()),
        },
        sender_jid: "bob@example.test/device".parse().expect("sender"),
        is_live_occupant: true,
        room_members_only: true,
        sender_can_broadcast_channel_mention: false,
        created_at_ms: 300,
    };
    let mut plan = empty_plan();
    for (thread, action, timestamp) in [
        (
            "thread-one",
            GroupchatNotificationRecoveryAction::Recorded,
            100,
        ),
        (
            "thread-one",
            GroupchatNotificationRecoveryAction::Completed,
            200,
        ),
        (
            "thread-two",
            GroupchatNotificationRecoveryAction::Recorded,
            400,
        ),
    ] {
        plan.intents
            .push(IngressEffectIntent::GroupchatNotificationRecovery {
                mutation: GroupchatNotificationRecoveryMutation {
                    recipient: owner.clone(),
                    room: room.clone(),
                    thread_id: ThreadId::new(thread),
                    archive_stanza_id: recovery.key.archive_stanza_id.clone(),
                    sender: recovery.sender_jid.clone(),
                    is_live_occupant: true,
                    room_members_only: true,
                    sender_can_broadcast_channel_mention: false,
                    created_at_ms: timestamp,
                    action,
                },
            });
    }
    plan.plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Room(
            DurableRoomEffect::ProjectGroupchatInbox {
                archive_stanza_id: recovery.key.archive_stanza_id.clone(),
                owner: owner.clone(),
                entry: Box::new(InboxEntry::new(
                    room.clone(),
                    ConversationKind::MucRoom,
                    "archive",
                    0,
                )),
                is_recipient: true,
                recovery: Some(recovery.clone()),
            },
        ))));
    plan.plan
        .push(PlannedEffect::new(Effect::External(ExternalEffect::Room(
            ExternalRoomEffect::NotificationCandidate {
                owner,
                room,
                archive_stanza_id: recovery.key.archive_stanza_id.clone(),
                candidate: None,
                recovery: Some(recovery),
            },
        ))));
    let result = apply_recorded_intents(&plan, &plan.intents);
    let Effect::Durable(DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
        recovery: Some(durable),
        ..
    })) = &result.plan[0].effect
    else {
        panic!("durable recovery")
    };
    let Effect::External(ExternalEffect::Room(ExternalRoomEffect::NotificationCandidate {
        recovery: Some(external),
        ..
    })) = &result.plan[1].effect
    else {
        panic!("external recovery")
    };
    assert_eq!(durable.created_at_ms, 100);
    assert_eq!(external.created_at_ms, 200);
    assert_eq!(durable.key.thread_id.as_deref(), Some("thread-one"));
    assert_eq!(external.key.thread_id.as_deref(), Some("thread-one"));
}

#[test]
fn recorded_invite_timestamp_is_applied_and_receipted_without_crossing_claim() {
    use crate::server::routes::websocket::{
        handlers::message::muc_invite::InviteLedgerMutation, muc_invites::OutstandingInvite,
    };
    use waddle_xmpp::ingress::{MucInviteLedgerAction, MucInviteLedgerMutation};
    let invite = OutstandingInvite {
        room: "room@example.test".parse().expect("room"),
        invitee: "invitee@example.test".parse().expect("invitee"),
        inviter: "inviter@example.test".parse().expect("inviter"),
    };
    let old_time = chrono::DateTime::from_timestamp(100, 0).expect("time");
    let new_time = chrono::DateTime::from_timestamp(200, 0).expect("time");
    let offered = MucInviteLedgerMutation {
        room: invite.room.clone(),
        invitee: invite.invitee.clone(),
        inviter: invite.inviter.clone(),
        action: MucInviteLedgerAction::Recorded,
        recorded_at: Some(new_time),
    };
    let mut saved = offered.clone();
    saved.recorded_at = Some(old_time);
    let mut claim = offered.clone();
    claim.action = MucInviteLedgerAction::Claimed;
    claim.recorded_at = None;
    let recorded = vec![
        IngressEffectIntent::MucInviteLedger { mutation: claim },
        IngressEffectIntent::MucInviteLedger { mutation: saved },
    ];
    let mut plan = empty_plan();
    plan.intents
        .push(IngressEffectIntent::MucInviteLedger { mutation: offered });
    plan.plan.push(PlannedEffect::new(Effect::External(
        ExternalEffect::InviteLedger(InviteLedgerMutation::Record {
            invite,
            recorded_at: new_time,
            failure: None,
        }),
    )));
    let result = apply_recorded_intents(&plan, &recorded);
    assert_eq!(result.intents, vec![recorded[1].clone()]);
    let Effect::External(effect) = &result.plan[0].effect else {
        panic!("external")
    };
    let ExternalEffect::InviteLedger(InviteLedgerMutation::Record { recorded_at, .. }) = effect
    else {
        panic!("record")
    };
    assert_eq!(*recorded_at, old_time);
    let receipts =
        crate::ingress::receipts::external_receipts(std::slice::from_ref(effect), &result.intents)
            .expect("receipts");
    assert_eq!(
        receipts[0],
        vec![crate::ingress::durable::receipt_key(&recorded[1]).expect("key")]
    );
}

#[test]
fn recorded_dm_pin_metadata_is_applied_and_receipted() {
    use crate::server::routes::websocket::{handlers::message::dm_pin::DmPinMutation, DmPairKey};
    use waddle_xmpp::ingress::DmPinMutationAction;
    let saved = IngressEffectIntent::storage_round_trip_samples()
        .into_iter()
        .find(|intent| matches!(intent, IngressEffectIntent::DmPinMutation { .. }))
        .expect("pin sample");
    let mut offered = saved.clone();
    let IngressEffectIntent::DmPinMutation {
        pair,
        target_stanza_id,
        action,
    } = &mut offered
    else {
        panic!("pin")
    };
    let DmPinMutationAction::Pin { entry } = action else {
        panic!("pin action")
    };
    entry.pinner_jid = "another@example.test".parse().expect("actor");
    let effect = ExternalEffect::DmPinMutation(DmPinMutation {
        pair: DmPairKey::new(pair.0.clone(), pair.1.clone()),
        target_stanza_id: target_stanza_id.clone(),
        action: action.clone(),
    });
    let mut plan = empty_plan();
    plan.intents.push(offered);
    plan.plan.push(PlannedEffect::new(Effect::External(effect)));
    let result = apply_recorded_intents(&plan, std::slice::from_ref(&saved));
    let Effect::External(effect) = &result.plan[0].effect else {
        panic!("external")
    };
    let receipts =
        crate::ingress::receipts::external_receipts(std::slice::from_ref(effect), &result.intents)
            .expect("receipts");
    assert_eq!(
        receipts[0],
        vec![crate::ingress::durable::receipt_key(&saved).expect("key")]
    );
}

#[test]
fn recorded_room_archive_timestamp_matches_typed_stamp_not_client_id() {
    let room: jid::BareJid = "room@conference.example.test".parse().expect("room");
    let stamp = waddle_xmpp_core::xep0359::StanzaId::new("archive-id", room.clone().into());
    let archive_intent = |seconds| IngressEffectIntent::ArchiveAuthoritative {
        archive: room.clone(),
        stanza_id: stamp.clone(),
        by: room.clone(),
        archived_at: chrono::DateTime::from_timestamp(seconds, 0).expect("timestamp"),
    };
    let mut plan = empty_plan();
    plan.intents.push(archive_intent(300));
    plan.plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Room(
            DurableRoomEffect::ProjectGroupchatInbox {
                archive_stanza_id: stamp.clone(),
                owner: "alice@example.test".parse().expect("owner"),
                entry: Box::new(InboxEntry::new(
                    room.clone(),
                    ConversationKind::MucRoom,
                    "client-wire-id",
                    300,
                )),
                is_recipient: true,
                recovery: None,
            },
        ))));
    let result = apply_recorded_intents(&plan, &[archive_intent(100)]);
    let Effect::Durable(DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
        entry,
        archive_stanza_id,
        ..
    })) = &result.plan[0].effect
    else {
        panic!("room inbox projection");
    };
    assert_eq!(entry.last_updated, 100);
    assert_eq!(entry.last_stanza_id, "client-wire-id");
    assert_eq!(archive_stanza_id, &stamp);
}
