//! XEP-0045 ghost users: evict the occupancies a stalled groupchat
//! obligation proves abandoned, then settle the copies they pinned.
//!
//! A `route_muc` obligation freezes its occupant full JIDs at acceptance, and
//! its aggregate receipt lands only once every one of them has delivery
//! progress. `recovery_departed` settles the copies of occupants the room no
//! longer lists; an occupant the room DOES still list but that has no socket,
//! no resumable XEP-0198 session and no cluster presence is the residue of a
//! cleanup leak (#1803) — the room owes it a copy forever and the row never
//! terminalizes.
//!
//! XEP-0045 §"Ghost Users" (`impl-service-ghosts`) gives the rule: a service
//! SHOULD remove a user on a delivery-related error for a stanza it sent, MAY
//! use XEP-0199 "or similar methods to periodically check the availability of
//! room occupants", and once it determines the user has gone offline "it must
//! treat the user as if the user had itself sent unavailable presence". The
//! availability check here is the similar method: a durable obligation that
//! three bounded attempts could not move, plus direct proof that no socket,
//! no resumable session and no peer node holds the full JID. The unavailable
//! treatment is the ordinary leave sweep, which broadcasts the §7.14
//! `<presence type='unavailable'/>` to the remaining occupants. It runs only
//! once maintenance has classified the row as stalled — never as a delivery
//! fallback.
//!
//! The sweep is told WHY, so the broadcast says so. XEP-0045 §"Service removes
//! user because of error response" (`#service-error-kick`) lets a service add
//! status code 333 when it removes an occupant because of a technical problem,
//! and requires it on the presence to the removed user and on the presences to
//! the remaining occupants once the service supports it — this eviction is
//! precisely that case, so it passes [`MucRemovalCause::TechnicalProblem`]
//! into both the retained janitor sweep and the inline one. 307 is not emitted
//! alongside it; the XEP calls that "generally not advisable", because such a
//! removal follows no moderator action. An ordinary disconnect keeps the bare
//! §7.14 shape, so a client can tell the two apart. The cause is
//! presentational only: it is a separate value from the durable
//! `OccupancyLeaveCause` the room projection records, which must not grow a
//! variant because it is fingerprinted into the room's durable lifecycle.
//!
//! Reachability is proven against SOCKETS, never against ownership claims. A
//! `UserActor` claim is routing authority: a live idle socket on a peer is
//! known only to that peer's own connection registry, and nothing
//! re-registers it when the claim owner dies or moves — so the very condition
//! that stalls delivery would also defeat a claim-shaped guard, and a LIVE
//! user could be torn out of every room. Every unexpired cluster member is
//! therefore asked about the exact full JID, and each answers only about its
//! own sockets and sessions.
//!
//! The copy is settled BEFORE the sweep runs, not after it. The sweep is the
//! full disconnect path, so removing the last occupant of a non-persistent
//! room also runs the empty-room destroy, and that destroy — immediately, or
//! on the local-departure janitor's next tick once the departure receipt is
//! acknowledged — removes the registry entry AND releases the durable room
//! claim. A settlement that re-derived its authority afterwards would find no
//! room to settle against. `recovery_departed::RoomAuthority::Unhosted`
//! recovers that state on a LATER pass — a room no node hosts has no roster
//! anywhere, so its frozen occupants are absent by definition — but only after
//! another whole maintenance cycle, and only for the occupants the every-peer
//! proof still clears. Settling first keeps the attempt that gathered the
//! strictly stronger evidence from throwing it away, and spares it a second
//! every-peer fan-out over an occupant it has just proven.
//!
//! What `Unhosted` does reach is the SIBLING rows. One ghost can pin several
//! canonical rows; this repair evicts it once, and the destroy that follows
//! takes the room away from every other row that named it. Those rows have no
//! eviction left to make — nobody is seated in a room nobody hosts — so this
//! module simply does not run for them: `resolve_authority` answers `Unhosted`
//! rather than `Local`, the room is skipped here, and the departed-copy
//! settlement discharges the copies instead.
//!
//! The consequences of that order are deliberate. The settlement asserts the
//! same exact room claim inside its transaction, so a steal that committed
//! since the authority was resolved rolls it back and settles nothing. A full
//! JID that rebound and rejoined between the proof and the write is seated at
//! a different generation and is dropped from both the settlement and the
//! sweep, so a live rejoin keeps its seat AND its copy. A rejoin in the
//! remaining window (write committed, sweep not yet run) makes the sweep
//! answer `Superseded`: the copy is settled against the occupancy that was
//! proven abandoned, and XEP-0045 §7.2.14 owes a new occupancy the room's
//! history rather than the traffic that predates its join — the same
//! reasoning `recovery_departed` already rests on. The eviction counter still
//! counts only confirmed unseatings, so a sweep that could not remove the
//! occupancy settles the undeliverable copy without claiming an eviction; the
//! leaked occupancy then stays in the roster for another cleanup path, which
//! is where it already was.
//!
//! Every probe here fails closed: a registry that cannot answer, a session
//! probe that cannot read its durable store, a membership read that errors or
//! is truncated, any peer ask that errors or times out, a room this node does
//! not authoritatively host, a room whose exact claim fence this node no
//! longer holds, or a missing WebSocket state all mean "not a ghost", and the
//! copy stays owed.

