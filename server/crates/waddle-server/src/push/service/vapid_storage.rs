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
//!
//! ## Safety
//!
//! `load_or_provision` calls `std::env::remove_var(WADDLE_VAPID_PRIVATE_KEY)`
//! after the env-supplied scalar is persisted to the DB. The unsafe call is
//! justified by the invariant that no other code in this crate reads or
//! writes `WADDLE_VAPID_PRIVATE_KEY`. Operators MUST NOT pass the same env
//! variable name to other tooling expecting steady-state availability —
//! the value lives only for the boot window.

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
use waddle_xmpp::push::types::{
    Kid, VapidDbReadStage, VapidDbWriteStage, VapidEnvParseError, VapidLoadError,
    VapidUnsealErrorKind,
};
use waddle_xmpp::push::vapid::{InProcessVapidSigner, VapidSigner};
use waddle_xmpp::XmppError;
use zeroize::Zeroizing;

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
    /// Read-only variant for a node whose startup lineage attestation failed
    /// (#1652): never write into an unattested database — not the env-var
    /// bootstrap persist, not a fresh-keypair persist. The node is
    /// permanently unready in that state, so when no key exists an
    /// ephemeral, unpersisted signer is enough to keep the push-service
    /// object graph constructible.
    pub async fn load_or_ephemeral(
        db: Database,
        root_key: &[u8],
    ) -> Result<Arc<dyn VapidSigner>, VapidLoadError> {
        let storage =
            Self::new(db.clone(), root_key)
                .await
                .map_err(|source| VapidLoadError::DbWrite {
                    stage: VapidDbWriteStage::Initialize,
                    source: Box::new(source),
                })?;
        if let Some((kid, secret_key)) = storage.load_latest().await? {
            return Ok(Arc::new(
                InProcessVapidSigner::new(kid, secret_key)
                    .map_err(|source| VapidLoadError::SignerInit { source })?,
            ));
        }
        let kid = Kid::new();
        let secret_key = p256::SecretKey::random(&mut OsRng);
        Ok(Arc::new(
            InProcessVapidSigner::new(kid, secret_key)
                .map_err(|source| VapidLoadError::SignerInit { source })?,
        ))
    }

    pub async fn load_or_provision(
        db: Database,
        root_key: &[u8],
    ) -> Result<Arc<dyn VapidSigner>, VapidLoadError> {
        let storage =
            Self::new(db.clone(), root_key)
                .await
                .map_err(|source| VapidLoadError::DbWrite {
                    stage: VapidDbWriteStage::Initialize,
                    source: Box::new(source),
                })?;

        // 1. Env-var bootstrap path.
        //
        // Ordering matters: `env::remove_var` MUST be the last fallible
        // operation in this branch. If it ran before any later step that
        // can fail (signer init, DB insert), a failure would leave the
        // process without the env var even though `load_or_provision`
        // returns `Err`, breaking the documented invariant
        // ("remove only after successful bootstrap").
        //
        // We therefore derive scalar/public bytes BEFORE the signer
        // consumes `secret_key`, then sequence: signer-init → persist →
        // remove_var. If any step fails, env remains set so the
        // operator can fix and retry.
        if let Ok(raw) = env::var(VAPID_ENV_VAR) {
            let raw = Zeroizing::new(raw);
            let secret_key = parse_env_scalar(raw.as_str())?;
            let kid = Kid::new();
            let scalar_bytes = Zeroizing::new(secret_key.to_bytes());
            let public_bytes = secret_key
                .public_key()
                .to_encoded_point(false)
                .as_bytes()
                .to_vec();
            // (a) signer-init (consumes secret_key — fallible).
            let signer = InProcessVapidSigner::new(kid, secret_key)
                .map_err(|source| VapidLoadError::SignerInit { source })?;
            // (b) persist (fallible).
            storage
                .persist_keypair(&kid, &scalar_bytes[..], &public_bytes)
                .await?;
            // (c) all fallible steps succeeded — clear env.
            // SAFETY: see module-level invariant — `WADDLE_VAPID_PRIVATE_KEY`
            // is read exactly once at boot via `load_or_provision`; no other
            // code in this crate reads or writes it.
            unsafe {
                env::remove_var(VAPID_ENV_VAR);
            }
            return Ok(Arc::new(signer));
        }

        // 2. Read latest non-retired row.
        if let Some((kid, secret_key)) = storage.load_latest().await? {
            return Ok(Arc::new(
                InProcessVapidSigner::new(kid, secret_key)
                    .map_err(|source| VapidLoadError::SignerInit { source })?,
            ));
        }

        // 3. Generate fresh.
        let kid = Kid::new();
        let secret_key = p256::SecretKey::random(&mut OsRng);
        let scalar_bytes = Zeroizing::new(secret_key.to_bytes());
        let public_bytes = secret_key
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        let signer = InProcessVapidSigner::new(kid, secret_key)
            .map_err(|source| VapidLoadError::SignerInit { source })?;
        storage
            .persist_keypair(&kid, &scalar_bytes[..], &public_bytes)
            .await?;
        Ok(Arc::new(signer))
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
        // `BLOB` is the SQLite byte type; Postgres requires `BYTEA`. Use the
        // configured driver to emit the dialect-correct column type.
        let blob_type = match self.db.driver() {
            crate::db::DatabaseDriver::Sqlite => "BLOB",
            crate::db::DatabaseDriver::Postgres => "BYTEA",
        };
        // `kid` is a UUIDv4 and already `NOT NULL UNIQUE`; using it as the
        // primary key avoids the SQLite-vs-Postgres autoincrement-integer
        // mismatch that an `id INTEGER PRIMARY KEY` column would create.
        let sql = format!(
            r#"
            CREATE TABLE IF NOT EXISTS push_service_vapid_keys (
                kid TEXT NOT NULL PRIMARY KEY,
                sealed_private_key TEXT NOT NULL,
                public_key {blob_type} NOT NULL,
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

    /// Persist a sealed VAPID keypair from pre-derived byte material.
    ///
    /// Taking bytes rather than the `SecretKey` itself lets callers
    /// construct the in-process signer (which consumes the `SecretKey`)
    /// before the persist step, so `remove_var(WADDLE_VAPID_PRIVATE_KEY)`
    /// can be deferred until ALL fallible operations succeed.
    ///
    /// `scalar_bytes` MUST be 32 bytes (a raw P-256 secret scalar) and
    /// is wrapped in `Zeroizing` by the caller. `public_bytes` is the
    /// 65-byte uncompressed SEC1 point (public, no zeroize needed).
    async fn persist_keypair(
        &self,
        kid: &Kid,
        scalar_bytes: &[u8],
        public_bytes: &[u8],
    ) -> Result<(), VapidLoadError> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let sealed = self
            .cipher
            .seal(scalar_bytes, AAD_LABEL, kid.as_bytes())
            .map_err(|source| VapidLoadError::DbWrite {
                stage: VapidDbWriteStage::SealPrivateKey,
                source: Box::new(source),
            })?;

        let sql = "INSERT INTO push_service_vapid_keys \
            (kid, sealed_private_key, public_key, root_key_version, created_at_ms) \
            VALUES (?, ?, ?, ?, ?)";
        self.run_execute(
            sql,
            crate::db_params![
                kid.to_string(),
                sealed,
                public_bytes.to_vec(),
                CURRENT_ROOT_KEY_VERSION as i64,
                now_ms,
            ],
        )
        .await
        .map_err(|source| VapidLoadError::DbWrite {
            stage: VapidDbWriteStage::InsertRow,
            source: Box::new(source),
        })?;
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
            .map_err(|source| VapidLoadError::DbRead {
                stage: VapidDbReadStage::Query,
                source: Box::new(source),
            })?;
        let Some(row) = rows.next().await.map_err(|source| VapidLoadError::DbRead {
            stage: VapidDbReadStage::NextRow,
            source: Box::new(source),
        })?
        else {
            return Ok(None);
        };
        let kid_str: String = row.get(0).map_err(|source| VapidLoadError::DbRead {
            stage: VapidDbReadStage::DecodeKid,
            source: Box::new(source),
        })?;
        let sealed: String = row.get(1).map_err(|source| VapidLoadError::DbRead {
            stage: VapidDbReadStage::DecodeSealedPrivateKey,
            source: Box::new(source),
        })?;
        let version: i64 = row.get(2).map_err(|source| VapidLoadError::DbRead {
            stage: VapidDbReadStage::DecodeRootKeyVersion,
            source: Box::new(source),
        })?;
        // Compare as `i64` directly to avoid the `as u32` wraparound foot-gun:
        // a negative value (or any v where `(v % 2^32) == 1`) would silently
        // pass an `i64 as u32 == 1` check.
        if version != i64::from(CURRENT_ROOT_KEY_VERSION) {
            return Err(VapidLoadError::UnknownRootKeyVersion {
                found: u32::try_from(version).unwrap_or(u32::MAX),
                max_installed: CURRENT_ROOT_KEY_VERSION,
            });
        }
        let kid_uuid: Uuid =
            kid_str
                .parse()
                .map_err(|source: uuid::Error| VapidLoadError::DbRead {
                    stage: VapidDbReadStage::ParseKidUuid,
                    source: Box::new(source),
                })?;
        let kid = Kid(kid_uuid);
        let scalar = self
            .cipher
            .open(&sealed, AAD_LABEL, kid.as_bytes())
            .map_err(|kind| VapidLoadError::Unseal { kind })?;
        let secret_key = p256::SecretKey::from_slice(scalar.as_slice()).map_err(|source| {
            VapidLoadError::DbRead {
                stage: VapidDbReadStage::ParseSecretKeyScalar,
                source: Box::new(source),
            }
        })?;
        // `scalar` is `Zeroizing<Vec<u8>>` and zeroes here.
        Ok(Some((kid, secret_key)))
    }
}

/// AES-256-GCM sealing for VAPID private keys.
///
/// AAD is **length-prefixed** to prevent canonicalization collisions:
/// `u16_be(len(label)) || label || u16_be(len(kid_bytes)) || kid_bytes`.
struct VapidKeyCipher {
    /// Initialized once at storage construction. AES-256-GCM key was derived
    /// from the root key via HMAC-SHA256 with a domain-separating label.
    cipher: Aes256Gcm,
}

#[derive(Debug, thiserror::Error)]
enum VapidSealError {
    #[error("AES-256-GCM seal failed")]
    EncryptFailed,
}

impl VapidKeyCipher {
    fn new(root_key: &[u8]) -> Self {
        let mut mac = <Hmac<Sha256> as HmacKeyInit>::new_from_slice(root_key)
            .expect("HMAC-SHA256 accepts arbitrary-length keys");
        mac.update(b"waddle:push-service:vapid-key:enc:v1");
        // `hmac 0.13` emits the newer `hybrid_array::Array<u8, U32>`, while
        // `aes-gcm 0.10` still consumes the legacy `GenericArray<u8, U32>`.
        // The two are layout-compatible but not type-compatible, so we
        // route through `new_from_slice` (which takes `&[u8]`); the
        // length check is statically guaranteed by the 32-byte MAC output.
        let derived = mac.finalize().into_bytes();
        let cipher = Aes256Gcm::new_from_slice(&derived)
            .expect("HMAC-SHA256 output is 32 bytes — exactly the AES-256 key size");
        Self { cipher }
    }

    fn seal(
        &self,
        plaintext: &[u8],
        label: &[u8],
        kid_bytes: &[u8],
    ) -> Result<String, VapidSealError> {
        // Length-prefix encoding uses u16, so each field is capped at 65535
        // bytes. Practical labels (~32 B) and kids (16 B UUID) are far below
        // the bound; the assertion catches future regressions.
        assert!(label.len() <= u16::MAX as usize, "AAD label too long");
        assert!(kid_bytes.len() <= u16::MAX as usize, "AAD kid too long");
        let aad = build_aad(label, kid_bytes);
        let mut nonce = [0u8; 12];
        rand::rng().fill(&mut nonce[..]);
        let ciphertext = self
            .cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| VapidSealError::EncryptFailed)?;
        Ok(format!(
            "{SEALED_PREFIX}:{}:{}",
            URL_SAFE_NO_PAD.encode(nonce),
            URL_SAFE_NO_PAD.encode(ciphertext)
        ))
    }

    fn open(
        &self,
        stored: &str,
        label: &[u8],
        kid_bytes: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, VapidUnsealErrorKind> {
        let mut parts = stored.split(':');
        let prefix = parts.next().ok_or(VapidUnsealErrorKind::MissingPrefix)?;
        if prefix != SEALED_PREFIX {
            return Err(VapidUnsealErrorKind::UnexpectedPrefix);
        }
        let nonce_b64 = parts.next().ok_or(VapidUnsealErrorKind::MissingNonce)?;
        let ct_b64 = parts
            .next()
            .ok_or(VapidUnsealErrorKind::MissingCiphertext)?;
        if parts.next().is_some() {
            return Err(VapidUnsealErrorKind::UnexpectedTrailingField);
        }
        let nonce = URL_SAFE_NO_PAD
            .decode(nonce_b64)
            .map_err(|_| VapidUnsealErrorKind::NonceBase64urlDecode)?;
        let nonce: [u8; 12] = nonce
            .try_into()
            .map_err(|_: Vec<u8>| VapidUnsealErrorKind::NonceWrongLength)?;
        let ciphertext = URL_SAFE_NO_PAD
            .decode(ct_b64)
            .map_err(|_| VapidUnsealErrorKind::CiphertextBase64urlDecode)?;
        let aad = build_aad(label, kid_bytes);
        let plaintext = self
            .cipher
            .decrypt(
                (&nonce).into(),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| VapidUnsealErrorKind::AeadOpenFailed)?;
        Ok(Zeroizing::new(plaintext))
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
    // Wrap the decoded bytes in Zeroizing so the raw P-256 scalar zeroes
    // out of heap when `bytes` falls out of scope, regardless of whether
    // `from_slice` succeeds.
    let bytes = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(raw.trim_end_matches('='))
            .map_err(|source| VapidEnvParseError::Base64urlDecode { source })?,
    );
    if bytes.len() != 32 {
        return Err(VapidEnvParseError::InvalidLength { found: bytes.len() }.into());
    }
    p256::SecretKey::from_slice(bytes.as_slice())
        .map_err(|source| VapidEnvParseError::InvalidScalar { source }.into())
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
        assert_eq!(opened.as_slice(), plaintext);
    }

    #[test]
    fn cipher_rejects_mismatched_kid() {
        let cipher = VapidKeyCipher::new(b"root-key-for-tests");
        let plaintext = b"my-32-byte-p256-scalar-padding!!";
        let sealed = cipher.seal(plaintext, AAD_LABEL, b"kid-a").unwrap();
        let err = cipher.open(&sealed, AAD_LABEL, b"kid-b").unwrap_err();
        assert_eq!(err, VapidUnsealErrorKind::AeadOpenFailed);
    }

    #[test]
    fn cipher_rejects_mismatched_label() {
        let cipher = VapidKeyCipher::new(b"root-key-for-tests");
        let plaintext = b"my-32-byte-p256-scalar-padding!!";
        let sealed = cipher.seal(plaintext, b"label-a", b"kid").unwrap();
        let err = cipher.open(&sealed, b"label-b", b"kid").unwrap_err();
        assert_eq!(err, VapidUnsealErrorKind::AeadOpenFailed);
    }

    #[test]
    fn cipher_rejects_wrong_root_key() {
        let cipher_a = VapidKeyCipher::new(b"root-key-A");
        let cipher_b = VapidKeyCipher::new(b"root-key-B");
        let plaintext = b"my-32-byte-p256-scalar-padding!!";
        let sealed = cipher_a.seal(plaintext, AAD_LABEL, b"kid").unwrap();
        let err = cipher_b.open(&sealed, AAD_LABEL, b"kid").unwrap_err();
        assert_eq!(err, VapidUnsealErrorKind::AeadOpenFailed);
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
        assert!(matches!(
            err,
            VapidLoadError::EnvParse(VapidEnvParseError::InvalidLength { found: 16 })
        ));
    }

    #[test]
    fn parse_env_scalar_accepts_leading_zero() {
        let mut bytes = [0u8; 32];
        bytes[31] = 1; // scalar = 1 (smallest valid)
        let encoded = URL_SAFE_NO_PAD.encode(bytes);
        let _ = parse_env_scalar(&encoded).expect("leading-zero scalar is valid");
    }
}
