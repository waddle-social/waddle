//! #1803: "can anything at all still hand this exact full JID its copy?"
//!
//! The one answer both irreversible decisions rest on — evicting a ghost
//! occupancy (`recovery_ghosts`) and settling a frozen copy the room no longer
//! lists (`recovery_departed`). Both permanently take something away, so both
//! ask the same question with the same fail-closed rules; keeping it in one
//! place is what stops the two paths drifting apart, which is exactly how the
//! settlement came to trust a purely LOCAL answer while the eviction already
//! asked the whole cluster.
//!
//! Reachability is proven against SOCKETS, never against ownership claims: a
//! live idle socket is known only to the node holding it, and nothing
//! re-registers it anywhere when the account's claim owner dies or moves.

use std::time::Duration;

use jid::FullJid;
use waddle_xmpp::stream_management::ResumableSessionProbe;

use crate::server::routes::interpret::Deps;

/// Fan-out budget for ghost repair. It runs after maintenance has already
/// classified the row as stalled, outside the per-row execution deadline, so
/// it can afford to wait out a slow peer: half a second over one peer's own
/// one-second ask budget, so a single slow peer resolves inside it rather
/// than being cut off by it.
pub(super) const GHOST_FANOUT_BUDGET: Duration = Duration::from_millis(1_500);

/// Fan-out budget for the departed-copy settlement, which runs INSIDE
/// `recover_row`'s ~1 s row deadline and before the rebuild that still has to
/// execute this row's effects. A budget that outran the row deadline would be
/// cancelled mid-flight and lose the whole attempt's work, so the settlement
/// takes the tighter bound and simply leaves the copy owed when it elapses —
/// the next pass re-asks against a warm relay cache.
pub(super) const SETTLEMENT_FANOUT_BUDGET: Duration = Duration::from_millis(500);

/// Where else this exact resource can still be reached.
///
/// The three outcomes are deliberately distinct, because the caller's
/// *classification* of the attempt depends on which one it got — see
/// `recovery_departed::settle_departed_occupants`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResourceReachability {
    /// Nothing anywhere holds this resource: this node's actor tree does not
    /// list it and every unexpired peer denied it (including the degenerate
    /// single-node case). The only outcome that permits taking something
    /// away.
    AbsentEverywhere,
    /// Something holds it — this node's actor tree (a local socket, or a
    /// registered-remote mirror for a peer's socket), or a peer that answered
    /// `Present`. A STABLE fact about the cluster as it is right now, not a
    /// failure to read it.
    Reachable,
    /// A probe could not answer: the actor tree read failed, the membership
    /// read failed or was truncated, a peer ask failed (an old replica's
    /// `UnknownMessage` mid-rolling-update, a timeout, a transport or decode
    /// failure), or the fan-out budget elapsed. TRANSIENT.
    Unproven,
}

/// Whether THIS node can still hand the occupant its copy itself.
///
/// A missing SM registry cannot prove anything, so it also counts as
/// reachable. Note that `is_connected` is also true for the clustered
/// registered-remote mirror of a peer's socket, which this node cannot
/// actually deliver to — [`ResourceReachability`] is what distinguishes the
/// two, and either way the copy stays owed.
pub(super) async fn locally_reachable(deps: &Deps<'_>, occupant: &FullJid) -> bool {
    if deps.connection_registry.is_connected(occupant) {
        return true;
    }
    let Some(sm) = deps.sm_session_registry else {
        return true;
    };
    match sm.probe_resumable_session_for_full_jid(occupant).await {
        // Checks this node's memory AND the shared durable store, so a
        // session resume-stolen by another node still counts as resumable.
        ResumableSessionProbe::Present | ResumableSessionProbe::Failed => true,
        ResumableSessionProbe::Absent => false,
    }
}

/// Whether anything OTHER than this node's own socket/session state can still
/// reach the exact resource: this node's authoritative actor tree first (it is
/// free and answers locally), then every unexpired cluster peer.
pub(super) async fn reachable_elsewhere(
    deps: &Deps<'_>,
    occupant: &FullJid,
    budget: Duration,
) -> ResourceReachability {
    match registered_resource(deps, occupant).await {
        ResourceReachability::AbsentEverywhere => {}
        decided => return decided,
    }
    reachable_on_a_peer(deps, occupant, budget).await
}

/// Whether the authoritative actor tree lists this exact resource. That covers
/// a live local socket and the registered-remote mirror a clustered peer
/// installs for a socket it hosts. The non-degrading lookup is deliberate: the
/// routing variant reports an unanswered actor as "no resources", which is the
/// right default for a route and the wrong one for an irreversible decision.
async fn registered_resource(deps: &Deps<'_>, occupant: &FullJid) -> ResourceReachability {
    let Some(registry) = deps.user_registry else {
        return ResourceReachability::Unproven;
    };
    match waddle_xmpp::registry::try_get_resources_for_user(registry, &occupant.to_bare()).await {
        Ok(resources) if resources.contains(occupant) => ResourceReachability::Reachable,
        Ok(_) => ResourceReachability::AbsentEverywhere,
        Err(error) => {
            tracing::debug!(%occupant, %error, "reachability probe could not read the user's resources");
            ResourceReachability::Unproven
        }
    }
}

/// Whether any OTHER cluster node can still reach this exact RESOURCE.
///
/// The question is asked of every peer the control plane has not
/// committed-expired, never of an ownership claim. A claim is per account and
/// is routing authority, not socket liveness: in the production shape the
/// room is hosted here while the user's claim sits elsewhere because a
/// healthy `web-<uuid>` resource lives there, and a second, abandoned
/// resource pins the row forever (#1803) — but in the symmetric failure the
/// claim's owner is exactly what died, leaving a LIVE socket on a peer that
/// no claim row names. Only that peer knows about it, so only that peer can
/// be asked.
#[cfg(feature = "clustering")]
async fn reachable_on_a_peer(
    deps: &Deps<'_>,
    occupant: &FullJid,
    budget: Duration,
) -> ResourceReachability {
    use crate::clustering::resource_presence::{
        peer_resource_reachability, PeerResourceReachability,
    };

    let Some(state) = deps.web_socket_state else {
        return ResourceReachability::Unproven;
    };
    let handles = &state.deps.app_state.clustering_claims;
    let (membership, asker) = match (&handles.cluster_membership, &handles.resource_presence) {
        // No clustering is configured at all, so this node is the whole
        // cluster and there is no peer that could hold the socket.
        (None, None) => return ResourceReachability::AbsentEverywhere,
        (Some(membership), Some(asker)) => (membership, asker),
        // Half-wired: the question cannot be asked, so nothing is proven.
        _ => return ResourceReachability::Unproven,
    };
    match peer_resource_reachability(membership.as_ref(), asker.as_ref(), occupant, budget).await {
        PeerResourceReachability::AbsentOnEveryPeer => ResourceReachability::AbsentEverywhere,
        PeerResourceReachability::PresentOnAPeer => ResourceReachability::Reachable,
        PeerResourceReachability::Unproven => ResourceReachability::Unproven,
    }
}

/// Cluster peers exist only behind the `clustering` feature; without it this
/// node is the only one there is.
#[cfg(not(feature = "clustering"))]
async fn reachable_on_a_peer(
    _deps: &Deps<'_>,
    _occupant: &FullJid,
    _budget: Duration,
) -> ResourceReachability {
    ResourceReachability::AbsentEverywhere
}
