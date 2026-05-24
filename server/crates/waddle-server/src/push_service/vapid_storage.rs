//! Sealed VAPID keypair storage for the first-party XMPP Push Service.
//!
//! Stores ECDSA P-256 private keys for RFC 8292 VAPID JWT signing, sealed
//! at rest with AES-256-GCM and a length-prefixed AAD that binds the
//! sealed blob to its `label` + `kid` so a DB-write attacker swapping
//! blobs across rows or labels fails the GCM tag check.
//!
//! Implements the [`VapidSigner`] trait declared in `waddle-xmpp`,
//! delegating the actual JWT signing to [`InProcessVapidSigner`]. This
//! crate owns the durable lifecycle (env-var bootstrap, fresh
//! generation, DB persistence); the lower crate owns pure crypto.

use std::env;
use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, KeyInit as HmacKeyInit, Mac};
use p256::elliptic_curve::rand_core::OsRng;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use rand::RngExt;
use sha2::Sha256;
use uuid::Uuid;
use waddle_xmpp::push::types::{Kid, VapidJwt, VapidLoadError, VapidSignError, VapidSub};
use waddle_xmpp::push::vapid::{InProcessVapidSigner, VapidSigner};
use waddle_xmpp::XmppError;

use crate::db::{Database, IntoParams};

const VAPID_ENV_VAR: &str = "WADDLE_VAPID_PRIVATE_KEY";
const AAD_LABEL: &[u8] = b"waddle:push-service:vapid-key:v1";
const SEALED_PREFIX: &str = "waddle-vapid-v1";
/// Current root-key version. Multi-version rotation lands in a follow-up;
/// for v1, all rows are sealed at version 1 and read at version 1.
const CURRENT_ROOT_KEY_VERSION: u32 = 1;

/// Durable VAPID keypair storage for the Push Service component.
///
/// Construct via [`Self::load_or_provision`], which executes the boot
/// lifecycle:
///   1. If `WADDLE_VAPID_PRIVATE_KEY` env var is set: parse, **DB write
///      FIRST**, on `Ok` call `std::env::remove_var(...)`; on `Err`,
///      leave env set and fail boot loudly.
///   2. Else read the latest non-retired row.
///   3. Else generate a fresh P-256 keypair and persist.
pub struct VapidStorage {
    db: Database,
    cipher: VapidKeyCipher,
}

impl VapidStorage {
    pub async fn load_or_provision(
        db: Database,
        root_key: &[u8],
    ) -> Result<Arc<InProcessVapidSigner>, VapidLoadError> {
        let storage = Self::new(db.clone(), root_key)
            .await
            .map_err(|e| VapidLoadError::DbWrite(e.to_string()))?;

        // 1. Env-var bootstrap path.
        if let Ok(raw) = env::var(VAPID_ENV_VAR) {
            let secret_key = parse_env_scalar(&raw)?;
            let kid = Kid::new();
            storage.persist_keypair(&kid, &secret_key).await?;
            // SAFETY: remove_var is unsafe in newer Rust. We call this once
            // at boot before spawning any threads that might read env.
            // SAFETY justified: this is the boot path, single-threaded.
            #[allow(unsafe_code)]
            unsafe {
                env::remove_var(VAPID_ENV_VAR);
            }
            return Ok(Arc::new(
                InProcessVapidSigner::new(kid, secret_key)
                    .map_err(|e| VapidLoadError::DbWrite(e.to_string()))?,
            ));
        }

        // 2. Read latest non-retired row.
        if let Some((kid, secret_key)) = storage.load_latest().await? {
            return Ok(Arc::new(
                InProcessVapidSigner::new(kid, secret_key)
                    .map_err(|e| VapidLoadError::DbWrite(e.to_string()))?,
            ));
        }

        // 3. Generate fresh.
        let kid = Kid::new();
        let secret_key = p256::SecretKey::random(&mut OsRng);
        storage.persist_keypair(&kid, &secret_key).await?;
        Ok(Arc::new(
            InProcessVapidSigner::new(kid, secret_key)
                .map_err(|e| VapidLoadError::DbWrite(e.to_string()))?,
        ))
    }

    async fn new(db: Database, root_key: &[u8]) -> Result<Self, XmppError> {
        let storage = Self {
            db,
            cipher: VapidKeyCipher::new(root_key),
        };
        storage.initialize().await?;
        Ok(storage)
    }

