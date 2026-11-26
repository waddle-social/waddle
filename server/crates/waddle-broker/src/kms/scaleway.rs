//! Scaleway Key Manager HTTP API [`KmsProvider`] client.
//!
//! Uses `reqwest::Client::builder().no_proxy()` with `X-Auth-Token` header and
//! Scaleway Key Manager v1alpha1 endpoints:
//! - `GET  /key-manager/v1alpha1/regions/{region}/keys?project_id=...&name=...`
//! - `POST /key-manager/v1alpha1/regions/{region}/keys`
//! - `GET  /key-manager/v1alpha1/regions/{region}/keys/{key_id}`
//! - `POST /key-manager/v1alpha1/regions/{region}/keys/{key_id}/encrypt`
//! - `POST /key-manager/v1alpha1/regions/{region}/keys/{key_id}/decrypt`
//! - `POST /key-manager/v1alpha1/regions/{region}/keys/{key_id}/delete-key-material`

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};
use zeroize::Zeroizing;

use crate::error::KmsError;
use crate::kms::{
    decode_b64, decode_b64_key256, encode_b64, prefixed_tenant_key_ref, prefixed_user_key_ref,
    validate_non_empty_config, verify_wrapped_key_ref, KmsKeyRef, KmsProvider, WrappedKeyBlob,
};
use crate::secret::Key256;

/// Configuration for [`ScalewayKmsProvider`].
#[derive(Clone)]
pub struct ScalewayKmsConfig {
    pub base_url: String,
    pub region: String,
    pub project_id: String,
    pub auth_token: Zeroizing<String>,
    pub root_key_id: Option<String>,
    pub timeout: Duration,
}

impl std::fmt::Debug for ScalewayKmsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScalewayKmsConfig")
            .field("base_url", &self.base_url)
            .field("region", &self.region)
            .field("project_id", &self.project_id)
            .field("auth_token", &"[REDACTED]")
            .field("root_key_id", &self.root_key_id)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Serialize)]
struct ScalewayKeyUsage {
    symmetric_encryption: &'static str,
}

#[derive(Serialize)]
struct ScalewayCreateKeyRequest<'a> {
    project_id: &'a str,
    name: &'a str,
    usage: ScalewayKeyUsage,
    unprotected: bool,
    origin: &'static str,
}

#[derive(Deserialize)]
struct ScalewayKeyRecord {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize)]
struct ScalewayListKeysResponse {
    #[serde(default)]
    keys: Vec<ScalewayKeyRecord>,
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

#[derive(Serialize)]
struct ScalewayEmptyBody {}

fn is_destroyed_state(state: Option<&str>) -> bool {
    matches!(
        state.map(str::to_ascii_lowercase).as_deref(),
        Some("destroyed" | "deleted" | "empty" | "pending_key_material")
    )
}

/// Scaleway Key Manager broker-only KMS provider.
pub struct ScalewayKmsProvider {
    base_url: String,
    region: String,
    project_id: String,
    root_key_id: Option<String>,
    auth_token: Zeroizing<String>,
    client: reqwest::Client,
    /// Cache mapping logical key names (`scw-tenant-...`, `scw-user-...`) to
    /// Scaleway Key Manager key UUIDs.
    key_cache: RwLock<HashMap<String, String>>,
}

impl ScalewayKmsProvider {
    pub fn new(config: ScalewayKmsConfig) -> Result<Self, KmsError> {
        validate_non_empty_config(
            &config.base_url,
            &config.region,
            "Scaleway KMS base_url and region must be non-empty",
        )?;
        let project_id = config.project_id.trim().to_owned();
        let root_key_id = config
            .root_key_id
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty());
        if project_id.is_empty() && root_key_id.is_none() {
            return Err(KmsError::InvalidConfig(
                "Scaleway KMS project_id or root_key_id must be non-empty".to_owned(),
            ));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout)
            .build()
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        Ok(Self {
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            region: config.region.trim().to_owned(),
            project_id,
            root_key_id,
            auth_token: config.auth_token,
            client,
            key_cache: RwLock::new(HashMap::new()),
        })
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn root_key_id(&self) -> Option<&str> {
        self.root_key_id.as_deref()
    }

    fn keys_collection_url(&self) -> String {
        format!(
            "{}/key-manager/v1alpha1/regions/{}/keys",
            self.base_url, self.region
        )
    }

