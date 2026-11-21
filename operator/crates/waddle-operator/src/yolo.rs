use std::collections::{BTreeMap, HashMap};
use std::fmt;

use ed25519_dalek::SigningKey;
use rand::rngs::SysRng;
use uuid::Uuid;
use waddle_operator_crd::{ContentDigest, DeploymentUuid};
use zeroize::Zeroizing;

use crate::error::OperatorError;

const ED25519_SEED_BYTES: usize = 32;
const GENERATED_SECRET_BYTES: usize = 48;

/// Wrapper for secret material that zeroes memory on drop and redacts all `Debug` formatting.
///
/// Intentionally does not implement `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct RedactedSecret(Zeroizing<String>);

impl RedactedSecret {
    /// Wraps secret text inside a `Zeroizing` buffer.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    /// Exposes the underlying secret only at Kubernetes API / serialization boundaries.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for RedactedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Errors validating an enrolled Ed25519 keypair pool.
///
/// Diagnostic messages intentionally name only slot indices, never key material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum KeypoolValidationError {
    /// Pool entry was not valid standard base64.
    #[error("keypool slot {slot} is not valid standard base64")]
    InvalidBase64 {
        /// Zero-based slot index.
        slot: usize,
    },
    /// Decoded entry did not have the exact 32-byte Ed25519 seed length.
    #[error("keypool slot {slot} must decode to exactly 32 bytes")]
    InvalidLength {
        /// Zero-based slot index.
        slot: usize,
    },
    /// Two pool entries held identical seeds, which would collide PeerIds.
    #[error("keypool slots {first} and {second} contain duplicate seeds")]
    DuplicateEntry {
        /// First colliding slot index.
        first: usize,
        /// Second colliding slot index.
        second: usize,
    },
    /// Fewer keys were present than required by `replicas + maxSurge + headroom`.
    #[error("keypool has {found} keys, but at least {required} are required")]
    InsufficientKeys {
        /// Number of keys parsed.
        found: usize,
        /// Minimum keys required.
        required: usize,
    },
}

/// Generates 48 cryptographically random bytes encoded in standard base64 (64 ASCII chars).
///
/// Matches `waddle-server`'s requirement (`>= 32` bytes) for `WADDLE_SESSION_KEY` and
/// `WADDLE_OCCUPANT_ID_SECRET`.
///
/// # Errors
/// Returns [`OperatorError::RandomGeneration`] if the OS CSPRNG fails.
pub fn generate_base64_secret_48() -> Result<RedactedSecret, OperatorError> {
    use base64::Engine;
    use rand::TryRng;

    let mut raw = Zeroizing::new([0u8; GENERATED_SECRET_BYTES]);
    SysRng
        .try_fill_bytes(&mut *raw)
        .map_err(|_| OperatorError::RandomGeneration)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(*raw);
    Ok(RedactedSecret::new(encoded))
}

/// Generates a lower-case RFC-4122 v4 [`DeploymentUuid`].
#[must_use]
pub fn generate_deployment_uuid() -> DeploymentUuid {
    DeploymentUuid::from_uuid(Uuid::new_v4())
}

/// Generates a 32-byte Ed25519 secret seed encoded in standard base64, matching
/// `waddle-cluster-keypool` and `clustering::identity::keypair_from_pool_entry`.
///
/// # Errors
/// Returns [`OperatorError::RandomGeneration`] if the OS CSPRNG fails.
pub fn generate_ed25519_seed_base64() -> Result<RedactedSecret, OperatorError> {
    use base64::Engine;
    use rand::TryRng;

    let mut seed = Zeroizing::new([0u8; ED25519_SEED_BYTES]);
    SysRng
        .try_fill_bytes(&mut *seed)
        .map_err(|_| OperatorError::RandomGeneration)?;
    let _signing_key = SigningKey::from_bytes(&seed);
    let encoded = base64::engine::general_purpose::STANDARD.encode(*seed);
    Ok(RedactedSecret::new(encoded))
}

