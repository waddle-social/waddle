//! Execution of the frozen room payloads at the effect boundary.
use super::super::Deps;
use super::room::{DurableRoomEffect, ExternalRoomEffect, RoomActorMutation, RoomFenceRequirement};
use super::EffectOutcome;
use waddle_xmpp::{
    ingress::IngressEffectIntent,
    muc::room_actor::{ApplyPin, GetRoomSnapshot, SetSubject},
    muc::room_registry_actor::GetRoom,
    Stanza,
};

pub(super) async fn execute_durable(effect: DurableRoomEffect, deps: &Deps<'_>) -> EffectOutcome {
    match effect {
        DurableRoomEffect::ArchiveGroupchat {
            room,
            message,
            fence,
            archive_expectation: _,
        } => {
            // Immediate callers retain the current MAM store contract. Ingress
            // ArchiveExpectation is applied by the transaction repository in Phase B.
            let Some(storage) = deps.mam_storage else {
                return EffectOutcome::Unavailable;
            };
            let outcome = match fence {
                RoomFenceRequirement::Unfenced => storage.store_message(&room, &message).await,
                RoomFenceRequirement::Guarded(context) => {
                    storage
                        .store_message_fenced(&room, &message, &context)
                        .await
                }
            };
            EffectOutcome::Archive(outcome)
        }
        DurableRoomEffect::ProjectGroupchatInbox {
            owner,
            entry,
            is_recipient,
            recovery,
        } => {
            let Some(storage) = deps.inbox_storage else {
                return EffectOutcome::Unavailable;
            };
            EffectOutcome::Inbox(
                storage
                    .upsert_with_groupchat_notification_recovery(
                        &owner,
                        *entry,
                        is_recipient,
                        recovery,
                    )
                    .await,
            )
        }
    }
}

pub(super) async fn execute_external(effect: ExternalRoomEffect, deps: &Deps<'_>) -> EffectOutcome {
    match effect {
        // Deferred archives require the ingress transaction executor.
        ExternalRoomEffect::ArchiveAfterPin { .. } => EffectOutcome::Unavailable,
        ExternalRoomEffect::RoomActorMutation { room, mutation } => {
            mutate_room(deps, room, mutation).await
        }
        ExternalRoomEffect::ObserveRoomMessage {
            room,
            message,
            requester,
            sender,
            error_request,
        } => observe_room(deps, room, message, requester, sender, error_request).await,
        ExternalRoomEffect::NotificationCandidate {
            owner,
            room,
            archive_stanza_id,
            candidate,
            recovery,
        } => {
            let Some(state) = deps.web_socket_state else {
                return EffectOutcome::Unavailable;
            };
            if let Some(candidate) = candidate {
                let outcome = match state
                    .deps
                    .protocol
                    .notification_outbox
                    .insert_candidate(&candidate)
                    .await
                {
                    Ok(
                        crate::notification_outbox::NotificationCandidateInsertOutcome::Inserted,
                    ) => waddle_xmpp::ingress::NotificationCandidateOutcome::Inserted,
                    Ok(
                        crate::notification_outbox::NotificationCandidateInsertOutcome::Duplicate,
                    ) => waddle_xmpp::ingress::NotificationCandidateOutcome::Duplicate,
                    Err(_) => return EffectOutcome::Unavailable,
                };
                deps.capture_intent(IngressEffectIntent::NotificationActivityPreview {
                    owner,
                    mutation:
                        waddle_xmpp::ingress::NotificationActivityMutation::NotificationCandidate {
                            conversation: room,
                            archive_stanza_id,
                            outcome,
                        },
                });
            }
            if let Some(recovery) = recovery {
                let Some(storage) = deps.inbox_storage else {
                    return EffectOutcome::Unavailable;
                };
                match storage
                    .mark_groupchat_notification_recovery_completed(&recovery.key)
                    .await
                {
                    Ok(marked) if marked > 0 => {
                        super::super::groupchat_inbox::capture_recovery_completion(deps, &recovery)
                    }
                    // Zero conflates an already completed item with a missing
                    // item or a storage implementation's no-op default. It is
                    // not evidence for an ingress completion receipt.
                    Ok(_) | Err(_) => return EffectOutcome::Unavailable,
                }
            }
            EffectOutcome::Completed
        }
        #[cfg(feature = "clustering")]
        ExternalRoomEffect::RelayMucProxy {
            admission,
            room,
            stanza,
            kind,
            muc_origin,
            origin,
            reflect_replies_to_sender,
        } => {
            let Some(bridge) = deps.web_socket_state.and_then(|state| {
                state
                    .deps
                    .app_state
                    .clustering_claims
                    .ordered_relay_delivery_bridge
                    .as_ref()
            }) else {
                return EffectOutcome::Unavailable;
            };
            match bridge
                .try_proxy_muc_remote(
                    &room,
                    &stanza,
                    kind,
                    muc_origin,
                    &origin,
                    admission.as_ref(),
                )
                .await
            {
                Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(
                    replies,
                )) if reflect_replies_to_sender => EffectOutcome::Frames(replies),
                Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(
                    _,
                )) => EffectOutcome::Completed,
                _ => EffectOutcome::Unavailable,
            }
        }
    }
}

