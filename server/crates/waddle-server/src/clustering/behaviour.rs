//! The owned libp2p `NetworkBehaviour` for the clustering swarm.
//!
//! We compose `kameo::remote::Behaviour` into a behaviour we build and drive
//! ourselves, rather than using `kameo::remote::bootstrap()` (an mDNS
//! development helper). This is what lets us own the transports, the event
//! loop, peer dialing, the peer allowlist, and — in a later slice — the
//! per-peer relay actors.

use kameo::remote;
use libp2p::swarm::NetworkBehaviour;
use libp2p::PeerId;
use libp2p::{
    allow_block_list::{self, AllowedPeers},
    connection_limits,
};
use std::collections::HashSet;

/// One inbound plus one outbound transport is the normal simultaneous-dial
/// convergence shape. A third connection cannot improve reachability and
/// would multiply the per-connection remote-messaging budget.
const MAX_ESTABLISHED_CONNECTIONS_PER_PEER: u32 = 2;

// `derive(NetworkBehaviour)` auto-generates the out-event enum named after the
// struct — `WaddleBehaviourEvent` — with one variant per field.
#[derive(NetworkBehaviour)]
pub struct WaddleBehaviour {
    /// Peer authorization (ADR element 3): connections from peers not on the
    /// enrolled allowlist are denied at establishment, and `disallow_peer`
    /// closes live connections — completing the Noise handshake is necessary
    /// but never sufficient. Composed FIRST so its deny verdict is reached
    /// before any other behaviour handles the connection (denial by any
    /// composed behaviour denies the connection regardless of order; the
    /// position is for clarity).
    pub allowed: allow_block_list::Behaviour<AllowedPeers>,
    /// Bound transport fan-out from one enrolled PeerId while retaining the
    /// two connections required when both endpoints dial simultaneously.
    pub connection_limits: connection_limits::Behaviour,
    /// kameo remote actors: request-response messaging plus the kademlia-based
    /// registry (demoted to node discovery only this phase).
    pub kameo: remote::Behaviour,
}

impl WaddleBehaviour {
    /// Build the behaviour for `local_peer_id` with the given kameo messaging
    /// limits and the initially enrolled peer set.
    pub fn new(
        local_peer_id: PeerId,
        messaging_config: remote::messaging::Config,
        enrolled: &HashSet<PeerId>,
    ) -> Self {
        let mut allowed = allow_block_list::Behaviour::<AllowedPeers>::default();
        for peer in enrolled {
            allowed.allow_peer(*peer);
        }
        Self {
            allowed,
            connection_limits: connection_limits::Behaviour::new(
                connection_limits::ConnectionLimits::default()
                    .with_max_established_per_peer(Some(MAX_ESTABLISHED_CONNECTIONS_PER_PEER)),
            ),
            kameo: remote::Behaviour::new(local_peer_id, messaging_config),
        }
    }
}