    async fn initialize(&self) -> Result<(), XmppError> {
        let i64_type = crate::db::i64_sql_type(self.db.driver());
        let sql = format!(
            r#"
            CREATE TABLE IF NOT EXISTS push_service_vapid_keys (
                id INTEGER PRIMARY KEY,
                kid TEXT NOT NULL UNIQUE,
                sealed_private_key TEXT NOT NULL,
                public_key BLOB NOT NULL,
                root_key_version {i64_type} NOT NULL,
                created_at_ms {i64_type} NOT NULL,
                retired_at_ms {i64_type}
            )
            "#
        );
        self.run_execute(&sql, ()).await?;
        self.run_execute(
            "CREATE INDEX IF NOT EXISTS idx_push_service_vapid_keys_active \
             ON push_service_vapid_keys (retired_at_ms, created_at_ms)",
            (),
        )
        .await?;
        Ok(())
    }

    async fn run_execute(&self, sql: &str, params: impl IntoParams) -> Result<u64, XmppError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| XmppError::internal(e.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|e| XmppError::internal(e.to_string()))
    }

    async fn run_query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, XmppError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| XmppError::internal(e.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|e| XmppError::internal(e.to_string()))
    }

    async fn persist_keypair(
        &self,
        kid: &Kid,
        secret_key: &p256::SecretKey,
    ) -> Result<(), VapidLoadError> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let scalar = secret_key.to_bytes();
        let sealed = self
            .cipher
            .seal(&scalar[..], AAD_LABEL, kid.as_bytes())
            .map_err(|e| VapidLoadError::DbWrite(format!("seal: {e}")))?;
        let public_bytes = secret_key
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();

        let sql = "INSERT INTO push_service_vapid_keys \
            (kid, sealed_private_key, public_key, root_key_version, created_at_ms) \
            VALUES (?, ?, ?, ?, ?)";
        self.run_execute(
            sql,
            crate::db_params![
                kid.to_string(),
                sealed,
                public_bytes,
                CURRENT_ROOT_KEY_VERSION as i64,
                now_ms,
            ],
        )
        .await
        .map_err(|e| VapidLoadError::DbWrite(e.to_string()))?;
        Ok(())
    }

    async fn load_latest(&self) -> Result<Option<(Kid, p256::SecretKey)>, VapidLoadError> {
        let sql = "SELECT kid, sealed_private_key, root_key_version \
            FROM push_service_vapid_keys \
            WHERE retired_at_ms IS NULL \
            ORDER BY created_at_ms DESC LIMIT 1";
        let mut rows = self
            .run_query(sql, ())
            .await
            .map_err(|e| VapidLoadError::DbRead(e.to_string()))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|e| VapidLoadError::DbRead(e.to_string()))?
        else {
            return Ok(None);
        };
        let kid_str: String = row
            .get(0)
            .map_err(|e| VapidLoadError::DbRead(format!("kid: {e}")))?;
        let sealed: String = row
            .get(1)
            .map_err(|e| VapidLoadError::DbRead(format!("sealed_private_key: {e}")))?;
        let version: i64 = row
            .get(2)
            .map_err(|e| VapidLoadError::DbRead(format!("root_key_version: {e}")))?;
        if version as u32 != CURRENT_ROOT_KEY_VERSION {
            return Err(VapidLoadError::UnknownRootKeyVersion {
                found: version as u32,
                max_installed: CURRENT_ROOT_KEY_VERSION,
            });
        }
        let kid_uuid: Uuid = kid_str
            .parse()
            .map_err(|e: uuid::Error| VapidLoadError::DbRead(format!("kid not a UUID: {e}")))?;
        let kid = Kid(kid_uuid);
        let scalar = self
            .cipher
            .open(&sealed, AAD_LABEL, kid.as_bytes())
            .map_err(|e| VapidLoadError::Unseal(e.to_string()))?;
        let secret_key = p256::SecretKey::from_slice(&scalar)
            .map_err(|e| VapidLoadError::DbRead(format!("bad P-256 scalar: {e}")))?;
        Ok(Some((kid, secret_key)))
    }
}

