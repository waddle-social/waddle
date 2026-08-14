//! Interpreter arms for pin events (#414).
//!
//! Resolves an `OutboundEvent::ApplyPinChange` request by:
//!
//! 1. For pins: looking up the target message in the room's MAM
//!    archive to populate the preview (author bare-JID, body text
//!    truncated to 280 chars, original message timestamp). The chain
//!    handler is synchronous and cannot do this lookup — the
//!    interpreter is the async boundary that builds the resolved
//!    [`PinnedEntry`].
//! 2. Forwarding the resolved [`PinStateChange`] to the room actor's
//!    `ApplyPin` message.
//! 3. Building the `<pin-event/>` system message with the resolved
//!    preview and broadcasting it via
//!    [`super::room_system_message::broadcast_room_system_message_event`].
//!
//! The XEP-0424 retraction cascade emits `ApplyPinChange { Unpin {
//! reason: "retracted" } }` through this same path so the unpin
//! attribution and broadcast logic stays consistent.

use super::*;
use jid::BareJid;
use waddle_xmpp::muc::pin::{
    PinChangeRequest, PinPreview, PinStateChange, PinnedEntry, MAX_PREVIEW_LEN,
};
use waddle_xmpp::muc::room_actor::{GetPinList, GetRoomSnapshot};
use waddle_xmpp::protocol::room::pin::{
    build_pinned_system_message, build_unpinned_system_message,
};
use waddle_xmpp::xep::xep_waddle_pin::MAX_TARGET_STANZA_ID_LEN;
use waddle_xmpp_core::xep0359::StanzaId;

pub(super) async fn apply_pin_change_event(
    deps: &Deps<'_>,
    room: BareJid,
    request: PinChangeRequest,
    recursion_depth: u8,
) {
    if request.target_stanza_id().id.is_empty()
        || request.target_stanza_id().id.len() > MAX_TARGET_STANZA_ID_LEN
    {
        warn!(
            room = %room,
            target = %request.target_stanza_id().id,
            "ApplyPinChange: target stanza-id failed length validation; dropping"
        );
        return;
    }
    match request {
        PinChangeRequest::Pin { .. } => {
            apply_pin(deps, room, request, recursion_depth).await;
        }
        PinChangeRequest::Unpin { .. } => {
            apply_unpin(deps, room, request, recursion_depth).await;
        }
    }
}

async fn apply_pin(deps: &Deps<'_>, room: BareJid, request: PinChangeRequest, recursion_depth: u8) {
    let PinChangeRequest::Pin {
        target_stanza_id,
        pinner_jid,
        pinner_nick,
        pinned_at,
    } = request
    else {
        return;
    };
    let Some(room_actor) = lookup_room_actor(deps, &room, "ApplyPinChange::Pin").await else {
        return;
    };

    // Pull the room snapshot once: we need its occupant list to map
    // the archived `room/nick` from-JID back to the author's real
    // bare JID for the preview.
    let synthetic_sender = match room.clone().with_resource_str("__pin_resolver__") {
        Ok(s) => s,
        Err(error) => {
            warn!(
                room = %room,
                ?error,
                "ApplyPinChange::Pin: failed to build resolver sender; dropping"
            );
            return;
        }
    };
    let snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: synthetic_sender,
        })
        .await
    {
        Ok(snap) => snap,
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "ApplyPinChange::Pin: GetRoomSnapshot failed; dropping"
            );
            return;
        }
    };

    // Resolve the preview from MAM. If the target row is missing
    // (e.g., archive purged or wire id mismatched), fall back to a
    // placeholder preview keyed on the pinner — better than dropping
    // the pin silently. Most real pin requests target a recent
    // message that's still archived.
    let preview = resolve_preview_from_mam(deps, &room, &target_stanza_id, &snapshot.occupants)
        .await
        .unwrap_or_else(|| {
            warn!(
                room = %room,
                target = %target_stanza_id.id,
                "ApplyPinChange::Pin: target not found in MAM; storing placeholder preview"
            );
            PinPreview::new(pinner_jid.clone(), None, "", pinned_at)
        });

    let entry = PinnedEntry {
        target_stanza_id: target_stanza_id.clone(),
        pinner_jid: pinner_jid.clone(),
        pinned_at,
        preview: preview.clone(),
    };

    if let Err(error) = room_actor
        .ask(ApplyPin {
            change: PinStateChange::Pin(entry.clone()),
        })
        .await
    {
        warn!(
            room = %room,
            error = ?error,
            "ApplyPinChange::Pin: ApplyPin ask failed; pin not stored or broadcast"
        );
        return;
    }
    deps.capture_intent(IngressEffectIntent::Pin {
        room: room.clone(),
        mutation: waddle_xmpp::ingress::RoomPinMutation::Pin { entry },
    });

    let system_message = build_pinned_system_message(
        &room,
        &pinner_jid,
        &pinner_nick,
        &target_stanza_id,
        Some(&preview),
        None,
    );
    super::room_system_message::broadcast_room_system_message_event(
        deps,
        room,
        Box::new(system_message),
        recursion_depth,
    )
    .await;
}

