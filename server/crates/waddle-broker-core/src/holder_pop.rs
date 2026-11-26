//! Cryptographic proof-of-possession binding an ephemeral process incarnation
//! key (`Ed25519`) to an mTLS peer SPIFFE identity and RFC 9266 `tls-exporter`
//! 32-byte channel binding value.
//!
//! Two pods in the same Kubernetes ServiceAccount share the same `SpiffeId` and
//! `CellRole`, so mTLS alone cannot distinguish a stale replica from a new
//! leaseholder. Signing a domain-separated, length-prefixed transcript over the
//! peer SPIFFE ID, incarnation public key, and per-handshake `tls-exporter`
//! channel binding guarantees that a stale pod cannot forge or replay another
//! pod's proof on its own TLS connection.

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use thiserror::Error;

use crate::identity::{HolderInstance, IncarnationPubKey, SpiffeId, TlsExporterProof};

/// RFC 9266 §2 TLS 1.3 channel-binding exporter label.
pub const RFC9266_TLS_EXPORTER_LABEL: &[u8] = b"EXPORTER-Channel-Binding";

/// Fixed 32-byte RFC 9266 `tls-exporter` output length.
pub const TLS_EXPORTER_CHANNEL_BINDING_LEN: usize = 32;

/// Domain separation prefix for holder proof-of-possession transcripts.
pub const HOLDER_POP_DOMAIN_SEPARATOR: &[u8] = b"waddle.holder.pop.v1\0";

/// Exact byte length of an Ed25519 signature.
pub const ED25519_SIGNATURE_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HolderPopError {
    #[error("failed to generate ephemeral Ed25519 incarnation keypair")]
    KeyGenerationFailed,
    #[error("claimed SPIFFE ID does not match verified mTLS peer SPIFFE ID")]
    SpiffeIdMismatch,
    #[error("invalid Ed25519 proof length: expected {ED25519_SIGNATURE_LEN} bytes, got {0}")]
    InvalidProofLength(usize),
    #[error("channel-bound holder signature verification failed")]
    InvalidSignature,
}

/// Ephemeral Ed25519 keypair generated fresh per cell/broker process incarnation.
pub struct HolderKeypair {
    keypair: Ed25519KeyPair,
    pubkey: IncarnationPubKey,
}

impl std::fmt::Debug for HolderKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolderKeypair")
            .field("pubkey", &hex::encode(self.pubkey.as_bytes()))
            .finish_non_exhaustive()
    }
}

impl HolderKeypair {
    /// Generate a fresh ephemeral Ed25519 incarnation keypair using OS CSPRNG.
    pub fn generate() -> Result<Self, HolderPopError> {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| HolderPopError::KeyGenerationFailed)?;
        let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
            .map_err(|_| HolderPopError::KeyGenerationFailed)?;
        let raw_pub: [u8; 32] = keypair
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| HolderPopError::KeyGenerationFailed)?;
        Ok(Self {
            keypair,
            pubkey: IncarnationPubKey::from_bytes(raw_pub),
        })
    }

    pub const fn public_key(&self) -> IncarnationPubKey {
        self.pubkey
    }

    /// Sign the canonical RFC 9266 channel-binding transcript for `spiffe_id`
    /// and `channel_binding` to construct a [`HolderInstance`] proof.
    pub fn sign_channel_binding(
        &self,
        spiffe_id: &SpiffeId,
        channel_binding: &[u8; TLS_EXPORTER_CHANNEL_BINDING_LEN],
    ) -> HolderInstance {
        let transcript = build_holder_pop_transcript(spiffe_id, &self.pubkey, channel_binding);
        let sig = self.keypair.sign(&transcript);
        let proof = TlsExporterProof::new(sig.as_ref().to_vec())
            .expect("Ed25519 signature is 64 bytes <= MAX_TLS_EXPORTER_PROOF_BYTES");
        HolderInstance {
            spiffe_id: spiffe_id.clone(),
            incarnation_pubkey: self.pubkey,
            tls_exporter_proof: proof,
        }
    }
}

/// A [`HolderInstance`] whose `spiffe_id` matched the verified mTLS leaf
/// certificate and whose Ed25519 signature verified over the server's own
/// RFC 9266 `tls-exporter` channel binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedHolderInstance {
    holder: HolderInstance,
}

impl VerifiedHolderInstance {
    pub const fn holder(&self) -> &HolderInstance {
        &self.holder
    }

    pub const fn spiffe_id(&self) -> &SpiffeId {
        &self.holder.spiffe_id
    }

    pub const fn incarnation_pubkey(&self) -> IncarnationPubKey {
        self.holder.incarnation_pubkey
    }

    /// Secret-free holder identifier for structured PObserve traces:
    /// `<spiffe_id>#<first-16-hex-chars-of-pubkey>`. Never includes proof bytes.
    pub fn trace_holder_key(&self) -> String {
        format_trace_holder_key(&self.holder)
    }

    pub fn into_inner(self) -> HolderInstance {
        self.holder
    }
}

