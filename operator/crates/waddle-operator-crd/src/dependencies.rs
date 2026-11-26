use kube::KubeSchema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Reference to a key inside a Kubernetes `Secret` in the same namespace.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SecretKeyRef {
    /// Name of the Kubernetes `Secret` in the same namespace.
    #[x_kube(validation = "size(self) > 0")]
    pub name: String,
    /// Key inside the `Secret` data map.
    #[x_kube(validation = "size(self) > 0")]
    pub key: String,
}

/// Reference to a named Kubernetes object in the same namespace.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalObjectRef {
    /// Name of the referenced Kubernetes resource in the same namespace.
    #[x_kube(validation = "size(self) > 0")]
    pub name: String,
}

/// Storage backend kind for `WaddleServer`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StorageBackendKind {
    /// Per-tenant embedded Turso/libSQL databases stored on local PVC/filesystem.
    #[default]
    Turso,
    /// External PostgreSQL database (CloudNativePG or External Secret).
    Postgres,
}

impl StorageBackendKind {
    /// Returns the lowercase wire value expected by `WADDLE_DB_DRIVER`.
    #[must_use]
    pub const fn as_env_value(self) -> &'static str {
        match self {
            Self::Turso => "turso",
            Self::Postgres => "postgres",
        }
    }
}

/// Local Turso/libSQL storage settings (`WADDLE_TURSO_*`).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TursoStorageSpec {
    /// Directory storing per-tenant Turso databases (`WADDLE_TURSO_DATA_DIR`).
    #[serde(default = "default_turso_data_dir")]
    #[x_kube(validation = "size(self) > 0")]
    pub data_dir: String,
    /// File descriptors reserved for non-tenant I/O (`WADDLE_TURSO_RESERVED_FDS`, default `256`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_fds: Option<u32>,
    /// Optional `RLIMIT_NOFILE` hint (`WADDLE_TURSO_RLIMIT_NOFILE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rlimit_nofile: Option<u32>,
}

impl Default for TursoStorageSpec {
    fn default() -> Self {
        Self {
            data_dir: default_turso_data_dir(),
            reserved_fds: None,
            rlimit_nofile: None,
        }
    }
}

/// Pluggable storage backend configuration for `WaddleServer`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[x_kube(
    cel,
    validation = "(self.backend == 'Turso' && !has(self.postgres)) || (self.backend == 'Postgres' && has(self.postgres) && !has(self.turso))"
)]
pub struct StorageSpec {
    /// Storage backend driver (`Turso` or `Postgres`).
    #[serde(default)]
    pub backend: StorageBackendKind,
    /// Turso storage settings when `backend` is `Turso`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turso: Option<TursoStorageSpec>,
    /// External PostgreSQL dependency when `backend` is `Postgres`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgres: Option<PostgresDependencySpec>,
}

/// Authorization backend kind for `WaddleServer`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthzBackendKind {
    /// OpenFGA HTTP API (`WADDLE_AUTHZ_BACKEND=openfga`).
    #[default]
    OpenFga,
    /// SpiceDB gRPC API (`WADDLE_AUTHZ_BACKEND=spicedb`).
    SpiceDb,
}

impl AuthzBackendKind {
    /// Returns the lowercase wire value expected by `WADDLE_AUTHZ_BACKEND`.
    #[must_use]
    pub const fn as_env_value(self) -> &'static str {
        match self {
            Self::OpenFga => "openfga",
            Self::SpiceDb => "spicedb",
        }
    }
}

