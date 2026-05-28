//! Shared MUC Muji-presence cleanup for SFU-driven and client-driven
//! call departures.
//!
//! Both the LiveKit webhook bridge and the Muji Jingle
//! `session-terminate` path need to clear the room actor's per-session
//! Muji state and broadcast the XEP-0272 leave marker to remaining
//! occupants. Centralising the logic keeps the wire shape identical.

use jid::{BareJid, FullJid};
use tracing::{debug, warn};
use waddle_xmpp::muc::build_occupant_presence;
use waddle_xmpp::muc::room_actor::{ClearMujiPresence, MujiPresenceUpdateOutcome};
use waddle_xmpp::xep::xep0272::Muji;
use waddle_xmpp::xep::xep0421::OccupantIdentity;
use waddle_xmpp_core::Stanza;

use super::websocket::{get_room_actor, note_participant_left_from_webhook, WebSocketState};

/// Clear `full_jid`'s Muji advertisement in `room_jid` and broadcast
/// the leave marker to remaining occupants. Idempotent: a participant
/// already cleared via presence-update returns `Ok(None)` and skips
/// the broadcast.
pub(crate) async fn clear_muji_presence_for_departure(
    state: &WebSocketState,
    room_jid: &BareJid,
    full_jid: &FullJid,
) {
    debug!(
        room = %room_jid,
        identity = %full_jid,
        "Clearing Muji presence for departed participant"
    );

    let Some(actor) = get_room_actor(state, room_jid).await else {
        return;
    };
    let outcome = match actor
        .ask(ClearMujiPresence {
            sender_jid: full_jid.clone(),
        })
        .await
    {
        Ok(Some(outcome)) => outcome,
        Ok(None) => {
            debug!(
                room = %room_jid,
                identity = %full_jid,
                "Participant not in MUC actor; SFU registry cleanup only"
            );
            note_participant_left_from_webhook(state, room_jid, full_jid);
            return;
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                identity = %full_jid,
                error = ?error,
                "Room actor rejected Muji clear; falling through to SFU unregister"
            );
            note_participant_left_from_webhook(state, room_jid, full_jid);
            return;
        }
    };

    broadcast_muji_clear(state, room_jid, full_jid, &outcome);
    note_participant_left_from_webhook(state, room_jid, full_jid);
}

/// Broadcast a server-originated Muji-presence clear to every remaining
/// occupant of the room.
pub(crate) fn broadcast_muji_clear(
    state: &WebSocketState,
    room_jid: &BareJid,
    leaving_real_jid: &FullJid,
    outcome: &MujiPresenceUpdateOutcome,
) {
    let from_room_jid = room_jid
        .clone()
        .with_resource_str(&outcome.update.sender_nick)
        .unwrap_or_else(|_| leaving_real_jid.clone());

    let mut entries: Vec<(FullJid, Option<Muji>)> =
        Vec::with_capacity(outcome.session_mujis.len() + 1);
    entries.push((leaving_real_jid.clone(), None));
    for (owner, muji) in &outcome.session_mujis {
        if owner == leaving_real_jid {
            continue;
        }
        entries.push((owner.clone(), Some(muji.clone())));
    }

    for recipient in &outcome.update.recipients {
        for (owner_jid, muji) in &entries {
            let owner_bare = owner_jid.to_bare();
            let identity = OccupantIdentity {
                bare_jid: &owner_bare,
                real_jid: Some(owner_jid),
                secret: &state.deps.occupant_id_secret,
            };
            let is_self = recipient.to_bare() == owner_bare;
            let mut presence = build_occupant_presence(
                &from_room_jid,
                recipient,
                outcome.update.sender_affiliation,
                outcome.update.sender_role,
                is_self,
                &identity,
            );
            if let Some(muji_ref) = muji {
                if !muji_ref.is_empty() {
                    presence.payloads.push(muji_ref.to_element());
                }
            }
            let _ = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(recipient, Stanza::Presence(presence));
        }
    }
}
