//! Interpreter arms for pin events (#414).
//!
//! Forwards `OutboundEvent::ApplyPinChange` to the per-room
//! `RoomActor` via the `ApplyPin` actor message and runs the XEP-0424
//! retraction → auto-unpin cascade. The cascade reads the room's
//! current pin list, removes the matching entry atomically inside the
//! actor (via the actor's serial mailbox), then broadcasts a
//! synthetic `<unpinned reason='retracted'/>` system message via
//! `broadcast_room_system_message_event`.

use super::*;
use waddle_xmpp::muc::pin::PinStateChange;
use waddle_xmpp::muc::room_actor::GetPinList;
use waddle_xmpp::protocol::room::pin::build_unpinned_system_message;
use waddle_xmpp::xep::xep_waddle_pin::MAX_TARGET_STANZA_ID_LEN;
use waddle_xmpp_core::xep0359::StanzaId as Xep0359StanzaIdTyped;

pub(super) async fn apply_pin_change_event(deps: &Deps<'_>, room: BareJid, change: PinStateChange) {
    let Some(room_registry) = deps.room_registry else {
        debug!(
            room = %room,
            "ApplyPinChange: no room_registry in Deps; skipping"
        );
        return;
    };
    let room_actor = match room_registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            debug!(
                room = %room,
                "ApplyPinChange: room not registered; skipping"
            );
            return;
        }
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "ApplyPinChange: room registry lookup failed; skipping"
            );
            return;
        }
    };
    if let Err(error) = room_actor.ask(ApplyPin { change }).await {
        warn!(
            room = %room,
            error = ?error,
            "ApplyPinChange: ApplyPin ask failed; pin state left at previous value"
        );
    }
}

/// XEP-0424 retraction → pin auto-unpin cascade (#414, Q8 = a).
///
/// When a groupchat message is retracted, check whether its stanza-id
/// is in the room's current pin list. If so, remove the pin entry and
/// broadcast a synthetic unpin system message so live clients see the
/// projection update without re-querying.
pub(super) async fn cascade_retraction_to_pin_list(
    registry: &ConnectionRegistry,
    deps: &Deps<'_>,
    room: BareJid,
    target_message_id: String,
    recursion_depth: u8,
) {
    // Defense in depth: an oversized id can't legitimately match any
    // pin entry, but bounding it prevents allocation amplification on
    // the per-occupant fan-out below if something upstream was lax.
    if target_message_id.is_empty() || target_message_id.len() > MAX_TARGET_STANZA_ID_LEN {
        return;
    }
    let Some(room_registry) = deps.room_registry else {
        return;
    };
    let room_actor = match room_registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .await
    {
        Ok(Some(actor)) => actor,
        _ => return,
    };
    let entries = match room_actor.ask(GetPinList).await {
        Ok(entries) => entries,
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "Pin retraction cascade: GetPinList ask failed; skipping"
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
    let target_typed = Xep0359StanzaIdTyped::new(target_message_id, Jid::from(room.clone()));
    if let Err(error) = room_actor
        .ask(ApplyPin {
            change: PinStateChange::Unpin {
                target_stanza_id: target_typed.clone(),
            },
        })
        .await
    {
        warn!(
            room = %room,
            error = ?error,
            "Pin retraction cascade: ApplyPin Unpin ask failed"
        );
        return;
    }
    let pinner = entry.pinner_jid.clone();
    let system_message =
        build_unpinned_system_message(&room, &pinner, "", &target_typed, Some("retracted"));
    super::room_system_message::broadcast_room_system_message_event(
        registry,
        deps,
        room,
        Box::new(system_message),
        recursion_depth,
    )
    .await;
}