use std::time::Duration;

use jid::FullJid;
use kameo::actor::ActorRef;
use waddle_xmpp::{
    ingress::MessageKey,
    muc::{
        room_actor::{GetOccupantSessionGeneration, LeaveSessionSelector, RoomActor},
        MucRemovalCause,
    },
};
use waddle_xmpp_core::OccupancySessionGeneration;

use crate::{
    ingress_uow::{
        CanonicalMessageRepository, DeliveryProgressRepository, EffectIntentRepository,
        EffectReceiptRepository, IngressUnitOfWork, IngressUowError,
    },
    server::routes::{
        interpret::{DeliveryExecutionContext, Deps},
        websocket::{
            retain_abandoned_muc_occupancy_sweep, sweep_abandoned_muc_occupancy, MucCleanupOutcome,
            WebSocketState,
        },
    },
};

use super::{
    recovery_departed::{self, RoomAuthority},
    recovery_reachability, RouteProgress,
};

/// Test seam for the window the generation pin exists to close: everything
/// between pinning the seated generation and the reachability probes, which a
/// rebinding client can rejoin inside.
///
/// Registered per canonical row rather than per task: the repair runs on the
/// recovery-accounting worker task, so a `task_local` set around a
/// maintenance pass never reaches it.
#[cfg(test)]
#[async_trait::async_trait]
pub(crate) trait GhostProbeWindow: Send + Sync {
    async fn enter(&self, occupant: &FullJid);
}

#[cfg(test)]
static GHOST_PROBE_WINDOWS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<MessageKey, std::sync::Arc<dyn GhostProbeWindow>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
pub(crate) fn hook_ghost_probe_window(key: MessageKey, hook: std::sync::Arc<dyn GhostProbeWindow>) {
    GHOST_PROBE_WINDOWS
        .lock()
        .expect("ghost probe hooks")
        .insert(key, hook);
}

#[cfg(test)]
async fn enter_probe_window(key: MessageKey, occupant: &FullJid) {
    let hook = GHOST_PROBE_WINDOWS
        .lock()
        .expect("ghost probe hooks")
        .get(&key)
        .map(std::sync::Arc::clone);
    if let Some(hook) = hook {
        hook.enter(occupant).await;
    }
}

/// Test seam for the moment a confirmed eviction has changed the room: the
/// leave sweep has just queued the empty-room destroy, so from here the
/// registry entry and the room claim the repair's authority rests on can go
/// away under it.
///
/// Registered per canonical row for the same reason as [`GhostProbeWindow`].
#[cfg(test)]
#[async_trait::async_trait]
pub(crate) trait GhostEvictionWindow: Send + Sync {
    async fn enter(&self, occupant: &FullJid);
}

#[cfg(test)]
static GHOST_EVICTION_WINDOWS: std::sync::LazyLock<
    std::sync::Mutex<
        std::collections::HashMap<MessageKey, std::sync::Arc<dyn GhostEvictionWindow>>,
    >,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
pub(crate) fn hook_ghost_eviction_window(
    key: MessageKey,
    hook: std::sync::Arc<dyn GhostEvictionWindow>,
) {
    GHOST_EVICTION_WINDOWS
        .lock()
        .expect("ghost eviction hooks")
        .insert(key, hook);
}

#[cfg(test)]
async fn enter_eviction_window(key: MessageKey, occupant: &FullJid) {
    let hook = GHOST_EVICTION_WINDOWS
        .lock()
        .expect("ghost eviction hooks")
        .get(&key)
        .map(std::sync::Arc::clone);
    if let Some(hook) = hook {
        hook.enter(occupant).await;
    }
}

/// Bounds the read that reconstructs the row's frozen groupchat obligations,
/// matching every other recovery transaction on this row.
const READ_LOCK_TIMEOUT: Duration = Duration::from_millis(100);
const READ_STATEMENT_TIMEOUT: Duration = Duration::from_millis(250);

