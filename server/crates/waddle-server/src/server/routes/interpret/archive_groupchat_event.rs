use super::*;

pub(super) enum ArchiveGroupchatEventOutcome {
    Stored(Option<ArchiveIdRewrite>),
    Deduplicated {
        rewrite: Option<ArchiveIdRewrite>,
        sender: BareJid,
    },
    TombstoneHit,
    Skipped,
    OwnershipLost(Box<Message>),
}

pub(super) async fn archive_groupchat_event(
    deps: &Deps<'_>,
    room: BareJid,
    sender: FullJid,
    message: Box<Message>,
    sender_nickname_generation: u64,
    sender_item: Option<waddle_xmpp_core::mam::ArchivedMucSender>,
) -> ArchiveGroupchatEventOutcome {
    let Some(mam_storage) = deps.mam_storage else {
        debug!(
            room = %room,
            sender = %sender,
            "ArchiveGroupchat: no mam_storage in Deps; skipping (test fixture?)"
        );
        return ArchiveGroupchatEventOutcome::Skipped;
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
    let fence = resolve_room_claim_fence(deps, &room).await;
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
        ArchiveGroupchatOutcome::Deduplicated(result) => {
            return ArchiveGroupchatEventOutcome::Deduplicated {
                rewrite: result.rewrite,
                sender: sender.to_bare(),
            };
        }
        ArchiveGroupchatOutcome::TombstoneHit => {
            return ArchiveGroupchatEventOutcome::TombstoneHit;
        }
        ArchiveGroupchatOutcome::Skipped => return ArchiveGroupchatEventOutcome::Skipped,
        ArchiveGroupchatOutcome::OwnershipLost => {
            let bounce = build_message_error_reply(
                &message,
                &room,
                &sender,
                resource_constraint_error("This room is temporarily unavailable; please retry."),
            );
            return ArchiveGroupchatEventOutcome::OwnershipLost(Box::new(bounce));
        }
    };
    debug!(
        room = %room,
        archive_id = %archive_id.stored_id,
        "ArchiveGroupchat: persisted"
    );
    update_groupchat_link_preview_refs(deps, &room, &archive_id.stored_id, &message).await;
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
    ArchiveGroupchatEventOutcome::Stored(archive_id.rewrite)
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
