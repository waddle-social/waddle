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
//! 3. Routes a copy to every joined occupant (per-resource fan-out). In a
//!    room an enrolled host-grant scope manages, only to the occupants
//!    whose canonical grants allow reading the room, decided again after
//!    the archive write and asked again before each copy is routed.
//!
//! Bypasses the occupancy gate, rich-target validation, and extension
//! enrichment — the sender is the room itself.

use super::groupchat_archive::RoomArchiveFence;
use super::{
    archive_groupchat_message, debug, effects, resolve_room_claim_fence, route_to_connection, warn,
    ArchiveGroupchatOutcome, BareJid, Deps, GetRoom, GetRoomSnapshot, Jid, Message, Stanza,
};
use crate::ingress::RoomManifestSnapshot;
use waddle_xmpp::muc::room_actor::RoomChainOccupant;
use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId};

pub(super) async fn broadcast_room_system_message_event(
    deps: &Deps<'_>,
    room: BareJid,
    message: Box<Message>,
    recursion_depth: u8,
) -> Option<String> {
    broadcast_room_system_message_with_identity(deps, room, message, recursion_depth, None).await
}

pub(super) async fn broadcast_room_system_message_with_identity(
    deps: &Deps<'_>,
    room: BareJid,
    mut message: Box<Message>,
    recursion_depth: u8,
    identity: Option<StanzaId>,
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
        .reply_timeout(std::time::Duration::from_secs(5))
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
            deps.effects
                .fail_plan(effects::PlanFailure::RoomSnapshotUnavailable);
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
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    {
        Ok(snap) => snap,
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "BroadcastRoomSystemMessage: GetRoomSnapshot failed; dropping"
            );
            deps.effects
                .fail_plan(effects::PlanFailure::RoomSnapshotUnavailable);
            return None;
        }
    };

    // A room an enrolled host-grant scope manages archives its own message
    // like any other, but delivers it only to occupants whose canonical
    // grants allow reading the room: the actor's roster may still seat an
    // occupant whose grant was revoked. When that cannot be decided nothing
    // is archived or sent, and the caller's own retry applies.
    if system_message_audience(deps, &room, &snapshot.occupants)
        .await
        .is_none()
    {
        deps.effects
            .fail_plan(effects::PlanFailure::RoomSnapshotUnavailable);
        return None;
    }

    // Stamp a canonical XEP-0359 `<stanza-id by='room'/>` so the
    // message is uniquely addressable in MAM and from clients.
    let stanza_id = match identity {
        Some(identity) if identity.by == *room => identity.id,
        Some(_) => {
            deps.effects
                .fail_plan(effects::PlanFailure::InvalidSystemMessage);
            return None;
        }
        None => uuid::Uuid::new_v4().to_string(),
    };
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
        #[cfg(feature = "clustering")]
        if deps.effects.is_planning() {
            if let RoomArchiveFence::Guarded { context, .. } = &fence {
                if snapshot.claim_fence.as_ref() != Some(context) {
                    return None;
                }
            }
        }

        if deps.effects.is_planning()
            && matches!(
                deps.effects.room_execution(),
                super::effects::RoomExecutionPath::None
            )
        {
            let requirement = match &fence {
                RoomArchiveFence::Unfenced => super::effects::room::RoomFenceRequirement::Unfenced,
                #[cfg(feature = "clustering")]
                RoomArchiveFence::Guarded { context, .. } => {
                    super::effects::room::RoomFenceRequirement::Guarded(context.clone())
                }
                #[cfg(feature = "clustering")]
                RoomArchiveFence::OwnershipLost => return None,
            };
            deps.effects
                .set_room_execution(super::effects::RoomExecutionPath::Local {
                    room: room.clone(),
                    client_origin: None,
                    fence: requirement,
                    snapshot_generation: snapshot.admission_revision,
                    manifest_snapshot: Some(Box::new(RoomManifestSnapshot::from(&snapshot))),
                });
        }

        // Room-authored system messages have no occupant sender, so
        // there is no real-JID `<x xmlns='muc#user'/>` to disclose.
        let outcome = if deps.effects.is_planning() {
            super::groupchat_archive::archive_groupchat_message_with_effects(
                Some(deps),
                mam_storage,
                &room,
                &message,
                0,
                &fence,
                None,
            )
            .await
        } else {
            archive_groupchat_message(mam_storage, &room, &message, 0, &fence, None).await
        };
        match outcome {
            ArchiveGroupchatOutcome::Stored(result) => {
                let sequence = deps.ingress_effect_capture.as_ref().map_or(0, |capture| {
                    system_archive_sequence(&capture.snapshot().intents, &room)
                });
                deps.capture_intent(
                    waddle_xmpp::ingress::IngressEffectIntent::SystemMessageArchive {
                        sequence,
                        archive: room.clone(),
                        stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
                            result.stored_id.clone(),
                            jid::Jid::from(room.clone()),
                        ),
                        by: room.clone(),
                        archived_at: result.archived_at,
                        ordinal: None,
                    },
                );
                debug!(
                    room = %room,
                    stanza_id = %result.stored_id,
                    "BroadcastRoomSystemMessage: archived"
                );
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

    // Match `serialize_groupchat_stanza_xml`: the archive freezes a source
    // without a destination, then each live/rebuilt occupant copy sets its own.
    let mut frozen_message = (*message).clone();
    frozen_message.to = None;
    let system_message = match waddle_xmpp::ingress::StoredMessagePayload::try_new(frozen_message) {
        Ok(payload) => payload,
        Err(error) => {
            warn!(room = %room, ?error, "could not freeze room system message");
            deps.effects
                .fail_plan(effects::PlanFailure::InvalidSystemMessage);
            return None;
        }
    };

    // The audience was decided before the archive write. A revoke
    // acknowledged meanwhile must keep its occupant from receiving the
    // message, so the audience is decided again for the delivery, and each
    // occupant is asked again before its copy is routed.
    crate::permissions::host_grant::reach_checkpoint(
        crate::permissions::host_grant::DecisionCheckpoint::SystemMessageArchived,
        &room,
    )
    .await;
    let mut audience = delivery_audience(deps, &room, &snapshot.occupants).await;

    // Fan out to every joined occupant. One `RouteToConnection`
    // per occupant full JID with the message's `to` set to that
    // occupant's full JID, matching `ReflectorHandler`'s
    // per-recipient personalization. Without this, downstream
    // recipient-pass logic (incoming-blocking, archive, inbox) sees
    // a stanza addressed to the room bare JID and may misroute.
    let mut occupants = Vec::with_capacity(audience.occupants.len());
    for occupant in std::mem::take(&mut audience.occupants) {
        if !audience.still_reads(deps, &room, occupant).await {
            continue;
        }
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
        occupants.push(occupant);
    }
    if !occupants.is_empty() {
        let room_generation = snapshot
            .claim_fence
            .as_ref()
            .and_then(|fence| u64::try_from(fence.epoch.0).ok())
            .map(waddle_xmpp::ingress::EntityGeneration::from_storage)
            .unwrap_or(waddle_xmpp::ingress::EntityGeneration::INITIAL);
        deps.capture_intent(
            waddle_xmpp::ingress::IngressEffectIntent::RouteMucSystemBroadcast {
                room: room.clone(),
                occupants: occupants
                    .iter()
                    .map(|occupant| occupant.full_jid.clone())
                    .collect(),
                room_generation,
                system_message: Some(system_message),
                route_identity: waddle_xmpp::ingress::EffectMessageIdentity::stanza(StanzaId::new(
                    stanza_id.clone(),
                    Jid::from(room.clone()),
                )),
            },
        );
    }
    Some(stanza_id)
}

