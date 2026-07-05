//! The owned libp2p `NetworkBehaviour` for the clustering swarm.
//!
//! We compose `kameo::remote::Behaviour` into a behaviour we build and drive
//! ourselves, rather than using `kameo::remote::bootstrap()` (an mDNS
//! development helper). This is what lets us own the transports, the event
//! loop, peer dialing, and — in later slices — the allowlist and per-peer
//! relay actors.

use kameo::remote;
use libp2p::swarm::NetworkBehaviour;
use libp2p::PeerId;

/// Waddle's composed swarm behaviour.
///
/// Phase 2 carries only kameo's remote behaviour (messaging + kademlia
/// registry). The `derive(NetworkBehaviour)` macro generates the
/// [`WaddleBehaviourEvent`] enum (one `Kameo(remote::Event)` variant) and the
/// combined connection handler.
// `derive(NetworkBehaviour)` auto-generates the out-event enum named after the
// struct — `WaddleBehaviourEvent` — with one `Kameo(remote::Event)` variant.
#[derive(NetworkBehaviour)]
pub struct WaddleBehaviour {
    /// kameo remote actors: request-response messaging plus the kademlia-based
    /// registry (demoted to node discovery only this phase).
    pub kameo: remote::Behaviour,
}

impl WaddleBehaviour {
    /// Build the behaviour for `local_peer_id` with the given kameo messaging
    /// limits (request timeout, concurrent-stream cap, envelope size maxima).
    pub fn new(local_peer_id: PeerId, messaging_config: remote::messaging::Config) -> Self {
        Self {
            kameo: remote::Behaviour::new(local_peer_id, messaging_config),
        }
    }
}
