//! #1803 asking side: "can ANY other node in this cluster still reach this
//! exact full JID?"
//!
//! The ghost-occupant guard used to ask only the node holding the account's
//! `UserActor` claim. That was wrong: a claim is ROUTING AUTHORITY, not socket
//! liveness. A live, idle socket on node B is known only to B's own
//! `ConnectionRegistry` — nothing re-registers it anywhere when the account's
//! claim owner dies or moves — so the very condition that stalls delivery
//! (the claim owner going away) also defeated every claim-shaped guard, and a
//! LIVE user could be torn out of every room.
//!
//! The question is therefore asked of every cluster member that has not been
//! committed-expired, and each of them answers about its OWN sockets and
//! sessions, which it is unconditionally authoritative about. The occupant is
//! unreachable elsewhere ONLY IF the membership read succeeded and every peer
//! answered [`RelayResourcePresenceReply::Absent`].
//!
//! Every failure is fail-closed: a membership read that errors or is
//! truncated, any per-peer `Err` (an old peer answering `UnknownMessage`, a
//! timeout, a transport or decode failure), and the overall budget elapsing
//! all leave the occupant seated and its copy owed.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use jid::FullJid;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::{ClaimError, NodeIdentity, SharedNodeIdentity};

use super::claims::NodeLeaseStore;
use super::relay::{
    RelayAskError, RelayHandle, RelayResourcePresenceReply, RelaySendEffect, RelaySendFailure,
};
use super::NodeId;

/// Overall bound on one cross-node presence hop, kademlia name resolution
/// included (that resolution sits OUTSIDE the per-ask timeouts and can burn
/// its own backoff budget). Deliberately tight: this runs inside one bounded
/// maintenance recovery attempt on a row that is already stalled, and an
/// answer that arrives late is worth nothing — the next stalled pass re-asks
/// against a warm relay cache.
const RESOURCE_PRESENCE_ASK_TIMEOUT: Duration = Duration::from_secs(1);
/// Per-ask mailbox/reply split inside [`RESOURCE_PRESENCE_ASK_TIMEOUT`], far
/// under the clustering defaults (5s/20s) for the same reason.
const RESOURCE_PRESENCE_MAILBOX_TIMEOUT: Duration = Duration::from_millis(250);
const RESOURCE_PRESENCE_REPLY_TIMEOUT: Duration = Duration::from_millis(750);

/// Page bound on the membership read. Waddle runs two replicas; a cluster that
/// somehow shows more unexpired rows than this is a control plane nobody
/// should be evicting occupants on, so a full page is treated as "the
/// membership read did not answer".
const MEMBERSHIP_PAGE_LIMIT: usize = 64;

/// Ask one node about one exact resource.
#[async_trait]
pub trait ResourcePresenceAsker: Send + Sync {
    /// `Ok(Absent)` is the only answer that proves the resource is gone on
    /// that peer. Every `Err` means "could not prove absence".
    async fn resource_presence(
        &self,
        peer: &NodeIdentity,
        target: &FullJid,
    ) -> Result<RelayResourcePresenceReply, RelayAskError>;
}

/// The cluster members a ghost eviction must clear with first.
#[async_trait]
pub trait ClusterMembership: Send + Sync {
    /// Every node other than this one that the control plane has not
    /// committed-expired. An empty list means this node is the whole
    /// cluster. An `Err` means the membership is unknown, which the caller
    /// must treat as "the occupant may be reachable".
    async fn peers(&self) -> Result<Vec<NodeIdentity>, ClaimError>;
}

/// Whether the cluster proved that no OTHER node can reach the resource.
///
/// `PresentOnAPeer` and `Unproven` are kept apart on purpose: the first is a
/// STABLE fact about the cluster as it is right now (a peer is holding that
/// socket and will deliver to it), the second is a FAILURE TO READ. Callers
/// classify a recovery attempt differently on each — see
/// `ingress::recovery_reachability::ResourceReachability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerResourceReachability {
    /// The membership read succeeded and every peer in it answered `Absent`
    /// (including the degenerate single-node case: no peers to ask). The only
    /// outcome that permits an eviction.
    AbsentOnEveryPeer,
    /// Some peer answered `Present`: it is holding that exact resource.
    PresentOnAPeer,
    /// An ask failed, the membership read failed or was truncated, or the
    /// fan-out budget elapsed.
    Unproven,
}

/// Ask every unexpired peer about `target`, concurrently, inside `budget`.
/// Fails closed on every error path.
///
/// `budget` covers the membership read plus every ask, which run
/// concurrently. It is the CALLER's policy, because the two callers run under
/// very different deadlines: ghost repair runs outside the per-row execution
/// deadline and can afford to wait out a slow peer, while the departed-copy
/// settlement runs inside it and must leave time for the rebuild.
pub async fn peer_resource_reachability(
    membership: &dyn ClusterMembership,
    asker: &dyn ResourcePresenceAsker,
    target: &FullJid,
    budget: Duration,
) -> PeerResourceReachability {
    match tokio::time::timeout(budget, fan_out(membership, asker, target)).await {
        Ok(reachability) => reachability,
        Err(_elapsed) => {
            tracing::debug!(%target, ?budget, "resource-presence fan-out exceeded its budget");
            PeerResourceReachability::Unproven
        }
    }
}