async fn apply_unpin(
    deps: &Deps<'_>,
    room: BareJid,
    request: PinChangeRequest,
    recursion_depth: u8,
) {
    let PinChangeRequest::Unpin {
        target_stanza_id,
        pinner_jid,
        pinner_nick,
        reason,
    } = request
    else {
        return;
    };
    let Some(room_actor) = lookup_room_actor(deps, &room, "ApplyPinChange::Unpin").await else {
        return;
    };

    if let Err(error) = room_actor
        .ask(ApplyPin {
            change: PinStateChange::Unpin {
                target_stanza_id: target_stanza_id.clone(),
            },
        })
        .await
    {
        warn!(
            room = %room,
            error = ?error,
            "ApplyPinChange::Unpin: ApplyPin ask failed; pin still in list, no broadcast"
        );
        return;
    }
    deps.capture_intent(IngressEffectIntent::Pin {
        room: room.clone(),
        mutation: waddle_xmpp::ingress::RoomPinMutation::Unpin {
            target_stanza_id: target_stanza_id.clone(),
        },
    });

    let system_message = build_unpinned_system_message(
        &room,
        &pinner_jid,
        &pinner_nick,
        &target_stanza_id,
        reason.as_deref(),
    );
    super::room_system_message::broadcast_room_system_message_event(
        deps,
        room,
        Box::new(system_message),
        recursion_depth,
    )
    .await;
}

async fn lookup_room_actor(
    deps: &Deps<'_>,
    room: &BareJid,
    op: &'static str,
) -> Option<kameo::actor::ActorRef<RoomActor>> {
    let Some(room_registry) = deps.room_registry else {
        debug!(
            room = %room,
            op,
            "no room_registry in Deps; skipping"
        );
        return None;
    };
    match room_registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .await
    {
        Ok(Some(actor)) => Some(actor),
        Ok(None) => {
            debug!(
                room = %room,
                op,
                "room not registered; skipping"
            );
            None
        }
        Err(error) => {
            warn!(
                room = %room,
                op,
                error = ?error,
                "room registry lookup failed; skipping"
            );
            None
        }
    }
}

/// Look up the target message in the room's MAM archive and build a
/// `PinPreview` from it. Returns `None` if the message isn't found or
/// MAM storage isn't wired in `Deps`. The body is truncated to
/// `MAX_PREVIEW_LEN` UTF-8 chars by `PinPreview::new`.
///
/// Author resolution: groupchat archive rows store `from` as
/// `room@conf/nick` (XEP-0045 §7.2.13 canonicalized form), so the
/// resource part is the nickname and `to_bare()` collapses to the
/// room JID. We extract the nick from the resource and consult the
/// supplied occupant snapshot to recover the *current* real bare JID
/// for that nick; if no current occupant matches (the author has
/// left the room since posting), the preview falls back to the room
/// JID as `author_jid` with the captured `author_nick` carrying the
/// identity. This trades a small amount of fidelity for a stable
/// non-async lookup — pinning is interactive, so racing with leaves
/// is rare.
async fn resolve_preview_from_mam(
    deps: &Deps<'_>,
    room: &BareJid,
    target_stanza_id: &StanzaId,
    occupants: &[waddle_xmpp::muc::room_actor::RoomChainOccupant],
) -> Option<PinPreview> {
    let mam_storage = deps.mam_storage?;
    let row = match mam_storage
        .get_message_by_stanza_id(room, &target_stanza_id.id)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return None,
        Err(error) => {
            warn!(
                room = %room,
                target = %target_stanza_id.id,
                error = ?error,
                "ApplyPinChange::Pin: MAM lookup failed"
            );
            return None;
        }
    };
    let nick = row.from.resource().map(|r| r.to_string());
    let author_bare = nick
        .as_deref()
        .and_then(|nick| {
            occupants
                .iter()
                .find(|o| o.nick == nick)
                .map(|o| o.full_jid.to_bare())
        })
        .unwrap_or_else(|| room.clone());
    let body = row.body.clone().unwrap_or_default();
    let truncated = if body.chars().count() > MAX_PREVIEW_LEN {
        body.chars().take(MAX_PREVIEW_LEN).collect()
    } else {
        body
    };
    Some(PinPreview::new(
        author_bare,
        nick,
        &truncated,
        row.timestamp,
    ))
}