/// The occupants a room's own message is delivered to, and the decision
/// that named them in a managed room.
#[derive(Default)]
struct SystemMessageAudience<'o> {
    occupants: Vec<&'o RoomChainOccupant>,
    /// The managed room's decision, `None` for a room no scope manages.
    decision: Option<crate::permissions::host_grant::ManagedRoomGrants>,
}

impl SystemMessageAudience<'_> {
    /// Whether `occupant` still reads the room as its copy is about to be
    /// routed: always in a room no scope manages; in a managed room, by the
    /// audience's decision while its ticket is current, otherwise by a
    /// decision read now, which then serves the remaining occupants.
    async fn still_reads(
        &mut self,
        deps: &Deps<'_>,
        room: &BareJid,
        occupant: &RoomChainOccupant,
    ) -> bool {
        use crate::permissions::host_grant::{HostGrantAction, RoomBinding, RoomGate};
        let Some(decision) = &self.decision else {
            return true;
        };
        if !decision.fence.is_current() {
            let Some(state) = deps.web_socket_state else {
                return false;
            };
            match state
                .deps
                .app_state
                .host_grants
                .room(RoomBinding::Room(room))
                .await
            {
                RoomGate::Managed(now) => self.decision = Some(now),
                RoomGate::Unmanaged | RoomGate::Unavailable => return false,
            }
        }
        self.decision.as_ref().is_some_and(|decision| {
            decision.allows(&occupant.full_jid.to_bare(), HostGrantAction::Read)
        })
    }
}

