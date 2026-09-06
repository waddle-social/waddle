//! Apply the payload-complete policy decisions retained by reconciliation.
#[cfg(test)]
mod tests;

use crate::server::routes::interpret::effects::{
    direct::{DurableDirectEffect, ExternalDirectEffect},
    room::{DurableRoomEffect, ExternalRoomEffect, RoomActorMutation},
    DurableEffect, Effect, ExternalEffect, IngressPlan,
};
use waddle_xmpp::inbox::storage::GroupchatNotificationRecovery;
use waddle_xmpp::ingress::{
    GroupchatNotificationRecoveryAction, GroupchatNotificationRecoveryMutation,
    InboxProjectionMutation, IngressEffectIntent, RoomPinMutation,
};

/// Reconciliation preserves recorded payloads when the policy or audience
/// changes. Both application and receipt identity must use those same payloads.
/// Recorded-only obligations stay pending unless the plan contains their work.
pub fn apply_recorded_intents(plan: &IngressPlan, recorded: &[IngressEffectIntent]) -> IngressPlan {
    let mut result = plan.clone();
    for original in &plan.intents {
        let Some(authoritative) = recorded_match(recorded, original) else {
            continue;
        };
        for effect in &mut result.plan {
            if let (
                IngressEffectIntent::Pin {
                    room,
                    mutation: original_mutation,
                },
                IngressEffectIntent::Pin { mutation, .. },
            ) = (original, authoritative)
            {
                for dependency in &mut effect.dependencies {
                    if let crate::server::routes::interpret::effects::PlanEffectDependency::AfterRoomPin { room: dependent_room, change } = dependency {
                        if dependent_room == room && *change == pin_change(original_mutation) {
                            *change = pin_change(mutation);
                        }
                    }
                }
            }
            apply_effect(&mut effect.effect, original, authoritative);
        }
    }
    for intent in &mut result.intents {
        if let Some(authoritative) = recorded_match(recorded, intent) {
            *intent = authoritative.clone();
        }
    }
    result
}

fn recorded_match<'a>(
    recorded: &'a [IngressEffectIntent],
    planned: &IngressEffectIntent,
) -> Option<&'a IngressEffectIntent> {
    recorded
        .iter()
        .find(|row| *row == planned)
        .or_else(|| {
            recorded
                .iter()
                .find(|row| row.semantic_key() == planned.semantic_key())
        })
        .or_else(|| {
            recorded.iter().find(|row| {
                row.authority_key() == planned.authority_key() && same_mutation_shape(row, planned)
            })
        })
}

fn same_mutation_shape(recorded: &IngressEffectIntent, planned: &IngressEffectIntent) -> bool {
    match (recorded, planned) {
        (
            IngressEffectIntent::MucInviteLedger { mutation: saved },
            IngressEffectIntent::MucInviteLedger { mutation: offered },
        ) => saved.action == offered.action,
        (
            IngressEffectIntent::DmPinMutation { action: saved, .. },
            IngressEffectIntent::DmPinMutation {
                action: offered, ..
            },
        ) => std::mem::discriminant(saved) == std::mem::discriminant(offered),
        (
            IngressEffectIntent::NotificationActivityPreview {
                mutation: saved, ..
            },
            IngressEffectIntent::NotificationActivityPreview {
                mutation: offered, ..
            },
        ) => std::mem::discriminant(saved) == std::mem::discriminant(offered),
        (
            IngressEffectIntent::InboxProject {
                mutation: saved, ..
            },
            IngressEffectIntent::InboxProject {
                mutation: offered, ..
            },
        ) => std::mem::discriminant(saved) == std::mem::discriminant(offered),
        (
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: saved },
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: offered },
        ) => saved.action == offered.action,
        (
            IngressEffectIntent::LinkPreviewMediaRef { mutation: saved },
            IngressEffectIntent::LinkPreviewMediaRef { mutation: offered },
        ) => saved.state == offered.state,
        _ => true,
    }
}

fn apply_effect(
    effect: &mut Effect,
    original: &IngressEffectIntent,
    recorded: &IngressEffectIntent,
) {
    match effect {
        Effect::Durable(effect) => apply_durable(effect, original, recorded),
        Effect::External(effect) => apply_external(effect, original, recorded),
        Effect::Immediate(_) => {}
    }
}

