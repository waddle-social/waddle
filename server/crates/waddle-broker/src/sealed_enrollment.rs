//! Client-pinned Root Broker-Key-Signing Key (BKSK, P-256 ECDSA), rotatable
//! Broker Encryption Subkeys (BESK, P-256 ECDH), and client-side envelope
//! sealing (`ECDH-ES + HKDF-SHA256 + AES-256-GCM`) decrypted solely at the
//! broker.

use std::collections::HashMap;
use std::sync::RwLock;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use p256::ecdh::diffie_hellman;
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::{PublicKey, SecretKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};
use zeroize::Zeroizing;

use crate::error::{CryptoError, SealError};
use crate::secret::SecretBytes;

fn encode_len_prefixed(parts: &[&[u8]]) -> Vec<u8> {
    let total: usize = parts.iter().map(|p| 4 + p.len()).sum();
    let mut out = Vec::with_capacity(total);
    for p in parts {
        let len = u32::try_from(p.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(p);
    }
    out
}

fn generate_p256_secret_key() -> Result<SecretKey, SealError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    let rng = SystemRandom::new();
    for _ in 0..8 {
        rng.fill(bytes.as_mut_slice())
            .map_err(|_| SealError::Crypto(CryptoError::RngFailure))?;
        if let Ok(sk) = SecretKey::from_slice(bytes.as_slice()) {
            return Ok(sk);
        }
    }
    Err(SealError::Crypto(CryptoError::RngFailure))
}

/// Computes the 16-hex-char `root_kid = hex(SHA-256(public_key_sec1))[..16]`.
pub fn compute_root_kid(public_key_sec1: &[u8]) -> String {
    let digest = Sha256::digest(public_key_sec1);
    hex::encode(&digest[..8])
}

/// Signed rotatable Broker Encryption Subkey (BESK) bundle delivered over XMPP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedSubkeyBundle {
    pub trust_domain: String,
    pub root_kid: String,
    pub subkey_id: Uuid,
    pub key_epoch: u64,
    pub not_before_ms: i64,
    pub not_after_ms: i64,
    pub subkey_public_sec1: Vec<u8>,
    /// Raw 64-byte `r || s` P-256 ECDSA signature by the offline Root BKSK.
    pub root_signature: Vec<u8>,
}

impl SignedSubkeyBundle {
    /// Canonical length-prefixed TBS payload signed by the Root BKSK.
    pub fn to_be_signed(&self) -> Vec<u8> {
        encode_len_prefixed(&[
            b"waddle-broker-subkey-v1",
            self.trust_domain.as_bytes(),
            self.root_kid.as_bytes(),
            self.subkey_id.as_bytes(),
            b"p256-ecdh",
            &self.subkey_public_sec1,
            &self.not_before_ms.to_be_bytes(),
            &self.not_after_ms.to_be_bytes(),
            &self.key_epoch.to_be_bytes(),
        ])
    }
}

/// Offline/HSM Root Broker-Key-Signing Key (BKSK) signer for issuing rotatable
/// subkey bundles.
pub struct RootBkskSigner {
    root_kid: String,
    signing_key: SigningKey,
}

impl RootBkskSigner {
    pub fn generate() -> Result<Self, SealError> {
        let sk = generate_p256_secret_key()?;
        let signing_key = SigningKey::from(&sk);
        let pub_sec1 = signing_key
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        let root_kid = compute_root_kid(&pub_sec1);
        Ok(Self {
            root_kid,
            signing_key,
        })
    }

    pub fn root_kid(&self) -> &str {
        &self.root_kid
    }

    pub fn public_key_sec1(&self) -> Vec<u8> {
        self.signing_key
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec()
    }