async fn fan_out(
    membership: &dyn ClusterMembership,
    asker: &dyn ResourcePresenceAsker,
    target: &FullJid,
) -> PeerResourceReachability {
    let peers = match membership.peers().await {
        Ok(peers) => peers,
        Err(error) => {
            tracing::debug!(%target, %error, "resource-presence probe could not read the cluster membership");
            return PeerResourceReachability::Unproven;
        }
    };
    // No other node exists, so nothing else can be holding the socket.
    if peers.is_empty() {
        return PeerResourceReachability::AbsentOnEveryPeer;
    }
    let mut asks: FuturesUnordered<_> = peers
        .iter()
        .map(|peer| async move { (peer, asker.resource_presence(peer, target).await) })
        .collect();
    while let Some((peer, reply)) = asks.next().await {
        match reply {
            Ok(RelayResourcePresenceReply::Absent) => {}
            Ok(RelayResourcePresenceReply::Present) => {
                tracing::debug!(
                    %target,
                    peer = %peer.node_id,
                    "a cluster peer still knows the resource"
                );
                return PeerResourceReachability::PresentOnAPeer;
            }
            Err(error) => {
                tracing::debug!(
                    %target,
                    peer = %peer.node_id,
                    %error,
                    "resource-presence probe could not ask a cluster peer about the resource"
                );
                return PeerResourceReachability::Unproven;
            }
        }
    }
    PeerResourceReachability::AbsentOnEveryPeer
}

/// Production membership: the `clustering_nodes` liveness rows, filtered to
/// the nodes the control plane has not committed-expired. See
/// [`NodeLeaseStore::list_other_unexpired_nodes`] for why that predicate — and
/// not the isolation heuristic's stricter "live" one — is the right
/// over-approximation for an irreversible eviction.
pub struct NodeLeaseClusterMembership {
    lease: Arc<dyn NodeLeaseStore>,
    identity: SharedNodeIdentity,
}

impl NodeLeaseClusterMembership {
    pub fn new(lease: Arc<dyn NodeLeaseStore>, identity: SharedNodeIdentity) -> Arc<Self> {
        Arc::new(Self { lease, identity })
    }
}

#[async_trait]
impl ClusterMembership for NodeLeaseClusterMembership {
    async fn peers(&self) -> Result<Vec<NodeIdentity>, ClaimError> {
        let me = self.identity.current();
        let peers = self
            .lease
            .list_other_unexpired_nodes(&me, MEMBERSHIP_PAGE_LIMIT)
            .await?;
        if peers.len() >= MEMBERSHIP_PAGE_LIMIT {
            // A truncated page would silently shrink the set an eviction has
            // to clear with, so refuse to answer at all.
            return Err(ClaimError::Backend(format!(
                "cluster membership exceeded the {MEMBERSHIP_PAGE_LIMIT}-row probe bound"
            )));
        }
        Ok(peers)
    }
}

/// Production implementation: one bounded [`RelayHandle`] ask per peer,
/// resolved through kademlia exactly like the #1594 webhook relay hop.
pub struct RelayResourcePresenceAsker {
    stop_token: CancellationToken,
}

impl RelayResourcePresenceAsker {
    pub fn new(stop_token: CancellationToken) -> Arc<Self> {
        Arc::new(Self { stop_token })
    }
}

#[async_trait]
impl ResourcePresenceAsker for RelayResourcePresenceAsker {
    async fn resource_presence(
        &self,
        peer: &NodeIdentity,
        target: &FullJid,
    ) -> Result<RelayResourcePresenceReply, RelayAskError> {
        let mut relay =
            RelayHandle::new(NodeId::new(peer.node_id.clone()), self.stop_token.clone())
                .with_ask_timeouts(
                    RESOURCE_PRESENCE_MAILBOX_TIMEOUT,
                    RESOURCE_PRESENCE_REPLY_TIMEOUT,
                );
        match tokio::time::timeout(
            RESOURCE_PRESENCE_ASK_TIMEOUT,
            relay.resource_presence(target.clone()),
        )
        .await
        {
            Ok(result) => result,
            // The receiver's handler is read-only, so an elapsed overall
            // budget has no effect to reconcile — it is simply "no answer".
            Err(_elapsed) => Err(RelayAskError::Send {
                failure: RelaySendFailure::ReplyTimeout,
                effect: RelaySendEffect::NoEffect,
                message: "resource-presence ask exceeded its overall budget".to_string(),
            }),
        }
    }
}

#[cfg(test)]
#[path = "resource_presence_tests.rs"]
mod tests;
