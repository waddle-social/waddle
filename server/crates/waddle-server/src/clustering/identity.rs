//! Per-node libp2p identity for the clustering swarm.
//!
//! Two paths:
//! - **Leased pool identity** (production shape, ADR element 3): the node
//!   leases one slot of the pre-enrolled keypair pool via a Postgres CAS and
//!   uses that slot's keypair, giving a stable, revocable, per-pod identity
//!   with at most one live holder per keypair.
//! - **Ephemeral** fallback: when no pool is configured, a fresh ed25519
//!   keypair is generated per process — fine for the discovery spike and
//!   tests, but not a stable identity.

use base64::Engine;
use libp2p::identity::{ed25519, Keypair};

/// Length of an ed25519 secret key seed in bytes.
const ED25519_SECRET_LEN: usize = 32;

/// Generate a fresh ephemeral ed25519 keypair (no pool configured).
pub fn ephemeral_keypair() -> Keypair {
    Keypair::generate_ed25519()
}

/// Decode one enrolled keypair-pool entry (base64-encoded 32-byte ed25519
/// secret key) into a libp2p `Keypair`.
pub fn keypair_from_pool_entry(entry: &str) -> Result<Keypair, IdentityError> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(entry.trim())
        .map_err(|error| IdentityError::Decode(error.to_string()))?;
    let mut seed: [u8; ED25519_SECRET_LEN] = raw.as_slice().try_into().map_err(|_| {
        IdentityError::Decode(format!(
            "expected a {ED25519_SECRET_LEN}-byte ed25519 secret, got {} bytes",
            raw.len()
        ))
    })?;
    // `try_from_bytes` zeroizes the input slice, so `seed` is cleared after.
    let secret = ed25519::SecretKey::try_from_bytes(&mut seed)
        .map_err(|error| IdentityError::Decode(error.to_string()))?;
    Ok(Keypair::from(ed25519::Keypair::from(secret)))
}

/// Failures decoding an enrolled keypair-pool entry.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("failed to decode enrolled keypair pool entry: {0}")]
    Decode(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_generated_ed25519_seed() {
        // Take a real ed25519 secret, base64 it, and confirm decode yields the
        // same PeerId.
        let original = ed25519::Keypair::generate();
        let seed = original.secret().as_ref().to_vec();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&seed);

        let decoded = keypair_from_pool_entry(&b64).expect("decodes");
        let expected = Keypair::from(original).public().to_peer_id();
        assert_eq!(decoded.public().to_peer_id(), expected);
    }

    #[test]
    fn rejects_wrong_length_seed() {
        let b64 = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
        assert!(matches!(
            keypair_from_pool_entry(&b64),
            Err(IdentityError::Decode(_))
        ));
    }

    #[test]
    fn rejects_non_base64() {
        assert!(matches!(
            keypair_from_pool_entry("not valid base64!!!"),
            Err(IdentityError::Decode(_))
        ));
    }
}
