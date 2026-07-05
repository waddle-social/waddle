//! Per-node libp2p identity for the clustering swarm.
//!
//! Phase 2 Slice 1 uses an **ephemeral** ed25519 keypair generated at
//! startup — sufficient for the node-discovery spike, where a fresh `PeerId`
//! per process is harmless. Slice 2 replaces this with a keypair leased from a
//! pre-enrolled pool via a Postgres CAS (ADR element 3), giving a stable,
//! revocable, per-pod identity (and a fallible acquisition path).

use libp2p::identity::Keypair;

/// Produce the node's libp2p keypair.
///
/// Currently generates a fresh ed25519 keypair each start. Slice 2 turns this
/// into a fallible pool-lease acquisition.
pub fn node_keypair() -> Keypair {
    Keypair::generate_ed25519()
}