/// AES-256-GCM sealing for VAPID private keys.
///
/// AAD is **length-prefixed** to prevent canonicalization collisions:
/// `u16_be(len(label)) || label || u16_be(len(kid_bytes)) || kid_bytes`.
struct VapidKeyCipher {
    /// 32-byte AES-256 key derived from the root key via HMAC-SHA256.
    key: [u8; 32],
}

impl VapidKeyCipher {
    fn new(root_key: &[u8]) -> Self {
        let mut mac = <Hmac<Sha256> as HmacKeyInit>::new_from_slice(root_key)
            .expect("HMAC supports any key length");
        mac.update(b"waddle:push-service:vapid-key:enc:v1");
        let derived = mac.finalize().into_bytes();
        let mut key = [0u8; 32];
        key.copy_from_slice(&derived);
        Self { key }
    }

    fn seal(&self, plaintext: &[u8], label: &[u8], kid_bytes: &[u8]) -> Result<String, String> {
        debug_assert!(label.len() <= u16::MAX as usize);
        debug_assert!(kid_bytes.len() <= u16::MAX as usize);
        let aad = build_aad(label, kid_bytes);
        let mut nonce = [0u8; 12];
        rand::rng().fill(&mut nonce[..]);
        let cipher =
            Aes256Gcm::new_from_slice(&self.key).map_err(|e| format!("AES-256-GCM init: {e}"))?;
        let ciphertext = cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|e| format!("AES-256-GCM seal: {e}"))?;
        Ok(format!(
            "{SEALED_PREFIX}:{}:{}",
            URL_SAFE_NO_PAD.encode(nonce),
            URL_SAFE_NO_PAD.encode(ciphertext)
        ))
    }

    fn open(&self, stored: &str, label: &[u8], kid_bytes: &[u8]) -> Result<Vec<u8>, String> {
        let mut parts = stored.split(':');
        let prefix = parts.next().ok_or("missing prefix")?;
        if prefix != SEALED_PREFIX {
            return Err(format!("unexpected sealed prefix {prefix:?}"));
        }
        let nonce_b64 = parts.next().ok_or("missing nonce")?;
        let ct_b64 = parts.next().ok_or("missing ciphertext")?;
        if parts.next().is_some() {
            return Err("unexpected trailing field".into());
        }
        let nonce = URL_SAFE_NO_PAD
            .decode(nonce_b64)
            .map_err(|e| format!("nonce base64url: {e}"))?;
        let nonce: [u8; 12] = nonce
            .try_into()
            .map_err(|v: Vec<u8>| format!("nonce wrong length: {}", v.len()))?;
        let ciphertext = URL_SAFE_NO_PAD
            .decode(ct_b64)
            .map_err(|e| format!("ciphertext base64url: {e}"))?;
        let aad = build_aad(label, kid_bytes);
        let cipher =
            Aes256Gcm::new_from_slice(&self.key).map_err(|e| format!("AES-256-GCM init: {e}"))?;
        cipher
            .decrypt(
                (&nonce).into(),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|e| format!("AES-256-GCM open (AAD/tag check failed): {e}"))
    }
}

fn build_aad(label: &[u8], kid_bytes: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(2 + label.len() + 2 + kid_bytes.len());
    aad.extend_from_slice(&(label.len() as u16).to_be_bytes());
    aad.extend_from_slice(label);
    aad.extend_from_slice(&(kid_bytes.len() as u16).to_be_bytes());
    aad.extend_from_slice(kid_bytes);
    aad
}

fn parse_env_scalar(raw: &str) -> Result<p256::SecretKey, VapidLoadError> {
    let raw = raw.trim();
    let bytes = URL_SAFE_NO_PAD
        .decode(raw.trim_end_matches('='))
        .map_err(|e| VapidLoadError::EnvParse(format!("base64url decode: {e}")))?;
    if bytes.len() != 32 {
        return Err(VapidLoadError::EnvParse(format!(
            "expected 32-byte P-256 scalar; got {} bytes",
            bytes.len()
        )));
    }
    p256::SecretKey::from_slice(&bytes)
        .map_err(|e| VapidLoadError::EnvParse(format!("invalid P-256 scalar: {e}")))
}

/// Adapter wrapping a `VapidStorage`-produced signer that also exposes
/// the storage handle for future operations (e.g. rotation).
///
/// Constructed indirectly via [`VapidStorage::load_or_provision`]; this
/// is the public type held by the Push Service component.
pub struct StoredVapidSigner {
    inner: Arc<InProcessVapidSigner>,
}