    pub fn sign_subkey(
        &self,
        trust_domain: impl Into<String>,
        subkey_id: Uuid,
        key_epoch: u64,
        not_before_ms: i64,
        not_after_ms: i64,
        subkey_public_sec1: Vec<u8>,
    ) -> SignedSubkeyBundle {
        let mut bundle = SignedSubkeyBundle {
            trust_domain: trust_domain.into(),
            root_kid: self.root_kid.clone(),
            subkey_id,
            key_epoch,
            not_before_ms,
            not_after_ms,
            subkey_public_sec1,
            root_signature: Vec::new(),
        };
        let tbs = bundle.to_be_signed();
        let sig: Signature = self.signing_key.sign(&tbs);
        bundle.root_signature = sig.to_bytes().to_vec();
        bundle
    }
}

/// Client-side compile-time pinned Root BKSK keyset (supports 1–2 pinned roots
/// for zero-downtime root rollover + anti-rollback `key_epoch` tracking).
pub struct RootBkskPinSet {
    roots: HashMap<String, VerifyingKey>,
    last_seen_epoch: RwLock<u64>,
}

impl RootBkskPinSet {
    pub fn new(pinned_public_keys_sec1: &[Vec<u8>]) -> Result<Self, SealError> {
        let mut roots = HashMap::new();
        for pk in pinned_public_keys_sec1 {
            let vk = VerifyingKey::from_sec1_bytes(pk).map_err(|_| SealError::InvalidPublicKey)?;
            let kid = compute_root_kid(pk);
            roots.insert(kid, vk);
        }
        Ok(Self {
            roots,
            last_seen_epoch: RwLock::new(0),
        })
    }

    /// Verifies `bundle` against the pinned root keyset, trust domain, validity
    /// window, and monotonic `key_epoch`. Rejects unsigned, wrong-root-signed,
    /// expired, or rolled-back subkeys.
    pub fn verify_subkey_bundle(
        &self,
        bundle: &SignedSubkeyBundle,
        expected_trust_domain: &str,
        now_ms: i64,
    ) -> Result<PublicKey, SealError> {
        let root_vk = self
            .roots
            .get(&bundle.root_kid)
            .ok_or_else(|| SealError::UnknownRootKid(bundle.root_kid.clone()))?;

        if bundle.trust_domain != expected_trust_domain {
            return Err(SealError::TrustDomainMismatch);
        }
        if now_ms < bundle.not_before_ms || now_ms > bundle.not_after_ms {
            return Err(SealError::SubkeyValidityWindow);
        }

        let tbs = bundle.to_be_signed();
        let sig = Signature::from_slice(&bundle.root_signature)
            .or_else(|_| Signature::from_der(&bundle.root_signature))
            .map_err(|_| SealError::InvalidRootSignature)?;
        root_vk
            .verify(&tbs, &sig)
            .map_err(|_| SealError::InvalidRootSignature)?;

        let mut seen = self
            .last_seen_epoch
            .write()
            .map_err(|_| SealError::InvalidRootSignature)?;
        if bundle.key_epoch < *seen {
            return Err(SealError::SubkeyEpochRollback {
                seen: *seen,
                got: bundle.key_epoch,
            });
        }
        *seen = bundle.key_epoch;

        PublicKey::from_sec1_bytes(&bundle.subkey_public_sec1)
            .map_err(|_| SealError::InvalidPublicKey)
    }
}

/// Canonical context bound into client-to-broker sealed enrollment AAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSealContext {
    pub trust_domain: String,
    pub tenant_id: TenantId,
    pub account_id: Option<AccountId>,
    pub provider: String,
    pub purpose: String,
    pub nonce: String,
    pub expires_at_ms: i64,
    pub version: u32,
}

impl ClientSealContext {
    fn encode_seal_aad(&self, subkey_id: Uuid, eph_pk_sec1: &[u8]) -> Vec<u8> {
        let account_str = self.account_id.map(|a| a.to_string()).unwrap_or_default();
        let version_str = self.version.to_string();
        let subkey_str = subkey_id.to_string();
        encode_len_prefixed(&[
            b"waddle-cred-seal-v1",
            version_str.as_bytes(),
            self.trust_domain.as_bytes(),
            self.tenant_id.as_str().as_bytes(),
            account_str.as_bytes(),
            self.provider.as_bytes(),
            self.purpose.as_bytes(),
            self.nonce.as_bytes(),
            &self.expires_at_ms.to_be_bytes(),
            subkey_str.as_bytes(),
            eph_pk_sec1,
        ])
    }
}

