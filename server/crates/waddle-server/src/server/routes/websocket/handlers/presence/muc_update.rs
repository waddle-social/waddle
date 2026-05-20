//! In-room MUC presence updates from existing occupants.
//!
//! Distinct from a fresh join: the sender is already an occupant of
//! the room, so per XEP-0045 §5.1.3 / §7.7 the server reflects the
//! presence — preserving the sender's extension payloads (`<show/>`,
//! `<status/>`, and the XEP-0272 `<muji xmlns='urn:xmpp:jingle:muji:0'/>`
//! extension among them) — to every occupant.
//!
//! The Waddle-specific gain is that the Muji advertisement reaches
//! other occupants in real time: clicking "voice call" in a channel
//! now lights up the "N in call" chip across the room without any
//! second discovery round-trip.
//!
//! Persistence: the room actor snapshots the latest `<muji/>` state
//! per occupant in `MucRoom.muji_state`. Later joiners pick that up
//! via the join-replay path (`handle_muc_join` appends each
//! occupant's stored `<muji/>` extension to the replayed presence),
//! so a user who arrives ten seconds after the call started still
//! sees the chip.
//!
//! Conformance:
//! - XEP-0045 §5.1.3 / §7.7 — room reflects in-room presence
//!   updates to all occupants.
//! - XEP-0272 §Joining / §Leaving — `<muji>` presence is the join
//!   advertisement; its absence is the leave marker.
//! - Reflection uses the existing typed
//!   `waddle_xmpp::muc::build_occupant_presence_update` builder
//!   (server replaces `from`, strips server-controlled
//!   `<x xmlns='muc#user'>` / hats / occupant-id, then re-adds them
//!   from the room's trusted occupant table), preserving the
//!   client's `<muji/>` payload as a regular extension child.
//! - XEP-0421 — occupant-id is regenerated from the server-known
//!   bare JID, not echoed from the client.

use jid::{BareJid, FullJid};
use tracing::{debug, warn};
use waddle_xmpp::muc::{build_occupant_presence_update, room_actor::UpsertMujiPresence};
use waddle_xmpp::xep::xep0272::{find_muji, Muji, NS_MUJI};
use waddle_xmpp::xep::xep0421::OccupantIdentity;
use waddle_xmpp::Stanza;
use xmpp_parsers::presence::Presence;

use super::super::super::{get_room_actor, stanza_to_xml};
use crate::server::routes::websocket::WebSocketState;

