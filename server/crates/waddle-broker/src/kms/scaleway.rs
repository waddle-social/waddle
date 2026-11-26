//! Scaleway Key Manager HTTP API [`KmsProvider`] client.
//!
//! Uses `reqwest::Client::builder().no_proxy()` with `X-Auth-Token` header and
//! Scaleway Key Manager endpoints:
//! - `POST /key-manager/v1alpha1/regions/{region}/keys/{key_id}/encrypt`
//! - `POST /key-manager/v1alpha1/regions/{region}/keys/{key_id}/decrypt`
//! - `POST /key-manager/v1alpha1/regions/{region}/keys/{key_id}/delete-key-material`

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use waddle_broker_core::identity::{AccountId, TenantId};
use zeroize::Zeroizing;

use crate::error::KmsError;
use crate::kms::{
    decode_b64, decode_b64_key256, encode_b64, prefixed_tenant_key_ref, prefixed_user_key_ref,
    validate_non_empty_config, verify_wrapped_key_ref, HttpKmsClient, KmsKeyRef, KmsProvider,
    WrappedKeyBlob,
};
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
    base_url: String,
    region: String,
    http: HttpKmsClient,
}

impl ScalewayKmsProvider {
    pub fn new(config: ScalewayKmsConfig) -> Result<Self, KmsError> {
        validate_non_empty_config(
            &config.base_url,
            &config.region,
            "Scaleway KMS base_url and region must be non-empty",
        )?;
        let http = HttpKmsClient::new(
            "Scaleway KMS",
            "X-Auth-Token",
            config.auth_token,
            None,
            config.timeout,
        )?;
        Ok(Self {
            base_url: config.base_url,
            region: config.region,
            http,
        })
    }

    fn key_base_url(&self, key_ref: &KmsKeyRef) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!(
            "{base}/key-manager/v1alpha1/regions/{}/keys/{}",
            self.region,
            key_ref.as_str()
        )
    }

    fn key_url(&self, key_ref: &KmsKeyRef, action: &str) -> String {
        format!("{}/{action}", self.key_base_url(key_ref))
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
            plaintext: encode_b64(plaintext.as_bytes()),
            associated_data: encode_b64(aad),
        };
        let url = self.key_url(key_ref, "encrypt");
        let body: ScalewayEncryptResponse = self.http.post_json(&url, "encrypt", &req).await?;
        let ciphertext = decode_b64(&body.ciphertext)?;
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
        verify_wrapped_key_ref(key_ref, wrapped)?;
        let req = ScalewayDecryptRequest {
            ciphertext: encode_b64(&wrapped.ciphertext),
            associated_data: encode_b64(aad),
        };
        let url = self.key_url(key_ref, "decrypt");
        let body: ScalewayDecryptResponse = self.http.post_json(&url, "decrypt", &req).await?;
        decode_b64_key256(&body.plaintext)
    }

    fn user_wrapping_secret_ref(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError> {
        prefixed_user_key_ref("scw", tenant, account)
    }

    fn tenant_kek_ref(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError> {
        prefixed_tenant_key_ref("scw", tenant)
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
        let url = self.key_url(key_ref, "delete-key-material");
        self.http
            .destroy_key(reqwest::Method::POST, &url, "delete-key-material", key_ref)
            .await
    }

    async fn is_destroyed(&self, key_ref: &KmsKeyRef) -> Result<bool, KmsError> {
        let status = self.http.get_status(&self.key_base_url(key_ref)).await?;
        Ok(matches!(status.as_u16(), 404 | 410))
    }
}