/// Format a secret-free holder identifier (`<spiffe_id>#<pubkey-prefix-hex>`)
/// suitable for PObserve traces and logs. Never includes `tls_exporter_proof`.
pub fn format_trace_holder_key(holder: &HolderInstance) -> String {
    format!(
        "{}#{}",
        holder.spiffe_id.as_str(),
        hex::encode(&holder.incarnation_pubkey.as_bytes()[..8])
    )
}

/// Construct the canonical domain-separated, length-prefixed PoP transcript:
/// `b"waddle.holder.pop.v1\0" || len(spiffe_id):u32be || spiffe_id || pubkey(32) || channel_binding(32)`.
pub fn build_holder_pop_transcript(
    spiffe_id: &SpiffeId,
    incarnation_pubkey: &IncarnationPubKey,
    channel_binding: &[u8; TLS_EXPORTER_CHANNEL_BINDING_LEN],
) -> Vec<u8> {
    let spiffe_bytes = spiffe_id.as_str().as_bytes();
    let spiffe_len = u32::try_from(spiffe_bytes.len()).unwrap_or(u32::MAX);
    let mut transcript =
        Vec::with_capacity(HOLDER_POP_DOMAIN_SEPARATOR.len() + 4 + spiffe_bytes.len() + 32 + 32);
    transcript.extend_from_slice(HOLDER_POP_DOMAIN_SEPARATOR);
    transcript.extend_from_slice(&spiffe_len.to_be_bytes());
    transcript.extend_from_slice(spiffe_bytes);
    transcript.extend_from_slice(incarnation_pubkey.as_bytes());
    transcript.extend_from_slice(channel_binding);
    transcript
}

/// Verify that `claimed` matches `peer_spiffe_id` (extracted from the verified
/// mTLS leaf certificate) and carries a valid Ed25519 signature under
/// `claimed.incarnation_pubkey` over the server's own RFC 9266 `channel_binding`.
pub fn verify_channel_bound_holder(
    peer_spiffe_id: &SpiffeId,
    server_channel_binding: &[u8; TLS_EXPORTER_CHANNEL_BINDING_LEN],
    claimed: &HolderInstance,
) -> Result<VerifiedHolderInstance, HolderPopError> {
    if &claimed.spiffe_id != peer_spiffe_id {
        return Err(HolderPopError::SpiffeIdMismatch);
    }
    let proof_bytes = claimed.tls_exporter_proof.as_bytes();
    if proof_bytes.len() != ED25519_SIGNATURE_LEN {
        return Err(HolderPopError::InvalidProofLength(proof_bytes.len()));
    }
    let transcript = build_holder_pop_transcript(
        peer_spiffe_id,
        &claimed.incarnation_pubkey,
        server_channel_binding,
    );
    let verifier = UnparsedPublicKey::new(&ED25519, claimed.incarnation_pubkey.as_bytes());
    verifier
        .verify(&transcript, proof_bytes)
        .map_err(|_| HolderPopError::InvalidSignature)?;

    Ok(VerifiedHolderInstance {
        holder: HolderInstance {
            spiffe_id: peer_spiffe_id.clone(),
            incarnation_pubkey: claimed.incarnation_pubkey,
            tls_exporter_proof: claimed.tls_exporter_proof.clone(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_channel_binding_proof_verifies_and_rejects_replay_and_forgery() {
        let spiffe = SpiffeId::parse("spiffe://waddle.internal/ns/cells/sa/shared-pool")
            .expect("valid spiffe");
        let other_spiffe = SpiffeId::parse("spiffe://waddle.internal/ns/cells/sa/dedicated-acme")
            .expect("valid spiffe");

        let kp_a = HolderKeypair::generate().expect("keypair A");
        let kp_b = HolderKeypair::generate().expect("keypair B");
        let cb_a = [0x11u8; 32];
        let cb_b = [0x22u8; 32];

        let holder_a = kp_a.sign_channel_binding(&spiffe, &cb_a);
        let verified = verify_channel_bound_holder(&spiffe, &cb_a, &holder_a)
            .expect("matching channel binding must verify");
        assert_eq!(verified.incarnation_pubkey(), kp_a.public_key());

        // Replay of holder A's proof over holder B's TLS channel must fail.
        let replay_err = verify_channel_bound_holder(&spiffe, &cb_b, &holder_a)
            .expect_err("replayed proof on different TLS channel must fail");
        assert_eq!(replay_err, HolderPopError::InvalidSignature);

        // Claiming a different SPIFFE ID than the mTLS peer cert must fail.
        let spiffe_err = verify_channel_bound_holder(&other_spiffe, &cb_a, &holder_a)
            .expect_err("mismatched SPIFFE ID must fail");
        assert_eq!(spiffe_err, HolderPopError::SpiffeIdMismatch);

        // Pod A attempting to forge a proof claiming Pod B's pubkey must fail.
        let forged = HolderInstance {
            spiffe_id: spiffe.clone(),
            incarnation_pubkey: kp_b.public_key(),
            tls_exporter_proof: holder_a.tls_exporter_proof.clone(),
        };
        let forge_err = verify_channel_bound_holder(&spiffe, &cb_a, &forged)
            .expect_err("forged pubkey must fail");
        assert_eq!(forge_err, HolderPopError::InvalidSignature);
    }
}
