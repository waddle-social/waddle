use super::*;

pub(super) async fn validate_groupchat_rich_targets(
    deps: &Deps<'_>,
    room: &BareJid,
    message: &Message,
    sender_room_nick_jid: Option<&Jid>,
    room_actor: &ActorRef<RoomActor>,
    sender_nickname_generation: Option<u64>,
) -> Result<(), Box<StanzaError>> {
    if message.from.is_none() {
        return Ok(());
    }
    if has_malformed_rich_payload(message) {
        return Err(bad_request_error(
            "Rich-message payload is missing a required identifier or contains an invalid JID.",
        )
        .into());
    }
    let Some(mam_storage) = deps.mam_storage else {
        // No archive available — nothing to validate against. Mirrors
        // the legacy bridge's `state.deps.protocol.mam_storage` use:
        // production always supplies it; in test fixtures without
        // storage we treat the validation as a no-op.
        return Ok(());
    };
    // The archive stores `from` in the XEP-0045 §7.2.13 `room/nick`
    // form (the chain stamps it AFTER validation), so the
    // same-sender check compares against the sender's room/nick view
    // — not against `prototype.from` (alice's real full JID, set by
    // the user-side state machine before `DispatchToRoom` was
    // emitted). When the snapshot has no nick for the sender (sender
    // not currently joined under any nickname), any rich-target
    // operation is forbidden because we cannot satisfy the
    // continuity check.
    let Some(sender_archive_view) = sender_room_nick_jid else {
        if extract_correction_from_message(message).is_some()
            || matches!(
                extract_retraction_from_message(message),
                Some(RetractionKind::Request(_))
            )
        {
            return Err(forbidden_error(
                "Sender is not joined to the room; rich-target operations require occupancy.",
            )
            .into());
        }
        return Ok(());
    };

    if let Some(correction) = extract_correction_from_message(message) {
        let original = match mam_storage
            .get_message_by_message_id(room, &correction.replaces_id)
            .await
        {
            Ok(Some(original)) => original,
            Ok(None) => return Err(item_not_found_error("Correction target not found.").into()),
            Err(_) => return Err(internal_server_error_for_lookup().into()),
        };
        if !sender_matches_groupchat_from(sender_archive_view, &original.from) {
            return Err(forbidden_error("Only the original sender may correct a message.").into());
        }
        verify_groupchat_occupancy_generation(
            sender_archive_view,
            &original,
            room_actor,
            sender_nickname_generation,
        )
        .await?;
    }

    if let Some(RetractionKind::Request(retraction)) = extract_retraction_from_message(message) {
        let original =
            match lookup_groupchat_retraction_target(mam_storage, room, &retraction.retracts_id)
                .await
            {
                Ok(Some(original)) => original,
                Ok(None) => return Err(item_not_found_error("Retraction target not found.").into()),
                Err(_) => return Err(internal_server_error_for_lookup().into()),
            };
        if !sender_matches_groupchat_from(sender_archive_view, &original.from) {
            return Err(forbidden_error("Only the original sender may retract a message.").into());
        }
    }
    Ok(())
}

/// XEP-0424 §3 retraction target resolution for groupchat. Per
/// xep-0424.xml lines 158 & 230-232, a `<retract/>` in a group chat
/// cites the target by the **room-assigned XEP-0359 stanza-id** (the
/// `<stanza-id by='room'/>` value), which waddle persists as the archive
/// primary key (see `finish_archive_groupchat_message`). Resolve
/// strictly by that id via [`MamStorage::get_message`] (a primary-key
/// lookup) and confirm the row belongs to this room — never by the
/// original wire `id` attribute (the `stanza_id` column) or the client
/// origin-id. This mirrors the XEP-0425 moderation target lookup, which
/// also keys off the room stanza-id (the PK) plus a room-membership
/// check.
///
/// A `get_message_by_message_id` lookup keyed off the `stanza_id` column
/// can never resolve the room stanza-id on a SQL backend (PK and
/// `stanza_id` are distinct values), which silently dropped every
/// conformant channel retraction — including waddle's own client, which
/// sends `replyableId = stampedByRoom` — as `<item-not-found/>`.
pub(super) async fn lookup_groupchat_retraction_target(
    mam_storage: &Arc<dyn MamStorage>,
    room: &BareJid,
    target_id: &str,
) -> Result<Option<MamArchivedMessage>, waddle_xmpp::mam::MamStorageError> {
    // The room stanza-id is a globally-unique server UUID stamped as the
    // archive PK; scope to this room's archive (the archived `to` is the
    // room JID) so a retraction can only target a row in the same room.
    Ok(mam_storage
        .get_message(target_id)
        .await?
        .filter(|row| row.to.to_bare() == *room))
}

/// Compare a XEP-0045 sender (the in-room full JID `room/nick`) against
/// the archived `from` JID for groupchat ownership checks. Both are
/// typed `Jid` so we can compare structurally without round-tripping
/// through strings.
pub(super) fn sender_matches_groupchat_from(sender: &Jid, original_from: &Jid) -> bool {
    sender == original_from
}

