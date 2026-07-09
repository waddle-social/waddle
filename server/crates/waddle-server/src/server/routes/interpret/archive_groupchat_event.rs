use super::*;

/// Outcome of the [`OutboundEvent::ArchiveGroupchat`] interpreter arm
/// (ADR-0017 Phase 3 Slice 7 FIX 1, council-adjudicated). `OwnershipUncertain`
/// carries the pre-built bounce reply the caller pushes to `outcome.frames`
/// AND uses as the signal to suppress every remaining event in this same
/// dispatch batch (the archive handler always runs before the reflector
/// fan-out handler in the locked Q7 chain order, so this is reached before
/// any `RouteToConnection` fan-out for the same message).
pub(super) enum ArchiveGroupchatEventOutcome {
    Rewrite(Option<ArchiveIdRewrite>),
    OwnershipUncertain(Box<Message>),
}

/// Prove that a nested room batch still belongs to the exact actor
/// incarnation that produced it and that actor's immutable claim is live.
/// The actor-ref comparison closes same-claim E1/E2 replacement; the actor's
/// mutation gate closes ordinary claim/node-lease loss.
pub(super) async fn exact_room_effect_authorized(deps: &Deps<'_>, room: &BareJid) -> bool {
    #[cfg(feature = "clustering")]
    let actor_bound = deps.room_actor_incarnation.is_some()
        || deps.clustered_muc_ownership_required
        || deps.muc_durable_store.is_some();
    #[cfg(not(feature = "clustering"))]
    let actor_bound = deps.room_actor_incarnation.is_some();
    if !actor_bound {
        // Portable direct interpreter fixtures and genuinely single-node
        // server-authored events have no clustered actor incarnation to
        // protect. Nested room dispatch always supplies one.
        return true;
    }
    let actor = match exact_room_actor_for_effect(deps, room).await {
        Ok(actor) => actor,
        Err(_) => return false,
    };
    let checked = actor
        .ask(waddle_xmpp::muc::room_actor::CheckMutationOwnership)
        .await;
    match checked {
        Ok(()) => true,
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::RoomMutationError::NotOwner,
        )) => {
            if let Some(registry) = deps.room_registry {
                let _ = registry
                    .ask(waddle_xmpp::muc::room_registry_actor::DestroyRoomExact {
                        room_jid: room.clone(),
                        expected_actor: actor.clone(),
                    })
                    .await;
            }
            actor.kill();
            false
        }
        Err(_) => false,
    }
}

fn ownership_uncertain_bounce(
    room: &BareJid,
    sender: &FullJid,
    message: &Message,
) -> ArchiveGroupchatEventOutcome {
    let bounce = build_message_error_reply(
        message,
        room,
        sender,
        resource_constraint_error(
            "This room's ownership cannot currently be verified; please retry.",
        ),
    );
    ArchiveGroupchatEventOutcome::OwnershipUncertain(Box::new(bounce))
}

pub(super) async fn archive_groupchat_event(
    deps: &Deps<'_>,
    room: BareJid,
    sender: FullJid,
    message: Box<Message>,
    sender_nickname_generation: u64,
    sender_item: Option<waddle_xmpp_core::mam::ArchivedMucSender>,
) -> ArchiveGroupchatEventOutcome {
    if !exact_room_effect_authorized(deps, &room).await {
        return ownership_uncertain_bounce(&room, &sender, &message);
    }
    // Resolve clustered ownership before the optional archive backend. A
    // missing MAM fixture is a benign archive-policy skip only when MUC
    // ownership itself is not uncertain.
    let fence = resolve_room_claim_fence(deps, &room);
    if fence.is_ownership_uncertain() {
        return ownership_uncertain_bounce(&room, &sender, &message);
    }
    let Some(mam_storage) = deps.mam_storage else {
        if fence.is_fenced() {
            return ownership_uncertain_bounce(&room, &sender, &message);
        }
        debug!(
            room = %room,
            sender = %sender,
            "ArchiveGroupchat: no mam_storage in Deps; skipping (test fixture?)"
        );
        return ArchiveGroupchatEventOutcome::Rewrite(None);
    };
    // Per XEP-0313 §5.1.3 the eligibility check ran inside
    // `MucArchiveHandler` before this event was emitted —
    // the interpreter only persists. Mirrors the legacy
    // `archive_groupchat_message` projection: derive a
    // fresh archive id, stamp the canonical
    // `<stanza-id by='room'/>` for replay, then persist.
    // `sender_nickname_generation` rides on the event so
    // we don't pay a second `RoomActor::GetRoomSnapshot`
    // round-trip per archive write (Copilot review on
    // PR #279).
    //
    // ADR-0017 Phase 3 Slice 7 FIX 1: resolve the SAME typed fencing
    // context `dispatch_to_room`'s own pre-fan-out check reads, so the
    // fenced archive write below agrees with it by construction.
    let archive_id = match archive_groupchat_message(
        mam_storage,
        &room,
        &message,
        sender_nickname_generation,
        &fence,
        sender_item.as_ref(),
    )
    .await
    {
        ArchiveGroupchatOutcome::Stored(result) => result,
        ArchiveGroupchatOutcome::Skipped => {
            return if exact_room_effect_authorized(deps, &room).await {
                ArchiveGroupchatEventOutcome::Rewrite(None)
            } else {
                ownership_uncertain_bounce(&room, &sender, &message)
            };
        }
        ArchiveGroupchatOutcome::OwnershipUncertain => {
            return ownership_uncertain_bounce(&room, &sender, &message);
        }
    };
    if !exact_room_effect_authorized(deps, &room).await {
        return ownership_uncertain_bounce(&room, &sender, &message);
    }
    debug!(
        room = %room,
        archive_id = %archive_id.stored_id,
        "ArchiveGroupchat: persisted"
    );
    update_groupchat_link_preview_refs(deps, &room, &archive_id.stored_id, &message).await;
    if !exact_room_effect_authorized(deps, &room).await {
        return ownership_uncertain_bounce(&room, &sender, &message);
    }
    // Notification activity ingest (slice 2b): committing the sender's
    // groupchat message into the room archive is the strongest
    // "currently active" signal we have for `(sender, room)`. Unlike
    // `ArchiveDirect`, the groupchat arm runs once per message (the
    // room owns the canonical archive), so no symmetric gate is
    // required.
    super::notification_activity_ingest::record_outbound_message_activity(
        deps,
        &sender.to_bare(),
        &room,
        &message,
    )
    .await;
    if exact_room_effect_authorized(deps, &room).await {
        ArchiveGroupchatEventOutcome::Rewrite(archive_id.rewrite)
    } else {
        ownership_uncertain_bounce(&room, &sender, &message)
    }
}

async fn update_groupchat_link_preview_refs(
    deps: &Deps<'_>,
    room: &BareJid,
    archive_id: &str,
    message: &Message,
) {
    let Some(state) = deps.web_socket_state else {
        return;
    };
    let global_db_actor = state.deps.app_state.db_pool.global_actor();
    let correction_target_message_id =
        if let Some(correction) = waddle_xmpp::xep::extract_correction_from_message(message) {
            Some(correction.replaces_id)
        } else {
            None
        };
    let message_id = correction_target_message_id
        .as_deref()
        .or_else(|| message.id.as_ref().map(|id| id.0.as_str()));
    let Some(message_id) = message_id else { return };
    crate::server::routes::websocket::link_preview_refs::record_current_message_preview_refs(
        global_db_actor,
        state.deps.auth_state.base_url.as_str(),
        room,
        message_id,
        archive_id,
        message,
    )
    .await;
}
