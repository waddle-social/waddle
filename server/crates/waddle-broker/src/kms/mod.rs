//! Broker-only Key Management Service (KMS) abstraction and providers.

pub mod local;
pub mod scaleway;
pub mod vault;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use waddle_broker_core::identity::{AccountId, TenantId};

use crate::error::KmsError;
use crate::secret::Key256;

/// Opaque identifier of a KMS-managed wrapping key or per-user wrapping secret.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KmsKeyRef(String);

impl KmsKeyRef {
    pub fn new(raw: impl Into<String>) -> Result<Self, KmsError> {
        let value = raw.into();
        if value.trim().is_empty() {
            return Err(KmsError::InvalidConfig(
                "KMS key reference must not be empty".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque KMS-wrapped key material persisted alongside envelopes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrappedKeyBlob {
    pub key_ref: KmsKeyRef,
    pub ciphertext: Vec<u8>,
}

/// Broker-only KMS custody interface.
///
/// Cells never receive KMS credentials or call this trait.
#[async_trait]
pub trait KmsProvider: Send + Sync {
    /// Wraps a 256-bit DEK under `key_ref` bound to `aad`.
    async fn wrap_key(
        &self,
        key_ref: &KmsKeyRef,
        plaintext: &Key256,
        aad: &[u8],
    ) -> Result<WrappedKeyBlob, KmsError>;

    /// Unwraps a 256-bit DEK under `key_ref` bound to `aad`.
    async fn unwrap_key(
        &self,
        key_ref: &KmsKeyRef,
        wrapped: &WrappedKeyBlob,
        aad: &[u8],
    ) -> Result<Key256, KmsError>;

    /// Ensures a KMS-resident wrapping key exists for `(tenant, account)` so
    /// destroying it cryptographically shreds historical backups of that user.
    async fn ensure_user_wrapping_secret(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError>;

    /// Ensures a KMS-resident wrapping key exists for `tenant`.
    async fn ensure_tenant_kek(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError>;

    /// Permanently destroys a KMS wrapping key/secret (irrecoverable).
    async fn destroy_wrapping_secret(&self, key_ref: &KmsKeyRef) -> Result<(), KmsError>;

    /// Returns `true` if `key_ref` has been permanently destroyed.
    async fn is_destroyed(&self, key_ref: &KmsKeyRef) -> Result<bool, KmsError>;
}
