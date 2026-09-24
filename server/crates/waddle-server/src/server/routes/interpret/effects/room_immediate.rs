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
            // TODO(#1831 Phase 2): fire-and-forget enqueue into
            // `message_judgment_outbox::enqueue_pending` here, gated on
            // `MessageJudgmentOutboxConfig::enabled` and on `outcome` being
            // `Ok(StoreOutcome::Stored { stanza_id, .. })` (never on a
            // fenced-ownership-lost or storage-error outcome). This is the
            // correct seam — not `groupchat_archive.rs`'s
            // `finish_archive_groupchat_message_with_effects` — because
            // that function also runs during the two-phase *planning* pass
            // (`deps.effects.is_planning() == true`), where `deps.effects
            // .execute(...)` only records an assumed outcome and never
            // touches `storage`; enqueueing there would fire for plans that
            // are later rejected and never actually committed. This
            // `execute_durable` arm is the single place a groupchat archive
            // write is ever really performed (both for a directly-executed
            // `ImmediateSink` message and for a previously-planned effect
            // replayed for real by the ingress commit path), so it is the
            // only site that reliably fires exactly once per real archive.
            // `message.body` (`Option<String>`) is already the RFC 6121
            // §5.2.3-aware body extraction done upstream in
            // `groupchat_archive.rs`'s `prototype_body` — reuse it directly
            // rather than re-deriving a body extractor. Left as a TODO: a
            // fire-and-forget `tokio::spawn` needs a `Database` handle and
            // the config flag, and neither reaches `Deps` today without a
            // new field threaded through `Deps`/`WebSocketState::deps` and
            // every one of their ~51 literal construction sites — out of
            // scope to force safely in this PR. The real Jev client +
            // startup wiring for `run_drain_loop` lands in the same
            // follow-up.
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