impl StoredVapidSigner {
    pub fn new(inner: Arc<InProcessVapidSigner>) -> Self {
        Self { inner }
    }
}

impl VapidSigner for StoredVapidSigner {
    fn sign(&self, aud: &url::Origin, sub: &VapidSub) -> Result<VapidJwt, VapidSignError> {
        self.inner.sign(aud, sub)
    }

    fn current_public_key(&self) -> p256::PublicKey {
        self.inner.current_public_key()
    }

    fn current_kid(&self) -> Kid {
        self.inner.current_kid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_aad_is_length_prefixed() {
        let aad = build_aad(b"label", b"kid_bytes");
        // u16_be(5) || "label" || u16_be(9) || "kid_bytes"
        assert_eq!(&aad[..2], &5u16.to_be_bytes());
        assert_eq!(&aad[2..7], b"label");
        assert_eq!(&aad[7..9], &9u16.to_be_bytes());
        assert_eq!(&aad[9..], b"kid_bytes");
    }

    #[test]
    fn build_aad_prevents_canonicalization_collision() {
        // "vapid" + "-extra" vs "vapid-" + "extra" — same byte sequence
        // without length-prefixing, but our length-prefixed AAD differs.
        let a = build_aad(b"vapid", b"-extra");
        let b = build_aad(b"vapid-", b"extra");
        assert_ne!(a, b);
    }

    #[test]
    fn cipher_round_trip() {
        let cipher = VapidKeyCipher::new(b"root-key-for-tests");
        let plaintext = b"my-32-byte-p256-scalar-padding!!";
        let sealed = cipher.seal(plaintext, AAD_LABEL, b"some-kid").unwrap();
        let opened = cipher.open(&sealed, AAD_LABEL, b"some-kid").unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn cipher_rejects_mismatched_kid() {
        let cipher = VapidKeyCipher::new(b"root-key-for-tests");
        let plaintext = b"my-32-byte-p256-scalar-padding!!";
        let sealed = cipher.seal(plaintext, AAD_LABEL, b"kid-a").unwrap();
        let err = cipher.open(&sealed, AAD_LABEL, b"kid-b").unwrap_err();
        assert!(err.contains("AAD/tag check failed"), "{err}");
    }

    #[test]
    fn cipher_rejects_mismatched_label() {
        let cipher = VapidKeyCipher::new(b"root-key-for-tests");
        let plaintext = b"my-32-byte-p256-scalar-padding!!";
        let sealed = cipher.seal(plaintext, b"label-a", b"kid").unwrap();
        let err = cipher.open(&sealed, b"label-b", b"kid").unwrap_err();
        assert!(err.contains("AAD/tag check failed"), "{err}");
    }

    #[test]
    fn cipher_rejects_wrong_root_key() {
        let cipher_a = VapidKeyCipher::new(b"root-key-A");
        let cipher_b = VapidKeyCipher::new(b"root-key-B");
        let plaintext = b"my-32-byte-p256-scalar-padding!!";
        let sealed = cipher_a.seal(plaintext, AAD_LABEL, b"kid").unwrap();
        let err = cipher_b.open(&sealed, AAD_LABEL, b"kid").unwrap_err();
        assert!(err.contains("AAD/tag check failed"), "{err}");
    }

    #[test]
    fn parse_env_scalar_accepts_base64url_no_pad_32_bytes() {
        let bytes = [0xABu8; 32];
        let encoded = URL_SAFE_NO_PAD.encode(bytes);
        let key = parse_env_scalar(&encoded).expect("valid scalar");
        assert_eq!(&key.to_bytes()[..], &bytes[..]);
    }

    #[test]
    fn parse_env_scalar_rejects_wrong_length() {
        let encoded = URL_SAFE_NO_PAD.encode([0u8; 16]);
        let err = parse_env_scalar(&encoded).unwrap_err();
        assert!(matches!(err, VapidLoadError::EnvParse(_)));
    }

    #[test]
    fn parse_env_scalar_accepts_leading_zero() {
        let mut bytes = [0u8; 32];
        bytes[31] = 1; // scalar = 1 (smallest valid)
        let encoded = URL_SAFE_NO_PAD.encode(bytes);
        let _ = parse_env_scalar(&encoded).expect("leading-zero scalar is valid");
    }
}
