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
use waddle_xmpp::muc::{
    build_occupant_presence_update,
    presence::NS_MUC,
    room_actor::{ClearMujiPresence, UpsertInCallState, UpsertMujiPresence},
};
use waddle_xmpp::xep::xep0167::MediaKind;
use waddle_xmpp::xep::xep0272::{find_muji, Muji, NS_MUJI};
use waddle_xmpp::xep::xep0421::OccupantIdentity;
use waddle_xmpp::xep::{
    build_call_thread_anchor, build_hint_element, build_in_call_presence_state_element,
    parse_in_call_presence_state, CallThreadAnchor, CallThreadKind, CallThreadMedia, Hint,
    InCallPresenceState, NS_WADDLE_CALL_THREAD, NS_WADDLE_IN_CALL,
};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::mam::ThreadId;
use waddle_xmpp_core::xep0201::{build_thread_element, ThreadInfo, CLIENT_STANZA_NS};
use waddle_xmpp_core::xep0359::{build_origin_id_element, NS_SID};
use xmpp_parsers::jingle::SessionId;
use xmpp_parsers::message::{Lang, Message, MessageType};
use xmpp_parsers::presence::Presence;

use super::super::super::{get_room_actor, stanza_to_xml};
use crate::server::routes::websocket::{
    interpret_loop::build_interpret_deps, ActiveCallThread, WebSocketState,
};

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

/// Pull the typed [`InCallPresenceState`] out of an inbound presence's
/// `<in-call xmlns='urn:waddle:in-call:0'>` child (#1029), defaulting to
/// the lowered state when the child is absent or fails to parse — a
/// missing/malformed marker is a cleared hand, not an error.
pub(super) fn extract_in_call_presence_state(presence: &Presence) -> InCallPresenceState {
    presence
        .payloads
        .iter()
        .find(|elem| elem.name() == "in-call" && elem.ns() == NS_WADDLE_IN_CALL)
        .and_then(|elem| parse_in_call_presence_state(elem).ok())
        .unwrap_or_default()
}

pub(crate) fn is_muc_join_presence(presence: &Presence) -> bool {
    presence.type_ == xmpp_parsers::presence::Type::None
        && presence
            .payloads
            .iter()
            .any(|elem| elem.name() == "x" && elem.ns() == NS_MUC)
}

#[derive(Debug, Clone, Copy)]
struct ReflectedMujiEntry<'a> {
    owner_jid: &'a FullJid,
    muji: Option<&'a Muji>,
}

fn muji_reflection_rank(muji: Option<&Muji>) -> u8 {
    match muji {
        None => 0,
        Some(muji) if muji.is_empty() => 0,
        Some(muji) if muji.is_active() => 2,
        Some(_) => 1,
    }
}

fn reflected_muji_entries<'a>(
    sender_jid: &'a FullJid,
    sender_muji: Option<&'a Muji>,
    session_mujis: &'a [(FullJid, Muji)],
) -> Vec<ReflectedMujiEntry<'a>> {
    let mut entries = vec![ReflectedMujiEntry {
        owner_jid: sender_jid,
        muji: sender_muji,
    }];
    entries.extend(
        session_mujis
            .iter()
            .filter(|(owner_jid, _)| owner_jid != sender_jid)
            .map(|(owner_jid, muji)| ReflectedMujiEntry {
                owner_jid,
                muji: Some(muji),
            }),
    );
    entries.sort_by_key(|entry| {
        (
            muji_reflection_rank(entry.muji),
            entry.owner_jid.to_string(),
        )
    });
    entries
}

