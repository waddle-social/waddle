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
//! Reachability is proven against SOCKETS, never against ownership claims. A
//! `UserActor` claim is routing authority: a live idle socket on a peer is
//! known only to that peer's own connection registry, and nothing
//! re-registers it when the claim owner dies or moves — so the very condition
//! that stalls delivery would also defeat a claim-shaped guard, and a LIVE
//! user could be torn out of every room. Every unexpired cluster member is
//! therefore asked about the exact full JID, and each answers only about its
//! own sockets and sessions.
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
    muc::room_actor::{
        GetOccupantSessionGeneration, LeaveAttemptId, LeaveSessionSelector, RoomActor,
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
        websocket::{redrive_local_muc_cleanup, MucCleanupOutcome, WebSocketState},
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

/// Bounds the read that reconstructs the row's frozen groupchat obligations,
/// matching every other recovery transaction on this row.
const READ_LOCK_TIMEOUT: Duration = Duration::from_millis(100);
const READ_STATEMENT_TIMEOUT: Duration = Duration::from_millis(250);

/// One bounded repair of a row maintenance just classified as stalled.
///
/// Returns `true` only when an eviction happened AND the settlement it enabled
/// recorded durable progress: the caller then restarts the stall streak
/// instead of parking and classifying the row.
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
    let mut evicted = false;
    for route in &routes {
        let Some(room) = route.room() else {
            continue;
        };
        // Only the authoritative local incarnation's roster may be acted on:
        // another node hosting the room runs its own maintenance for it.
        let RoomAuthority::Local { actor, .. } =
            recovery_departed::resolve_authority(rooms, deps, room).await
        else {
            continue;
        };
        for occupant in recovery_departed::owed_occupants(deps, route) {
            let Some(generation) = abandoned_occupancy(&actor, deps, key, &occupant).await else {
                continue;
            };
            if evict(state, &actor, &occupant, generation).await {
                // Ticked per eviction, immediately: a budget timeout later in
                // this loop must not lose the ones already performed.
                waddle_xmpp::telemetry::reliability::add_muc_ghost_occupants_evicted(1);
                // A ghost means a cleanup leak happened upstream; keep it
                // visible rather than silently repairing it forever.
                tracing::warn!(
                    ?key,
                    %room,
                    %occupant,
                    "evicting a ghost MUC occupant that pinned a stalled groupchat obligation"
                );
                evicted = true;
            }
        }
    }
    if !evicted {
        return Ok(false);
    }
    // The room no longer lists the evicted occupants, so the same settlement
    // that discharges a departed occupant's copy now discharges theirs — in
    // this attempt, rather than after the parking cooldown.
    let settlement = recovery_departed::settle_departed_occupants(uow, deps, key, &routes).await?;
    if settlement.occupants.is_empty() {
        return Ok(false);
    }
    if !settlement.settled.is_empty() {
        super::execute::terminalize_if_complete_outcome(
            uow,
            key,
            DeliveryExecutionContext::MaintenanceRecovery.into(),
        )
        .await?;
    }
    Ok(true)
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
/// `true` only when the full JID is provably no longer seated at all.
/// The sweep reports `Completed` for a `Superseded` disposition too — a
/// client that rebound the same full JID and rejoined during the probes keeps
/// its new seat, which is the point of the generation selector — and that is
/// a no-op, not an eviction: it must neither tick the counter nor claim the
/// progress that lets the settlement run. A room that cannot answer the
/// confirmation is treated the same way, since nothing was proven.
async fn evict(
    state: &WebSocketState,
    actor: &ActorRef<RoomActor>,
    occupant: &FullJid,
    generation: OccupancySessionGeneration,
) -> bool {
    // A fresh pass mints its own remote-membership ceiling, exactly as the
    // disconnect-time sweep does.
    let remote_ceiling = state
        .deps
        .protocol
        .remote_muc_memberships
        .generation_watermark();
    if redrive_local_muc_cleanup(
        state,
        occupant,
        LeaveSessionSelector::Generation(generation),
        LeaveAttemptId::generate(),
        remote_ceiling,
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
