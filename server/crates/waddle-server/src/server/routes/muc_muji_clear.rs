//! Shared MUC Muji-presence cleanup for SFU-driven and client-driven
//! call departures.
//!
//! Both the LiveKit webhook bridge and the Muji Jingle
//! `session-terminate` path need to clear the room actor's per-session
//! Muji state and broadcast the XEP-0272 leave marker to remaining
//! occupants. Centralising the logic keeps the wire shape identical.

use jid::{BareJid, FullJid};
use minidom::Element;
use tracing::{debug, warn};
use waddle_xmpp::muc::build_occupant_presence;
use waddle_xmpp::muc::room_actor::{ClearMujiPresence, MujiPresenceUpdateOutcome};
use waddle_xmpp::xep::xep0272::Muji;
use waddle_xmpp::xep::xep0421::OccupantIdentity;
use waddle_xmpp::xep::{
    build_call_thread_ended, build_hint_element, CallThreadDuration, CallThreadEnded, Hint,
    NS_FASTEN,
};
use waddle_xmpp_core::Stanza;
use xmpp_parsers::message::{Message, MessageType};

use super::websocket::{
    get_room_actor, interpret_loop::build_interpret_deps, note_participant_left_from_webhook,
    WebSocketState,
};

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
        note_participant_left_from_webhook(state, room_jid, full_jid);
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
            if super::interpret::actor_send_maybe_enqueued(&error) {
                state.deps.room_serving.mark_unsafe_to_release();
            }
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
    maybe_broadcast_call_thread_ended(state, room_jid).await;
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
                waddle_xmpp::muc::MucPresenceStatus::new(is_self, false),
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

pub(crate) async fn maybe_broadcast_call_thread_ended(state: &WebSocketState, room_jid: &BareJid) {
    let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
        return;
    };
    let Ok(call_id) = waddle_sfu::CallId::new(room_jid.to_string()) else {
        return;
    };
    if !sfu.participants_for_call(&call_id).is_empty() {
        return;
    }
    let Some((_, active)) = state.deps.protocol.call_threads.remove(room_jid) else {
        return;
    };

    let ended = chrono::Utc::now();
    let duration = ended.signed_duration_since(active.started);
    let duration = CallThreadDuration::parse(&format_call_thread_duration(duration))
        .expect("formatted call-thread duration is valid");
    let message = build_call_thread_ended_message(
        room_jid,
        &active.anchor_origin_id,
        &CallThreadEnded {
            ended,
            duration: duration.clone(),
        },
    );
    let deps = build_interpret_deps(state, None);
    let _ =
        super::interpret::broadcast_room_system_message(&deps, room_jid.clone(), Box::new(message))
            .await;

    // Stamp the ended summary onto every subscriber's inbox/threads
    // projection of this thread. The fastening above is the wire record;
    // this persists the same `ended` + `duration` onto the durable rows
    // keyed by the anchor's `urn:waddle:threads:0` thread id so the
    // threads view can surface the ended summary without replaying MAM.
    if let Err(error) = state
        .deps
        .protocol
        .inbox_storage
        .mark_call_thread_ended(room_jid, &active.thread_id, ended, &duration)
        .await
    {
        warn!(
            room = %room_jid,
            %error,
            "failed to persist call-thread ended summary to inbox"
        );
    }
}

fn build_call_thread_ended_message(
    room_jid: &BareJid,
    anchor_origin_id: &str,
    ended: &CallThreadEnded,
) -> Message {
    let apply_to = Element::builder("apply-to", NS_FASTEN)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            anchor_origin_id,
        )
        .append(build_call_thread_ended(ended))
        .build();
    let mut message = Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.from = Some(jid::Jid::from(room_jid.clone()));
    message.type_ = MessageType::Groupchat;
    message.payloads.push(apply_to);
    message.payloads.push(build_hint_element(Hint::Store));
    message
}

fn format_call_thread_duration(duration: chrono::Duration) -> String {
    let seconds = duration.num_seconds().max(0);
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("PT{hours}H{minutes}M{seconds}S")
    } else if minutes > 0 {
        format!("PT{minutes}M{seconds}S")
    } else {
        format!("PT{seconds}S")
    }
}