/// Wire ciphertext sealed by the client to the broker's verified BESK subkey.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSealedBlob {
    pub subkey_id: Uuid,
    pub ephemeral_public_sec1: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

fn derive_seal_cek_and_nonce(
    shared_secret: &[u8],
    nonce_salt: &[u8],
    seal_aad: &[u8],
) -> Result<(Zeroizing<[u8; 32]>, [u8; 12]), SealError> {
    let hk = Hkdf::<Sha256>::new(Some(nonce_salt), shared_secret);
    let mut info = Vec::with_capacity(b"waddle-cred-seal-key-v1".len() + seal_aad.len());
    info.extend_from_slice(b"waddle-cred-seal-key-v1");
    info.extend_from_slice(seal_aad);

    let mut okm = Zeroizing::new([0u8; 44]);
    hk.expand(&info, okm.as_mut_slice())
        .map_err(|_| SealError::Crypto(CryptoError::KdfFailed))?;
    let mut cek = Zeroizing::new([0u8; 32]);
    cek.copy_from_slice(&okm[..32]);
    let mut iv = [0u8; 12];
    iv.copy_from_slice(&okm[32..44]);
    Ok((cek, iv))
}

/// Client-side sealing: verifies `bundle` against `pins`, performs ephemeral
/// `P-256` ECDH + `HKDF-SHA256` + `AES-256-GCM` bound to `ctx`.
pub fn seal_credential_for_broker(
    pins: &RootBkskPinSet,
    bundle: &SignedSubkeyBundle,
    ctx: &ClientSealContext,
    plaintext: &SecretBytes,
    now_ms: i64,
) -> Result<ClientSealedBlob, SealError> {
    if ctx.expires_at_ms <= now_ms {
        return Err(SealError::EnvelopeExpired);
    }
    let broker_pk = pins.verify_subkey_bundle(bundle, &ctx.trust_domain, now_ms)?;

    let eph_sk = generate_p256_secret_key()?;
    let eph_pk = eph_sk.public_key();
    let eph_pk_sec1 = eph_pk.to_sec1_bytes().to_vec();
    let shared = diffie_hellman(eph_sk.to_nonzero_scalar(), broker_pk.as_affine());
    let seal_aad = ctx.encode_seal_aad(bundle.subkey_id, &eph_pk_sec1);
    let (cek, iv) = derive_seal_cek_and_nonce(
        &shared.raw_secret_bytes()[..],
        ctx.nonce.as_bytes(),
        &seal_aad,
    )?;

    let cipher = Aes256Gcm::new_from_slice(cek.as_slice())
        .map_err(|_| SealError::Crypto(CryptoError::EncryptFailed))?;
    let ciphertext = cipher
        .encrypt(
            &Nonce::from(iv),
            Payload {
                msg: plaintext.as_bytes(),
                aad: &seal_aad,
            },
        )
        .map_err(|_| SealError::Crypto(CryptoError::EncryptFailed))?;

    // Explicitly drop ephemeral secret so it cannot linger in scope.
    drop(eph_sk);

    Ok(ClientSealedBlob {
        subkey_id: bundle.subkey_id,
        ephemeral_public_sec1: eph_pk_sec1,
        ciphertext,
    })
}

/// Broker-side ring of rotatable `P-256` ECDH encryption subkeys (BESK).
pub struct BrokerSubkeyRing {
    trust_domain: String,
    active_subkey_id: RwLock<Option<Uuid>>,
    subkeys: RwLock<HashMap<Uuid, (SecretKey, SignedSubkeyBundle)>>,
}

