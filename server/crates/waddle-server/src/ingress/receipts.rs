//! Correlate completion with exact recorded obligations, never vector position.
use waddle_xmpp::ingress::IngressEffectIntent;

use super::{decision::EffectReceiptKey, durable::receipt_key};
use crate::{
    ingress_uow::IngressUowError,
    server::routes::interpret::effects::{
        direct::ExternalDirectEffect,
        room::{ExternalRoomEffect, RoomActorMutation},
        ExternalEffect,
    },
};

#[path = "receipts_routing.rs"]
mod routing;

pub(super) fn external_receipts(
    external: &[ExternalEffect],
    intents: &[IngressEffectIntent],
) -> Result<Vec<Vec<EffectReceiptKey>>, IngressUowError> {
    let mut receipts = vec![Vec::new(); external.len()];
    for intent in intents {
        let indices = routing::route_receipts(external, intent).unwrap_or_else(|| {
            external
                .iter()
                .enumerate()
                .filter_map(|(index, effect)| exact_mutation(effect, intent).then_some(index))
                .collect()
        });
        if indices.is_empty() {
            continue;
        }
        let key = receipt_key(intent)?;
        for index in indices {
            if !receipts[index].contains(&key) {
                receipts[index].push(key.clone());
            }
        }
    }
    Ok(receipts)
}

