//! XEP-0045 ghost users: settle frozen copies the room no longer owes.
//!
//! A `route_muc` obligation freezes its occupant full JIDs at acceptance. An
//! occupant that has since left the room — or whose session died as a ghost —
//! can never take its copy, so the single aggregate receipt never lands and
//! maintenance retries the row forever (#1803). XEP-0045 states the rule
//! exactly ("Ghost Users" and §7.14): an entity that is no longer an occupant
//! is owed no groupchat message.
//!
//! Only maintenance recovery settles these copies, and only against an
//! authoritative local room incarnation. A missing registry, a missing local
//! actor, or a probe that does not answer never proves absence: another node
//! may host the room, so the occupant stays owed its copy.
//!
//! With durable room ownership in play, "authoritative" means the EXACT claim
//! is still on file, proven against the store — not merely that the actor is
//! carrying a fence. `RoomActor::durable_claim_fence` is set once and never
//! cleared, so an incarnation sealed `OwnershipLost`, or one that has not yet
//! noticed its supersession, keeps reporting `Some(fence)` and keeps serving
//! a stale roster. The settlement permanently drops a copy, so the same exact
//! fence is asserted again INSIDE the settlement transaction wherever the
//! unit of work is clustered, closing the probe-to-commit window: a steal
//! that commits in between rolls the settlement back instead of dropping a
//! copy for an occupant who joined on the new owner.
//!
//! A settlement is also never used for an occupant this node can still reach
//! itself. Rosters are memory-only, so after a room-host restart every frozen
//! pre-restart occupant reads "absent from the roster" — settling on that
//! would permanently drop copies for users sitting right here. Such an
//! occupant falls through to the ordinary rebuild, which delivers or queues
//! its copy in the same pass.

use std::time::Duration;