/// One bounded repair of a row maintenance just classified as stalled.
///
/// Returns `true` only when the repair recorded durable progress for at least
/// one ghost's copy: the caller then restarts the stall streak instead of
/// parking and classifying the row.
pub(super) async fn repair_stalled_row(
    uow: &IngressUnitOfWork,
    deps: &Deps<'_>,
    key: MessageKey,
) -> Result<bool, IngressUowError> {
    // The leave sweep is the only place §7.14 presence, SFU teardown, empty-
    // room eviction and failure retention live; without it there is no
    // eviction to make.
    let (Some(state), Some(rooms)) = (deps.web_socket_state, deps.room_registry) else {
        return Ok(false);
    };
    let routes = pending_groupchat_routes(uow, key).await?;
    let mut settled_any = false;
    let mut receipted_any = false;
    for route in &routes {
        let Some(room) = route.room() else {
            continue;
        };
        // Only the authoritative local incarnation's roster may be acted on:
        // another node hosting the room runs its own maintenance for it, and a
        // room nobody hosts (`Unhosted`) seats nobody, so there is no ghost to
        // evict — the departed-copy settlement discharges those copies.
        let RoomAuthority::Local { actor, fence } =
            recovery_departed::resolve_authority(rooms, deps, room).await
        else {
            continue;
        };
        let owed = recovery_departed::owed_occupants(deps, route);
        // Bounded chunks, each settled (and swept) before the next is probed:
        // every probe below may spend the whole ghost fan-out budget, so a
        // room with several owed occupants would otherwise exhaust
        // maintenance's repair budget before the first write and discard the
        // prefix it had already proven. See `PROBE_CONCURRENCY`.
        for chunk in owed.chunks(recovery_departed::PROBE_CONCURRENCY) {
            let ghosts = probe_chunk(&actor, deps, key, chunk).await;
            let ghosts = still_abandoned(&actor, ghosts).await;
            if ghosts.is_empty() {
                continue;
            }
            let occupants: Vec<FullJid> = ghosts.iter().map(|(jid, _)| jid.clone()).collect();
            // Settled BEFORE the sweep, under the authority the proof rests on
            // — see this function's ordering note above `evict`. The
            // in-transaction claim assertion still fences a steal that
            // committed since the authority was resolved.
            let settled = match super::execute_uow::record_delivery_progress(
                uow,
                key,
                route,
                &occupants,
                fence.as_ref().map(|fence| (room, fence.as_ref())),
            )
            .await
            {
                Ok(settled) => settled,
                Err(error) if recovery_departed::lost_room_claim(&error) => {
                    tracing::debug!(
                        %room,
                        "ghost repair lost the room claim fence before it committed"
                    );
                    continue;
                }
                Err(error) => return Err(error),
            };
            let copies = u64::try_from(occupants.len()).unwrap_or(u64::MAX);
            waddle_xmpp::telemetry::reliability::add_ingress_maintenance_departed_occupant_copies(
                copies,
            );
            tracing::info!(
                ?key,
                %room,
                copies,
                occupants = ?occupants,
                "maintenance settled groupchat copies for ghost MUC occupants"
            );
            settled_any = true;
            receipted_any |= !settled.is_empty();
            // Every settled ghost gets a retained janitor sweep NOW, with no
            // await between the commit above and these records: the sweeps
            // below run one by one under the repair budget, and a cancellation
            // between two of them must not leave a settled ghost seated with
            // nobody owing its removal (the row may already be terminal, so
            // recovery would never revisit it).
            for (occupant, generation) in &ghosts {
                retain_abandoned_muc_occupancy_sweep(
                    state,
                    occupant,
                    LeaveSessionSelector::Generation(*generation),
                    MucRemovalCause::TechnicalProblem,
                );
            }
            // Sequential on purpose: each sweep MUTATES the room, and the
            // empty-room destroy one of them triggers must not race the next.
            for (occupant, generation) in ghosts {
                if evict(state, &actor, &occupant, generation).await {
                    // Ticked per eviction, immediately: a budget timeout later
                    // in this loop must not lose the ones already performed.
                    waddle_xmpp::telemetry::reliability::add_muc_ghost_occupants_evicted(1);
                    // A ghost means a cleanup leak happened upstream; keep it
                    // visible rather than silently repairing it forever.
                    tracing::warn!(
                        ?key,
                        %room,
                        %occupant,
                        "evicting a ghost MUC occupant that pinned a stalled groupchat obligation"
                    );
                    #[cfg(test)]
                    enter_eviction_window(key, &occupant).await;
                } else {
                    // The copy is settled either way — nothing can take it —
                    // but the XEP-0045 removal did not happen yet. A sweep
                    // that could not enumerate or resolve the rooms left the
                    // local-departure janitor a `FullJidSweep` to retry; a
                    // per-room failure is retained by the sweep itself.
                    tracing::warn!(
                        ?key,
                        %room,
                        %occupant,
                        "settled a ghost MUC occupant's copy but could not unseat it"
                    );
                }
            }
        }
    }
    if receipted_any {
        super::execute::terminalize_if_complete_outcome(
            uow,
            key,
            DeliveryExecutionContext::MaintenanceRecovery.into(),
        )
        .await?;
    }
    Ok(settled_any)
}

