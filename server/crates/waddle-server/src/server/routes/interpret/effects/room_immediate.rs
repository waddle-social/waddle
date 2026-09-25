//! Execution of the frozen room payloads at the effect boundary.
use super::super::Deps;
use super::room::{DurableRoomEffect, ExternalRoomEffect, RoomActorMutation, RoomFenceRequirement};
use super::EffectOutcome;
use waddle_xmpp::{
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
            // CORRECTION (#1831 Phase 2): an earlier draft of this comment
            // claimed this `execute_durable` arm was "the single place a
            // groupchat archive write is ever really performed," including
            // for the primary ingress commit path, and planned to enqueue
            // `message_judgment_outbox` rows here. Two independent
            // adversarial reviews traced the real call graph and found
            // that claim false: `ingress::execute::execute_effects` (the
            // post-commit "Phase C" replay `commit_submission` actually
            // drives) only ever reconstructs `Effect::External` payloads
            // to run through `ImmediateSink` — never `Effect::Durable` —
            // so this arm is *never* reached by a normal user chat
            // message's commit. The real (and only) archive write for
            // that path is `ingress_uow::MamArchiveRepository::store` /
            // `store_fenced`, called from `ingress::durable::apply_durable`
            // inside the same database transaction the ingress commit
            // uses; the `message_judgment_outbox` enqueue now lives there
            // too, in the same transaction — see that function's docs.
            //
            // This arm instead serves callers that construct and execute a
            // `Durable(ArchiveGroupchat)` effect directly through
            // `ImmediateSink`, outside any ingress transaction — room
            // system messages (`room_system_message.rs`), not ordinary
            // occupant chat messages. Those do not enqueue a judgment-
            // outbox row: they are synthetic notices (subject changes and
            // similar), not user content, so nothing here needs to change
            // that.
            EffectOutcome::Archive(outcome)
        }
        DurableRoomEffect::ProjectGroupchatInbox {
            owner,
            entry,
            is_recipient,
            ..
        } => {
            let Some(storage) = deps.inbox_storage else {
                return EffectOutcome::Unavailable;
            };
            EffectOutcome::Inbox(storage.upsert(&owner, *entry, is_recipient).await)
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
            plugin,
            message,
            requester,
            sender,
            error_request,
        } => {
            observe_room(
                deps,
                room,
                plugin,
                message,
                requester,
                sender,
                error_request,
            )
            .await
        }
        ExternalRoomEffect::NotificationCandidate { .. } => EffectOutcome::Unavailable,
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
                Some(
                    crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::PendingFrames {
                        frames,
                        completion,
                    },
                ) if reflect_replies_to_sender => EffectOutcome::RelayFrames { frames, completion },
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
    plugin: waddle_extensions::PluginId,
    message: Box<xmpp_parsers::message::Message>,
    requester: jid::BareJid,
    sender: jid::FullJid,
    error_request: Box<xmpp_parsers::message::Message>,
) -> EffectOutcome {
    let Some(state) = deps.web_socket_state else {
        return EffectOutcome::Unavailable;
    };
    let Some(outcome) = state
        .deps
        .protocol
        .extension_manager
        .process_message_observer(
            &plugin,
            &message,
            super::super::waddle_id_for_room_jid(&room),
            Some(requester),
        )
        .await
    else {
        return EffectOutcome::Unavailable;
    };
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
