//! Broker-only Key Management Service (KMS) abstraction and providers.

pub mod local;
pub mod scaleway;
pub mod vault;

use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use waddle_broker_core::identity::{AccountId, TenantId};
use zeroize::Zeroizing;

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

    /// Resolves the provider-specific KMS key reference for `(tenant, account)`
    /// without creating or mutating key material.
    fn user_wrapping_secret_ref(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError>;

    /// Resolves the provider-specific KMS key reference for `tenant`
    /// without creating or mutating key material.
    fn tenant_kek_ref(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError>;

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

/// Shared no-proxy, no-redirect HTTP client and error mapper for HTTP KMS backends.
pub(crate) struct HttpKmsClient {
    provider_name: &'static str,
    auth_header: &'static str,
    auth_token: Zeroizing<String>,
    extra_header: Option<(&'static str, String)>,
    client: reqwest::Client,
}

impl HttpKmsClient {
    pub(crate) fn new(
        provider_name: &'static str,
        auth_header: &'static str,
        auth_token: Zeroizing<String>,
        extra_header: Option<(&'static str, String)>,
        timeout: Duration,
    ) -> Result<Self, KmsError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        Ok(Self {
            provider_name,
            auth_header,
            auth_token,
            extra_header,
            client,
        })
    }

    fn apply_headers(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req = req.header(self.auth_header, self.auth_token.as_str());
        if let Some((name, value)) = &self.extra_header {
            req = req.header(*name, value.as_str());
        }
        req
    }

    pub(crate) async fn post_json<Req, Resp>(
        &self,
        url: &str,
        operation: &str,
        payload: &Req,
    ) -> Result<Resp, KmsError>
    where
        Req: Serialize + ?Sized,
        Resp: DeserializeOwned,
    {
        let resp = self
            .apply_headers(self.client.post(url))
            .json(payload)
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        if !resp.status().is_success() {
            return Err(KmsError::Unavailable(format!(
                "{} {operation} returned HTTP {}",
                self.provider_name,
                resp.status()
            )));
        }
        resp.json()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))
    }

    pub(crate) async fn destroy_key(
        &self,
        method: reqwest::Method,
        url: &str,
        operation: &str,
        key_ref: &KmsKeyRef,
    ) -> Result<(), KmsError> {
        let resp = self
            .apply_headers(self.client.request(method, url))
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        let status = resp.status();
        match status.as_u16() {
            404 => Err(KmsError::KeyNotFound(key_ref.as_str().to_owned())),
            410 => Err(KmsError::KeyDestroyed(key_ref.as_str().to_owned())),
            _ if status.is_success() => Ok(()),
            _ => Err(KmsError::Unavailable(format!(
                "{} {operation} returned HTTP {status}",
                self.provider_name
            ))),
        }
    }

    pub(crate) async fn get_status(&self, url: &str) -> Result<reqwest::StatusCode, KmsError> {
        let resp = self
            .apply_headers(self.client.get(url))
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        Ok(resp.status())
    }
}

pub(crate) fn validate_non_empty_config(
    first: &str,
    second: &str,
    error_message: &str,
) -> Result<(), KmsError> {
    if first.trim().is_empty() || second.trim().is_empty() {
        return Err(KmsError::InvalidConfig(error_message.to_owned()));
    }
    Ok(())
}

pub(crate) fn verify_wrapped_key_ref(
    key_ref: &KmsKeyRef,
    wrapped: &WrappedKeyBlob,
) -> Result<(), KmsError> {
    if wrapped.key_ref != *key_ref {
        return Err(KmsError::Crypto("wrapped blob key_ref mismatch".to_owned()));
    }
    Ok(())
}

pub(crate) fn encode_b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub(crate) fn decode_b64(encoded: &str) -> Result<Vec<u8>, KmsError> {
    STANDARD
        .decode(encoded)
        .map_err(|err| KmsError::Crypto(err.to_string()))
}

pub(crate) fn decode_b64_key256(encoded: &str) -> Result<Key256, KmsError> {
    let plaintext = decode_b64(encoded)?;
    let arr: [u8; 32] = plaintext
        .try_into()
        .map_err(|_| KmsError::Crypto("unwrapped key is not 32 bytes".to_owned()))?;
    Ok(Key256::from_bytes(arr))
}

pub(crate) fn prefixed_user_key_ref(
    prefix: &str,
    tenant: &TenantId,
    account: AccountId,
) -> Result<KmsKeyRef, KmsError> {
    KmsKeyRef::new(format!("{prefix}-user-{}-{}", tenant.as_str(), account))
}

pub(crate) fn prefixed_tenant_key_ref(
    prefix: &str,
    tenant: &TenantId,
) -> Result<KmsKeyRef, KmsError> {
    KmsKeyRef::new(format!("{prefix}-tenant-{}", tenant.as_str()))
}