use jid::{BareJid, FullJid};
use kameo::actor::ActorRef;
use waddle_xmpp::{
    ingress::{IngressEffectIntent, MessageKey},
    muc::{
        durable::RoomClaimFenceContext,
        room_actor::{GetOccupantByJid, RoomActor},
        room_registry_actor::{GetRoom, RoomRegistryActor},
    },
    stream_management::ResumableSessionProbe,
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
        let (actor, fence) = match authority {
            RoomAuthority::Local { actor, fence } => (actor, fence),
            RoomAuthority::Unreadable => {
                settlement.classification = AttemptClassification::Inconclusive;
                continue;
            }
            RoomAuthority::Unproven => continue,
        };
        let mut departed = Vec::new();
        for occupant in owed {
            // H1: a roster answer never settles a copy for somebody this node
            // itself can still hand it to — the rebuild delivers or queues it
            // in this same pass instead.
            if locally_reachable(deps, &occupant).await {
                continue;
            }
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
        let settled = match super::execute_uow::record_delivery_progress(
            uow,
            key,
            progress,
            &departed,
            fence.as_ref().map(|fence| (room, fence.as_ref())),
        )
        .await
        {
            Ok(settled) => settled,
            // The exact claim moved between the roster probe and the commit:
            // nothing settled, and this attempt proved nothing.
            Err(error) if lost_room_claim(&error) => {
                tracing::debug!(
                    %room,
                    "departed-occupant settlement lost the room claim fence before it committed"
                );
                settlement.classification = AttemptClassification::Inconclusive;
                continue;
            }
            Err(error) => return Err(error),
        };
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
    /// The authoritative local incarnation; its roster decides. `fence`
    /// carries the exact claim that proof rests on, so the settlement
    /// transaction can re-assert it under `FOR SHARE`; `None` means durable
    /// room ownership is not in play at all.
    Local {
        actor: ActorRef<RoomActor>,
        // Boxed so the authoritative variant does not make every
        // `RoomAuthority` the size of a claim fence context.
        fence: Option<Box<RoomClaimFenceContext>>,
    },
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
    let Some(store) = durable_room_store(deps) else {
        return RoomAuthority::Local { actor, fence: None };
    };
    proven_fence(actor, store, room).await
}

/// Whether a settlement transaction failed because the room claim it was
/// asserting is no longer on file — a steal that committed between the roster
/// probe and the write.
#[cfg(feature = "clustering")]
fn lost_room_claim(error: &IngressUowError) -> bool {
    matches!(error, IngressUowError::ClaimFenceMissing)
}

/// Without clustering nothing asserts a room claim, so no error can mean this.
#[cfg(not(feature = "clustering"))]
fn lost_room_claim(_error: &IngressUowError) -> bool {
    false
}

/// The durable MUC room-ownership store this node was built with.
type DurableRoomStore = std::sync::Arc<dyn waddle_xmpp::muc::MucDurableStore>;

/// The durable MUC store, when durable room ownership is in play: a local
/// actor is then the authoritative incarnation only while the store still
/// holds its exact claim fence.
#[cfg(feature = "clustering")]
fn durable_room_store(deps: &Deps<'_>) -> Option<DurableRoomStore> {
    deps.web_socket_state.and_then(|state| {
        state
            .deps
            .app_state
            .clustering_claims
            .muc_durable_store
            .clone()
    })
}

/// Room claim fences are minted only by the durable MUC store, which exists
/// only behind the `clustering` feature; without it a local actor is the only
/// incarnation there can be.
#[cfg(not(feature = "clustering"))]
fn durable_room_store(_deps: &Deps<'_>) -> Option<DurableRoomStore> {
    None
}

/// Prove the incarnation's retained fence against the store.
///
/// `RoomActor::durable_claim_fence` is assigned once and never cleared, so a
/// snapshot carrying `Some(fence)` proves only that this actor was granted
/// the room at some point — an incarnation sealed `OwnershipLost`, or one
/// that has not yet processed its supersession, still reports one and still
/// serves the roster it had. `check_exact_claim_fence` is the same mutation
/// gate `RoomActor` itself runs before touching durable state: `Ok(true)`
/// means the exact `(entity, epoch, node)` tuple is still on file.
#[cfg(feature = "clustering")]
async fn proven_fence(
    actor: ActorRef<RoomActor>,
    store: DurableRoomStore,
    room: &BareJid,
) -> RoomAuthority {
    let fence = match actor
        .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
        .reply_timeout(PROBE_TIMEOUT)
        .await
    {
        Ok(snapshot) => match snapshot.claim_fence {
            Some(fence) => fence,
            None => {
                tracing::debug!(%room, "departed-occupant probe found an unfenced room incarnation");
                return RoomAuthority::Unproven;
            }
        },
        Err(error) => {
            tracing::debug!(%room, ?error, "departed-occupant probe could not read the room fence");
            return RoomAuthority::Unreadable;
        }
    };
    match store.check_exact_claim_fence(room, &fence).await {
        Ok(true) => RoomAuthority::Local {
            actor,
            fence: Some(Box::new(fence)),
        },
        Ok(false) => {
            tracing::debug!(
                %room,
                "departed-occupant probe found a superseded room incarnation serving a stale roster"
            );
            RoomAuthority::Unproven
        }
        Err(error) => {
            tracing::debug!(%room, %error, "departed-occupant probe could not prove the room claim fence");
            RoomAuthority::Unreadable
        }
    }
}

#[cfg(not(feature = "clustering"))]
async fn proven_fence(
    actor: ActorRef<RoomActor>,
    _store: DurableRoomStore,
    _room: &BareJid,
) -> RoomAuthority {
    RoomAuthority::Local { actor, fence: None }
}

/// Whether THIS node can still hand the occupant its copy itself, in which
/// case the roster's "absent" must not settle (H1). A missing SM registry
/// cannot prove anything, so it also counts as reachable.
async fn locally_reachable(deps: &Deps<'_>, occupant: &FullJid) -> bool {
    if deps.connection_registry.is_connected(occupant) {
        return true;
    }
    let Some(sm) = deps.sm_session_registry else {
        return true;
    };
    match sm.probe_resumable_session_for_full_jid(occupant).await {
        ResumableSessionProbe::Present | ResumableSessionProbe::Failed => true,
        ResumableSessionProbe::Absent => false,
    }
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
