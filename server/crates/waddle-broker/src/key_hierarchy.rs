//! 3-tier broker key hierarchy:
//! 1. Root KEK (broker-only KMS, never leaves KMS)
//! 2. Per-tenant DEK (`TenantDekRecord`), wrapped by KMS under tenant KEK
//! 3. Per-user DEK (`UserDekRecord`), wrapped under BOTH the tenant DEK
//!    (AES-256-GCM) AND a KMS per-user wrapping secret via HKDF-SHA256 so
//!    destroying the KMS user wrapping secret cryptographically shreds backups.

use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::kernels::aad_codec::{encode_aad, AadFields};
use zeroize::Zeroizing;

use crate::error::{BrokerError, CryptoError};
use crate::kms::{KmsKeyRef, KmsProvider, WrappedKeyBlob};
use crate::secret::Key256;

/// Persisted wrapped per-tenant Data Encryption Key (zero plaintext).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantDekRecord {
    pub dek_id: Uuid,
    pub tenant_id: TenantId,
    pub version: u32,
    pub wrapped_by_kms: WrappedKeyBlob,
}

/// Persisted wrapped per-user Data Encryption Key (zero plaintext).
///
/// The effective user DEK is derived via `HKDF-SHA256(ikm = user_dek || kms_user_secret, info = aad)`
/// where `user_dek` is wrapped by the tenant DEK and `kms_user_secret` is wrapped by the
/// KMS per-user wrapping key (`user_kms_key_ref`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserDekRecord {
    pub dek_id: Uuid,
    pub tenant_id: TenantId,
    pub account_id: AccountId,
    pub version: u32,
    pub tenant_dek_id: Uuid,
    pub tenant_dek_version: u32,
    pub wrapped_by_tenant_nonce: [u8; 12],
    pub wrapped_by_tenant_ciphertext: Vec<u8>,
    pub kms_user_secret_blob: WrappedKeyBlob,
}

fn tenant_dek_aad(
    trust_domain: &str,
    tenant_id: &TenantId,
    dek_id: Uuid,
    version: u32,
) -> Result<Vec<u8>, CryptoError> {
    let version_bytes = version.to_be_bytes();
    encode_aad(&AadFields {
        trust_domain: trust_domain.as_bytes(),
        tenant_id: tenant_id.as_str().as_bytes(),
        account_id: b"",
        provider: b"tenant-dek",
        credential_id: dek_id.as_bytes(),
        version: &version_bytes,
    })
    .map_err(|err| CryptoError::AadEncoding(err.to_string()))
}

fn user_dek_aad(
    trust_domain: &str,
    tenant_id: &TenantId,
    account_id: AccountId,
    dek_id: Uuid,
    version: u32,
) -> Result<Vec<u8>, CryptoError> {
    let version_bytes = version.to_be_bytes();
    encode_aad(&AadFields {
        trust_domain: trust_domain.as_bytes(),
        tenant_id: tenant_id.as_str().as_bytes(),
        account_id: account_id.as_uuid().as_bytes(),
        provider: b"user-dek",
        credential_id: dek_id.as_bytes(),
        version: &version_bytes,
    })
    .map_err(|err| CryptoError::AadEncoding(err.to_string()))
}

fn derive_effective_user_key(
    user_dek: &Key256,
    kms_user_secret: &Key256,
    aad: &[u8],
) -> Result<Key256, CryptoError> {
    let mut ikm = Zeroizing::new([0u8; 64]);
    ikm[..32].copy_from_slice(user_dek.as_bytes());
    ikm[32..].copy_from_slice(kms_user_secret.as_bytes());
    let hk = Hkdf::<Sha256>::new(Some(b"waddle-user-dek-hkdf-v1"), ikm.as_slice());
    let mut okm = Zeroizing::new([0u8; 32]);
    hk.expand(aad, okm.as_mut_slice())
        .map_err(|_| CryptoError::KdfFailed)?;
    Ok(Key256::from_bytes(*okm))
}

/// Manages 3-tier key wrapping (KMS KEK -> TenantDek -> UserDek).
pub struct KeyHierarchyService {
    trust_domain: String,
    kms: Arc<dyn KmsProvider>,
}

impl KeyHierarchyService {
    pub fn new(trust_domain: impl Into<String>, kms: Arc<dyn KmsProvider>) -> Self {
        Self {
            trust_domain: trust_domain.into(),
            kms,
        }
    }

    pub fn trust_domain(&self) -> &str {
        &self.trust_domain
    }

    pub fn kms(&self) -> &Arc<dyn KmsProvider> {
        &self.kms
    }

    /// Generates and wraps a fresh per-tenant DEK under the tenant's KMS KEK.
    pub async fn create_tenant_dek(
        &self,
        tenant_id: &TenantId,
        version: u32,
    ) -> Result<(TenantDekRecord, Key256), BrokerError> {
        let kek_ref = self.kms.ensure_tenant_kek(tenant_id).await?;
        let dek_id = Uuid::now_v7();
        let dek = Key256::generate()?;
        let aad = tenant_dek_aad(&self.trust_domain, tenant_id, dek_id, version)?;
        let wrapped = self.kms.wrap_key(&kek_ref, &dek, &aad).await?;
        let record = TenantDekRecord {
            dek_id,
            tenant_id: tenant_id.clone(),
            version,
            wrapped_by_kms: wrapped,
        };
        Ok((record, dek))
    }

