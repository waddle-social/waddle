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
//! authoritative answer about who owns the room. A missing registry or a probe
//! that does not answer never proves absence: another node may host the room,
//! so the occupant stays owed its copy.
//!
//! There are two authoritative answers, not one. The first is an authoritative
//! LOCAL incarnation, whose roster decides. The second is the room being hosted
//! NOWHERE: rosters are memory-only, so a room with no live actor on any node
//! has no occupants at all, and every frozen occupant is roster-absent by
//! definition. That second answer is what keeps a row settleable after the
//! room it names is destroyed — the empty-room destroy a ghost eviction
//! triggers removes the registry entry AND releases the durable room claim, so
//! any SIBLING row the same ghost pinned would otherwise find no room to settle
//! against on every later pass, forever (#1803). Proving it is the whole
//! difficulty: `GetRoom -> Ok(None)` alone means only "not here". Without
//! clustering that IS the proof, because there is no other node. With
//! clustering it is the durable room claim that decides — `Ok(None)` from the
//! claim store means no node holds `EntityType::RoomActor` for this room, and
//! the empty-room destroy's `release_exact` is what deletes that row. Any claim
//! still on file leaves the copy owed, including one whose owner's node lease
//! has lapsed: a node that is merely partitioned from Postgres keeps serving
//! the rosters and sockets it already holds, and its self-fence demotion is
//! not instantaneous, so a lapsed lease is not proof that its rooms are gone.
//! That fails closed, and it self-heals: the orphan reaper steals or exactly
//! releases a dead owner's room claim, after which the row is provably absent.
//!
//! The false positive to keep in view is a room being (re)created concurrently
//! with the settlement. It is the same one the roster path already accepts, and
//! the same XEP-0045 reasoning answers it: a join into the new incarnation is a
//! NEW occupancy, and §7.2.14 gives a new occupant the room's discussion
//! history on join rather than the traffic that predates it. The copy frozen
//! against the previous occupancy was never owed to the new one, and it stays
//! in the room's XEP-0313 archive either way. The proof is also re-read
//! immediately before the write — the hosting state takes the place the roster
//! recheck holds on the local path — and the every-peer reachability proof
//! below is unchanged, so an occupant anything can still deliver to keeps its
//! copy whether or not its room still exists.
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

/// How many occupants of one route are probed at the same time.
///
/// A probe fans out to every unexpired cluster peer, so N occupants in flight
/// mean N × peers asks on the relay at once: a large room must not turn one
/// recovery attempt into an unbounded burst. Sixteen is the smallest bound
/// that keeps every room Waddle actually runs to a single probe budget — the
/// production ghost shape pins a row on one or two occupants — while capping
/// the in-flight asks at a few dozen against the two-replica deployment.
/// Each chunk is settled before the next is probed, so a deadline that elapses
/// mid-route can only lose the chunk in flight, never the proven prefix.
pub(super) const PROBE_CONCURRENCY: usize = 16;

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
    /// Whether some frozen MUC route still owes a copy to an occupant this
    /// attempt neither completed nor settled.
    ///
    /// The proof a settlement rests on is TIME-VARYING — a terminated pod's
    /// node row stays unexpired for minutes after a rolling deploy, a peer that
    /// holds the socket today may not tomorrow, a room becomes hosted or
    /// unhosted — so an attempt that could not settle such a copy has proven
    /// nothing durable about the row. `recover_row` must therefore never report
    /// it `unsupported`, which would cache it until its receipt or progress
    /// counts changed: the very counts only a later settlement can change
    /// (#1803).
    pub(super) still_owed: bool,
}

/// Record delivery progress for every frozen occupant this node can prove is
/// no longer in the room, settling each obligation whose fanout that completes,
/// and report whether any copy is still owed afterwards.
pub(super) async fn settle_departed_occupants(
    uow: &IngressUnitOfWork,
    deps: &Deps<'_>,
    key: MessageKey,
    route_progress: &[RouteProgress],
) -> Result<DepartedSettlement, IngressUowError> {
    let mut settlement = settle_proven_departures(uow, deps, key, route_progress).await?;
    settlement.still_owed = still_owed(deps, route_progress, &settlement.occupants);
    Ok(settlement)
}

