//! Scaleway Key Manager HTTP API [`KmsProvider`] client.
//!
//! Uses `reqwest::Client::builder().no_proxy()` with `X-Auth-Token` header and
//! Scaleway Key Manager endpoints:
//! - `POST /key-manager/v1alpha1/regions/{region}/keys/{key_id}/encrypt`
//! - `POST /key-manager/v1alpha1/regions/{region}/keys/{key_id}/decrypt`
//! - `POST /key-manager/v1alpha1/regions/{region}/keys/{key_id}/delete-key-material`

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

/// Configuration for [`ScalewayKmsProvider`].
#[derive(Clone)]
pub struct ScalewayKmsConfig {
    pub base_url: String,
    pub region: String,
    pub auth_token: Zeroizing<String>,
    pub timeout: Duration,
}

impl std::fmt::Debug for ScalewayKmsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScalewayKmsConfig")
            .field("base_url", &self.base_url)
            .field("region", &self.region)
            .field("auth_token", &"[REDACTED]")
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Serialize)]
struct ScalewayEncryptRequest {
    plaintext: String,
    associated_data: String,
}

#[derive(Deserialize)]
struct ScalewayEncryptResponse {
    ciphertext: String,
}

#[derive(Serialize)]
struct ScalewayDecryptRequest {
    ciphertext: String,
    associated_data: String,
}

#[derive(Deserialize)]
struct ScalewayDecryptResponse {
    plaintext: String,
}

/// Scaleway Key Manager broker-only KMS provider.
pub struct ScalewayKmsProvider {
    config: ScalewayKmsConfig,
    client: reqwest::Client,
}

impl ScalewayKmsProvider {
    pub fn new(config: ScalewayKmsConfig) -> Result<Self, KmsError> {
        if config.base_url.trim().is_empty() || config.region.trim().is_empty() {
            return Err(KmsError::InvalidConfig(
                "Scaleway KMS base_url and region must be non-empty".to_owned(),
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

    fn key_url(&self, key_ref: &KmsKeyRef, action: &str) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        format!(
            "{base}/key-manager/v1alpha1/regions/{}/keys/{}/{action}",
            self.config.region,
            key_ref.as_str()
        )
    }
}

#[async_trait]
impl KmsProvider for ScalewayKmsProvider {
    async fn wrap_key(
        &self,
        key_ref: &KmsKeyRef,
        plaintext: &Key256,
        aad: &[u8],
    ) -> Result<WrappedKeyBlob, KmsError> {
        let req = ScalewayEncryptRequest {
            plaintext: STANDARD.encode(plaintext.as_bytes()),
            associated_data: STANDARD.encode(aad),
        };
        let resp = self
            .client
            .post(self.key_url(key_ref, "encrypt"))
            .header("X-Auth-Token", self.config.auth_token.as_str())
            .json(&req)
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;

        if !resp.status().is_success() {
            return Err(KmsError::Unavailable(format!(
                "Scaleway KMS encrypt returned HTTP {}",
                resp.status()
            )));
        }
        let body: ScalewayEncryptResponse = resp
            .json()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        let ciphertext = STANDARD
            .decode(body.ciphertext)
            .map_err(|err| KmsError::Crypto(err.to_string()))?;
        Ok(WrappedKeyBlob {
            key_ref: key_ref.clone(),
            ciphertext,
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
        let req = ScalewayDecryptRequest {
            ciphertext: STANDARD.encode(&wrapped.ciphertext),
            associated_data: STANDARD.encode(aad),
        };
        let resp = self
            .client
            .post(self.key_url(key_ref, "decrypt"))
            .header("X-Auth-Token", self.config.auth_token.as_str())
            .json(&req)
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;

        if !resp.status().is_success() {
            return Err(KmsError::Unavailable(format!(
                "Scaleway KMS decrypt returned HTTP {}",
                resp.status()
            )));
        }
        let body: ScalewayDecryptResponse = resp
            .json()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        let pt = STANDARD
            .decode(body.plaintext)
            .map_err(|err| KmsError::Crypto(err.to_string()))?;
        let arr: [u8; 32] = pt
            .try_into()
            .map_err(|_| KmsError::Crypto("unwrapped key is not 32 bytes".to_owned()))?;
        Ok(Key256::from_bytes(arr))
    }

    async fn ensure_user_wrapping_secret(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError> {
        KmsKeyRef::new(format!("scw-user-{}-{}", tenant.as_str(), account))
    }

    async fn ensure_tenant_kek(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError> {
        KmsKeyRef::new(format!("scw-tenant-{}", tenant.as_str()))
    }

    async fn destroy_wrapping_secret(&self, key_ref: &KmsKeyRef) -> Result<(), KmsError> {
        let resp = self
            .client
            .post(self.key_url(key_ref, "delete-key-material"))
            .header("X-Auth-Token", self.config.auth_token.as_str())
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        if !resp.status().is_success() {
            return Err(KmsError::Unavailable(format!(
                "Scaleway KMS delete-key-material returned HTTP {}",
                resp.status()
            )));
        }
        Ok(())
    }

    async fn is_destroyed(&self, key_ref: &KmsKeyRef) -> Result<bool, KmsError> {
        let base = self.config.base_url.trim_end_matches('/');
        let url = format!(
            "{base}/key-manager/v1alpha1/regions/{}/keys/{}",
            self.config.region,
            key_ref.as_str()
        );
        let resp = self
            .client
            .get(url)
            .header("X-Auth-Token", self.config.auth_token.as_str())
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        Ok(resp.status().as_u16() == 404 || resp.status().as_u16() == 410)
    }
}