/// Try to broadcast an in-room presence update for an existing
/// occupant. Returns `Some(replies)` when the room actor confirmed
/// the sender is already an occupant — even if there's nothing to
/// echo back to the sender directly, the broadcast still happens
/// out-of-band via the connection registry. Returns `None` when
/// the sender is NOT yet an occupant of this room; the caller
/// falls back to `handle_muc_join` to actually let the user in.
pub(crate) async fn try_handle_muc_presence_update(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    incoming: &Presence,
) -> Option<Vec<String>> {
    if incoming.type_ != xmpp_parsers::presence::Type::None {
        return None;
    }

    let actor = get_room_actor(state, room_jid).await?;
    let muji = extract_muji(incoming);
    // XEP-0045 join/rejoin presence carries `<x xmlns='.../muc'/>`.
    // On stream resume the client may replay that autojoin while the
    // room actor still has the full JID as an occupant. Treating that
    // as a Muji clear would suppress the join replay that refreshes
    // active-call state; XEP-0272 leave markers are plain available
    // presence without this MUC join payload.
    if muji.is_none() && is_muc_join_presence(incoming) {
        return None;
    }

    // XEP-0045 §7.6 (#1252): resolve the sender's authoritative nick
    // BEFORE any state mutation. An in-room presence addressed to
    // `room/<other-nick>` is a nickname-change request; Waddle locks
    // nicknames to identity, so the service MUST deny it with
    // `<not-acceptable/>` — and must do so without first tearing down
    // the sender's Muji/SFU call state (previously this path cleared
    // the call state and then dropped the stanza silently).
    let Ok(context) = actor
        .ask(waddle_xmpp::muc::room_actor::GetAdminContext {
            sender_jid: sender_jid.clone(),
        })
        .await
    else {
        return None;
    };
    let current_nick = context.nick?;
    if current_nick != nick {
        warn!(
            room = %room_jid,
            requested_nick = %nick,
            current_nick = %current_nick,
            sender = %sender_jid,
            "MUC nick change denied (nicknames locked); no call state torn down"
        );
        return Some(vec![super::muc::build_muc_presence_error_xml(
            room_jid,
            nick,
            sender_jid,
            xmpp_parsers::stanza_error::StanzaError::new(
                xmpp_parsers::stanza_error::ErrorType::Cancel,
                xmpp_parsers::stanza_error::DefinedCondition::NotAcceptable,
                "en",
                "Nickname changes are not allowed in this room.",
            ),
        )]);
    }

    let clears_muji_presence = muji.as_ref().is_none_or(Muji::is_empty);
    let outcome = match muji {
        Some(muji) => match actor
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
        },
        None => match actor
            .ask(ClearMujiPresence {
                sender_jid: sender_jid.clone(),
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
                    "Failed to clear MUC Muji presence; falling back to join path"
                );
                return None;
            }
        },
    };
    if clears_muji_presence {
        // XEP-0272 §Leaving is the absence of `<muji/>` in in-room
        // presence. Mirror that XMPP-native leave marker to the SFU
        // registry just like full MUC leave / unclean disconnect do,
        // otherwise a hard-refreshed tab can clear the room indicator
        // while its LiveKit participant and issued token JTIs linger.
        super::super::super::muc_call_sfu::unregister_participant_from_room(
            state, room_jid, sender_jid,
        );
    }

    // XEP-0045 §7.7: a user may change their *own* in-room presence
    // only. The nick pre-check above already rejected any presence
    // addressed to a nick other than the sender's authoritative one
    // (with `<not-acceptable/>`, per §7.6), so by this point the
    // resolved nick always matches `to=room/<nick>`.
    debug!(
        room = %room_jid,
        nick = %outcome.update.sender_nick,
        active = outcome.active_muji.is_some(),
        session_mujis = outcome.session_mujis.len(),
        recipients = outcome.update.recipients.len(),
        "Broadcasting MUC Muji presence update"
    );

    // Apply the in-call presence state (#1029 raised hand / #1030 mute)
    // carried on the same stanza. Tracked independently of muji so each
    // stays single-purpose; the post-update per-session states decorate
    // the reflected presences below with the room-authoritative
    // `<in-call>` child rather than echoing the client payload (which
    // would misattribute a sibling session's hand/mute). The sender was
    // already confirmed as an occupant via the muji upsert above, so a
    // `None`/error here just means no in-call state to reflect.
    // Enforce the invariant "in-call state <-> active call participant"
    // server-side: when this stanza is the XEP-0272 leave marker
    // (`clears_muji_presence`), the occupant is no longer in the call, so
    // the hand MUST be lowered and the mute dropped regardless of any
    // `<in-call>` a buggy or hostile client left on the stanza. Only an
    // in-call presence (muji active) may carry in-call sub-states.
    let in_call_state = if clears_muji_presence {
        InCallPresenceState::default()
    } else {
        extract_in_call_presence_state(incoming)
    };
    let in_call_sessions = match actor
        .ask(UpsertInCallState {
            sender_jid: sender_jid.clone(),
            state: in_call_state,
        })
        .await
    {
        Ok(Some(in_call_outcome)) => in_call_outcome.in_call_sessions,
        Ok(None) | Err(_) => Vec::new(),
    };

    let from_room_jid = room_jid
        .clone()
        .with_resource_str(&outcome.update.sender_nick)
        .unwrap_or_else(|_| sender_jid.clone());
    let reflected_entries = reflected_muji_entries(
        sender_jid,
        outcome.sender_muji.as_ref(),
        &outcome.session_mujis,
    );
    // Gate the entire call-thread lifecycle on a configured SFU. The
    // `active_call_started` flag comes from client-driven Muji presence,
    // not the SFU; without this gate a no-SFU deployment would build,
    // broadcast, and persist a call-thread anchor whose end path
    // (`maybe_broadcast_call_thread_ended` / the `call_threads` registry)
    // is itself SFU-gated — leaving a permanent call-thread row and a
    // false Join affordance that can never be ended. Per #918,
    // call-thread tracking is disabled when no SFU bridge is configured.
    let call_thread_anchor = if outcome.active_call_started && state.deps.protocol.sfu.is_some() {
        build_call_thread_anchor_message(
            room_jid,
            &sender_jid.to_bare(),
            outcome.active_muji.as_ref(),
            &outcome.update.sender_nick,
        )
    } else {
        None
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
        for entry in &reflected_entries {
            // XEP-0045 §7.1: sessions sharing the owner bare JID
            // receive status-110 for that occupant presence. Muji
            // preparing state remains resource-owned, so reflections
            // use the exact session JID that owns each `<muji/>`.
            let owner_bare = entry.owner_jid.to_bare();
            let identity = OccupantIdentity {
                bare_jid: &owner_bare,
                real_jid: Some(entry.owner_jid),
                secret: &state.deps.occupant_id_secret,
            };
            let is_self = recipient.to_bare() == owner_bare;
            let mut presence = build_occupant_presence_update(
                incoming,
                &from_room_jid,
                recipient,
                outcome.update.sender_affiliation,
                outcome.update.sender_role,
                waddle_xmpp::muc::MucPresenceStatus::new(is_self, false),
                &identity,
            );

            // Replace the client-authored `<muji/>` with the room
            // actor's authoritative per-session state. A `None` entry
            // is still the sender's exact clear marker; sibling Muji
            // entries then preserve any remaining preparing/active
            // state under their real full JID.
            presence
                .payloads
                .retain(|payload| !(payload.name() == "muji" && payload.ns() == NS_MUJI));
            if let Some(muji) = entry.muji {
                presence.payloads.push(muji.to_element());
            }

            // Replace any client-authored `<in-call>` with the room
            // actor's authoritative per-session state (raised hand / mute),
            // so a sibling resource's presence is never stamped with
            // another session's hand/mute (#1029/#1030). The element sits
            // alongside `<muji>`.
            presence.payloads.retain(|payload| {
                !(payload.name() == "in-call" && payload.ns() == NS_WADDLE_IN_CALL)
            });
            if let Some((_, owner_state)) = in_call_sessions
                .iter()
                .find(|(jid, _)| jid == entry.owner_jid)
            {
                presence
                    .payloads
                    .push(build_in_call_presence_state_element(owner_state));
            }

            if recipient == sender_jid {
                responses.push(stanza_to_xml(&Stanza::Presence(presence)));
            } else {
                let stanza = Stanza::Presence(presence);
                super::muc::route_room_presence_to_occupant(state, room_jid, recipient, stanza)
                    .await;
            }
        }
    }

    if clears_muji_presence {
        let outcome = crate::server::routes::muc_muji_clear::maybe_broadcast_call_thread_ended(
            state, room_jid,
        )
        .await;
        // Unlike the webhook ingress, this ordinary MUC presence path has
        // no LiveKit redelivery behind it: a retryable completion failure
        // (transient inbox-storage or broadcast error) would otherwise be
        // dropped here, silently suppressing the ended broadcast and
        // leaving the active call-thread entry in memory (#1612 review).
        // Hand the retry to the durable outbox via the COMPLETION-ONLY
        // target — the presence clear itself already succeeded, and
        // replaying it could clobber a quick rejoin's advertisement.
        if matches!(
            outcome,
            crate::server::routes::muc_muji_clear::WebhookEffectOutcome::Retryable(_)
        ) {
            // A retryable failure leaves the active entry in place, so
            // the FAILED thread's identity is still readable here; the
            // durable retry is fenced to it so a newer call replacing
            // the entry before the row drains cannot be clobbered
            // (#1612 review round 10).
            let failed_thread = state
                .deps
                .protocol
                .call_threads
                .get(room_jid)
                .and_then(|active| {
                    Some((
                        ThreadId::new(active.thread_id.clone())?,
                        waddle_xmpp_core::xep0359::OriginId::new(active.anchor_origin_id.clone()),
                        active.started,
                    ))
                });
            if let Some((failed_thread, anchor_origin_id, started)) = failed_thread {
                if let Err(error) =
                    crate::server::routes::muc_muji_clear::enqueue_call_thread_end_retry(
                        state,
                        room_jid,
                        failed_thread,
                        anchor_origin_id,
                        started,
                    )
                    .await
                {
                    tracing::warn!(
                        room = %room_jid,
                        %error,
                        "call-thread completion retry could not be enqueued; the ended \
                         broadcast is deferred to the room-sweep reconcile backstop"
                    );
                }
            }
        }
    }

    if let Some(CallThreadAnchorMessage {
        message: anchor,
        thread_id,
    }) = call_thread_anchor
    {
        let marker = anchor
            .payloads
            .iter()
            .find(|payload| {
                payload.name() == "call-thread" && payload.ns() == NS_WADDLE_CALL_THREAD
            })
            .and_then(|payload| waddle_xmpp::xep::parse_call_thread_anchor(payload).ok());
        let anchor_origin_id = anchor
            .payloads
            .iter()
            .find(|payload| payload.name() == "origin-id" && payload.ns() == NS_SID)
            .and_then(|payload| payload.attr("id"))
            .map(str::to_owned);
        let deps = build_interpret_deps(state, None);
        let _ = crate::server::routes::interpret::broadcast_room_system_message(
            &deps,
            room_jid.clone(),
            Box::new(anchor),
        )
        .await;
        // The anchor only exists when an SFU is configured (gated at the
        // build site above), so no inner SFU check is needed here.
        if let Some((anchor_origin_id, marker)) = anchor_origin_id.zip(marker) {
            state.deps.protocol.call_threads.insert(
                room_jid.clone(),
                ActiveCallThread {
                    anchor_origin_id,
                    initiator: marker.initiator,
                    media: marker.media,
                    started: marker.started,
                    thread_id: thread_id.as_str().to_owned(),
                },
            );
        }
    }

    // Silence unused-import warning when feature flags trim the path.
    let _ = find_muji;
    Some(responses)
}