/// Prove one chunk of owed occupants CONCURRENTLY.
///
/// Each occupant's evidence is independent and every probe inside it is
/// bounded, so the chunk costs one fan-out budget rather than one per
/// occupant. The generation is still pinned FIRST inside each occupant's own
/// future, which is what makes a rejoin during the probes `Superseded` rather
/// than evicted — see [`abandoned_occupancy`].
async fn probe_chunk(
    actor: &ActorRef<RoomActor>,
    deps: &Deps<'_>,
    key: MessageKey,
    chunk: &[FullJid],
) -> Vec<(FullJid, OccupancySessionGeneration)> {
    let probed = futures::future::join_all(
        chunk
            .iter()
            .map(|occupant| abandoned_occupancy(actor, deps, key, occupant)),
    )
    .await;
    chunk
        .iter()
        .zip(probed)
        .filter_map(|(occupant, generation)| Some((occupant.clone(), generation?)))
        .collect()
}

/// Re-ask the room about every proven ghost, immediately before the write.
///
/// The probes above may have spent the whole fan-out budget, and the roster is
/// actor memory that no transaction can hold still — exactly the window
/// `recovery_departed::still_departed` narrows for a departed occupant. Only
/// the pinned generation may be settled: a full JID that rebound and rejoined
/// during the probes is seated at a DIFFERENT generation, and that new
/// occupancy keeps both its seat and its copy. An occupancy that vanished on
/// its own (`None`) is an ordinary departure, which the departed-copy
/// settlement discharges with its own proof; a room that cannot answer proves
/// nothing.
///
/// The recheck is concurrent for the same reason the probes are: it sits
/// between the evidence and the write, and one roster question per ghost in
/// series would re-open the budget overrun the chunking closes.
async fn still_abandoned(
    actor: &ActorRef<RoomActor>,
    ghosts: Vec<(FullJid, OccupancySessionGeneration)>,
) -> Vec<(FullJid, OccupancySessionGeneration)> {
    let seated = futures::future::join_all(ghosts.iter().map(|(occupant, _)| async {
        actor
            .ask(GetOccupantSessionGeneration {
                jid: occupant.clone(),
            })
            .reply_timeout(recovery_departed::PROBE_TIMEOUT)
            .await
    }))
    .await;
    let mut proven = Vec::with_capacity(ghosts.len());
    for ((occupant, generation), answer) in ghosts.into_iter().zip(seated) {
        match answer {
            Ok(Some(seated)) if seated == generation => proven.push((occupant, generation)),
            Ok(Some(_)) => tracing::debug!(
                %occupant,
                "ghost rejoined before the settlement committed; keeping its seat and its copy"
            ),
            Ok(None) => tracing::debug!(
                %occupant,
                "ghost occupancy left before the settlement committed; leaving it to the \
                 departed-copy settlement"
            ),
            Err(error) => {
                tracing::debug!(%occupant, ?error, "ghost recheck did not answer")
            }
        }
    }
    proven
}

/// The occupancy generation to evict, or `None` when anything at all leaves
/// the occupant reachable — or leaves a probe unable to answer.
///
/// The generation is pinned FIRST, before any probe runs. The probes read a
/// database, ask actors and fan out across the cluster, which takes up to
/// seconds; the web client reuses its `web-<uuid>` resource across reconnects
/// within a page, so the same full JID can rebind and rejoin inside that
/// window. Pinning first means the eviction names the generation that was
/// seated when the evidence was gathered: a session that joined during the
/// probes carries a different generation, and
/// `LeaveSessionSelector::Generation` makes the sweep classify it
/// `Superseded` instead of tearing it out.
///
/// `None` from the room means the occupant is not seated at all, so there is
/// nothing to evict and no reason to spend the probes.
async fn abandoned_occupancy(
    actor: &ActorRef<RoomActor>,
    deps: &Deps<'_>,
    key: MessageKey,
    occupant: &FullJid,
) -> Option<OccupancySessionGeneration> {
    let generation = match actor
        .ask(GetOccupantSessionGeneration {
            jid: occupant.clone(),
        })
        .reply_timeout(recovery_departed::PROBE_TIMEOUT)
        .await
    {
        Ok(generation) => generation?,
        Err(error) => {
            tracing::debug!(%occupant, ?error, "ghost probe could not pin the occupancy generation");
            return None;
        }
    };
    #[cfg(test)]
    enter_probe_window(key, occupant).await;
    #[cfg(not(test))]
    let _ = key;
    if recovery_reachability::locally_reachable(deps, occupant).await {
        return None;
    }
    // Ghost repair runs after the row was already classified as stalled, so
    // it takes the generous fan-out budget; anything short of "nothing
    // anywhere holds this resource" leaves the occupant seated.
    if recovery_reachability::reachable_elsewhere(
        deps,
        occupant,
        recovery_reachability::GHOST_FANOUT_BUDGET,
    )
    .await
        != recovery_reachability::ResourceReachability::AbsentEverywhere
    {
        return None;
    }
    Some(generation)
}