    fn key_base_url(&self, key_id: &str) -> String {
        format!("{}/{key_id}", self.keys_collection_url())
    }

    fn key_url(&self, key_id: &str, action: &str) -> String {
        format!("{}/{action}", self.key_base_url(key_id))
    }

    fn cached_key_id(&self, logical_name: &str) -> Option<String> {
        self.key_cache
            .read()
            .ok()
            .and_then(|cache| cache.get(logical_name).cloned())
    }

    fn cache_key_id(&self, logical_name: &str, key_id: &str) {
        if let Ok(mut cache) = self.key_cache.write() {
            cache.insert(logical_name.to_owned(), key_id.to_owned());
        }
    }

    fn evict_cached_key(&self, key_ref: &str, resolved_id: &str) {
        if let Ok(mut cache) = self.key_cache.write() {
            cache.retain(|name, id| name != key_ref && id != resolved_id && id != key_ref);
        }
    }

    fn resolve_cached_or_raw(&self, key_ref: &KmsKeyRef) -> String {
        self.cached_key_id(key_ref.as_str())
            .unwrap_or_else(|| key_ref.as_str().to_owned())
    }

    async fn lookup_key_id_by_name(&self, logical_name: &str) -> Result<Option<String>, KmsError> {
        let mut url = url::Url::parse(&self.keys_collection_url())
            .map_err(|err| KmsError::InvalidConfig(format!("invalid Scaleway KMS URL: {err}")))?;
        url.query_pairs_mut()
            .append_pair("project_id", self.project_id.as_str())
            .append_pair("name", logical_name);
        let resp = self
            .client
            .get(url)
            .header("X-Auth-Token", self.auth_token.as_str())
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(KmsError::Unavailable(format!(
                "Scaleway KMS list keys returned HTTP {status}"
            )));
        }
        let list: ScalewayListKeysResponse = resp
            .json()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        let found = list
            .keys
            .into_iter()
            .find(|k| k.name == logical_name && !is_destroyed_state(k.state.as_deref()))
            .map(|k| k.id);
        Ok(found)
    }

