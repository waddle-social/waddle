//! Interpreter arm for [`OutboundEvent::BroadcastRoomSystemMessage`] (#414).
//!
//! Server-authored groupchat messages originating from the room bare
//! JID itself: pin/unpin events, future room-level notifications. The
//! arm:
//!
//! 1. Stamps a fresh XEP-0359 `<stanza-id by='room'/>` so the message
//!    can be referenced by clients (jump-to-from-pin-list).
//! 2. Persists the message to MAM via the regular groupchat archive
//!    helper.
//! 3. Routes a copy to every joined occupant (per-resource fan-out).
//!
//! Bypasses the occupancy gate, rich-target validation, and extension
//! enrichment — the sender is the room itself.

use super::*;
use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId};

pub(super) async fn broadcast_room_system_message_event(
    deps: &Deps<'_>,
    room: BareJid,
    mut message: Box<Message>,
    recursion_depth: u8,
) -> Option<String> {
    let Some(room_registry) = deps.room_registry else {
        debug!(
            room = %room,
            "BroadcastRoomSystemMessage: no room_registry in Deps; skipping"
        );
        return None;
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
                "BroadcastRoomSystemMessage: room not registered; dropping"
            );
            return None;
        }
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "BroadcastRoomSystemMessage: room registry lookup failed; dropping"
            );
            return None;
        }
    };

    // The system message has `from = room@conf` (bare). The room
    // snapshot query is keyed by a full JID for the sender-occupancy
    // calculation; we don't need that here, so we synthesize a sender
    // full JID purely for the snapshot RPC. The snapshot's occupant
    // list is independent of the sender argument.
    let synthetic_sender = match room.clone().with_resource_str("__system__") {
        Ok(s) => s,
        Err(error) => {
            warn!(
                room = %room,
                ?error,
                "BroadcastRoomSystemMessage: failed to build synthetic sender; dropping"
            );
            return None;
        }
    };
    let snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: synthetic_sender.clone(),
        })
        .await
    {
        Ok(snap) => snap,
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "BroadcastRoomSystemMessage: GetRoomSnapshot failed; dropping"
            );
            return None;
        }
    };

    // Stamp a canonical XEP-0359 `<stanza-id by='room'/>` so the
    // message is uniquely addressable in MAM and from clients.
    let stanza_id = uuid::Uuid::new_v4().to_string();
    add_stanza_id(
        &mut message,
        &StanzaId::new(stanza_id.clone(), Jid::from(room.clone())),
    );

    // Archive in MAM. We use `0` for `sender_nickname_generation` —
    // the field is a XEP-0308 LMC-correction window guard for
    // user-authored messages; system messages are never corrected.
    //
    // ADR-0017 Phase 3 Slice 7 FIX 1: this is a groupchat archive write
    // like any other, so it is fenced the same way (`resolve_room_claim_fence`
    // reads the identical typed context `dispatch_to_room`'s pre-fan-out
    // check uses). On ownership loss, skip the fan-out below entirely —
    // this function's fan-out loop is the ONLY fan-out for this message
    // (there is no separate batch to suppress, unlike the interpreter's
    // `ArchiveGroupchat` event arm), so an early return here is the exact
    // "not archived, not fanned out" contract.
    if let Some(mam_storage) = deps.mam_storage {
        let fence = resolve_room_claim_fence(deps, &room).await;
        // Room-authored system messages have no occupant sender, so
        // there is no real-JID `<x xmlns='muc#user'/>` to disclose.
        match archive_groupchat_message(mam_storage, &room, &message, 0, &fence, None).await {
            ArchiveGroupchatOutcome::Stored(result) => {
                deps.capture_intent(
                    waddle_xmpp::ingress::IngressEffectIntent::ArchiveAuthoritative {
                        archive: room.clone(),
                        stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
                            result.stored_id.clone(),
                            jid::Jid::from(room.clone()),
                        ),
                        by: room.clone(),
                    },
                );
                debug!(
                    room = %room,
                    stanza_id = %result.stored_id,
                    "BroadcastRoomSystemMessage: archived"
                );
            }
            ArchiveGroupchatOutcome::Deduplicated(result) => {
                debug!(
                    room = %room,
                    stanza_id = %result.stored_id,
                    "BroadcastRoomSystemMessage: archive write deduplicated; dropping"
                );
                return None;
            }
            ArchiveGroupchatOutcome::TombstoneHit => {
                debug!(
                    room = %room,
                    "BroadcastRoomSystemMessage: archive write matched a tombstone; dropping"
                );
                return None;
            }
            ArchiveGroupchatOutcome::Skipped => debug!(
                room = %room,
                "BroadcastRoomSystemMessage: archive helper declined (chain bug?)"
            ),
            ArchiveGroupchatOutcome::OwnershipLost => {
                warn!(
                    room = %room,
                    "BroadcastRoomSystemMessage: fenced archive write observed ownership \
                     loss; dropping system message entirely (not archived, not fanned out)"
                );
                return None;
            }
        }
    }

    // Fan out to every joined occupant. One `RouteToConnection`
    // per occupant full JID with the message's `to` set to that
    // occupant's full JID, matching `ReflectorHandler`'s
    // per-recipient personalization. Without this, downstream
    // recipient-pass logic (incoming-blocking, archive, inbox) sees
    // a stanza addressed to the room bare JID and may misroute.
    for occupant in &snapshot.occupants {
        let mut copy = (*message).clone();
        copy.to = Some(Jid::from(occupant.full_jid.clone()));
        route_to_connection(
            deps,
            Jid::from(occupant.full_jid.clone()),
            Box::new(Stanza::Message(copy)),
            recursion_depth,
            None,
        )
        .await;
    }
    if !snapshot.occupants.is_empty() {
        let room_generation = snapshot
            .claim_fence
            .as_ref()
            .and_then(|fence| u64::try_from(fence.epoch.0).ok())
            .map(waddle_xmpp::ingress::EntityGeneration::from_storage)
            .unwrap_or(waddle_xmpp::ingress::EntityGeneration::INITIAL);
        deps.capture_intent(
            waddle_xmpp::ingress::IngressEffectIntent::RouteMucGroupchat {
                room: room.clone(),
                occupants: snapshot
                    .occupants
                    .iter()
                    .map(|occupant| occupant.full_jid.clone())
                    .collect(),
                reflection: synthetic_sender,
                room_generation,
                route_identity: waddle_xmpp::ingress::EffectMessageIdentity::stanza(StanzaId::new(
                    stanza_id.clone(),
                    Jid::from(room.clone()),
                )),
            },
        );
    }
    Some(stanza_id)
}
