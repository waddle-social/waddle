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
//! A settlement is also never used for an occupant ANY node can still reach.
//! Rosters are memory-only, so after a room-host restart the new incarnation
//! has a valid fence and an EMPTY roster, holds no registered-remote mirror
//! for a peer's socket, and reads `Absent` from the resumable-session probe
//! for a live attached one — every local signal says "gone" about a user who
//! is simply connected to the other replica. Settling on that would
//! permanently drop the copy. So a roster-absent occupant must additionally
//! be absent from this node's own state AND denied by every unexpired
//! cluster peer before its copy is dropped.
//!
//! Those probes take time — up to the whole fan-out budget — and the roster,
//! unlike the room claim, cannot be fenced against it: it is actor memory,
//! not durable state, so no transaction can hold it still. The roster is
//! therefore asked a SECOND time immediately before the write. That NARROWS
//! the window between the evidence and the commit; it does not close it, and
//! a resource that rejoins after the recheck still has its frozen copy
//! settled. What makes that acceptable is XEP-0045 itself: the rejoin is a
//! NEW occupancy, and §7.2.14 gives a new occupant the room's discussion
//! history on join rather than the traffic that predates it — the copy frozen
//! against the previous occupancy was never owed to the new one. The message
//! also stays in the room's XEP-0313 archive either way, so nothing leaves
//! the room's record.
//!
//! Where such a copy is then delivered depends on who holds the socket:
//!
//! - A socket or resumable session on THIS node: the ordinary rebuild in this
//!   same pass delivers or queues it.
//! - A socket on a PEER: the room host never delivers it — `is_connected` is
//!   true for the registered-remote mirror, but `deliver_direct_to_full_locally`
//!   only writes to locally hosted sockets. The replica that actually holds
//!   the socket delivers the copy in ITS own maintenance pass (the
//!   `muc_recovery_destination_owner_settles_local_copy` shape). The room
//!   host's job is simply not to drop it first.

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
};

use crate::{
    ingress_uow::{IngressUnitOfWork, IngressUowError},
    server::routes::interpret::Deps,
};

use super::{recovery_executor::AttemptClassification, recovery_reachability, RouteProgress};

/// Bounded budget for one room probe, matching the statement budget every
/// recovery transaction runs under. A slower answer is not proof of absence.
pub(super) const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Test seam for the window [`still_departed`] narrows: between the
/// reachability probes, which may spend the whole fan-out budget, and the
/// settlement write.
///
/// Registered per canonical row rather than per task, for the same reason as
/// `recovery_ghosts::GhostProbeWindow`: the settlement runs on the
/// recovery-accounting worker task, so a `task_local` set around a
/// maintenance pass never reaches it.
#[cfg(test)]
#[async_trait::async_trait]
pub(crate) trait SettlementWriteWindow: Send + Sync {
    async fn enter(&self, occupants: &[FullJid]);
}

#[cfg(test)]
static SETTLEMENT_WRITE_WINDOWS: std::sync::LazyLock<
    std::sync::Mutex<
        std::collections::HashMap<MessageKey, std::sync::Arc<dyn SettlementWriteWindow>>,
    >,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
pub(crate) fn hook_settlement_write_window(
    key: MessageKey,
    hook: std::sync::Arc<dyn SettlementWriteWindow>,
) {
    SETTLEMENT_WRITE_WINDOWS
        .lock()
        .expect("settlement write hooks")
        .insert(key, hook);
}

