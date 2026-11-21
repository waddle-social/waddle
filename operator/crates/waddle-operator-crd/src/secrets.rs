use kube::KubeSchema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Strategy for providing runtime secrets to the Waddle server.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SecretProviderMode {
    /// Consume secrets synced by External Secrets Operator (`external-secrets.io`).
    #[default]
    ExternalSecrets,
    /// Generate cryptographic runtime secrets automatically for single-node quickstarts.
    Yolo,
}

/// External Secrets Operator API group version preference.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EsoApiVersion {
    /// Automatically discover `external-secrets.io/v1` first, falling back to `v1beta1`.
    #[default]
    Auto,
    /// Explicitly target `external-secrets.io/v1`.
    V1,
    /// Explicitly target `external-secrets.io/v1beta1`.
    V1Beta1,
}

/// Kind of ESO SecretStore referenced by an operator-managed `ExternalSecret`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SecretStoreKind {
    /// Namespaced `SecretStore` in the same namespace.
    SecretStore,
    /// Cluster-scoped `ClusterSecretStore` (must be permitted by operator allowlist).
    #[default]
    ClusterSecretStore,
}

/// Reference to an ESO `SecretStore` or `ClusterSecretStore`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SecretStoreRef {
    /// Store resource kind (`SecretStore` or `ClusterSecretStore`).
    #[serde(default)]
    pub kind: SecretStoreKind,
    /// Store resource name.
    #[x_kube(validation = "size(self) > 0")]
    pub name: String,
}

/// Remote vault reference mapped into a Kubernetes `Secret` key by ESO.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EsoRemoteKeyRef {
    /// Remote secret key/path in the backing secret manager.
    #[x_kube(validation = "size(self) > 0")]
    pub key: String,
    /// Optional property/field inside a structured secret item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    /// Optional version pin in the backing store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Additional secret key mapping for an operator-managed `ExternalSecret`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EsoDataMapping {
    /// Target key in the synced Kubernetes `Secret`.
    #[x_kube(validation = "size(self) > 0")]
    pub secret_key: String,
    /// Remote store reference.
    pub remote_ref: EsoRemoteKeyRef,
}

/// Core remote secret mappings used when the operator creates a managed `ExternalSecret`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EsoRemoteRefs {
    /// Remote ref for `WADDLE_SESSION_KEY` (>= 32 bytes).
    pub session_key: EsoRemoteKeyRef,
    /// Remote ref for `WADDLE_OCCUPANT_ID_SECRET` (>= 32 bytes).
    pub occupant_id_secret: EsoRemoteKeyRef,
    /// Optional remote ref for `WADDLE_SPICEDB_PRESHARED_KEY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spicedb_preshared_key: Option<EsoRemoteKeyRef>,
    /// Optional remote ref for `WADDLE_S3_ACCESS_KEY_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3_access_key_id: Option<EsoRemoteKeyRef>,
    /// Optional remote ref for `WADDLE_S3_SECRET_ACCESS_KEY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3_secret_access_key: Option<EsoRemoteKeyRef>,
    /// Optional remote ref for `WADDLE_CLUSTERING_KEYPAIR_POOL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clustering_keypair_pool: Option<EsoRemoteKeyRef>,
    /// Optional remote ref for `WADDLE_AUTH_PROVIDERS_JSON`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_providers_json: Option<EsoRemoteKeyRef>,
    /// Arbitrary extra mappings included in the managed `ExternalSecret`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_data: Vec<EsoDataMapping>,
}

/// Specification for an operator-managed `ExternalSecret` resource.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedExternalSecretSpec {
    /// ESO store reference (`SecretStore` or `ClusterSecretStore`).
    pub secret_store_ref: SecretStoreRef,
    /// Sync refresh interval (Kubernetes duration string, default `"1h"`).
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval: String,
    /// Target `Secret` name created by ESO (defaults to `<waddleName>-runtime-secrets`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_secret_name: Option<String>,
    /// Remote key mappings.
    pub remote_refs: EsoRemoteRefs,
}