/// A freshly-built call-thread anchor system message together with the
/// `urn:waddle:threads:0` thread id it was assigned. The thread id is
/// surfaced so the caller can register it in
/// [`ActiveCallThread`](crate::server::routes::websocket::ActiveCallThread),
/// correlating the later `<call-thread-ended/>` fastening back to the
/// inbox/threads rows for this exact thread.
struct CallThreadAnchorMessage {
    message: Message,
    thread_id: ThreadId,
}

/// Build the call-thread anchor system message. Returns `None` only in
/// the impossible case that a freshly generated UUID is empty — handled
/// without panicking so the no-`expect` hard rule holds; the caller then
/// simply skips anchoring this call.
fn build_call_thread_anchor_message(
    room_jid: &BareJid,
    initiator: &BareJid,
    active_muji: Option<&Muji>,
    initiator_nick: &str,
) -> Option<CallThreadAnchorMessage> {
    let thread_id = ThreadId::new(uuid::Uuid::new_v4().to_string())?;
    let sid = SessionId(uuid::Uuid::new_v4().to_string());
    let media = media_from_muji(active_muji);
    let body = format!("{initiator_nick} started a call");
    let mut message = Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.from = Some(jid::Jid::from(room_jid.clone()));
    message.type_ = MessageType::Groupchat;
    message.bodies.insert(Lang(String::new()), body);
    let thread = ThreadInfo::root(thread_id.clone());
    message
        .payloads
        .push(build_thread_element(&thread, CLIENT_STANZA_NS));
    message
        .payloads
        .push(build_origin_id_element(&uuid::Uuid::new_v4().to_string()));
    message
        .payloads
        .push(build_call_thread_anchor(&CallThreadAnchor {
            kind: CallThreadKind::Muc,
            sid,
            media,
            initiator: initiator.clone(),
            started: chrono::Utc::now(),
        }));
    message.payloads.push(build_hint_element(Hint::Store));
    Some(CallThreadAnchorMessage { message, thread_id })
}

