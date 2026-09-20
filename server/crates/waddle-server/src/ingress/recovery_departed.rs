//! XEP-0045 ghost users: settle frozen copies the room no longer owes.
//!
//! A `route_muc` obligation freezes its occupant full JIDs at acceptance. An
//! occupant that has since left the room — or whose session died as a ghost —
//! can never take its copy, so the single aggregate receipt never lands and
//! maintenance retries the row forever (#1803). XEP-0045 states the rule
//! exactly ("Ghost Users" and §7.14): an entity that is no longer an occupant
//! is owed no groupchat message.
//!
//! Only maintenance recovery settles these copies, and only against an authoritative local
//! room incarnation. A missing registry, a missing local actor, or a probe that
//! does not answer never proves absence: another node may host the room, so the
//! occupant stays owed its copy.

use std::time::Duration;

use jid::{BareJid, FullJid};
use kameo::actor::ActorRef;
use waddle_xmpp::{
    ingress::{IngressEffectIntent, MessageKey},
    muc::{
        room_actor::{GetOccupantByJid, RoomActor},
        room_registry_actor::{GetRoom, RoomRegistryActor},
    },
};

use crate::{
    ingress_uow::{IngressUnitOfWork, IngressUowError},
    server::routes::interpret::Deps,
};

use super::{recovery_executor::AttemptClassification, RouteProgress};

/// Bounded budget for one room probe, matching the statement budget every
/// recovery transaction runs under. A slower answer is not proof of absence.
pub(super) const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// What one recovery attempt's departed-occupant settlement discharged.
pub(super) struct DepartedSettlement {
    /// Occupants whose frozen copy this attempt settled. No rebuilt effect may
    /// queue a copy for them.
    pub(super) occupants: Vec<FullJid>,
    /// Obligations whose aggregate receipt landed because the settlement
    /// completed their frozen fanout.
    pub(super) settled: Vec<IngressEffectIntent>,
    pub(super) classification: AttemptClassification,
}

/// Record delivery progress for every frozen occupant this node can prove is
/// no longer in the room, settling each obligation whose fanout that completes.
pub(super) async fn settle_departed_occupants(
    uow: &IngressUnitOfWork,
    deps: &Deps<'_>,
    key: MessageKey,
    route_progress: &[RouteProgress],
) -> Result<DepartedSettlement, IngressUowError> {
    let mut settlement = DepartedSettlement {
        occupants: Vec::new(),
        settled: Vec::new(),
        classification: AttemptClassification::Evaluable,
    };
    let Some(registry) = deps.room_registry else {
        return Ok(settlement);
    };
    // One authority decision per room per attempt: the roster answer is only
    // as good as the incarnation that gave it, and re-resolving it for every
    // occupant would spend the row deadline on the registry mailbox.
    let mut probed: Vec<(BareJid, RoomAuthority)> = Vec::new();
    for progress in route_progress {
        let Some(room) = progress.room() else {
            continue;
        };
        let owed = owed_occupants(deps, progress);
        if owed.is_empty() {
            continue;
        }
        if !probed.iter().any(|(candidate, _)| candidate == room) {
            let authority = resolve_authority(registry, deps, room).await;
            probed.push((room.clone(), authority));
        }
        let Some((_, authority)) = probed.iter().find(|(candidate, _)| candidate == room) else {
            continue;
        };
        let actor = match authority {
            RoomAuthority::Local(actor) => actor,
            RoomAuthority::Unreadable => {
                settlement.classification = AttemptClassification::Inconclusive;
                continue;
            }
            RoomAuthority::Unproven => continue,
        };
        let mut departed = Vec::new();
        for occupant in owed {
            match occupancy(actor, &occupant).await {
                Occupancy::Present => {}
                Occupancy::Absent => departed.push(occupant),
                Occupancy::Unknown => {
                    settlement.classification = AttemptClassification::Inconclusive;
                }
            }
        }
        if departed.is_empty() {
            continue;
        }
        let settled =
            super::execute_uow::record_delivery_progress(uow, key, progress, &departed).await?;
        let copies = u64::try_from(departed.len()).unwrap_or(u64::MAX);
        waddle_xmpp::telemetry::reliability::add_ingress_maintenance_departed_occupant_copies(
            copies,
        );
        tracing::info!(
            ?key,
            %room,
            copies,
            occupants = ?departed,
            "maintenance settled groupchat copies for occupants that left the room"
        );
        settlement.settled.extend(settled);
        settlement.occupants.extend(departed);
    }
    Ok(settlement)
}

