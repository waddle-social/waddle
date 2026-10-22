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
    /// Every unexpired peer denied this resource (including the degenerate
    /// single-node case). Combined with local absence, this is the only
    /// outcome that permits taking something away.
    AbsentEverywhere,
    /// A peer holds a socket or resumable session for this exact resource.
    /// A stable fact about the cluster, rather than a failure to read it.
    #[cfg(feature = "clustering")]
    Reachable,
    /// A probe could not answer: the membership read failed or was truncated,
    /// a peer ask failed (`UnknownMessage` mid-rolling-update, a timeout,
    /// a transport or decode
    /// failure), or the fan-out budget elapsed. TRANSIENT.
    #[cfg(feature = "clustering")]
    Unproven,
}

/// Whether THIS node can still hand the occupant its copy itself.
///
/// A missing SM registry cannot prove anything, so it also counts as
/// reachable. A registered-remote mirror is routing state, not evidence
/// that the socket still exists: only its hosting peer can answer that.
pub(super) async fn locally_reachable(deps: &Deps<'_>, occupant: &FullJid) -> bool {
    if deps
        .connection_registry
        .get_entry(occupant)
        .is_some_and(|entry| entry.is_locally_hosted())
    {
        return true;
    }
    let Some(sm) = deps.sm_session_registry else {
        return true;
    };
    match sm.probe_resumable_session_for_full_jid(occupant).await {
        // Checks this node's memory AND the shared durable store, so a
        // session resume-stolen by another node still counts as resumable.
        ResumableSessionProbe::Present | ResumableSessionProbe::Failed => true,
        // Resume publishes the local socket before deleting its detached
        // session. The asynchronous session read may overlap that transition.
        ResumableSessionProbe::Absent => deps
            .connection_registry
            .get_entry(occupant)
            .is_some_and(|entry| entry.is_locally_hosted()),
    }
}

/// Whether anything OTHER than this node's own socket/session state can still
/// reach the exact resource: every unexpired cluster peer must answer about
/// its own sockets and sessions. Neither an actor-tree entry nor a remote
/// connection mirror can vouch for another node's socket.
pub(super) async fn reachable_elsewhere(
    deps: &Deps<'_>,
    occupant: &FullJid,
    budget: Duration,
) -> ResourceReachability {
    reachable_on_a_peer(deps, occupant, budget).await
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