/// OpenFGA service dependency configuration (`WADDLE_OPENFGA_*`).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[x_kube(
    cel,
    validation = "!has(self.consistency) || self.consistency == 'higher_consistency' || self.consistency == 'minimize_latency'"
)]
pub struct OpenFgaDependencySpec {
    /// Name of the same-namespace Kubernetes `Service` exposing OpenFGA.
    #[x_kube(validation = "size(self) > 0")]
    pub service_name: String,
    /// HTTP port exposed by the OpenFGA Service (default `8080`).
    #[serde(default = "default_openfga_port")]
    pub port: u16,
    /// URI scheme (`http` or `https`, default `http`).
    #[serde(default = "default_http_scheme")]
    #[x_kube(validation = "self == 'http' || self == 'https'")]
    pub scheme: String,
    /// Optional same-namespace Secret key holding `WADDLE_OPENFGA_API_TOKEN`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_secret_ref: Option<SecretKeyRef>,
    /// OpenFGA read consistency (`higher_consistency` default or `minimize_latency`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consistency: Option<String>,
}

/// Pluggable authorization backend configuration for `WaddleServer`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[x_kube(
    cel,
    validation = "(self.backend == 'OpenFga' && has(self.openfga) && !has(self.spicedb)) || (self.backend == 'SpiceDb' && has(self.spicedb) && !has(self.openfga))"
)]
pub struct AuthzSpec {
    /// Authorization backend driver (`OpenFga` or `SpiceDb`).
    #[serde(default)]
    pub backend: AuthzBackendKind,
    /// OpenFGA dependency when `backend` is `OpenFga`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openfga: Option<OpenFgaDependencySpec>,
    /// SpiceDB dependency when `backend` is `SpiceDb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spicedb: Option<SpiceDbDependencySpec>,
}

/// KMS provider kind for the Waddle credential custody broker.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrokerKmsProviderKind {
    /// Scaleway Key Manager (`WADDLE_BROKER_KMS_PROVIDER=scaleway`).
    #[default]
    Scaleway,
    /// HashiCorp Vault Transit (`WADDLE_BROKER_KMS_PROVIDER=vault`).
    Vault,
    /// Local ephemeral/dev KMS (`WADDLE_BROKER_KMS_PROVIDER=local`).
    Local,
}

impl BrokerKmsProviderKind {
    /// Returns the lowercase wire value expected by `WADDLE_BROKER_KMS_PROVIDER`.
    #[must_use]
    pub const fn as_env_value(self) -> &'static str {
        match self {
            Self::Scaleway => "scaleway",
            Self::Vault => "vault",
            Self::Local => "local",
        }
    }
}

/// Scaleway Key Manager configuration (`WADDLE_SCW_KMS_*`).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScalewayKmsSpec {
    /// Scaleway API base URL (`WADDLE_SCW_KMS_BASE_URL`, default `https://api.scaleway.com`).
    #[serde(default = "default_scaleway_base_url")]
    #[x_kube(validation = "size(self) > 0")]
    pub base_url: String,
    /// Scaleway Key Manager region (`WADDLE_SCW_KMS_REGION`, default `fr-par`).
    #[serde(default = "default_scaleway_region")]
    #[x_kube(validation = "size(self) > 0")]
    pub region: String,
    /// Scaleway Project ID (`WADDLE_SCW_KMS_PROJECT_ID`).
    #[x_kube(validation = "size(self) > 0")]
    pub project_id: String,
    /// Same-namespace Secret key containing the Scaleway secret key (`WADDLE_SCW_KMS_AUTH_TOKEN`).
    pub auth_secret_ref: SecretKeyRef,
    /// Optional pre-provisioned Scaleway root key UUID (`WADDLE_SCW_KMS_ROOT_KEY_ID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_key_id: Option<String>,
}

/// HashiCorp Vault Transit KMS configuration (`WADDLE_VAULT_*`).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VaultKmsSpec {
    /// Vault API base URL (`WADDLE_VAULT_ADDR`).
    #[x_kube(validation = "size(self) > 0")]
    pub base_url: String,
    /// Vault transit mount path (`WADDLE_VAULT_TRANSIT_MOUNT`, default `transit`).
    #[serde(default = "default_vault_mount")]
    #[x_kube(validation = "size(self) > 0")]
    pub mount: String,
    /// Optional Vault namespace (`WADDLE_VAULT_NAMESPACE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Same-namespace Secret key containing the Vault token (`WADDLE_VAULT_TOKEN`).
    pub auth_secret_ref: SecretKeyRef,
}