/// Whether any frozen MUC route still owes a copy once `settled` is discounted.
///
/// Reads only the frozen fanout and this attempt's own settlements: no probe is
/// repeated, so the answer costs nothing beyond the route progress already in
/// hand.
fn still_owed(deps: &Deps<'_>, route_progress: &[RouteProgress], settled: &[FullJid]) -> bool {
    route_progress
        .iter()
        .filter(|progress| progress.room().is_some())
        .flat_map(|progress| owed_occupants(deps, progress))
        .any(|occupant| !settled.contains(&occupant))
}

async fn settle_proven_departures(
    uow: &IngressUnitOfWork,
    deps: &Deps<'_>,
    key: MessageKey,
    route_progress: &[RouteProgress],
) -> Result<DepartedSettlement, IngressUowError> {
    let mut settlement = DepartedSettlement {
        occupants: Vec::new(),
        settled: Vec::new(),
        classification: AttemptClassification::Evaluable,
        still_owed: false,
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
        // `None` is the unhosted room: no roster exists anywhere, so every
        // owed occupant is roster-absent and there is no claim to assert.
        let (roster, fence) = match authority {
            RoomAuthority::Local { actor, fence } => {
                (Some(actor), fence.as_ref().map(|fence| fence.as_ref()))
            }
            RoomAuthority::Unhosted => (None, None),
            RoomAuthority::Unreadable => {
                settlement.classification = AttemptClassification::Inconclusive;
                continue;
            }
            #[cfg(feature = "clustering")]
            RoomAuthority::Unproven => continue,
        };
        // Bounded chunks, each SETTLED before the next is probed. The probes
        // may spend a whole fan-out budget, so a route with many owed
        // occupants can outlive `recover_row`'s deadline: writing per chunk
        // means an elapsed deadline loses only the chunk in flight, never the
        // prefix this attempt already proved.
        for chunk in owed.chunks(PROBE_CONCURRENCY) {
            let probed = probe_chunk(deps, roster, chunk).await;
            if matches!(probed.classification, AttemptClassification::Inconclusive) {
                settlement.classification = AttemptClassification::Inconclusive;
            }
            if probed.departed.is_empty() {
                continue;
            }
            #[cfg(test)]
            enter_settlement_window(key, &probed.departed).await;
            // The probes above may have spent the whole fan-out budget. Ask
            // the evidence once more, immediately before the write: the roster
            // on the local path, so a resource that rejoined inside that
            // window keeps its copy; the hosting state on the unhosted path,
            // so a room that came back decides its own roster instead.
            let rechecked = recheck_departed(registry, deps, room, roster, probed.departed).await;
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
            match commit_departed(uow, key, progress, room, fence, &departed).await? {
                Some(settled) => {
                    settlement.settled.extend(settled);
                    settlement.occupants.extend(departed);
                }
                // The exact claim moved between the roster probe and the
                // commit: nothing settled, and this attempt proved nothing.
                None => settlement.classification = AttemptClassification::Inconclusive,
            }
        }
    }
    Ok(settlement)
}

/// What one chunk of concurrent probes proved.
struct ProbedChunk {
    /// Occupants nothing anywhere can hand a copy to.
    departed: Vec<FullJid>,
    /// `Inconclusive` when any probe in the chunk could not answer.
    classification: AttemptClassification,
}