    async fn create_symmetric_key(&self, logical_name: &str) -> Result<String, KmsError> {
        let req = ScalewayCreateKeyRequest {
            project_id: &self.project_id,
            name: logical_name,
            usage: ScalewayKeyUsage {
                symmetric_encryption: "aes_256_gcm",
            },
            unprotected: false,
            origin: "scaleway_kms",
        };
        let resp = self
            .client
            .post(self.keys_collection_url())
            .header("X-Auth-Token", self.auth_token.as_str())
            .json(&req)
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(KmsError::Unavailable(format!(
                "Scaleway KMS create key returned HTTP {status}"
            )));
        }
        let created: ScalewayKeyRecord = resp
            .json()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        if created.id.trim().is_empty() {
            return Err(KmsError::Unavailable(
                "Scaleway KMS create key returned empty id".to_owned(),
            ));
        }
        Ok(created.id)
    }

    async fn ensure_key_by_name(&self, logical_name: &str) -> Result<KmsKeyRef, KmsError> {
        if let Some(cached) = self.cached_key_id(logical_name) {
            return KmsKeyRef::new(cached);
        }
        if self.project_id.is_empty() {
            if let Some(root_id) = &self.root_key_id {
                return KmsKeyRef::new(root_id.clone());
            }
            return Err(KmsError::InvalidConfig(
                "Scaleway KMS project_id is required to provision keys".to_owned(),
            ));
        }
        if let Some(existing_id) = self.lookup_key_id_by_name(logical_name).await? {
            self.cache_key_id(logical_name, &existing_id);
            return KmsKeyRef::new(existing_id);
        }
        let created_id = self.create_symmetric_key(logical_name).await?;
        self.cache_key_id(logical_name, &created_id);
        KmsKeyRef::new(created_id)
    }

    async fn resolve_key_id_for_lookup(
        &self,
        key_ref: &KmsKeyRef,
    ) -> Result<Option<String>, KmsError> {
        if let Some(cached) = self.cached_key_id(key_ref.as_str()) {
            return Ok(Some(cached));
        }
        if Uuid::parse_str(key_ref.as_str()).is_ok() {
            return Ok(Some(key_ref.as_str().to_owned()));
        }
        if self.project_id.is_empty() {
            return Ok(Some(key_ref.as_str().to_owned()));
        }
        self.lookup_key_id_by_name(key_ref.as_str()).await
    }

    async fn post_key_operation<Req, Resp>(
        &self,
        key_ref: &KmsKeyRef,
        action: &str,
        payload: &Req,
    ) -> Result<Resp, KmsError>
    where
        Req: Serialize + ?Sized,
        Resp: serde::de::DeserializeOwned,
    {
        let key_id = self.resolve_cached_or_raw(key_ref);
        let url = self.key_url(&key_id, action);
        let resp = self
            .client
            .post(&url)
            .header("X-Auth-Token", self.auth_token.as_str())
            .json(payload)
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        let status = resp.status();
        match status.as_u16() {
            404 => Err(KmsError::KeyNotFound(key_id)),
            409 | 410 | 412 => Err(KmsError::KeyDestroyed(key_id)),
            _ if status.is_success() => resp
                .json()
                .await
                .map_err(|err| KmsError::Transport(err.to_string())),
            _ => Err(KmsError::Unavailable(format!(
                "Scaleway KMS {action} returned HTTP {status}"
            ))),
        }
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
        let body: ScalewayEncryptResponse =
            self.post_key_operation(key_ref, "encrypt", &req).await?;
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
        let body: ScalewayDecryptResponse =
            self.post_key_operation(key_ref, "decrypt", &req).await?;
        decode_b64_key256(&body.plaintext)
    }

    fn user_wrapping_secret_ref(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError> {
        let logical = prefixed_user_key_ref("scw", tenant, account)?;
        if let Some(cached) = self.cached_key_id(logical.as_str()) {
            return KmsKeyRef::new(cached);
        }
        Ok(logical)
    }

    fn tenant_kek_ref(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError> {
        let logical = prefixed_tenant_key_ref("scw", tenant)?;
        if let Some(cached) = self.cached_key_id(logical.as_str()) {
            return KmsKeyRef::new(cached);
        }
        Ok(logical)
    }

    async fn ensure_user_wrapping_secret(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError> {
        let logical = prefixed_user_key_ref("scw", tenant, account)?;
        self.ensure_key_by_name(logical.as_str()).await
    }

    async fn ensure_tenant_kek(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError> {
        let logical = prefixed_tenant_key_ref("scw", tenant)?;
        self.ensure_key_by_name(logical.as_str()).await
    }

    async fn destroy_wrapping_secret(&self, key_ref: &KmsKeyRef) -> Result<(), KmsError> {
        let Some(key_id) = self.resolve_key_id_for_lookup(key_ref).await? else {
            return Err(KmsError::KeyNotFound(key_ref.as_str().to_owned()));
        };
        let url = self.key_url(&key_id, "delete-key-material");
        let resp = self
            .client
            .post(&url)
            .header("X-Auth-Token", self.auth_token.as_str())
            .json(&ScalewayEmptyBody {})
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        let status = resp.status();
        match status.as_u16() {
            404 => Err(KmsError::KeyNotFound(key_id)),
            410 => Err(KmsError::KeyDestroyed(key_id)),
            _ if status.is_success() => {
                self.evict_cached_key(key_ref.as_str(), &key_id);
                Ok(())
            }
            _ => Err(KmsError::Unavailable(format!(
                "Scaleway KMS delete-key-material returned HTTP {status}"
            ))),
        }
    }

    async fn is_destroyed(&self, key_ref: &KmsKeyRef) -> Result<bool, KmsError> {
        let Some(key_id) = self.resolve_key_id_for_lookup(key_ref).await? else {
            return Ok(true);
        };
        let resp = self
            .client
            .get(self.key_base_url(&key_id))
            .header("X-Auth-Token", self.auth_token.as_str())
            .send()
            .await
            .map_err(|err| KmsError::Transport(err.to_string()))?;
        let status = resp.status();
        if matches!(status.as_u16(), 404 | 410) {
            return Ok(true);
        }
        if !status.is_success() {
            return Err(KmsError::Unavailable(format!(
                "Scaleway KMS get key returned HTTP {status}"
            )));
        }
        let body = resp.bytes().await.unwrap_or_default();
        if body.is_empty() {
            return Ok(false);
        }
        match serde_json::from_slice::<ScalewayKeyRecord>(&body) {
            Ok(record) => Ok(is_destroyed_state(record.state.as_deref())),
            Err(_) => Ok(false),
        }
    }
}