/// Remove the occupancy through the one full-JID leave sweep every other
/// departure path uses, so XEP-0045 §7.14 presence, SFU teardown, empty-room
/// eviction and failure retention stay in a single place.
///
/// Runs AFTER the copy is settled: the sweep's own empty-room destroy can
/// take the room — and its claim — away, and the settlement must not depend
/// on authority the eviction itself is allowed to destroy. See the module
/// docs for why that order is the safe one.
///
/// `true` only when the full JID is provably no longer seated at all.
/// The sweep reports `Completed` for a `Superseded` disposition too — a
/// client that rebound the same full JID and rejoined in the last window
/// keeps its new seat, which is the point of the generation selector — and
/// that is a no-op, not an eviction: it must not tick the counter. A room
/// that cannot answer the confirmation is treated the same way, since nothing
/// was proven.
async fn evict(
    state: &WebSocketState,
    actor: &ActorRef<RoomActor>,
    occupant: &FullJid,
    generation: OccupancySessionGeneration,
) -> bool {
    // A FRESH pass, not a janitor redrive: if the registry cannot enumerate or
    // resolve the rooms, the sweep records a `FullJidSweep` and the
    // local-departure janitor retries the eviction. The copy is already
    // settled by now, so nothing else would come back for this ghost until
    // another message stalled on it.
    if sweep_abandoned_muc_occupancy(
        state,
        occupant,
        LeaveSessionSelector::Generation(generation),
        MucRemovalCause::TechnicalProblem,
    )
    .await
        != MucCleanupOutcome::Completed
    {
        return false;
    }
    match actor
        .ask(GetOccupantSessionGeneration {
            jid: occupant.clone(),
        })
        .reply_timeout(recovery_departed::PROBE_TIMEOUT)
        .await
    {
        // Any seat at all means the room still owes this full JID a copy: a
        // `Superseded` sweep leaves the rejoined session sitting there, and
        // that is not an eviction.
        Ok(seated) => seated.is_none(),
        Err(error) => {
            tracing::debug!(%occupant, ?error, "ghost sweep could not confirm the eviction");
            false
        }
    }
}

/// The row's still-owed groupchat obligations with the progress each has
/// already accumulated.
///
/// A read-only sibling of the recovery freeze: the repair takes no canonical
/// lock and rebuilds no effects, it only needs to know which occupants a
/// `route_muc` receipt is still waiting for.
async fn pending_groupchat_routes(
    uow: &IngressUnitOfWork,
    key: MessageKey,
) -> Result<Vec<RouteProgress>, IngressUowError> {
    let mut tx = uow
        .begin_with_timeouts(READ_LOCK_TIMEOUT, READ_STATEMENT_TIMEOUT)
        .await?;
    let created_at = CanonicalMessageRepository::created_at(&mut tx, key).await?;
    let recorded = EffectIntentRepository::load(&mut tx, key).await?;
    let receipted = EffectReceiptRepository::keys(&mut tx, key).await?;
    let completed = DeliveryProgressRepository::load_all(&mut tx, key).await?;
    tx.commit().await?;
    let mut routes = Vec::new();
    for intent in &recorded {
        if receipted.contains(&super::receipt_key(intent)?) {
            continue;
        }
        let Some(mut route) = RouteProgress::from_intent(intent, Some(created_at), Vec::new())?
        else {
            continue;
        };
        if route.is_direct() || route.room().is_none() {
            continue;
        }
        route.completed = completed
            .iter()
            .find(|(receipt, _)| receipt == &route.receipt)
            .map(|(_, done)| done.clone())
            .unwrap_or_default();
        routes.push(route);
    }
    Ok(routes)
}