/// Probe one chunk of occupants CONCURRENTLY and fold their verdicts.
///
/// Every occupant's decision is independent and every probe inside it is
/// bounded, so the chunk costs one probe budget rather than one per occupant —
/// which is what keeps a route with several roster-absent occupants inside
/// `recover_row`'s deadline instead of being cancelled before its first write.
///
/// `roster` is `None` for a room no node hosts: there is no roster to ask, and
/// no occupancy it could report.
async fn probe_chunk(
    deps: &Deps<'_>,
    roster: Option<&ActorRef<RoomActor>>,
    chunk: &[FullJid],
) -> ProbedChunk {
    let verdicts = futures::future::join_all(
        chunk
            .iter()
            .map(|occupant| departed_verdict(deps, roster, occupant)),
    )
    .await;
    let mut probed = ProbedChunk {
        departed: Vec::with_capacity(chunk.len()),
        classification: AttemptClassification::Evaluable,
    };
    for (occupant, verdict) in chunk.iter().zip(verdicts) {
        match verdict {
            OccupantVerdict::Departed => probed.departed.push(occupant.clone()),
            OccupantVerdict::Owed => {}
            OccupantVerdict::Inconclusive => {
                probed.classification = AttemptClassification::Inconclusive
            }
        }
    }
    probed
}

/// What this attempt proved about one frozen occupant.
enum OccupantVerdict {
    /// Nothing anywhere can hand this occupant its copy: it may be settled.
    Departed,
    /// Something can still take the copy, and that is a STABLE fact rather
    /// than a failed read — the attempt stays evaluable.
    Owed,
    /// A probe did not answer: this attempt proved nothing about the occupant.
    Inconclusive,
}

/// The ordered evidence one frozen occupant's copy may be dropped on.
async fn departed_verdict(
    deps: &Deps<'_>,
    roster: Option<&ActorRef<RoomActor>>,
    occupant: &FullJid,
) -> OccupantVerdict {
    // The roster comes FIRST. A seated occupant is owed its copy whatever the
    // cluster says, so asking anything else about it would only spend the row
    // deadline — and the SM probe is a full-table read of `sm_sessions` per
    // occupant on this hot path. An unhosted room has no roster to ask: the
    // caller already proved no node holds one, which makes every frozen
    // occupant roster-absent without a probe.
    if let Some(actor) = roster {
        match occupancy(actor, occupant).await {
            Occupancy::Present => return OccupantVerdict::Owed,
            Occupancy::Unknown => return OccupantVerdict::Inconclusive,
            Occupancy::Absent => {}
        }
    }
    // H1: a roster answer never settles a copy for somebody this node itself
    // can still hand it to.
    if recovery_reachability::locally_reachable(deps, occupant).await {
        return OccupantVerdict::Owed;
    }
    // R2-1: nor for somebody a PEER can hand it to. After a room-host restart
    // the new incarnation has a valid fence and an EMPTY roster, holds no
    // mirror for a peer's socket, and the SM probe is `Absent` for a live
    // attached one — every local signal says "gone" about a user who is simply
    // connected to the other replica, whose own maintenance pass would deliver
    // the copy.
    match recovery_reachability::reachable_elsewhere(
        deps,
        occupant,
        recovery_reachability::SETTLEMENT_FANOUT_BUDGET,
    )
    .await
    {
        recovery_reachability::ResourceReachability::AbsentEverywhere => OccupantVerdict::Departed,
        // A peer is holding that socket. That is a STABLE fact, not a failed
        // read: re-running this attempt answers the same until the socket goes
        // away, and the peer delivers the copy in its own pass. Keeping the
        // attempt Evaluable is deliberate — a row whose only obstacle is a
        // stable fact must still be able to accumulate its stall streak and
        // reach ghost repair, which is the path that resolves a resource
        // nothing can actually deliver to.
        recovery_reachability::ResourceReachability::Reachable => OccupantVerdict::Owed,
        // A read failed or the budget elapsed. Transient, and the row must not
        // be parked on evidence this attempt never gathered.
        recovery_reachability::ResourceReachability::Unproven => OccupantVerdict::Inconclusive,
    }
}

