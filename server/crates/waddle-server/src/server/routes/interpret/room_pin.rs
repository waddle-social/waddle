//! Interpreter arm for [`OutboundEvent::ApplyPinChange`] (#414).
//!
//! Forwards the typed `PinStateChange` to the per-room `RoomActor` via
//! the `ApplyPin` actor message, which delegates to
//! [`waddle_xmpp::muc::MucRoom::upsert_pin`] /
//! [`waddle_xmpp::muc::MucRoom::remove_pin_by_target`]. Mirrors the
//! shape of [`super::room_subject::persist_room_subject_event`].

use super::*;
use waddle_xmpp::muc::pin::PinStateChange;
use waddle_xmpp::muc::room_actor::GetPinList;
use waddle_xmpp::xep::xep0470::NS_WADDLE_PIN_V0;
use xmpp_parsers::message::{Body, Message, MessageType};
use xmpp_parsers::minidom::Element;

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
        .find(|e| e.target_stanza_id == target_message_id)
        .cloned()
    else {
        return;
    };
    if let Err(error) = room_actor
        .ask(ApplyPin {
            change: PinStateChange::Unpin {
                target_stanza_id: target_message_id.clone(),
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
    let system_message = build_cascade_unpin_message(&room, &entry.pinner_jid, &target_message_id);
    super::room_system_message::broadcast_room_system_message_event(
        registry,
        deps,
        room,
        Box::new(system_message),
        recursion_depth,
    )
    .await;
}

fn build_cascade_unpin_message(
    room: &BareJid,
    original_pinner: &BareJid,
    target_stanza_id: &str,
) -> Message {
    let mut event = Element::builder("pin-event", NS_WADDLE_PIN_V0)
        .attr("action", "unpinned")
        .attr("by", original_pinner.to_string().as_str())
        .attr("reason", "retracted")
        .build();
    event.append_child(
        Element::builder("ref", NS_WADDLE_PIN_V0)
            .attr("id", target_stanza_id)
            .build(),
    );
    let mut msg = Message::new(Some(jid::Jid::from(room.clone())));
    msg.from = Some(jid::Jid::from(room.clone()));
    msg.type_ = MessageType::Groupchat;
    msg.bodies.insert(
        String::new(),
        Body("Pinned message was retracted by its author".into()),
    );
    msg.payloads.push(event);
    msg
}