/// KMS backend configuration for the credential custody broker.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[x_kube(
    cel,
    validation = "(self.provider == 'Scaleway' && has(self.scaleway) && !has(self.vault)) || (self.provider == 'Vault' && has(self.vault) && !has(self.scaleway)) || (self.provider == 'Local' && !has(self.scaleway) && !has(self.vault))"
)]
pub struct BrokerKmsSpec {
    /// KMS provider (`Scaleway`, `Vault`, or `Local`).
    #[serde(default)]
    pub provider: BrokerKmsProviderKind,
    /// Scaleway Key Manager settings when `provider` is `Scaleway`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scaleway: Option<ScalewayKmsSpec>,
    /// HashiCorp Vault Transit settings when `provider` is `Vault`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<VaultKmsSpec>,
}

/// Credential custody and inference egress broker configuration (`WADDLE_BROKER_*`).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[x_kube(cel, validation = "!self.enabled || has(self.kms)")]
pub struct BrokerSpec {
    /// Enable the credential custody and inference egress broker (`WADDLE_BROKER_ENABLED`).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Directory storing broker SQLite/Turso databases (`WADDLE_BROKER_DATA_DIR`).
    #[serde(default = "default_broker_data_dir")]
    #[x_kube(validation = "size(self) > 0")]
    pub data_dir: String,
    /// Optional explicit admission database path (`WADDLE_BROKER_ADMISSION_DB`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_db: Option<String>,
    /// KMS backend configuration (required when `enabled` is `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kms: Option<BrokerKmsSpec>,
    /// Optional SPIFFE ID identifying this cell (`WADDLE_CELL_SPIFFE_ID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_spiffe_id: Option<String>,
}

/// Waddle server extension runtime configuration (`WADDLE_EXTENSIONS_*`).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionsSpec {
    /// Enable server extensions (`WADDLE_EXTENSIONS_ENABLED`).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional path to extension plugin modules (`WADDLE_EXTENSIONS_PLUGINS_DIR`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins_dir: Option<String>,
}

/// Source mode for PostgreSQL connectivity.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PostgresSourceMode {
    /// Observe an external CloudNativePG `Cluster` (`postgresql.cnpg.io/v1`).
    #[default]
    CloudNativePg,
    /// Use a pre-existing PostgreSQL connection `Secret` and optional `Service`.
    External,
}

/// PostgreSQL dependency configuration (`WaddleServer` observes, never creates, Postgres).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[x_kube(
    cel,
    validation = "(self.mode == 'CloudNativePg' && has(self.cnpg) && !has(self.external)) || (self.mode == 'External' && has(self.external) && !has(self.cnpg))"
)]
pub struct PostgresDependencySpec {
    /// Selects how the PostgreSQL endpoint and credentials are discovered.
    #[serde(default)]
    pub mode: PostgresSourceMode,

    /// CloudNativePG cluster source when `mode` is `CloudNativePg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cnpg: Option<CloudNativePgSource>,

    /// External PostgreSQL secret source when `mode` is `External`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<ExternalPostgresSource>,

    /// Main/shared SQLx connection pool size (`WADDLE_DB_POOL_SIZE`).
    #[serde(default = "default_pool_size")]
    pub pool_size: u32,

    /// Dedicated control-plane pool size (`WADDLE_DB_CONTROL_PLANE_POOL_SIZE`).
    #[serde(default = "default_control_plane_pool_size")]
    pub control_plane_pool_size: u32,

    /// Optional SQLx pool connection acquire timeout in seconds (`1..=300`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquire_timeout_secs: Option<u64>,

    /// Restricted PostgreSQL runtime configuration matching `waddle-server` helm chart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restricted_runtime: Option<RestrictedRuntimeSpec>,
}