/// At most one warning a minute that a room's own message, already
/// archived, was delivered to no occupant because its audience could not be
/// decided again; only the kind of the failure is logged.
static UNDELIVERED: crate::permissions::host_grant::RateLimitedWarning =
    crate::permissions::host_grant::RateLimitedWarning::new();

/// How many times the audience of an archived room message is read before
/// it is delivered to no one.
const DELIVERY_AUDIENCE_READS: usize = 2;

/// The audience of a room's own message after its archive write. The message
/// is already archived, so a read that cannot decide is made once more; when
/// neither decides, the message is delivered to no occupant and a warning
/// naming only the kind of the failure is logged at most once a minute.
async fn delivery_audience<'o>(
    deps: &Deps<'_>,
    room: &BareJid,
    occupants: &'o [RoomChainOccupant],
) -> SystemMessageAudience<'o> {
    for _ in 0..DELIVERY_AUDIENCE_READS {
        if let Some(audience) = system_message_audience(deps, room, occupants).await {
            return audience;
        }
    }
    if UNDELIVERED.due() {
        warn!(
            kind = "audience_undecided",
            "an archived room system message was delivered to no occupant: \
             the room's grants could not be read again after the archive write"
        );
    }
    SystemMessageAudience::default()
}

/// The occupants a room's own message is delivered to: every seated
/// occupant of a room no enrolled scope manages; in a managed room, those
/// whose canonical grants allow `read`. `None` when a managed room's grants
/// cannot be read, and when the interpreter holds no WebSocket state, whose
/// host-grant gate is the only way to tell whether a scope manages the room.
async fn system_message_audience<'o>(
    deps: &Deps<'_>,
    room: &BareJid,
    occupants: &'o [RoomChainOccupant],
) -> Option<SystemMessageAudience<'o>> {
    use crate::permissions::host_grant::{HostGrantAction, RoomBinding, RoomGate};
    let state = deps.web_socket_state?;
    match state
        .deps
        .app_state
        .host_grants
        .room(RoomBinding::Room(room))
        .await
    {
        RoomGate::Unmanaged => Some(SystemMessageAudience {
            occupants: occupants.iter().collect(),
            decision: None,
        }),
        RoomGate::Managed(grants) => Some(SystemMessageAudience {
            occupants: occupants
                .iter()
                .filter(|occupant| {
                    grants.allows(&occupant.full_jid.to_bare(), HostGrantAction::Read)
                })
                .collect(),
            decision: Some(grants),
        }),
        RoomGate::Unavailable => None,
    }
}

fn system_archive_sequence(
    intents: &[waddle_xmpp::ingress::IngressEffectIntent],
    room: &BareJid,
) -> u32 {
    let count = intents
        .iter()
        .filter(|intent| {
            matches!(intent,
                waddle_xmpp::ingress::IngressEffectIntent::SystemMessageArchive { archive, .. }
                    if archive == room
            )
        })
        .count();
    u32::try_from(count).expect("ingress capture has a bounded intent count")
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    #[test]
    fn generated_archive_sequences_are_distinct_and_room_scoped() {
        let room: BareJid = "room@conference.example.com".parse().expect("room");
        let other: BareJid = "other@conference.example.com".parse().expect("other room");
        let mut intents = Vec::new();
        for expected in 0..3 {
            let sequence = system_archive_sequence(&intents, &room);
            assert_eq!(sequence, expected);
            intents.push(
                waddle_xmpp::ingress::IngressEffectIntent::SystemMessageArchive {
                    sequence,
                    archive: room.clone(),
                    by: room.clone(),
                    stanza_id: StanzaId::new(uuid::Uuid::new_v4().to_string(), room.clone().into()),
                    archived_at: chrono::Utc::now(),
                    ordinal: None,
                },
            );
        }
        assert_eq!(system_archive_sequence(&intents, &other), 0);
    }
}