/// XEP-0308 §3 occupancy continuity check: a full-JID that left the
/// room and rejoined under the same nickname MUST NOT be allowed to
/// correct messages from the previous occupancy. Compares the
/// per-nickname generation captured on the archive row at write time
/// against the room actor's current generation for the sender's
/// nickname.
pub(super) async fn verify_groupchat_occupancy_generation(
    sender: &Jid,
    original: &MamArchivedMessage,
    room_actor: &ActorRef<RoomActor>,
    sender_current_generation: Option<u64>,
) -> Result<(), Box<StanzaError>> {
    let Some(nick) = sender.resource().map(|r| r.to_string()) else {
        return Err(
            forbidden_error("Correction sender has no MUC nickname for occupancy check.").into(),
        );
    };
    let Some(archived_generation) = original.nickname_generation else {
        return Err(forbidden_error(
            "Original message predates occupancy tracking; correction window has closed.",
        )
        .into());
    };
    // Prefer the generation snapshot already captured by `dispatch_to_room`
    // (it came from the same `GetRoomSnapshot` query that populated the
    // chain context); fall back to a fresh per-nickname query if the
    // snapshot didn't include the sender (unlikely — would mean the
    // sender is not joined under any nickname).
    let current_generation = match sender_current_generation {
        Some(value) => value,
        None => match room_actor
            .ask(GetNicknameGeneration { nick: nick.clone() })
            .await
        {
            Ok(value) => value,
            Err(_) => return Err(internal_server_error_for_lookup().into()),
        },
    };
    if current_generation != archived_generation {
        return Err(forbidden_error(
            "Occupancy generation has advanced; correction is no longer permitted across the leave/rejoin boundary.",
        )
        .into());
    }
    Ok(())
}

pub(super) fn has_malformed_rich_payload(message: &Message) -> bool {
    message.payloads.iter().any(|payload| {
        (payload.ns() == NS_MESSAGE_CORRECT
            && payload.name() == "replace"
            && payload.attr("id").is_none_or(str::is_empty))
            || (payload.ns() == NS_MESSAGE_RETRACT
                && payload.name() == "retract"
                && payload.attr("id").is_none_or(str::is_empty))
            || (payload.ns() == NS_REACTIONS
                && payload.name() == "reactions"
                && payload.attr("id").is_none_or(str::is_empty))
            || (payload.ns() == NS_REPLY
                && payload.name() == "reply"
                && (payload.attr("id").is_none_or(str::is_empty)
                    || payload.attr("to").is_some_and(|to| {
                        to.trim().is_empty() || to.trim().parse::<Jid>().is_err()
                    })))
            || (payload.ns() == NS_REFERENCE
                && payload.name() == "reference"
                && (payload.attr("type").is_none_or(str::is_empty)
                    || payload.attr("uri").is_none_or(str::is_empty)))
            || (payload.ns() == NS_EXPLICIT_MENTIONS
                && payload.name() == "mention"
                && (payload.attr("jid").is_some()
                    || (payload.attr("occupantid").is_none_or(str::is_empty)
                        && payload.attr("mentions").is_none_or(str::is_empty))))
    })
}

pub(super) fn remove_framework_envelopes(message: &mut Message) {
    message
        .payloads
        .retain(|payload| !payload.ns().starts_with("urn:waddle:"));
}

pub(super) fn bad_request_error(text: &str) -> StanzaError {
    StanzaError::new(ErrorType::Modify, DefinedCondition::BadRequest, "en", text)
}

pub(super) fn item_not_found_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::ItemNotFound,
        "en",
        text,
    )
}

pub(super) fn forbidden_error(text: &str) -> StanzaError {
    StanzaError::new(ErrorType::Auth, DefinedCondition::Forbidden, "en", text)
}

pub(super) fn service_unavailable_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Wait,
        DefinedCondition::ServiceUnavailable,
        "en",
        text,
    )
}

/// ADR-0017 Phase 3 Slice 7: the ownership-gap bounce — "messages arriving
/// during the ownership gap are bounced with a typed recoverable
/// `<resource-constraint/>` error, never silently dropped." `type='wait'`
/// per RFC 6120 §8.3.3.20: recoverable, the sender may retry (the next
/// `GetOrCreateRoom` on any node re-claims and restores the room). Used by
/// `dispatch_to_room`'s fenced pre-fan-out backstop AND (FIX 1) the MAM
/// fenced-archive-write backstop in `groupchat_archive.rs` — both only
/// ever fire on a `clustering`-feature build with clustering enabled (the
/// `fence`/`muc_durable_store` context they gate on is `None` otherwise),
/// but the helper itself is plain data construction with nothing
/// feature-specific, so it is not itself `#[cfg]`-gated.
pub(super) fn resource_constraint_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Wait,
        DefinedCondition::ResourceConstraint,
        "en",
        text,
    )
}

pub(super) fn internal_server_error_for_lookup() -> StanzaError {
    StanzaError::new(
        ErrorType::Wait,
        DefinedCondition::InternalServerError,
        "en",
        "Archive lookup failed while validating rich-message target.",
    )
}

/// Build a typed `<message type='error'>` reply addressed from the
/// room JID back to the sender. Mirrors the legacy `error_message`
/// helper.
pub(super) fn build_message_error_reply(
    incoming: &Message,
    room: &BareJid,
    sender: &FullJid,
    error: StanzaError,
) -> Message {
    let mut reply = incoming.clone();
    reply.type_ = XmppMessageType::Error;
    reply.from = Some(Jid::from(room.clone()));
    reply.to = Some(Jid::from(sender.clone()));
    reply.payloads.push(Element::from(error));
    reply
}