/// Commit one chunk's proven departures, reporting the obligations whose
/// aggregate receipt that completed — or `None` when the room claim moved
/// between the roster probe and the write, which settles nothing.
async fn commit_departed(
    uow: &IngressUnitOfWork,
    key: MessageKey,
    progress: &RouteProgress,
    room: &BareJid,
    fence: Option<&RoomClaimFenceContext>,
    departed: &[FullJid],
) -> Result<Option<Vec<IngressEffectIntent>>, IngressUowError> {
    let settled = match super::execute_uow::record_delivery_progress(
        uow,
        key,
        progress,
        departed,
        fence.map(|fence| (room, fence)),
    )
    .await
    {
        Ok(settled) => settled,
        Err(error) if lost_room_claim(&error) => {
            tracing::debug!(
                %room,
                "departed-occupant settlement lost the room claim fence before it committed"
            );
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let copies = u64::try_from(departed.len()).unwrap_or(u64::MAX);
    waddle_xmpp::telemetry::reliability::add_ingress_maintenance_departed_occupant_copies(copies);
    tracing::info!(
        ?key,
        %room,
        copies,
        occupants = ?departed,
        "maintenance settled groupchat copies for occupants that left the room"
    );
    Ok(Some(settled))
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
    /// No node hosts this room at all, proven — not merely "not here". Rooms
    /// carry their rosters in actor memory, so an unhosted room has no
    /// occupants and owes no frozen copy. There is nothing to evict and no
    /// claim to assert.
    Unhosted,
    /// Nothing here can decide the room's occupancy: some OTHER node may be
    /// hosting the room, and it settles the row against its own roster.
    ///
    /// Only reachable with clustering, because only then is there another node
    /// that could hold the room. Without it a room the local registry does not
    /// know is hosted nowhere ([`RoomAuthority::Unhosted`]), and a local
    /// incarnation is the only incarnation there can be.
    #[cfg(feature = "clustering")]
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
        // A room this node does not host says nothing about its roster by
        // itself — only the ownership record can say whether ANY node does.
        Ok(None) => return hosted_elsewhere(deps, room).await,
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

/// Whether a room this node does not host is hosted by anybody else.
///
/// Without clustering there is no other node, so a room the local registry
/// does not know is hosted nowhere.
#[cfg(not(feature = "clustering"))]
async fn hosted_elsewhere(_deps: &Deps<'_>, _room: &BareJid) -> RoomAuthority {
    RoomAuthority::Unhosted
}

/// Whether a room this node does not host is hosted by anybody else.
///
/// The durable room claim is the only cluster-wide record of who owns a room,
/// and it is exactly what the empty-room destroy releases, so its absence is
/// the proof this settlement needs. Every other answer leaves the copy owed:
///
/// - a claim owned by ANOTHER node — fresh or not — means a live incarnation
///   may still be serving that room's roster, and that node's own maintenance
///   pass settles the row against it. A lapsed node lease is deliberately NOT
///   treated as proof of absence: a replica partitioned from Postgres keeps
///   serving the sockets and rosters it already holds while its lease expires,
///   and `clustering::self_fence`'s demotion is neither instant nor guaranteed
///   to have run. The orphan reaper's `steal_stale`/`release_exact` is what
///   turns a genuinely dead owner's claim into `Ok(None)` here.
/// - a claim owned by THIS node with no local actor is a handoff or restart in
///   progress: the actor is about to exist.
/// - a read that errors or outruns its budget proves nothing.
#[cfg(feature = "clustering")]
async fn hosted_elsewhere(deps: &Deps<'_>, room: &BareJid) -> RoomAuthority {
    use waddle_xmpp::ownership::{Entity, EntityType};

    let Some(state) = deps.web_socket_state else {
        return RoomAuthority::Unproven;
    };
    let handles = &state.deps.app_state.clustering_claims;
    let Some(store) = handles.claim_store.as_ref() else {
        // The same single-node carve-out `recovery_reachability` makes: no
        // claim store AND no cluster membership is the unclustered
        // configuration, where this node is the whole cluster. Exactly one of
        // the two missing is a half-wired cluster, which proves nothing.
        return if handles.cluster_membership.is_none() {
            RoomAuthority::Unhosted
        } else {
            RoomAuthority::Unproven
        };
    };
    let entity = Entity::new(EntityType::RoomActor, room.to_string());
    match tokio::time::timeout(PROBE_TIMEOUT, store.current_claim(&entity)).await {
        Ok(Ok(None)) => RoomAuthority::Unhosted,
        Ok(Ok(Some(claim))) => {
            tracing::debug!(
                %room,
                owner = %claim.owner.node_id,
                fresh = claim.owner_lease_fresh,
                "departed-occupant probe found a room claim without a local actor"
            );
            RoomAuthority::Unproven
        }
        Ok(Err(error)) => {
            tracing::debug!(%room, %error, "departed-occupant probe could not read the room claim");
            RoomAuthority::Unreadable
        }
        Err(_elapsed) => {
            tracing::debug!(%room, "departed-occupant probe timed out reading the room claim");
            RoomAuthority::Unreadable
        }
    }
}

/// Whether a settlement transaction failed because the room claim it was
/// asserting is no longer on file — a steal that committed between the roster
/// probe and the write.
#[cfg(feature = "clustering")]
pub(super) fn lost_room_claim(error: &IngressUowError) -> bool {
    matches!(error, IngressUowError::ClaimFenceMissing)
}

/// Without clustering nothing asserts a room claim, so no error can mean this.
#[cfg(not(feature = "clustering"))]
pub(super) fn lost_room_claim(_error: &IngressUowError) -> bool {
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

/// Re-prove, immediately before the write, whatever this chunk's evidence
/// rests on: the roster for a locally hosted room, the hosting state itself
/// for one no node hosts.
///
/// The unhosted proof is re-read rather than re-derived per occupant because
/// that is the whole of it — an unhosted room has no roster to disagree with.
/// Anything other than "still unhosted" drops the chunk and marks the attempt
/// inconclusive: a room that came back decides its own occupancy, and the next
/// pass asks its roster instead.
async fn recheck_departed(
    registry: &ActorRef<RoomRegistryActor>,
    deps: &Deps<'_>,
    room: &BareJid,
    roster: Option<&ActorRef<RoomActor>>,
    departed: Vec<FullJid>,
) -> RecheckedDeparted {
    let Some(actor) = roster else {
        return match resolve_authority(registry, deps, room).await {
            RoomAuthority::Unhosted => RecheckedDeparted {
                occupants: departed,
                classification: AttemptClassification::Evaluable,
            },
            _ => {
                tracing::debug!(
                    %room,
                    "room regained a host before the settlement committed; keeping its copies"
                );
                RecheckedDeparted {
                    occupants: Vec::new(),
                    classification: AttemptClassification::Inconclusive,
                }
            }
        };
    };
    still_departed(actor, departed).await
}

/// Re-ask the roster about every occupant this attempt is about to settle.
///
/// `Present` drops the occupant from the departed set — it rejoined during the
/// probes, which makes it a NEW occupancy owed traffic from its join onward,
/// and the ordinary rebuild handles the frozen copy. `Unknown` drops it too,
/// and marks the attempt inconclusive: an unanswered recheck is not the
/// second proof this settlement needs.
///
/// The recheck is concurrent for the same reason the probes are: it sits
/// between the evidence and the write, and asking one roster question per
/// occupant in series would re-open the very deadline overrun the chunking
/// closes.
async fn still_departed(actor: &ActorRef<RoomActor>, departed: Vec<FullJid>) -> RecheckedDeparted {
    let answers =
        futures::future::join_all(departed.iter().map(|occupant| occupancy(actor, occupant))).await;
    let mut rechecked = RecheckedDeparted {
        occupants: Vec::with_capacity(departed.len()),
        classification: AttemptClassification::Evaluable,
    };
    for (occupant, answer) in departed.into_iter().zip(answers) {
        match answer {
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
