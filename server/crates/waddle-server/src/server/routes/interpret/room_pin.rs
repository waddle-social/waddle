//! Interpreter arm for [`OutboundEvent::ApplyPinChange`] (#414).
//!
//! Forwards the typed `PinStateChange` to the per-room `RoomActor` via
//! the `ApplyPin` actor message, which delegates to
//! [`waddle_xmpp::muc::MucRoom::upsert_pin`] /
//! [`waddle_xmpp::muc::MucRoom::remove_pin_by_target`]. Mirrors the
//! shape of [`super::room_subject::persist_room_subject_event`].

use super::*;
use waddle_xmpp::muc::pin::PinStateChange;

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