impl BrokerSubkeyRing {
    pub fn new(trust_domain: impl Into<String>) -> Self {
        Self {
            trust_domain: trust_domain.into(),
            active_subkey_id: RwLock::new(None),
            subkeys: RwLock::new(HashMap::new()),
        }
    }

    /// Generates a new rotatable BESK subkey, signs it with `root_signer`, and
    /// sets it as the active subkey for distribution over XMPP.
    pub fn rotate_subkey(
        &self,
        root_signer: &RootBkskSigner,
        key_epoch: u64,
        not_before_ms: i64,
        not_after_ms: i64,
    ) -> Result<SignedSubkeyBundle, SealError> {
        let sk = generate_p256_secret_key()?;
        let pub_sec1 = sk.public_key().to_sec1_bytes().to_vec();
        let subkey_id = Uuid::now_v7();
        let bundle = root_signer.sign_subkey(
            self.trust_domain.clone(),
            subkey_id,
            key_epoch,
            not_before_ms,
            not_after_ms,
            pub_sec1,
        );

        let mut guard = self
            .subkeys
            .write()
            .map_err(|_| SealError::UnknownSubkey(subkey_id))?;
        guard.insert(subkey_id, (sk, bundle.clone()));
        let mut active = self
            .active_subkey_id
            .write()
            .map_err(|_| SealError::UnknownSubkey(subkey_id))?;
        *active = Some(subkey_id);
        Ok(bundle)
    }

    pub fn active_bundle(&self) -> Option<SignedSubkeyBundle> {
        let active_id = *self.active_subkey_id.read().ok()?;
        let id = active_id?;
        let guard = self.subkeys.read().ok()?;
        guard.get(&id).map(|(_, b)| b.clone())
    }

    /// Permanently removes/retires an old subkey after its overlap grace window.
    pub fn retire_subkey(&self, subkey_id: Uuid) {
        if let Ok(mut guard) = self.subkeys.write() {
            guard.remove(&subkey_id);
        }
    }

    /// Unseals a client-sealed enrollment blob exclusively inside broker memory.
    pub fn unseal_client_envelope(
        &self,
        blob: &ClientSealedBlob,
        ctx: &ClientSealContext,
        now_ms: i64,
    ) -> Result<SecretBytes, SealError> {
        if ctx.expires_at_ms <= now_ms {
            return Err(SealError::EnvelopeExpired);
        }

        let (sk_scalar, not_before_ms, not_after_ms) = {
            let guard = self
                .subkeys
                .read()
                .map_err(|_| SealError::UnknownSubkey(blob.subkey_id))?;
            let (sk, bundle) = guard
                .get(&blob.subkey_id)
                .ok_or(SealError::UnknownSubkey(blob.subkey_id))?;
            (
                sk.to_nonzero_scalar(),
                bundle.not_before_ms,
                bundle.not_after_ms,
            )
        };

        if now_ms < not_before_ms || now_ms > not_after_ms {
            return Err(SealError::SubkeyValidityWindow);
        }

        let eph_pk = PublicKey::from_sec1_bytes(&blob.ephemeral_public_sec1)
            .map_err(|_| SealError::InvalidPublicKey)?;
        let shared = diffie_hellman(sk_scalar, eph_pk.as_affine());
        let seal_aad = ctx.encode_seal_aad(blob.subkey_id, &blob.ephemeral_public_sec1);
        let (cek, iv) = derive_seal_cek_and_nonce(
            &shared.raw_secret_bytes()[..],
            ctx.nonce.as_bytes(),
            &seal_aad,
        )?;

        let cipher = Aes256Gcm::new_from_slice(cek.as_slice())
            .map_err(|_| SealError::Crypto(CryptoError::DecryptFailed))?;
        let plaintext = cipher
            .decrypt(
                &Nonce::from(iv),
                Payload {
                    msg: blob.ciphertext.as_slice(),
                    aad: &seal_aad,
                },
            )
            .map_err(|_| SealError::Crypto(CryptoError::DecryptFailed))?;
        Ok(SecretBytes::new(plaintext)?)
    }
}