fn apply_durable(
    effect: &mut DurableEffect,
    original: &IngressEffectIntent,
    recorded: &IngressEffectIntent,
) {
    match (effect, original, recorded) {
        (
            DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                owner,
                entry,
                is_recipient,
                ..
            }),
            IngressEffectIntent::InboxProject {
                owner: old_owner, ..
            },
            IngressEffectIntent::InboxProject {
                mutation:
                    InboxProjectionMutation::GroupchatChannel {
                        room,
                        increment_unread,
                    }
                    | InboxProjectionMutation::GroupchatChannelAndThread {
                        room,
                        increment_unread,
                        ..
                    },
                ..
            },
        ) if owner == old_owner && &entry.partner == room => *is_recipient = *increment_unread,
        (
            DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                entry,
                archive_stanza_id,
                ..
            }),
            IngressEffectIntent::ArchiveAuthoritative {
                archive, stanza_id, ..
            }
            | IngressEffectIntent::SystemMessageArchive {
                archive, stanza_id, ..
            },
            IngressEffectIntent::ArchiveAuthoritative { archived_at, .. }
            | IngressEffectIntent::SystemMessageArchive { archived_at, .. },
        ) if &entry.partner == archive && archive_stanza_id == stanza_id => {
            entry.last_updated = archived_at.timestamp()
        }
        (
            DurableEffect::Direct(DurableDirectEffect::ProjectInbox {
                owner,
                entry,
                increment_unread,
            }),
            IngressEffectIntent::InboxProject {
                owner: old_owner,
                mutation: InboxProjectionMutation::Direct { entry: old, .. },
            },
            IngressEffectIntent::InboxProject {
                mutation:
                    InboxProjectionMutation::Direct {
                        entry: saved,
                        increment_unread: saved_unread,
                    },
                ..
            },
        ) if owner == old_owner && entry.as_ref() == old => {
            **entry = saved.clone();
            *increment_unread = *saved_unread;
        }
        (
            DurableEffect::Direct(DurableDirectEffect::DmCallThreadProjection { owner, mutation }),
            IngressEffectIntent::InboxProject {
                owner: old_owner,
                mutation: old,
            },
            IngressEffectIntent::InboxProject {
                mutation: saved, ..
            },
        ) if owner == old_owner && mutation.as_ref() == old => {
            **mutation = saved.clone();
        }
        (
            DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
                archive, message, ..
            })
            | DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
                room: archive,
                message,
                ..
            }),
            IngressEffectIntent::ArchiveAuthoritative {
                archive: old_archive,
                stanza_id,
                ..
            }
            | IngressEffectIntent::SystemMessageArchive {
                archive: old_archive,
                stanza_id,
                ..
            },
            IngressEffectIntent::ArchiveAuthoritative { archived_at, .. }
            | IngressEffectIntent::SystemMessageArchive { archived_at, .. },
        ) if archive == old_archive && message.id == stanza_id.id => {
            message.timestamp = *archived_at
        }
        (
            DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                recovery: Some(recovery),
                ..
            }),
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: old },
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: saved },
        ) if old.action == GroupchatNotificationRecoveryAction::Recorded
            && recovery_matches(recovery, old) =>
        {
            apply_recovery(recovery, saved);
        }
        _ => {}
    }
}