/// Read-only file mount specification for Apple Push Notification service (APNs) private key.
///
/// The APNs PEM key is always mounted as a file (`0440`) and never injected via `envFrom`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApnsSecretMountSpec {
    /// Same-namespace `Secret` containing the APNs `.p8` PEM private key.
    #[x_kube(validation = "size(self) > 0")]
    pub secret_name: String,
    /// Key inside `secretName` containing the PEM bytes (default `WADDLE_APNS_KEY_PEM`).
    #[serde(default = "default_apns_pem_key")]
    pub secret_key: String,
    /// Apple Developer Team ID (`WADDLE_APNS_TEAM_ID`).
    #[x_kube(validation = "size(self) > 0")]
    pub team_id: String,
    /// Apple Push key ID (`WADDLE_APNS_KEY_ID`).
    #[x_kube(validation = "size(self) > 0")]
    pub key_id: String,
    /// Apple app bundle ID (`WADDLE_APNS_BUNDLE_ID`).
    #[x_kube(validation = "size(self) > 0")]
    pub bundle_id: String,
}

/// ESO-mode secret configuration (`ExternalSecret` owned externally or managed by this operator).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[x_kube(
    cel,
    validation = "(has(self.externalSecretName) && !has(self.managed)) || (!has(self.externalSecretName) && has(self.managed))"
)]
pub struct ExternalSecretsSpec {
    /// Name of a pre-existing `ExternalSecret` in the same namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_secret_name: Option<String>,

    /// Operator-managed `ExternalSecret` definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<ManagedExternalSecretSpec>,

    /// Preferred ESO API version (`Auto`, `V1`, `V1Beta1`).
    #[serde(default)]
    pub api_version: EsoApiVersion,

    /// Optional separate Secret holding `WADDLE_CLUSTERING_KEYPAIR_POOL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clustering_keypool_secret_name: Option<String>,

    /// Additional same-namespace Secrets mounted via `envFrom`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_secret_refs: Vec<String>,

    /// Optional APNs `.p8` key file mount (kept out of `envFrom`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apns_secret_ref: Option<ApnsSecretMountSpec>,
}

/// YOLO secret generation configuration for single-node dev/test deployments.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct YoloSecretsSpec {
    /// Optional custom Secret name (defaults to `<waddleName>-yolo-secrets`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_name: Option<String>,

    /// Emit `WADDLE_DB_LINEAGE_ACTION=enroll` (idempotent) for fresh single-node databases.
    #[serde(default = "default_true")]
    pub enroll_lineage: bool,

    /// Additional same-namespace Secrets mounted via `envFrom`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_secret_refs: Vec<String>,
}

impl Default for YoloSecretsSpec {
    fn default() -> Self {
        Self {
            secret_name: None,
            enroll_lineage: true,
            extra_secret_refs: Vec::new(),
        }
    }
}

/// Runtime secrets configuration (ESO by default, or opt-in YOLO generation).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[x_kube(
    cel,
    validation = "(self.mode == 'ExternalSecrets' && has(self.externalSecrets) && !has(self.yolo)) || (self.mode == 'Yolo' && !has(self.externalSecrets))"
)]
pub struct SecretsSpec {
    /// Secret provider mode (`ExternalSecrets` or `Yolo`). Immutable after creation.
    #[serde(default)]
    #[x_kube(validation = "self == oldSelf")]
    pub mode: SecretProviderMode,

    /// ESO configuration when `mode` is `ExternalSecrets`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_secrets: Option<ExternalSecretsSpec>,

    /// YOLO generator options when `mode` is `Yolo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yolo: Option<YoloSecretsSpec>,
}

fn default_refresh_interval() -> String {
    "1h".to_owned()
}

fn default_apns_pem_key() -> String {
    "WADDLE_APNS_KEY_PEM".to_owned()
}

const fn default_true() -> bool {
    true
}