/// Reference to a CloudNativePG `Cluster` in the same namespace.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CloudNativePgSource {
    /// Name of the `postgresql.cnpg.io/v1` `Cluster` resource in this namespace.
    #[x_kube(validation = "size(self) > 0")]
    pub cluster_name: String,

    /// Optional override for the application credential `Secret`. When omitted,
    /// the operator reads `Cluster.spec.bootstrap.initdb.secret.name` or defaults
    /// to `<clusterName>-app`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_name: Option<String>,

    /// Key in the CNPG application secret holding the connection URI (`uri` by default).
    #[serde(default = "default_uri_key")]
    pub uri_key: String,

    /// Key holding the username when the secret uses `kubernetes.io/basic-auth` format.
    #[serde(default = "default_username_key")]
    pub username_key: String,

    /// Key holding the password when the secret uses `kubernetes.io/basic-auth` format.
    #[serde(default = "default_password_key")]
    pub password_key: String,

    /// Require CNPG `Ready=True`, `readyInstances >= 1`, and a non-empty `currentPrimary`
    /// before moving out of `Pending`.
    #[serde(default = "default_true")]
    pub require_healthy: bool,
}

/// Reference to an external PostgreSQL connection secret in the same namespace.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPostgresSource {
    /// Name of the same-namespace `Secret` containing the Postgres DSN.
    #[x_kube(validation = "size(self) > 0")]
    pub secret_name: String,

    /// Key inside `secretName` holding the connection URI.
    #[serde(default = "default_uri_key")]
    pub uri_key: String,

    /// Optional same-namespace Kubernetes `Service` whose ready endpoints are checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
}

/// Restricted PostgreSQL runtime configuration (verified TLS + non-owner login + handoff).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RestrictedRuntimeSpec {
    /// Enables restricted runtime wiring (`WADDLE_DB_RUNTIME_ATTESTATION`, CA mount, role env).
    #[serde(default)]
    pub enabled: bool,

    /// 40-character lower-case Git SHA matching the server release image (`WADDLE_GIT_SHA`).
    #[serde(default)]
    pub release_git_sha: String,

    /// PostgreSQL application schema name (`WADDLE_XMPP_MAM_CLAIM_SCHEMA`).
    #[serde(default = "default_schema_public")]
    pub schema: String,

    /// PostgreSQL primary read-write service host.
    #[serde(default)]
    pub database_host: String,

    /// PostgreSQL port.
    #[serde(default = "default_pg_port")]
    pub database_port: u16,

    /// PostgreSQL database name.
    #[serde(default)]
    pub database_name: String,

    /// Same-namespace Secret containing the PostgreSQL server CA certificate.
    #[serde(default)]
    pub ca_secret_name: String,

    /// Key inside `caSecretName` containing the PEM CA certificate.
    #[serde(default = "default_ca_key")]
    pub ca_secret_key: String,

    /// Same-namespace Secret containing the restricted runtime login password.
    #[serde(default)]
    pub runtime_credential_secret: String,

    /// Key inside `runtimeCredentialSecret` holding the URL-safe password.
    #[serde(default = "default_password_key")]
    pub runtime_credential_key: String,

    /// Same-namespace ConfigMap holding the `handoff.json` attestation document.
    #[serde(default)]
    pub handoff_config_map_name: String,

    /// Role names shared with the server binary.
    pub roles: RestrictedRuntimeRolesSpec,
}

/// Role names required by the restricted PostgreSQL runtime.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RestrictedRuntimeRolesSpec {
    /// Login that ran the owner phase (`WADDLE_DB_MIGRATOR_LOGIN`).
    pub migrator_login: String,
    /// Schema owner role (`WADDLE_XMPP_MAM_SCHEMA_OWNER`).
    pub schema_owner: String,
    /// Least-privilege runtime serving login.
    pub runtime_login: String,
    /// Attestor installer login.
    pub attestor_installer_login: String,
    /// Attestor definer role.
    pub attestor_definer_role: String,
    /// Attestor schema name.
    pub attestor_schema: String,
}

/// Source mode for SpiceDB connectivity.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpiceDbSourceMode {
    /// Observe an external `SpiceDBCluster` (`authzed.com/v1alpha1`).
    #[default]
    SpiceDbOperator,
    /// Connect to a pre-existing SpiceDB endpoint and optional `Service`.
    External,
}