/// Validates a comma-separated `WADDLE_CLUSTERING_KEYPAIR_POOL` string without logging secrets.
///
/// # Errors
/// Returns a [`KeypoolValidationError`] naming only slot indices or counts.
pub fn validate_keypair_pool(raw: &str, min_count: usize) -> Result<usize, KeypoolValidationError> {
    use base64::Engine;
    use zeroize::Zeroize;

    let entries: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();

    if entries.len() < min_count {
        return Err(KeypoolValidationError::InsufficientKeys {
            found: entries.len(),
            required: min_count,
        });
    }

    let mut seen: HashMap<[u8; ED25519_SEED_BYTES], usize> = HashMap::new();
    for (slot, entry) in entries.iter().enumerate() {
        let mut decoded = base64::engine::general_purpose::STANDARD
            .decode(entry)
            .map_err(|_| KeypoolValidationError::InvalidBase64 { slot })?;
        if decoded.len() != ED25519_SEED_BYTES {
            decoded.zeroize();
            return Err(KeypoolValidationError::InvalidLength { slot });
        }
        let mut seed = [0u8; ED25519_SEED_BYTES];
        seed.copy_from_slice(&decoded);
        decoded.zeroize();

        let _signing_key = SigningKey::from_bytes(&seed);
        let duplicate = seen.insert(seed, slot);
        seed.zeroize();

        if let Some(first) = duplicate {
            return Err(KeypoolValidationError::DuplicateEntry {
                first,
                second: slot,
            });
        }
    }

    Ok(entries.len())
}

/// Computes a canonical, sorted, length-prefixed SHA-256 hex digest over only the keys in `keys`.
///
/// Returns `None` if none of the specified keys exist in `entries`.
#[must_use]
pub fn compute_consumed_secret_digest(
    entries: &BTreeMap<String, Vec<u8>>,
    keys: &[&str],
) -> Option<ContentDigest> {
    use sha2::Digest;

    let sorted_keys: BTreeMap<&str, &[u8]> = keys
        .iter()
        .filter_map(|&key| entries.get(key).map(|val| (key, val.as_slice())))
        .collect();

    if sorted_keys.is_empty() {
        return None;
    }

    let mut hasher = sha2::Sha256::new();
    for (key, value) in sorted_keys {
        hasher.update((key.len() as u64).to_be_bytes());
        hasher.update(key.as_bytes());
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value);
    }
    let hex_digest = hex::encode(hasher.finalize());
    ContentDigest::try_new(&hex_digest).ok()
}

/// Computes a SHA-256 hex fingerprint of a single secret value for rotation detection.
#[must_use]
pub fn compute_single_key_digest(bytes: &[u8]) -> Option<ContentDigest> {
    use sha2::Digest;

    let mut hasher = sha2::Sha256::new();
    hasher.update(b"waddle-key-fingerprint-v1:");
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    let hex_digest = hex::encode(hasher.finalize());
    ContentDigest::try_new(&hex_digest).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_secret_never_prints_value_in_debug() {
        let secret = RedactedSecret::new("super-secret-value-12345".to_owned());
        let formatted = format!("{secret:?}");
        assert_eq!(formatted, "[REDACTED]");
        assert!(!formatted.contains("super-secret"));
    }

    #[test]
    fn generated_secrets_meet_server_requirements() {
        let session = generate_base64_secret_48().expect("csprng works");
        assert!(
            session.expose_secret().len() >= 32,
            "base64 secret must be >= 32 bytes"
        );
        let uuid = generate_deployment_uuid();
        assert_eq!(
            DeploymentUuid::parse_exact(&uuid.to_string()).expect("uuid parses back"),
            uuid
        );
    }

    #[test]
    fn keypool_validator_accepts_valid_seeds_and_redacts_duplicates() {
        let k1 = generate_ed25519_seed_base64().expect("seed 1");
        let k2 = generate_ed25519_seed_base64().expect("seed 2");
        let combined = format!("{}, {}", k1.expose_secret(), k2.expose_secret());
        assert_eq!(
            validate_keypair_pool(&combined, 2).expect("2 distinct keys valid"),
            2
        );

        let dup = format!("{}, {}", k1.expose_secret(), k1.expose_secret());
        let err = validate_keypair_pool(&dup, 2).expect_err("duplicate keys must fail");
        assert_eq!(
            err,
            KeypoolValidationError::DuplicateEntry {
                first: 0,
                second: 1
            }
        );
        let msg = err.to_string();
        assert!(
            !msg.contains(k1.expose_secret()),
            "error message leaked seed material: {msg}"
        );
    }

    #[test]
    fn consumed_secret_digest_ignores_unrelated_keys() {
        let mut map = BTreeMap::new();
        map.insert("WADDLE_SESSION_KEY".to_owned(), b"session".to_vec());
        map.insert("UNRELATED".to_owned(), b"alpha".to_vec());

        let first = compute_consumed_secret_digest(&map, &["WADDLE_SESSION_KEY"]);
        map.insert("UNRELATED".to_owned(), b"beta".to_vec());
        let second = compute_consumed_secret_digest(&map, &["WADDLE_SESSION_KEY"]);
        assert_eq!(first, second, "unrelated secret key must not alter digest");
    }
}