/// Pull out the typed [`Muji`] element from an inbound presence's
/// payloads if it carries one. Returns `None` when the namespace
/// isn't present OR when the payload exists but fails to parse —
/// per the typed-payloads rule a malformed extension is dropped at
/// the boundary rather than coerced to a degraded form.
pub(super) fn extract_muji(presence: &Presence) -> Option<Muji> {
    presence
        .payloads
        .iter()
        .find(|elem| elem.name() == "muji" && elem.ns() == NS_MUJI)
        .and_then(|elem| Muji::try_from(elem).ok())
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
    let muji = extract_muji(incoming);

    // No `<muji/>` extension AND the client isn't otherwise updating
    // a payload we propagate — fall through to the join path. Today
    // the only payload we explicitly forward is the Muji extension;
    // generic `<show/>` / `<status/>` updates still re-run join,
    // which is wasteful but pre-existing behavior. Scoped fix.
    let muji = muji?;

    let outcome = match actor
        .ask(UpsertMujiPresence {
            sender_jid: sender_jid.clone(),
            muji,
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
                "Failed to upsert MUC Muji presence; falling back to join path"
            );
            return None;
        }
    };

    // XEP-0045 §7.7: a user may change their *own* in-room presence
    // only. The resolved nick comes from the room actor's
    // authoritative occupant table (keyed by the authenticated full
    // JID), so any mismatch with the `to=room/<nick>` resource the
    // client supplied is an attempt to impersonate another occupant
    // — drop the update silently rather than reflecting it.
    if outcome.update.sender_nick != nick {
        warn!(
            room = %room_jid,
            to_nick = %nick,
            actual_nick = %outcome.update.sender_nick,
            sender = %sender_jid,
            "MUC Muji presence to-JID nick mismatch; dropping reflection"
        );
        return Some(Vec::new());
    }

    debug!(
        room = %room_jid,
        nick = %outcome.update.sender_nick,
        active = outcome.active_muji.is_some(),
        recipients = outcome.update.recipients.len(),
        "Broadcasting MUC Muji presence update"
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
        // XEP-0045 §7.1: every session sharing the sender's occupant
        // nick must receive a presence stamped with `<status code='110'/>`,
        // not just the exact full JID that emitted the stanza. The
        // occupant_sessions table is keyed by nick and holds every
        // active full JID under that nick, so a same-bare multi-
        // session join (Alice on web AND mobile) needs `is_self` for
        // BOTH recipients when Alice updates her presence from web.
        let is_self = recipient.to_bare() == sender_jid.to_bare();
        let mut presence = build_occupant_presence_update(
            incoming,
            &from_room_jid,
            recipient,
            outcome.update.sender_affiliation,
            outcome.update.sender_role,
            is_self,
            &identity,
        );

        // If the actor cleared the Muji state (participant left the
        // call), the client's `<muji/>` element WAS preserved by
        // `build_occupant_presence_update`'s clone — but the room's
        // authoritative view says no call is active. Strip the
        // extension so the wire reflects the persisted state, not
        // the client's transitional message.
        if outcome.active_muji.is_none() {
            presence
                .payloads
                .retain(|payload| !(payload.name() == "muji" && payload.ns() == NS_MUJI));
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

    // Silence unused-import warning when feature flags trim the path.
    let _ = find_muji;
    Some(responses)
}

#[cfg(test)]
mod tests {
    //! XEP-conformance tests for the `<muji xmlns='urn:xmpp:jingle:muji:0'/>`
    //! presence-extension forwarding path. Per the CLAUDE.md
    //! XEP-custom-test-suite hard rule, every implemented XEP carries
    //! dedicated wire-level tests. These pin the extraction step;
    //! actor-level tests in `muc::room_actor::tests` cover the
    //! room-state side of the same boundary.
    use super::*;
    use minidom::Element;
    use waddle_xmpp::xep::xep0272::NS_MUJI;
    use xmpp_parsers::presence::{Presence as ParsedPresence, Type as PresenceType};

    fn presence_with_muji_contents() -> ParsedPresence {
        let mut presence = ParsedPresence::new(PresenceType::None);
        let xml = "<muji xmlns='urn:xmpp:jingle:muji:0'>\
                     <content creator='initiator' name='audio'>\
                       <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>\
                     </content>\
                   </muji>";
        let muji: Element = xml.parse().expect("muji XML parses");
        presence.payloads.push(muji);
        presence
    }

    fn presence_with_muji_preparing() -> ParsedPresence {
        let mut presence = ParsedPresence::new(PresenceType::None);
        let xml = "<muji xmlns='urn:xmpp:jingle:muji:0'><preparing/></muji>";
        let muji: Element = xml.parse().expect("muji XML parses");
        presence.payloads.push(muji);
        presence
    }

    #[test]
    fn extract_muji_returns_typed_active_advertisement() {
        let presence = presence_with_muji_contents();
        let muji = extract_muji(&presence).expect("active <muji/> parses");
        assert!(muji.is_active());
        assert_eq!(muji.contents.len(), 1);
        assert_eq!(muji.contents[0].name.0, "audio");
    }

    #[test]
    fn extract_muji_returns_preparing_advertisement() {
        let presence = presence_with_muji_preparing();
        let muji = extract_muji(&presence).expect("preparing <muji/> parses");
        assert!(muji.preparing);
        assert!(!muji.is_active());
    }

    #[test]
    fn extract_muji_returns_none_for_missing_payload() {
        // A presence with no `<muji/>` extension. The dispatcher uses
        // this as the signal to fall through to the regular join
        // path — never to silently treat the presence as a call
        // advertisement.
        let presence = ParsedPresence::new(PresenceType::None);
        assert!(extract_muji(&presence).is_none());
    }

    #[test]
    fn extract_muji_returns_none_for_wrong_namespace() {
        // An element named `muji` in a different namespace is NOT
        // the Muji extension — must be ignored.
        let mut presence = ParsedPresence::new(PresenceType::None);
        let muji = Element::builder("muji", "urn:example:other:0").build();
        presence.payloads.push(muji);
        assert!(extract_muji(&presence).is_none());
        let _ = NS_MUJI; // import sanity
    }
}