/// XEP-0424 retraction → pin auto-unpin cascade (#414, Q8 = a).
///
/// When a groupchat message is retracted, check whether its stanza-id
/// is in the room's current pin list. If so, emit an
/// `ApplyPinChange { Unpin { reason: "retracted" } }` event so the
/// regular interpreter path mutates the actor and broadcasts the
/// unpin system message — single code path, no special-case logic.
pub(super) async fn cascade_retraction_to_pin_list(
    deps: &Deps<'_>,
    room: BareJid,
    target_message_id: String,
    recursion_depth: u8,
) {
    if target_message_id.is_empty() || target_message_id.len() > MAX_TARGET_STANZA_ID_LEN {
        return;
    }
    let Some(room_actor) = lookup_room_actor(deps, &room, "PinRetractionCascade").await else {
        return;
    };
    let entries = match room_actor.ask(GetPinList).await {
        Ok(entries) => entries,
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "Pin retraction cascade: GetPinList ask failed"
            );
            return;
        }
    };
    let Some(entry) = entries
        .iter()
        .find(|e| e.target_stanza_id.id == target_message_id)
        .cloned()
    else {
        return;
    };

    apply_pin_change_event(
        deps,
        room,
        PinChangeRequest::Unpin {
            target_stanza_id: entry.target_stanza_id,
            pinner_jid: entry.pinner_jid,
            pinner_nick: String::new(),
            reason: Some("retracted".to_string()),
        },
        recursion_depth,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress_shadow::IngressEffectCapture;
    use crate::server::routes::interpret::Deps;
    use waddle_xmpp_core::xep0359::StanzaId;

    #[tokio::test]
    async fn missing_room_actor_does_not_capture_pin_intent() {
        let registry = ConnectionRegistry::new();
        let capture = IngressEffectCapture::new(None);
        let mut deps = Deps::registry_only(&registry);
        deps.ingress_effect_capture = Some(capture.clone());
        let room: BareJid = "room@muc.example.com".parse().expect("room");

        apply_pin_change_event(
            &deps,
            room.clone(),
            PinChangeRequest::Pin {
                target_stanza_id: StanzaId::new("pin-target", jid::Jid::from(room.clone())),
                pinner_jid: "alice@example.com".parse().expect("pinner"),
                pinner_nick: "alice".to_string(),
                pinned_at: chrono::Utc::now(),
            },
            0,
        )
        .await;

        assert!(
            !capture.snapshot().intents.iter().any(|intent| matches!(
                intent,
                IngressEffectIntent::Pin { room: captured_room, .. } if captured_room == &room
            )),
            "pin capture must not survive a missing room actor",
        );
    }

    #[tokio::test]
    async fn missing_room_actor_does_not_capture_unpin_intent() {
        let registry = ConnectionRegistry::new();
        let capture = IngressEffectCapture::new(None);
        let mut deps = Deps::registry_only(&registry);
        deps.ingress_effect_capture = Some(capture.clone());
        let room: BareJid = "room@muc.example.com".parse().expect("room");

        apply_pin_change_event(
            &deps,
            room.clone(),
            PinChangeRequest::Unpin {
                target_stanza_id: StanzaId::new("pin-target", jid::Jid::from(room.clone())),
                pinner_jid: "alice@example.com".parse().expect("pinner"),
                pinner_nick: "alice".to_string(),
                reason: Some("manual".to_string()),
            },
            0,
        )
        .await;

        assert!(
            !capture.snapshot().intents.iter().any(|intent| matches!(
                intent,
                IngressEffectIntent::Pin { room: captured_room, .. } if captured_room == &room
            )),
            "unpin capture must not survive a missing room actor",
        );
    }
}
