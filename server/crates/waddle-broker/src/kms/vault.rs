//! HashiCorp Vault Transit HTTP API [`KmsProvider`] client.
//!
//! Uses `reqwest::Client::builder().no_proxy()` with `X-Vault-Token` and
//! optional `X-Vault-Namespace` headers hitting:
//! - `POST /v1/{mount}/encrypt/{key}`
//! - `POST /v1/{mount}/decrypt/{key}`
//! - `DELETE /v1/{mount}/keys/{key}`

use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use waddle_broker_core::identity::{AccountId, TenantId};
use zeroize::Zeroizing;

use crate::error::KmsError;
use crate::kms::{KmsKeyRef, KmsProvider, WrappedKeyBlob};
use crate::secret::Key256;

/// Configuration for [`VaultTransitKmsProvider`].
#[derive(Clone)]
pub struct VaultTransitConfig {
    pub base_url: String,
    pub mount: String,
    pub token: Zeroizing<String>,
    pub namespace: Option<String>,
    pub timeout: Duration,
}

impl std::fmt::Debug for VaultTransitConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultTransitConfig")
            .field("base_url", &self.base_url)
            .field("mount", &self.mount)
            .field("token", &"[REDACTED]")
            .field("namespace", &self.namespace)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Serialize)]
struct VaultEncryptRequest {
    plaintext: String,
    context: String,
}

#[derive(Deserialize)]
struct VaultEncryptData {
    ciphertext: String,
}

#[derive(Deserialize)]
struct VaultEncryptResponse {
    data: VaultEncryptData,
}

#[derive(Serialize)]
struct VaultDecryptRequest {
    ciphertext: String,
    context: String,
}

#[derive(Deserialize)]
struct VaultDecryptData {
    plaintext: String,
}

#[derive(Deserialize)]
struct VaultDecryptResponse {
    data: VaultDecryptData,
}

/// HashiCorp Vault Transit broker-only KMS provider.
pub struct VaultTransitKmsProvider {
    config: VaultTransitConfig,
    client: reqwest::Client,
}

impl VaultTransitKmsProvider {
    pub fn new(config: VaultTransitConfig) -> Result<Self, KmsError> {
        if config.base_url.trim().is_empty() || config.mount.trim().is_empty() {
            return Err(KmsError::InvalidConfig(
                "Vault Transit base_url and mount must be non-empty".to_owned(),
            ));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout)
            .build()
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        Ok(Self { config, client })
    }

    fn apply_headers(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req = req.header("X-Vault-Token", self.config.token.as_str());
        if let Some(ns) = &self.config.namespace {
            req = req.header("X-Vault-Namespace", ns.as_str());
        }
        req
    }

    fn transit_url(&self, path: &str) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        let mount = self.config.mount.trim_matches('/');
        format!("{base}/v1/{mount}/{path}")
    }
}

#[async_trait]
impl KmsProvider for VaultTransitKmsProvider {
    async fn wrap_key(
        &self,
        key_ref: &KmsKeyRef,
        plaintext: &Key256,
        aad: &[u8],
    ) -> Result<WrappedKeyBlob, KmsError> {
        let req = VaultEncryptRequest {
            plaintext: STANDARD.encode(plaintext.as_bytes()),
            context: STANDARD.encode(aad),
        };
        let url = self.transit_url(&format!("encrypt/{}", key_ref.as_str()));
        let resp = self
            .apply_headers(self.client.post(url))
            .json(&req)
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        if !resp.status().is_success() {
            return Err(KmsError::Unavailable(format!(
                "Vault Transit encrypt returned HTTP {}",
                resp.status()
            )));
        }
        let body: VaultEncryptResponse = resp
            .json()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        Ok(WrappedKeyBlob {
            key_ref: key_ref.clone(),
            ciphertext: body.data.ciphertext.into_bytes(),
        })
    }

    async fn unwrap_key(
        &self,
        key_ref: &KmsKeyRef,
        wrapped: &WrappedKeyBlob,
        aad: &[u8],
    ) -> Result<Key256, KmsError> {
        if wrapped.key_ref != *key_ref {
            return Err(KmsError::Crypto("wrapped blob key_ref mismatch".to_owned()));
        }
        let ciphertext_str = String::from_utf8(wrapped.ciphertext.clone())
            .map_err(|_| KmsError::Crypto("Vault ciphertext must be UTF-8".to_owned()))?;
        let req = VaultDecryptRequest {
            ciphertext: ciphertext_str,
            context: STANDARD.encode(aad),
        };
        let url = self.transit_url(&format!("decrypt/{}", key_ref.as_str()));
        let resp = self
            .apply_headers(self.client.post(url))
            .json(&req)
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        if !resp.status().is_success() {
            return Err(KmsError::Unavailable(format!(
                "Vault Transit decrypt returned HTTP {}",
                resp.status()
            )));
        }
        let body: VaultDecryptResponse = resp
            .json()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        let pt = STANDARD
            .decode(body.data.plaintext)
            .map_err(|err| KmsError::Crypto(err.to_string()))?;
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
        KmsKeyRef::new(format!("vault-user-{}-{}", tenant.as_str(), account))
    }

    fn tenant_kek_ref(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError> {
        KmsKeyRef::new(format!("vault-tenant-{}", tenant.as_str()))
    }

    async fn ensure_user_wrapping_secret(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError> {
        self.user_wrapping_secret_ref(tenant, account)
    }

    async fn ensure_tenant_kek(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError> {
        self.tenant_kek_ref(tenant)
    }

    async fn destroy_wrapping_secret(&self, key_ref: &KmsKeyRef) -> Result<(), KmsError> {
        let url = self.transit_url(&format!("keys/{}", key_ref.as_str()));
        let resp = self
            .apply_headers(self.client.delete(url))
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Err(KmsError::KeyNotFound(key_ref.as_str().to_owned()));
        }
        if status.as_u16() == 410 {
            return Err(KmsError::KeyDestroyed(key_ref.as_str().to_owned()));
        }
        if !status.is_success() {
            return Err(KmsError::Unavailable(format!(
                "Vault Transit delete key returned HTTP {status}"
            )));
        }
        Ok(())
    }

    async fn is_destroyed(&self, key_ref: &KmsKeyRef) -> Result<bool, KmsError> {
        let url = self.transit_url(&format!("keys/{}", key_ref.as_str()));
        let resp = self
            .apply_headers(self.client.get(url))
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        Ok(resp.status().as_u16() == 404)
    }
}