async fn mutate_room(
    deps: &Deps<'_>,
    room: jid::BareJid,
    mutation: RoomActorMutation,
) -> EffectOutcome {
    let Some(registry) = deps.room_registry else {
        return EffectOutcome::Unavailable;
    };
    let Ok(Some(actor)) = registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    else {
        return EffectOutcome::Unavailable;
    };
    let Ok(sender_jid) = room.with_resource_str("__effect_executor__") else {
        return EffectOutcome::Unavailable;
    };
    let Ok(snapshot) = actor
        .ask(GetRoomSnapshot { sender_jid })
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    else {
        return EffectOutcome::Unavailable;
    };
    let fence = match &mutation {
        RoomActorMutation::SetSubject { claim_fence, .. }
        | RoomActorMutation::ApplyPin { claim_fence, .. } => claim_fence,
    };
    if fence.as_ref().is_some_and(|fence| {
        fence.entity
            != waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                room.to_string(),
            )
    }) || fence != &snapshot.claim_fence
    {
        return EffectOutcome::Unavailable;
    }
    let success = match mutation {
        RoomActorMutation::SetSubject { subject, .. } => actor
            .ask(SetSubject {
                texts: subject.texts,
                setter: subject.setter,
                setter_nick: subject.setter_nick,
                set_at: subject.set_at,
            })
            .reply_timeout(std::time::Duration::from_secs(5))
            .await
            .is_ok(),
        RoomActorMutation::ApplyPin { change, .. } => actor
            .ask(ApplyPin { change })
            .reply_timeout(std::time::Duration::from_secs(5))
            .await
            .is_ok(),
    };
    if success {
        EffectOutcome::Completed
    } else {
        EffectOutcome::Unavailable
    }
}

async fn observe_room(
    deps: &Deps<'_>,
    room: jid::BareJid,
    mut message: Box<xmpp_parsers::message::Message>,
    requester: jid::BareJid,
    sender: jid::FullJid,
    error_request: Box<xmpp_parsers::message::Message>,
) -> EffectOutcome {
    let Some(state) = deps.web_socket_state else {
        return EffectOutcome::Unavailable;
    };
    let outcome = state
        .deps
        .protocol
        .extension_manager
        .process_message_observers_for_waddle_with_requester(
            &mut message,
            super::super::waddle_id_for_room_jid(&room),
            Some(requester),
        )
        .await;
    let replies = outcome
        .effects
        .into_iter()
        .filter_map(|effect| match effect {
            waddle_extensions::ExtensionEffect::HostWarning(warning) => {
                Some(Stanza::Message(super::super::build_message_error_reply(
                    &error_request,
                    &room,
                    &sender,
                    super::super::service_unavailable_error(warning.as_str()),
                )))
            }
            _ => None,
        })
        .collect();
    EffectOutcome::Frames(replies)
}