fn media_from_muji(active_muji: Option<&Muji>) -> CallThreadMedia {
    let mut media = CallThreadMedia {
        audio: false,
        video: false,
    };
    if let Some(muji) = active_muji {
        for content in &muji.contents {
            match content.media {
                MediaKind::Audio => media.audio = true,
                MediaKind::Video => media.video = true,
            }
        }
    }
    if !media.audio && !media.video {
        media.audio = true;
    }
    media
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
    fn media_from_muji_uses_typed_rtp_media_not_content_name() {
        let xml = "<muji xmlns='urn:xmpp:jingle:muji:0'>\
                     <content creator='initiator' name='0'>\
                       <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='video'/>\
                     </content>\
                   </muji>";
        let element: Element = xml.parse().expect("muji XML parses");
        let muji = Muji::try_from(&element).expect("typed video Muji parses");
        let media = media_from_muji(Some(&muji));

        assert!(!media.audio);
        assert!(media.video);
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
        // A presence with no `<muji/>` extension. The update path may
        // use this as a clear marker, but never as a call advertisement.
        let presence = ParsedPresence::new(PresenceType::None);
        assert!(extract_muji(&presence).is_none());
    }

    #[test]
    fn is_muc_join_presence_detects_xep0045_join_payload() {
        let mut presence = ParsedPresence::new(PresenceType::None);
        presence
            .payloads
            .push(Element::builder("x", NS_MUC).build());

        assert!(is_muc_join_presence(&presence));
    }

    #[test]
    fn is_muc_join_presence_ignores_plain_muji_clear_presence() {
        let presence = ParsedPresence::new(PresenceType::None);

        assert!(!is_muc_join_presence(&presence));
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

    #[test]
    fn reflected_muji_entries_preserve_sibling_preparing_after_sender_clear() {
        let sender: FullJid = "alice@example.com/mobile".parse().expect("sender");
        let sibling: FullJid = "alice@example.com/desktop".parse().expect("sibling");
        let session_mujis = vec![(sibling.clone(), Muji::preparing())];
        let entries = reflected_muji_entries(&sender, None, &session_mujis);

        assert_eq!(entries.len(), 2);
        assert!(
            entries[0].owner_jid == &sender && entries[0].muji.is_none(),
            "the sender's exact clear is emitted first"
        );
        assert!(
            entries[1].owner_jid == &sibling
                && entries[1]
                    .muji
                    .is_some_and(|muji| muji.preparing && !muji.is_active()),
            "remaining sibling preparing state is preserved with its exact owner"
        );
    }

    #[test]
    fn reflected_muji_entries_preserve_sender_and_sibling_state() {
        let sender: FullJid = "alice@example.com/mobile".parse().expect("sender");
        let sibling: FullJid = "alice@example.com/desktop".parse().expect("sibling");
        let preparing = Muji::preparing();
        let active = extract_muji(&presence_with_muji_contents()).expect("active fixture parses");
        let session_mujis = vec![(sender.clone(), preparing.clone()), (sibling, active)];
        let entries = reflected_muji_entries(&sender, Some(&preparing), &session_mujis);

        assert_eq!(entries.len(), 2);
        assert!(entries[0].muji.is_some_and(|muji| muji.preparing));
        assert!(entries[1].muji.is_some_and(Muji::is_active));
    }

    #[test]
    fn reflected_muji_entries_emit_active_state_last_for_legacy_clients() {
        let sender: FullJid = "alice@example.com/web".parse().expect("sender");
        let sibling: FullJid = "alice@example.com/zphone".parse().expect("sibling");
        let active = extract_muji(&presence_with_muji_contents()).expect("active fixture parses");
        let preparing = Muji::preparing();
        let session_mujis = vec![(sender.clone(), active.clone()), (sibling, preparing)];
        let entries = reflected_muji_entries(&sender, Some(&active), &session_mujis);

        assert_eq!(entries.len(), 2);
        assert!(
            entries.last().is_some_and(|entry| entry.muji.is_some_and(Muji::is_active)),
            "the final same-nick stanza must carry active Muji so non-occupant-id clients do not settle on preparing"
        );
    }
}
