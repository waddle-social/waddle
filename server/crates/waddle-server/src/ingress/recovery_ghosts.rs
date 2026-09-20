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
//! Every probe here fails closed: a registry that cannot answer, a session
//! probe that cannot read its durable store, an indeterminate ownership claim,
//! a room this node does not authoritatively host, or a missing WebSocket
//! state all mean "not a ghost", and the copy stays owed.

use std::time::Duration;

use jid::FullJid;
use kameo::actor::ActorRef;
use waddle_xmpp::{
    ingress::MessageKey,
    muc::room_actor::{
        GetOccupantSessionGeneration, LeaveAttemptId, LeaveSessionSelector, RoomActor,
    },
    stream_management::ResumableSessionProbe,
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
    RouteProgress,
};

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
    let mut evicted = 0u64;
    for route in &routes {
        let Some(room) = route.room() else {
            continue;
        };
        // Only the authoritative local incarnation's roster may be acted on:
        // another node hosting the room runs its own maintenance for it.
        let RoomAuthority::Local(actor) =
            recovery_departed::resolve_authority(rooms, deps, room).await
        else {
            continue;
        };
        for occupant in recovery_departed::owed_occupants(deps, route) {
            let Some(generation) = abandoned_occupancy(&actor, deps, &occupant).await else {
                continue;
            };
            if evict(state, &occupant, generation).await {
                // A ghost means a cleanup leak happened upstream; keep it
                // visible rather than silently repairing it forever.
                tracing::warn!(
                    ?key,
                    %room,
                    %occupant,
                    "evicting a ghost MUC occupant that pinned a stalled groupchat obligation"
                );
                evicted += 1;
            }
        }
    }
    if evicted == 0 {
        return Ok(false);
    }
    waddle_xmpp::telemetry::reliability::add_muc_ghost_occupants_evicted(evicted);
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
/// The generation comes from the room in the same ask as the presence check:
/// a same-full-JID session that joined since carries a different generation,
/// and `LeaveSessionSelector::Generation` makes the sweep classify it
/// `Superseded` instead of evicting it.
async fn abandoned_occupancy(
    actor: &ActorRef<RoomActor>,
    deps: &Deps<'_>,
    occupant: &FullJid,
) -> Option<OccupancySessionGeneration> {
    if deps.connection_registry.is_connected(occupant) {
        return None;
    }
    // Checks this node's memory AND the shared durable store, so a session
    // resume-stolen by another node still counts as resumable.
    let sm = deps.sm_session_registry?;
    match sm.probe_resumable_session_for_full_jid(occupant).await {
        ResumableSessionProbe::Absent => {}
        ResumableSessionProbe::Present | ResumableSessionProbe::Failed => return None,
    }
    if reachable_on_another_node(deps, occupant).await {
        return None;
    }
    match actor
        .ask(GetOccupantSessionGeneration {
            jid: occupant.clone(),
        })
        .reply_timeout(recovery_departed::PROBE_TIMEOUT)
        .await
    {
        Ok(generation) => generation,
        Err(error) => {
            tracing::debug!(%occupant, ?error, "ghost probe could not pin the occupancy generation");
            None
        }
    }
}

/// Whether some other node can still reach this exact full JID. An
/// indeterminate read answers `true`: an eviction is not reversible.
async fn reachable_on_another_node(deps: &Deps<'_>, occupant: &FullJid) -> bool {
    registered_resource(deps, occupant).await || foreign_claim(deps, occupant).await
}

/// Whether the authoritative actor tree lists this exact resource. That covers
/// a live local socket and the registered-remote mirror a clustered peer
/// installs for a socket it hosts. The non-degrading lookup is deliberate: the
/// routing variant reports an unanswered actor as "no resources", which is the
/// right default for a route and the wrong one for an eviction.
async fn registered_resource(deps: &Deps<'_>, occupant: &FullJid) -> bool {
    let Some(registry) = deps.user_registry else {
        return true;
    };
    match waddle_xmpp::registry::try_get_resources_for_user(registry, &occupant.to_bare()).await {
        Ok(resources) => resources.contains(occupant),
        Err(error) => {
            tracing::debug!(%occupant, %error, "ghost probe could not read the user's resources");
            true
        }
    }
}

/// Whether a fresh `UserActor` claim for this occupant's account is held by
/// another node, decided exactly as live routing decides it — plus the one
/// difference an eviction needs: a claim read that fails is not proof.
#[cfg(feature = "clustering")]
async fn foreign_claim(deps: &Deps<'_>, occupant: &FullJid) -> bool {
    let Some(state) = deps.web_socket_state else {
        return true;
    };
    let handles = &state.deps.app_state.clustering_claims;
    let (Some(store), Some(identity)) = (&handles.claim_store, &handles.node_identity) else {
        // No cluster claim authority is configured, so no node can hold one.
        return false;
    };
    let entity = waddle_xmpp::ownership::Entity::new(
        waddle_xmpp::ownership::EntityType::UserActor,
        occupant.to_bare().to_string(),
    );
    match store.current_claim(&entity).await {
        Ok(Some(claim)) => claim.owner_lease_fresh && claim.owner != identity.current(),
        Ok(None) => false,
        Err(error) => {
            tracing::debug!(%occupant, %error, "ghost probe could not read the ownership claim");
            true
        }
    }
}

/// Cluster claims exist only behind the `clustering` feature; without it this
/// node is the only one there is.
#[cfg(not(feature = "clustering"))]
async fn foreign_claim(_deps: &Deps<'_>, _occupant: &FullJid) -> bool {
    false
}

/// Remove the occupancy through the one full-JID leave sweep every other
/// departure path uses, so XEP-0045 §7.14 presence, SFU teardown, empty-room
/// eviction and failure retention stay in a single place.
async fn evict(
    state: &WebSocketState,
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
    redrive_local_muc_cleanup(
        state,
        occupant,
        LeaveSessionSelector::Generation(generation),
        LeaveAttemptId::generate(),
        remote_ceiling,
    )
    .await
        == MucCleanupOutcome::Completed
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