fn exact_mutation(effect: &ExternalEffect, intent: &IngressEffectIntent) -> bool {
    match (effect, intent) {
        #[cfg(feature = "clustering")]
        (
            ExternalEffect::Room(ExternalRoomEffect::RelayMucProxy { room, .. }),
            IngressEffectIntent::DispatchToRoomRemote {
                room: recorded_room,
                ..
            },
        ) => room == recorded_room,
        (
            ExternalEffect::Room(ExternalRoomEffect::ArchiveAfterPin { room, message, .. }),
            IngressEffectIntent::SystemMessageArchive {
                archive, stanza_id, ..
            },
        ) => {
            room == archive
                && waddle_xmpp_core::xep0359::StanzaId::new(&message.id, room.clone().into())
                    == *stanza_id
        }

        (
            ExternalEffect::Direct(ExternalDirectEffect::NotificationActivity { owner, mutation }),
            IngressEffectIntent::NotificationActivityPreview {
                owner: recorded_owner,
                mutation: recorded,
            },
        ) => owner == recorded_owner && mutation == recorded,
        (
            ExternalEffect::Direct(
                ExternalDirectEffect::LinkPreviewRefs { mutations }
                | ExternalDirectEffect::ClearLinkPreviewRefs { mutations },
            ),
            IngressEffectIntent::LinkPreviewMediaRef { mutation },
        ) => mutations.contains(mutation),
        (
            ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
                room,
                mutation: RoomActorMutation::SetSubject { subject, .. },
            }),
            IngressEffectIntent::RoomSubjectMutation {
                room: recorded_room,
                state,
            },
        ) => room == recorded_room && subject == state,
        (
            ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
                room,
                mutation: RoomActorMutation::ApplyPin { change, .. },
            }),
            IngressEffectIntent::Pin {
                room: recorded_room,
                mutation,
            },
        ) => {
            use waddle_xmpp::{ingress::RoomPinMutation, muc::pin::PinStateChange};
            room == recorded_room
                && match (change, mutation) {
                    (PinStateChange::Pin(entry), RoomPinMutation::Pin { entry: recorded }) => {
                        entry == recorded
                    }
                    (
                        PinStateChange::Unpin { target_stanza_id },
                        RoomPinMutation::Unpin {
                            target_stanza_id: recorded,
                        },
                    ) => target_stanza_id == recorded,
                    _ => false,
                }
        }
        (
            ExternalEffect::Room(ExternalRoomEffect::NotificationCandidate {
                owner,
                room,
                archive_stanza_id,
                recovery: Some(recovery),
                ..
            }),
            IngressEffectIntent::GroupchatNotificationRecovery { mutation },
        ) => {
            mutation.action == waddle_xmpp::ingress::GroupchatNotificationRecoveryAction::Completed
                && owner == &recovery.key.recipient
                && room == &recovery.key.room
                && archive_stanza_id == &recovery.key.archive_stanza_id
                && mutation.recipient == recovery.key.recipient
                && mutation.room == recovery.key.room
                && mutation.archive_stanza_id == recovery.key.archive_stanza_id
                && mutation.thread_id.as_ref().map(|thread| thread.as_str())
                    == recovery.key.thread_id.as_deref()
                && mutation.sender == recovery.sender_jid
                && mutation.is_live_occupant == recovery.is_live_occupant
                && mutation.room_members_only == recovery.room_members_only
                && mutation.sender_can_broadcast_channel_mention
                    == recovery.sender_can_broadcast_channel_mention
                && mutation.created_at_ms == recovery.created_at_ms
        }
        // Recipient SM append identity and runtime-generated mutation results
        // are not exposed by ImmediateSink. Keep those intents unresolved until
        // the sink can provide their exact completion proof.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::{BareJid, FullJid};
    use waddle_xmpp::{
        ingress::{EffectMessageIdentity, FrozenStanzaError, FrozenStanzaErrorType},
        Stanza, StanzaErrorCondition,
    };
    use xmpp_parsers::message::{Message, MessageType};

    fn delivery(recipient: &FullJid) -> ExternalEffect {
        let mut message = Message::new(Some(recipient.clone().into()));
        waddle_xmpp_core::xep0359::add_origin_id(&mut message, "offered-origin");
        ExternalEffect::Delivery(crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect::RouteToPeer {
            jid: recipient.clone(), stanza: Box::new(Stanza::Message(message)),
            kind: crate::server::routes::interpret::effects::delivery::PeerDeliveryKind::PeerStanza,
            call_setup: None,
        })
    }

    #[test]
    fn suppressed_recipient_cannot_be_receipted_by_sender_reflection() {
        let owner: BareJid = "peer@example.com".parse().expect("owner");
        let first: FullJid = "peer@example.com/one".parse().expect("first");
        let second: FullJid = "peer@example.com/two".parse().expect("second");
        let intent = IngressEffectIntent::RouteDirect {
            recipient: owner,
            fanout: vec![first.clone(), second.clone()],
            route_identity: EffectMessageIdentity::OriginId(
                waddle_xmpp_core::xep0359::OriginId::new("offered-origin"),
            ),
        };
        let partial = external_receipts(&[delivery(&first)], std::slice::from_ref(&intent))
            .expect("partial mapping");
        assert_eq!(partial, vec![vec![]]);
        let complete = external_receipts(
            &[delivery(&first), delivery(&second)],
            std::slice::from_ref(&intent),
        )
        .expect("complete mapping");
        let key = receipt_key(&intent).expect("key");
        assert_eq!(complete, vec![vec![key.clone()], vec![key]]);
    }

    #[test]
    fn unrelated_error_reply_does_not_complete_recorded_intent() {
        let recipient: FullJid = "sender@example.com/resource".parse().expect("recipient");
        let expected = FrozenStanzaError::new(
            FrozenStanzaErrorType::Cancel,
            StanzaErrorCondition::Conflict,
        );
        let intent = IngressEffectIntent::ErrorReply {
            recipient: recipient.clone(),
            error: expected.clone(),
        };
        let mut message = Message::new(Some(recipient.clone().into()));
        message.type_ = MessageType::Error;
        message.payloads.push(
            FrozenStanzaError::new(
                FrozenStanzaErrorType::Modify,
                StanzaErrorCondition::BadRequest,
            )
            .to_xmpp()
            .into(),
        );
        let external = ExternalEffect::Frame(Box::new(Stanza::Message(message.clone())));
        assert_eq!(
            external_receipts(&[external], std::slice::from_ref(&intent)).expect("unrelated error"),
            vec![vec![]]
        );
        message.payloads.clear();
        message.payloads.push(expected.to_xmpp().into());
        let external = ExternalEffect::Frame(Box::new(Stanza::Message(message)));
        assert_eq!(
            external_receipts(&[external], std::slice::from_ref(&intent)).expect("exact error"),
            vec![vec![receipt_key(&intent).expect("key")]]
        );
    }
    #[test]
    fn notification_recovery_receipt_requires_exact_completed_action() {
        use waddle_xmpp::{
            inbox::storage::{GroupchatNotificationRecovery, GroupchatNotificationRecoveryKey},
            ingress::{GroupchatNotificationRecoveryAction, GroupchatNotificationRecoveryMutation},
        };
        let recipient: BareJid = "recipient@example.com".parse().expect("recipient");
        let room: BareJid = "room@example.com".parse().expect("room");
        let id = waddle_xmpp_core::xep0359::StanzaId::new("archive", room.clone().into());
        let recovery = GroupchatNotificationRecovery {
            key: GroupchatNotificationRecoveryKey {
                recipient: recipient.clone(),
                room: room.clone(),
                thread_id: None,
                archive_stanza_id: id.clone(),
            },
            sender_jid: "sender@example.com/device".parse().expect("sender"),
            is_live_occupant: true,
            room_members_only: true,
            sender_can_broadcast_channel_mention: false,
            created_at_ms: 42,
        };
        let effect = ExternalEffect::Room(ExternalRoomEffect::NotificationCandidate {
            owner: recipient.clone(),
            room: room.clone(),
            archive_stanza_id: id.clone(),
            candidate: None,
            recovery: Some(recovery.clone()),
        });
        let mutation = GroupchatNotificationRecoveryMutation {
            recipient,
            room,
            thread_id: None,
            archive_stanza_id: id,
            sender: recovery.sender_jid.clone(),
            is_live_occupant: true,
            room_members_only: true,
            sender_can_broadcast_channel_mention: false,
            created_at_ms: 42,
            action: GroupchatNotificationRecoveryAction::Completed,
        };
        let completed = IngressEffectIntent::GroupchatNotificationRecovery {
            mutation: mutation.clone(),
        };
        let mut recorded = mutation.clone();
        recorded.action = GroupchatNotificationRecoveryAction::Recorded;
        let mut mismatched = mutation;
        mismatched.created_at_ms += 1;
        let intents = vec![
            completed.clone(),
            IngressEffectIntent::GroupchatNotificationRecovery { mutation: recorded },
            IngressEffectIntent::GroupchatNotificationRecovery {
                mutation: mismatched,
            },
        ];
        assert_eq!(
            external_receipts(&[effect], &intents).expect("mapping"),
            vec![vec![receipt_key(&completed).expect("completed key")]]
        );
    }
}
