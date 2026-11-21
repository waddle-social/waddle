use kube::KubeSchema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Reference to a key inside a Kubernetes `Secret` in the same namespace as the `WaddleServer`.
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