    /// Unwraps a per-tenant DEK via KMS.
    pub async fn unwrap_tenant_dek(&self, record: &TenantDekRecord) -> Result<Key256, BrokerError> {
        let aad = tenant_dek_aad(
            &self.trust_domain,
            &record.tenant_id,
            record.dek_id,
            record.version,
        )?;
        let dek = self
            .kms
            .unwrap_key(&record.wrapped_by_kms.key_ref, &record.wrapped_by_kms, &aad)
            .await?;
        Ok(dek)
    }

    /// Generates and wraps a per-user DEK under both `tenant_dek` and the user's
    /// KMS wrapping secret. Returns `(UserDekRecord, effective_user_key)`.
    pub async fn create_user_dek(
        &self,
        tenant_record: &TenantDekRecord,
        tenant_dek: &Key256,
        account_id: AccountId,
        version: u32,
    ) -> Result<(UserDekRecord, Key256), BrokerError> {
        let dek_id = Uuid::now_v7();
        let user_dek = Key256::generate()?;
        let kms_user_secret = Key256::generate()?;
        let aad = user_dek_aad(
            &self.trust_domain,
            &tenant_record.tenant_id,
            account_id,
            dek_id,
            version,
        )?;

        let mut nonce_bytes = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| CryptoError::RngFailure)?;
        let cipher = Aes256Gcm::new_from_slice(tenant_dek.as_bytes())
            .map_err(|_| CryptoError::EncryptFailed)?;
        let wrapped_by_tenant_ciphertext = cipher
            .encrypt(
                &Nonce::from(nonce_bytes),
                Payload {
                    msg: user_dek.as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::EncryptFailed)?;

        let user_kms_ref = self
            .kms
            .ensure_user_wrapping_secret(&tenant_record.tenant_id, account_id)
            .await?;
        let kms_user_secret_blob = self
            .kms
            .wrap_key(&user_kms_ref, &kms_user_secret, &aad)
            .await?;

        let effective = derive_effective_user_key(&user_dek, &kms_user_secret, &aad)?;
        let record = UserDekRecord {
            dek_id,
            tenant_id: tenant_record.tenant_id.clone(),
            account_id,
            version,
            tenant_dek_id: tenant_record.dek_id,
            tenant_dek_version: tenant_record.version,
            wrapped_by_tenant_nonce: nonce_bytes,
            wrapped_by_tenant_ciphertext,
            kms_user_secret_blob,
        };
        Ok((record, effective))
    }

    /// Unwraps a per-user DEK and derives its effective AEAD key. Fails closed if
    /// either the tenant KMS KEK or the user's KMS wrapping secret was destroyed.
    pub async fn unwrap_user_dek(
        &self,
        tenant_record: &TenantDekRecord,
        user_record: &UserDekRecord,
    ) -> Result<Key256, BrokerError> {
        if tenant_record.tenant_id != user_record.tenant_id
            || tenant_record.dek_id != user_record.tenant_dek_id
            || tenant_record.version != user_record.tenant_dek_version
        {
            return Err(BrokerError::Crypto(CryptoError::DecryptFailed));
        }

        let tenant_dek = self.unwrap_tenant_dek(tenant_record).await?;
        let aad = user_dek_aad(
            &self.trust_domain,
            &user_record.tenant_id,
            user_record.account_id,
            user_record.dek_id,
            user_record.version,
        )?;

        let cipher = Aes256Gcm::new_from_slice(tenant_dek.as_bytes())
            .map_err(|_| CryptoError::DecryptFailed)?;
        let user_dek_bytes = cipher
            .decrypt(
                &Nonce::from(user_record.wrapped_by_tenant_nonce),
                Payload {
                    msg: user_record.wrapped_by_tenant_ciphertext.as_slice(),
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::DecryptFailed)?;
        let arr: [u8; 32] = user_dek_bytes
            .try_into()
            .map_err(|_| CryptoError::DecryptFailed)?;
        let user_dek = Key256::from_bytes(arr);

        let kms_user_secret = self
            .kms
            .unwrap_key(
                &user_record.kms_user_secret_blob.key_ref,
                &user_record.kms_user_secret_blob,
                &aad,
            )
            .await?;

        let effective = derive_effective_user_key(&user_dek, &kms_user_secret, &aad)?;
        Ok(effective)
    }

    /// Permanently destroys the KMS per-user wrapping secret for `(tenant_id, account_id)`.
    pub async fn destroy_user_kms_secret(
        &self,
        tenant_id: &TenantId,
        account_id: AccountId,
    ) -> Result<KmsKeyRef, BrokerError> {
        let key_ref = KmsKeyRef::new(format!("kms:user:{}:{}", tenant_id.as_str(), account_id))?;
        self.kms.destroy_wrapping_secret(&key_ref).await?;
        Ok(key_ref)
    }

    /// Permanently destroys the KMS tenant KEK for `tenant_id`.
    pub async fn destroy_tenant_kms_kek(
        &self,
        tenant_id: &TenantId,
    ) -> Result<KmsKeyRef, BrokerError> {
        let key_ref = KmsKeyRef::new(format!("kms:tenant:{}", tenant_id.as_str()))?;
        self.kms.destroy_wrapping_secret(&key_ref).await?;
        Ok(key_ref)
    }
}