/// Frozen occupants this obligation still owes a copy. Host-owned resources
/// belong to an extension host rather than a room roster.
pub(super) fn owed_occupants(deps: &Deps<'_>, progress: &RouteProgress) -> Vec<FullJid> {
    progress
        .fanout
        .iter()
        .filter(|occupant| {
            !progress.completed.contains(occupant) && !deps.owns_host_resource(occupant)
        })
        .cloned()
        .collect()
}

/// Whether this node's roster answer for a room is usable.
pub(super) enum RoomAuthority {
    /// The authoritative local incarnation; its roster decides.
    Local(ActorRef<RoomActor>),
    /// Nothing here can decide the room's occupancy.
    Unproven,
    /// A probe that did not answer: unproven, and the attempt is inconclusive.
    Unreadable,
}

pub(super) async fn resolve_authority(
    registry: &ActorRef<RoomRegistryActor>,
    deps: &Deps<'_>,
    room: &BareJid,
) -> RoomAuthority {
    let actor = match registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .reply_timeout(PROBE_TIMEOUT)
        .await
    {
        Ok(Some(actor)) => actor,
        // A room this node does not host says nothing about its roster.
        Ok(None) => return RoomAuthority::Unproven,
        Err(error) => {
            tracing::debug!(%room, ?error, "departed-occupant probe could not resolve the room");
            return RoomAuthority::Unreadable;
        }
    };
    if !fenced_room_ownership(deps) {
        return RoomAuthority::Local(actor);
    }
    retained_fence(actor, room).await
}

/// Whether durable room ownership is in play, so a local actor is the
/// authoritative incarnation only while it retains its exact claim fence.
#[cfg(feature = "clustering")]
fn fenced_room_ownership(deps: &Deps<'_>) -> bool {
    deps.web_socket_state.is_some_and(|state| {
        state
            .deps
            .app_state
            .clustering_claims
            .muc_durable_store
            .is_some()
    })
}

/// Room claim fences are minted only by the durable MUC store, which exists
/// only behind the `clustering` feature; without it a local actor is the only
/// incarnation there can be.
#[cfg(not(feature = "clustering"))]
fn fenced_room_ownership(_deps: &Deps<'_>) -> bool {
    false
}

/// A demoted or superseded incarnation keeps serving a stale roster, so only
/// the snapshot's retained `claim_fence` makes its answer authoritative.
#[cfg(feature = "clustering")]
async fn retained_fence(actor: ActorRef<RoomActor>, room: &BareJid) -> RoomAuthority {
    match actor
        .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
        .reply_timeout(PROBE_TIMEOUT)
        .await
    {
        Ok(snapshot) if snapshot.claim_fence.is_some() => RoomAuthority::Local(actor),
        Ok(_) => {
            tracing::debug!(%room, "departed-occupant probe found an unfenced room incarnation");
            RoomAuthority::Unproven
        }
        Err(error) => {
            tracing::debug!(%room, ?error, "departed-occupant probe could not read the room fence");
            RoomAuthority::Unreadable
        }
    }
}

#[cfg(not(feature = "clustering"))]
async fn retained_fence(actor: ActorRef<RoomActor>, _room: &BareJid) -> RoomAuthority {
    RoomAuthority::Local(actor)
}

enum Occupancy {
    Present,
    Absent,
    Unknown,
}

async fn occupancy(actor: &ActorRef<RoomActor>, occupant: &FullJid) -> Occupancy {
    match actor
        .ask(GetOccupantByJid {
            jid: occupant.clone(),
        })
        .reply_timeout(PROBE_TIMEOUT)
        .await
    {
        Ok(Some(_)) => Occupancy::Present,
        Ok(None) => Occupancy::Absent,
        Err(error) => {
            tracing::debug!(%occupant, ?error, "departed-occupant probe did not answer");
            Occupancy::Unknown
        }
    }
}
