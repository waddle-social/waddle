//! The owned libp2p `NetworkBehaviour` for the clustering swarm.
//!
//! We compose `kameo::remote::Behaviour` into a behaviour we build and drive
//! ourselves, rather than using `kameo::remote::bootstrap()` (an mDNS
//! development helper). This is what lets us own the transports, the event
//! loop, peer dialing, the peer allowlist, and — in a later slice — the
//! per-peer relay actors.

use kameo::remote;
use libp2p::allow_block_list::{self, AllowedPeers};
use libp2p::swarm::NetworkBehaviour;
use libp2p::PeerId;
use std::collections::HashSet;

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
            kameo: remote::Behaviour::new(local_peer_id, messaging_config),
        }
    }
}