#[cfg(test)]
async fn enter_settlement_window(key: MessageKey, occupants: &[FullJid]) {
    let hook = SETTLEMENT_WRITE_WINDOWS
        .lock()
        .expect("settlement write hooks")
        .get(&key)
        .map(std::sync::Arc::clone);
    if let Some(hook) = hook {
        hook.enter(occupants).await;
    }
}

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
            // The roster comes FIRST. A seated occupant is owed its copy
            // whatever the cluster says, so asking anything else about it
            // would only spend the row deadline — and the SM probe is a
            // full-table read of `sm_sessions` per occupant on this hot path.
            match occupancy(actor, &occupant).await {
                Occupancy::Present => continue,
                Occupancy::Unknown => {
                    settlement.classification = AttemptClassification::Inconclusive;
                    continue;
                }
                Occupancy::Absent => {}
            }
            // H1: a roster answer never settles a copy for somebody this node
            // itself can still hand it to.
            if recovery_reachability::locally_reachable(deps, &occupant).await {
                continue;
            }
            // R2-1: nor for somebody a PEER can hand it to. After a room-host
            // restart the new incarnation has a valid fence and an EMPTY
            // roster, holds no mirror for a peer's socket, and the SM probe
            // is `Absent` for a live attached one — every local signal says
            // "gone" about a user who is simply connected to the other
            // replica, whose own maintenance pass would deliver the copy.
            match recovery_reachability::reachable_elsewhere(
                deps,
                &occupant,
                recovery_reachability::SETTLEMENT_FANOUT_BUDGET,
            )
            .await
            {
                recovery_reachability::ResourceReachability::AbsentEverywhere => {
                    departed.push(occupant)
                }
                // A peer is holding that socket. That is a STABLE fact, not a
                // failed read: re-running this attempt answers the same until
                // the socket goes away, and the peer delivers the copy in its
                // own pass. Keeping the attempt Evaluable is deliberate — a
                // row whose only obstacle is a stable fact must still be able
                // to accumulate its stall streak and reach ghost repair,
                // which is the path that resolves a resource nothing can
                // actually deliver to.
                recovery_reachability::ResourceReachability::Reachable => {}
                // A read failed or the budget elapsed. Transient, and the row
                // must not be parked on evidence this attempt never gathered.
                recovery_reachability::ResourceReachability::Unproven => {
                    settlement.classification = AttemptClassification::Inconclusive;
                }
            }
        }
        if departed.is_empty() {
            continue;
        }
        #[cfg(test)]
        enter_settlement_window(key, &departed).await;
        // The probes above may have spent the whole fan-out budget. Ask the
        // roster once more, immediately before the write, so a resource that
        // rejoined inside that window keeps its copy.
        let rechecked = still_departed(actor, departed).await;
        if matches!(
            rechecked.classification,
            AttemptClassification::Inconclusive
        ) {
            settlement.classification = AttemptClassification::Inconclusive;
        }
        let departed = rechecked.occupants;
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

/// The outcome of the pre-write roster recheck.
struct RecheckedDeparted {
    /// Occupants the roster still does not list, and only those.
    occupants: Vec<FullJid>,
    /// `Inconclusive` when any recheck failed to answer: this attempt then
    /// proved nothing about the occupants it dropped.
    classification: AttemptClassification,
}

/// Re-ask the roster about every occupant this attempt is about to settle.
///
/// `Present` drops the occupant from the departed set — it rejoined during the
/// probes, which makes it a NEW occupancy owed traffic from its join onward,
/// and the ordinary rebuild handles the frozen copy. `Unknown` drops it too,
/// and marks the attempt inconclusive: an unanswered recheck is not the
/// second proof this settlement needs.
async fn still_departed(actor: &ActorRef<RoomActor>, departed: Vec<FullJid>) -> RecheckedDeparted {
    let mut rechecked = RecheckedDeparted {
        occupants: Vec::with_capacity(departed.len()),
        classification: AttemptClassification::Evaluable,
    };
    for occupant in departed {
        match occupancy(actor, &occupant).await {
            Occupancy::Absent => rechecked.occupants.push(occupant),
            Occupancy::Present => {
                tracing::debug!(
                    %occupant,
                    "departed occupant rejoined before the settlement committed; keeping its copy"
                );
            }
            Occupancy::Unknown => {
                rechecked.classification = AttemptClassification::Inconclusive;
            }
        }
    }
    rechecked
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
