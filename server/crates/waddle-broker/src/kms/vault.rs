//! HashiCorp Vault Transit HTTP API [`KmsProvider`] client.
//!
//! Uses `reqwest::Client::builder().no_proxy()` with `X-Vault-Token` and
//! optional `X-Vault-Namespace` headers hitting:
//! - `POST /v1/{mount}/encrypt/{key}`
//! - `POST /v1/{mount}/decrypt/{key}`
//! - `DELETE /v1/{mount}/keys/{key}`

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use waddle_broker_core::identity::{AccountId, TenantId};
use zeroize::Zeroizing;

use crate::error::KmsError;
use crate::kms::{
    decode_b64_key256, encode_b64, prefixed_tenant_key_ref, prefixed_user_key_ref,
    validate_non_empty_config, verify_wrapped_key_ref, HttpKmsClient, KmsKeyRef, KmsProvider,
    WrappedKeyBlob,
};
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
    base_url: String,
    mount: String,
    http: HttpKmsClient,
}

impl VaultTransitKmsProvider {
    pub fn new(config: VaultTransitConfig) -> Result<Self, KmsError> {
        validate_non_empty_config(
            &config.base_url,
            &config.mount,
            "Vault Transit base_url and mount must be non-empty",
        )?;
        let extra_header = config.namespace.map(|ns| ("X-Vault-Namespace", ns));
        let http = HttpKmsClient::new(
            "Vault Transit",
            "X-Vault-Token",
            config.token,
            extra_header,
            config.timeout,
        )?;
        Ok(Self {
            base_url: config.base_url,
            mount: config.mount,
            http,
        })
    }

    fn transit_url(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        let mount = self.mount.trim_matches('/');
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
            plaintext: encode_b64(plaintext.as_bytes()),
            context: encode_b64(aad),
        };
        let url = self.transit_url(&format!("encrypt/{}", key_ref.as_str()));
        let body: VaultEncryptResponse = self.http.post_json(&url, "encrypt", &req).await?;
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
        verify_wrapped_key_ref(key_ref, wrapped)?;
        let ciphertext_str = String::from_utf8(wrapped.ciphertext.clone())
            .map_err(|_| KmsError::Crypto("Vault ciphertext must be UTF-8".to_owned()))?;
        let req = VaultDecryptRequest {
            ciphertext: ciphertext_str,
            context: encode_b64(aad),
        };
        let url = self.transit_url(&format!("decrypt/{}", key_ref.as_str()));
        let body: VaultDecryptResponse = self.http.post_json(&url, "decrypt", &req).await?;
        decode_b64_key256(&body.data.plaintext)
    }

    fn user_wrapping_secret_ref(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError> {
        prefixed_user_key_ref("vault", tenant, account)
    }

    fn tenant_kek_ref(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError> {
        prefixed_tenant_key_ref("vault", tenant)
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
        self.http
            .destroy_key(reqwest::Method::DELETE, &url, "delete key", key_ref)
            .await
    }

    async fn is_destroyed(&self, key_ref: &KmsKeyRef) -> Result<bool, KmsError> {
        let url = self.transit_url(&format!("keys/{}", key_ref.as_str()));
        let status = self.http.get_status(&url).await?;
        Ok(status.as_u16() == 404)
    }
}
