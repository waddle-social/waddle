//! Authenticated envelope encryption using AES-256-GCM and Verus-verified
//! length-prefixed AAD (`waddle_broker_core::kernels::aad_codec::encode_aad`).

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::kernels::aad_codec::{encode_aad, AadFields};

use crate::error::CryptoError;
use crate::secret::{Key256, SecretBytes};

/// Supported envelope AEAD ciphers (extensible for future algorithms).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvelopeCipher {
    Aes256Gcm,
}

/// Canonical context bound into every credential envelope's AEAD AAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeContext {
    pub trust_domain: String,
    pub tenant_id: TenantId,
    pub account_id: Option<AccountId>,
    pub provider: String,
    pub credential_id: Uuid,
    pub version: u32,
}

impl EnvelopeContext {
    /// Encodes the 6 canonical fields with Verus-verified length-prefixed framing.
    pub fn encode_aad(&self) -> Result<Vec<u8>, CryptoError> {
        let account_bytes = self
            .account_id
            .map(|a| a.as_uuid().as_bytes().to_vec())
            .unwrap_or_default();
        let cred_bytes = self.credential_id.as_bytes();
        let version_bytes = self.version.to_be_bytes();
        let fields = AadFields {
            trust_domain: self.trust_domain.as_bytes(),
            tenant_id: self.tenant_id.as_str().as_bytes(),
            account_id: &account_bytes,
            provider: self.provider.as_bytes(),
            credential_id: cred_bytes,
            version: &version_bytes,
        };
        encode_aad(&fields).map_err(|err| CryptoError::AadEncoding(err.to_string()))
    }
}

/// Persisted ciphertext envelope containing zero plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedEnvelope {
    pub cipher: EnvelopeCipher,
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
    pub wrapped_dek_id: Uuid,
    pub dek_version: u32,
    pub credential_version: u32,
}

/// Encrypts `plaintext` under `dek` with canonical length-prefixed AAD from `ctx`.
pub fn encrypt_secret(
    dek: &Key256,
    ctx: &EnvelopeContext,
    wrapped_dek_id: Uuid,
    dek_version: u32,
    plaintext: &SecretBytes,
) -> Result<SealedEnvelope, CryptoError> {
    let aad = ctx.encode_aad()?;
    let mut nonce_bytes = [0u8; 12];
    SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| CryptoError::RngFailure)?;

    let cipher =
        Aes256Gcm::new_from_slice(dek.as_bytes()).map_err(|_| CryptoError::EncryptFailed)?;
    let nonce = Nonce::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext.as_bytes(),
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::EncryptFailed)?;

    Ok(SealedEnvelope {
        cipher: EnvelopeCipher::Aes256Gcm,
        nonce: nonce_bytes,
        ciphertext,
        wrapped_dek_id,
        dek_version,
        credential_version: ctx.version,
    })
}

/// Decrypts `envelope` under `dek` and verifies exact AAD binding against `ctx`.
pub fn decrypt_secret(
    dek: &Key256,
    ctx: &EnvelopeContext,
    envelope: &SealedEnvelope,
) -> Result<SecretBytes, CryptoError> {
    if envelope.credential_version != ctx.version {
        return Err(CryptoError::DecryptFailed);
    }
    let aad = ctx.encode_aad()?;
    let cipher =
        Aes256Gcm::new_from_slice(dek.as_bytes()).map_err(|_| CryptoError::DecryptFailed)?;
    let nonce = Nonce::from(envelope.nonce);
    let plaintext = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: envelope.ciphertext.as_slice(),
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::DecryptFailed)?;
    SecretBytes::new(plaintext)
}