fn apply_external(
    effect: &mut ExternalEffect,
    original: &IngressEffectIntent,
    recorded: &IngressEffectIntent,
) {
    use crate::server::routes::interpret::effects::early::RoomMembershipMutation;
    use crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerMutation;
    match (effect, original, recorded) {
        (
            ExternalEffect::Room(ExternalRoomEffect::ArchiveAfterPin {
                room,
                message,
                archive_expectation,
                ..
            }),
            IngressEffectIntent::SystemMessageArchive {
                archive, stanza_id, ..
            },
            IngressEffectIntent::SystemMessageArchive { archived_at, .. },
        ) if room == archive && message.id == stanza_id.id => {
            message.timestamp = *archived_at;
            *archive_expectation = waddle_xmpp::mam::ArchiveExpectation::Existing {
                stanza_id: stanza_id.clone(),
                archived_at: *archived_at,
            };
        }
        (
            ExternalEffect::InviteLedger(InviteLedgerMutation::Record {
                invite,
                recorded_at,
                ..
            }),
            IngressEffectIntent::MucInviteLedger { mutation: old },
            IngressEffectIntent::MucInviteLedger { mutation: saved },
        ) if invite.room == old.room
            && invite.invitee == old.invitee
            && invite.inviter == old.inviter
            && old.action == waddle_xmpp::ingress::MucInviteLedgerAction::Recorded
            && saved.action == old.action =>
        {
            if let Some(saved_at) = saved.recorded_at {
                *recorded_at = saved_at;
                invite.inviter = saved.inviter.clone();
            }
        }
        (
            ExternalEffect::DmPinMutation(mutation),
            IngressEffectIntent::DmPinMutation {
                pair,
                target_stanza_id,
                action: old,
            },
            IngressEffectIntent::DmPinMutation { action: saved, .. },
        ) if mutation.pair.low_peer == pair.0
            && mutation.pair.high_peer == pair.1
            && mutation.target_stanza_id == *target_stanza_id
            && mutation.action == *old =>
        {
            mutation.action = saved.clone();
        }
        (
            ExternalEffect::RoomMembershipMutation(RoomMembershipMutation::GroupDm(mutation)),
            IngressEffectIntent::GroupDmMembershipGrant { grant: old },
            IngressEffectIntent::GroupDmMembershipGrant { grant: saved },
        ) if mutation.grant == *old => mutation.grant = saved.clone(),
        (
            ExternalEffect::Room(ExternalRoomEffect::NotificationCandidate {
                recovery: Some(recovery),
                ..
            }),
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: old },
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: saved },
        ) if old.action == GroupchatNotificationRecoveryAction::Completed
            && recovery_matches(recovery, old) =>
        {
            apply_recovery(recovery, saved);
        }
        (
            ExternalEffect::Direct(ExternalDirectEffect::NotificationActivity { owner, mutation }),
            IngressEffectIntent::NotificationActivityPreview {
                owner: old_owner,
                mutation: old,
            },
            IngressEffectIntent::NotificationActivityPreview {
                mutation: saved, ..
            },
        ) if owner == old_owner && mutation == old => *mutation = saved.clone(),
        (
            ExternalEffect::Direct(
                ExternalDirectEffect::LinkPreviewRefs { mutations }
                | ExternalDirectEffect::ClearLinkPreviewRefs { mutations },
            ),
            IngressEffectIntent::LinkPreviewMediaRef { mutation: old },
            IngressEffectIntent::LinkPreviewMediaRef { mutation: saved },
        ) => {
            for mutation in mutations {
                if mutation == old {
                    *mutation = saved.clone();
                }
            }
        }
        (
            ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
                room,
                mutation: RoomActorMutation::SetSubject { subject, .. },
            }),
            IngressEffectIntent::RoomSubjectMutation { room: old_room, .. },
            IngressEffectIntent::RoomSubjectMutation { state, .. },
        ) if room == old_room => *subject = state.clone(),
        (
            ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
                room,
                mutation: RoomActorMutation::ApplyPin { change, .. },
            }),
            IngressEffectIntent::Pin { room: old_room, .. },
            IngressEffectIntent::Pin { mutation, .. },
        ) if room == old_room => {
            *change = pin_change(mutation);
        }
        _ => {}
    }
}

fn recovery_matches(
    recovery: &GroupchatNotificationRecovery,
    mutation: &GroupchatNotificationRecoveryMutation,
) -> bool {
    recovery.key.recipient == mutation.recipient
        && recovery.key.room == mutation.room
        && recovery.key.archive_stanza_id == mutation.archive_stanza_id
        && recovery.key.thread_id.as_deref() == mutation.thread_id.as_ref().map(|id| id.as_str())
}

fn apply_recovery(
    recovery: &mut GroupchatNotificationRecovery,
    saved: &GroupchatNotificationRecoveryMutation,
) {
    recovery.sender_jid = saved.sender.clone();
    recovery.is_live_occupant = saved.is_live_occupant;
    recovery.room_members_only = saved.room_members_only;
    recovery.sender_can_broadcast_channel_mention = saved.sender_can_broadcast_channel_mention;
    recovery.created_at_ms = saved.created_at_ms;
}

fn pin_change(mutation: &RoomPinMutation) -> waddle_xmpp::muc::pin::PinStateChange {
    match mutation {
        RoomPinMutation::Pin { entry } => waddle_xmpp::muc::pin::PinStateChange::Pin(entry.clone()),
        RoomPinMutation::Unpin { target_stanza_id } => {
            waddle_xmpp::muc::pin::PinStateChange::Unpin {
                target_stanza_id: target_stanza_id.clone(),
            }
        }
    }
}