/// SpiceDB dependency configuration (`WaddleServer` observes, never creates, SpiceDB).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[x_kube(
    cel,
    validation = "(self.mode == 'SpiceDbOperator' && has(self.operator) && !has(self.external)) || (self.mode == 'External' && has(self.external) && !has(self.operator))"
)]
pub struct SpiceDbDependencySpec {
    /// Selects how the SpiceDB endpoint and readiness are discovered.
    #[serde(default)]
    pub mode: SpiceDbSourceMode,

    /// `SpiceDBCluster` reference when `mode` is `SpiceDbOperator`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<SpiceDbOperatorSource>,

    /// Explicit endpoint configuration when `mode` is `External`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<ExternalSpiceDbSource>,
}

/// Reference to an AuthZed `SpiceDBCluster` in the same namespace.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpiceDbOperatorSource {
    /// Name of the `authzed.com/v1alpha1` `SpiceDBCluster` resource in this namespace.
    #[x_kube(validation = "size(self) > 0")]
    pub cluster_name: String,

    /// Optional Service name override (defaults to `clusterName`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,

    /// gRPC port exposed by the SpiceDB Service (default `50051`).
    #[serde(default = "default_spicedb_port")]
    pub port: u16,

    /// Override `WADDLE_SPICEDB_INSECURE`. When unset, derived automatically from
    /// `SpiceDBCluster.spec.config.tlsSecretName` (`true` when absent, `false` when set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insecure: Option<bool>,

    /// Optional explicit Secret key override for `WADDLE_SPICEDB_PRESHARED_KEY`.
    /// In ESO mode, defaults to the ESO target secret (`WADDLE_SPICEDB_PRESHARED_KEY`).
    /// In YOLO mode, defaults to `SpiceDBCluster.spec.secretName` (`preshared_key`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preshared_key_secret_ref: Option<SecretKeyRef>,

    /// Require `SpiceDBCluster` healthy status and >= 1 ready endpoint before leaving `Pending`.
    #[serde(default = "default_true")]
    pub require_ready: bool,
}

/// External SpiceDB endpoint specification in the same namespace.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSpiceDbSource {
    /// Full gRPC URI for SpiceDB (e.g. `http://spicedb:50051` or `https://spicedb.internal:50051`).
    #[x_kube(validation = "size(self) > 0")]
    pub endpoint: String,

    /// Controls `WADDLE_SPICEDB_INSECURE`.
    #[serde(default)]
    pub insecure: bool,

    /// Optional same-namespace Kubernetes `Service` whose ready endpoints are verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,

    /// Optional explicit Secret key reference for `WADDLE_SPICEDB_PRESHARED_KEY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preshared_key_secret_ref: Option<SecretKeyRef>,
}

fn default_turso_data_dir() -> String {
    "/var/lib/waddle/tenants".to_owned()
}

const fn default_openfga_port() -> u16 {
    8080
}

fn default_http_scheme() -> String {
    "http".to_owned()
}

fn default_scaleway_base_url() -> String {
    "https://api.scaleway.com".to_owned()
}

fn default_scaleway_region() -> String {
    "fr-par".to_owned()
}

fn default_vault_mount() -> String {
    "transit".to_owned()
}

fn default_broker_data_dir() -> String {
    "/var/lib/waddle/broker".to_owned()
}

const fn default_pool_size() -> u32 {
    10
}

const fn default_control_plane_pool_size() -> u32 {
    4
}

fn default_uri_key() -> String {
    "uri".to_owned()
}

fn default_username_key() -> String {
    "username".to_owned()
}

fn default_password_key() -> String {
    "password".to_owned()
}

fn default_schema_public() -> String {
    "public".to_owned()
}

const fn default_pg_port() -> u16 {
    5432
}

fn default_ca_key() -> String {
    "ca.crt".to_owned()
}

const fn default_spicedb_port() -> u16 {
    50051
}

const fn default_true() -> bool {
    true
}
