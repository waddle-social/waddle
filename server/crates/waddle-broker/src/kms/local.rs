//! Deterministic in-memory [`KmsProvider`] for tests, native fixtures, and
//! backup/restore drills. Supports permanent key tombstones and simulated KMS
//! outage (`set_available(false)`) to verify fail-closed custody.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use async_trait::async_trait;
use ring::rand::{SecureRandom, SystemRandom};
use waddle_broker_core::identity::{AccountId, TenantId};

use crate::error::KmsError;
use crate::kms::{KmsKeyRef, KmsProvider, WrappedKeyBlob};
use crate::secret::Key256;

#[derive(Default)]
struct LocalKmsState {
    keys: HashMap<KmsKeyRef, Key256>,
    destroyed: HashSet<KmsKeyRef>,
}

/// In-memory AES-256-GCM KMS provider with permanent key destruction tombstones.
pub struct LocalKmsProvider {
    state: RwLock<LocalKmsState>,
    available: AtomicBool,
}

impl Default for LocalKmsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalKmsProvider {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(LocalKmsState::default()),
            available: AtomicBool::new(true),
        }
    }

    /// Simulates a KMS outage when `available == false`.
    pub fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::SeqCst);
    }

    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }

    fn check_available(&self) -> Result<(), KmsError> {
        if !self.is_available() {
            return Err(KmsError::Unavailable(
                "local KMS simulated outage".to_owned(),
            ));
        }
        Ok(())
    }

    fn ensure_key_inner(&self, key_ref: KmsKeyRef) -> Result<KmsKeyRef, KmsError> {
        self.check_available()?;
        let mut guard = self
            .state
            .write()
            .map_err(|_| KmsError::Unavailable("KMS lock poisoned".to_owned()))?;
        if guard.destroyed.contains(&key_ref) {
            return Err(KmsError::KeyDestroyed(key_ref.as_str().to_owned()));
        }
        if !guard.keys.contains_key(&key_ref) {
            let key = Key256::generate().map_err(|err| KmsError::Crypto(err.to_string()))?;
            guard.keys.insert(key_ref.clone(), key);
        }
        Ok(key_ref)
    }
}

#[async_trait]
impl KmsProvider for LocalKmsProvider {
    async fn wrap_key(
        &self,
        key_ref: &KmsKeyRef,
        plaintext: &Key256,
        aad: &[u8],
    ) -> Result<WrappedKeyBlob, KmsError> {
        self.check_available()?;
        let kek = {
            let guard = self
                .state
                .read()
                .map_err(|_| KmsError::Unavailable("KMS lock poisoned".to_owned()))?;
            if guard.destroyed.contains(key_ref) {
                return Err(KmsError::KeyDestroyed(key_ref.as_str().to_owned()));
            }
            guard
                .keys
                .get(key_ref)
                .cloned()
                .ok_or_else(|| KmsError::KeyNotFound(key_ref.as_str().to_owned()))?
        };

        let mut nonce_bytes = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| KmsError::Crypto("CSPRNG failure".to_owned()))?;
        let cipher = Aes256Gcm::new_from_slice(kek.as_bytes())
            .map_err(|_| KmsError::Crypto("invalid KEK length".to_owned()))?;
        let ct = cipher
            .encrypt(
                &Nonce::from(nonce_bytes),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad,
                },
            )
            .map_err(|_| KmsError::Crypto("KMS wrap failed".to_owned()))?;

        let mut blob = Vec::with_capacity(12 + ct.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ct);
        Ok(WrappedKeyBlob {
            key_ref: key_ref.clone(),
            ciphertext: blob,
        })
    }

    async fn unwrap_key(
        &self,
        key_ref: &KmsKeyRef,
        wrapped: &WrappedKeyBlob,
        aad: &[u8],
    ) -> Result<Key256, KmsError> {
        self.check_available()?;
        if wrapped.key_ref != *key_ref {
            return Err(KmsError::Crypto("wrapped blob key_ref mismatch".to_owned()));
        }
        if wrapped.ciphertext.len() < 12 + 16 {
            return Err(KmsError::Crypto("truncated wrapped key blob".to_owned()));
        }

        let kek = {
            let guard = self
                .state
                .read()
                .map_err(|_| KmsError::Unavailable("KMS lock poisoned".to_owned()))?;
            if guard.destroyed.contains(key_ref) {
                return Err(KmsError::KeyDestroyed(key_ref.as_str().to_owned()));
            }
            guard
                .keys
                .get(key_ref)
                .cloned()
                .ok_or_else(|| KmsError::KeyNotFound(key_ref.as_str().to_owned()))?
        };

        let (nonce_bytes, ct) = wrapped.ciphertext.split_at(12);
        let cipher = Aes256Gcm::new_from_slice(kek.as_bytes())
            .map_err(|_| KmsError::Crypto("invalid KEK length".to_owned()))?;
        let nonce_arr: [u8; 12] = nonce_bytes
            .try_into()
            .map_err(|_| KmsError::Crypto("invalid nonce length".to_owned()))?;
        let pt = cipher
            .decrypt(&Nonce::from(nonce_arr), Payload { msg: ct, aad })
            .map_err(|_| KmsError::Crypto("KMS unwrap authentication failed".to_owned()))?;
        let arr: [u8; 32] = pt
            .try_into()
            .map_err(|_| KmsError::Crypto("unwrapped key is not 32 bytes".to_owned()))?;
        Ok(Key256::from_bytes(arr))
    }

    fn user_wrapping_secret_ref(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError> {
        KmsKeyRef::new(format!("kms:user:{}:{}", tenant.as_str(), account))
    }

    fn tenant_kek_ref(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError> {
        KmsKeyRef::new(format!("kms:tenant:{}", tenant.as_str()))
    }

    async fn ensure_user_wrapping_secret(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError> {
        let key_ref = self.user_wrapping_secret_ref(tenant, account)?;
        self.ensure_key_inner(key_ref)
    }

    async fn ensure_tenant_kek(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError> {
        let key_ref = self.tenant_kek_ref(tenant)?;
        self.ensure_key_inner(key_ref)
    }

    async fn destroy_wrapping_secret(&self, key_ref: &KmsKeyRef) -> Result<(), KmsError> {
        self.check_available()?;
        let mut guard = self
            .state
            .write()
            .map_err(|_| KmsError::Unavailable("KMS lock poisoned".to_owned()))?;
        guard.keys.remove(key_ref);
        guard.destroyed.insert(key_ref.clone());
        Ok(())
    }

    async fn is_destroyed(&self, key_ref: &KmsKeyRef) -> Result<bool, KmsError> {
        self.check_available()?;
        let guard = self
            .state
            .read()
            .map_err(|_| KmsError::Unavailable("KMS lock poisoned".to_owned()))?;
        Ok(guard.destroyed.contains(key_ref))
    }
}
