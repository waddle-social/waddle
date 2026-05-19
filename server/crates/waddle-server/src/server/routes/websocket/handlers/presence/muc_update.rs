//! In-room MUC presence updates from existing occupants.
//!
//! Distinct from a fresh join: the sender is already an occupant of
//! the room, so per XEP-0045 §5.1.3 / §7.7 the server reflects the
//! presence — preserving the sender's extension payloads (`<show/>`,
//! `<status/>`, and our `<call xmlns='urn:waddle:muc-call:0'/>`
//! extension among them) — to every occupant.
//!
//! The Waddle-specific gain is that the `<call/>` advertisement
//! reaches other occupants in real time: clicking "voice call" in a
//! channel now lights up the "N in call" chip across the room
//! without any second discovery round-trip.
//!
//! Persistence: the room actor snapshots the latest `<call/>` state
//! per occupant in `MucRoom.call_state`. Later joiners pick that up
//! via the join-replay path (`handle_muc_join` appends each
//! occupant's stored `<call/>` extension to the replayed presence),
//! so a user who arrives ten seconds after the call started still
//! sees the chip.
//!
//! Conformance:
//! - XEP-0045 §5.1.3 / §7.7 — room reflects in-room presence
//!   updates to all occupants.
//! - Reflection uses the existing typed
//!   `waddle_xmpp::muc::build_occupant_presence_update` builder
//!   (server replaces `from`, strips server-controlled
//!   `<x xmlns='muc#user'>` / hats / occupant-id, then re-adds them
//!   from the room's trusted occupant table), preserving the
//!   client's `<call/>` payload as a regular extension child.
//! - XEP-0421 — occupant-id is regenerated from the server-known
//!   bare JID, not echoed from the client.

use jid::{BareJid, FullJid};
use tracing::{debug, warn};
use waddle_xmpp::muc::{build_occupant_presence_update, room_actor::UpsertCallPresence};
use waddle_xmpp::xep::xep0421::OccupantIdentity;
use waddle_xmpp::xep::xep_waddle_muc_call::MucCallExtension;
use waddle_xmpp::Stanza;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::presence::Presence;

use super::super::super::{get_room_actor, stanza_to_xml};
use crate::server::routes::websocket::WebSocketState;

/// Pull out the typed `MucCallExtension` from an inbound presence's
/// payloads if it carries one. Returns `None` when the namespace
/// isn't present OR when the payload exists but fails to parse —
/// per the typed-payloads rule a malformed extension is dropped at
/// the boundary rather than coerced to a degraded form.
pub(super) fn extract_call_extension(presence: &Presence) -> Option<MucCallExtension> {
    presence
        .payloads
        .iter()
        .find(|elem: &&Element| {
            elem.name() == "call"
                && elem.ns() == waddle_xmpp::xep::xep_waddle_muc_call::NS_WADDLE_MUC_CALL
        })
        .and_then(|elem| MucCallExtension::try_from(elem).ok())
}

/// Try to broadcast an in-room presence update for an existing
/// occupant. Returns `Some(replies)` when the room actor confirmed
/// the sender is already an occupant — even if there's nothing to
/// echo back to the sender directly, the broadcast still happens
/// out-of-band via the connection registry. Returns `None` when
/// the sender is NOT yet an occupant of this room; the caller
/// falls back to `handle_muc_join` to actually let the user in.
pub(super) async fn try_handle_muc_presence_update(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    incoming: &Presence,
) -> Option<Vec<String>> {
    let actor = get_room_actor(state, room_jid).await?;
    let call_extension = extract_call_extension(incoming);

    // No `<call/>` extension AND the client isn't otherwise updating
    // a payload we propagate — fall through to the join path. Today
    // the only payload we explicitly forward is the call extension;
    // generic `<show/>` / `<status/>` updates still re-run join,
    // which is wasteful but pre-existing behavior. Scoped fix.
    let ext = call_extension?;

    let outcome = match actor
        .ask(UpsertCallPresence {
            sender_jid: sender_jid.clone(),
            extension: ext,
        })
        .await
    {
        Ok(Some(outcome)) => outcome,
        Ok(None) => return None,
        Err(error) => {
            warn!(
                room = %room_jid,
                nick = %nick,
                sender = %sender_jid,
                error = ?error,
                "Failed to upsert MUC call presence; falling back to join path"
            );
            return None;
        }
    };

    debug!(
        room = %room_jid,
        nick = %outcome.update.sender_nick,
        active = outcome.active_extension.is_some(),
        recipients = outcome.update.recipients.len(),
        "Broadcasting MUC call presence update"
    );

    let from_room_jid = room_jid
        .clone()
        .with_resource_str(&outcome.update.sender_nick)
        .unwrap_or_else(|_| sender_jid.clone());
    let real_bare = sender_jid.to_bare();
    let identity = OccupantIdentity {
        bare_jid: &real_bare,
        real_jid: Some(sender_jid),
        secret: &state.deps.occupant_id_secret,
    };

    // Author the canonical presence the server will reflect. Built
    // ONCE here, cloned per recipient for the self vs other status
    // bit. `build_occupant_presence_update` clones the incoming
    // stanza, strips server-trusted payloads (`muc#user`, hats,
    // occupant-id), then re-adds them from the room actor's
    // authoritative table — so a malicious client cannot inject
    // bogus `<x xmlns='muc#user'>` items.
    let mut responses = Vec::new();
    for recipient in &outcome.update.recipients {
        let is_self = recipient == sender_jid;
        let mut presence = build_occupant_presence_update(
            incoming,
            &from_room_jid,
            recipient,
            outcome.update.sender_affiliation,
            outcome.update.sender_role,
            is_self,
            &identity,
        );

        // If the actor cleared the call state (`state='inactive'`),
        // the client's `<call/>` element WAS preserved by
        // `build_occupant_presence_update`'s clone — but the room's
        // authoritative view says no call is active. Strip the
        // extension so the wire reflects the persisted state, not
        // the client's transitional message.
        if outcome.active_extension.is_none() {
            presence.payloads.retain(|payload| {
                !(payload.name() == "call"
                    && payload.ns() == waddle_xmpp::xep::xep_waddle_muc_call::NS_WADDLE_MUC_CALL)
            });
        }

        if is_self {
            // Self-presence goes back over the WebSocket session
            // that sent it (via the `responses` vec); other
            // recipients are dispatched through the cross-session
            // connection registry below.
            responses.push(stanza_to_xml(&Stanza::Presence(presence)));
        } else {
            let stanza = Stanza::Presence(presence);
            let _ = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(recipient, stanza);
        }
    }

    Some(responses)
}
