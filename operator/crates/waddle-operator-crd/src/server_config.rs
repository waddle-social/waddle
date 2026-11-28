use std::collections::BTreeMap;

use kube::KubeSchema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Seconds reserved for identity retirement after drains (mirrors `waddle-server`).
pub const SHUTDOWN_RETIRE_BUDGET_SECS: i64 = 5;

/// Seconds reserved for local terminal sweep after retirement (mirrors `waddle-server`).
pub const SHUTDOWN_TERMINAL_SWEEP_BUDGET_SECS: i64 = 2;

/// Seconds reserved as Kubernetes SIGKILL margin after the server deadline.
pub const SHUTDOWN_KILL_MARGIN_SECS: i64 = 5;

/// Operating mode passed via `WADDLE_MODE`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerMode {
    /// Full homeserver mode with HTTP auth broker and XMPP.
    #[default]
    HomeServer,
    /// Standalone XMPP-only mode.
    Standalone,
}

impl ServerMode {
    /// Returns the lowercase wire value expected by `WADDLE_MODE`.
    #[must_use]
    pub const fn as_env_value(self) -> &'static str {
        match self {
            Self::HomeServer => "homeserver",
            Self::Standalone => "standalone",
        }
    }
}

/// Deployment rollout strategy.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdateStrategyKind {
    /// Terminates old pods before starting new ones (safe for RWO PVCs).
    #[default]
    Recreate,
    /// Rolling surge deployment (requires RWX or disabled PVC).
    RollingUpdate,
}

/// Kubernetes container image pull policy.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullPolicy {
    /// Pull only if not present locally.
    #[default]
    IfNotPresent,
    /// Always pull the image.
    Always,
    /// Never pull the image.
    Never,
}

impl PullPolicy {
    /// Returns the Kubernetes string representation.
    #[must_use]
    pub const fn as_k8s_str(self) -> &'static str {
        match self {
            Self::IfNotPresent => "IfNotPresent",
            Self::Always => "Always",
            Self::Never => "Never",
        }
    }
}

/// Container image specification for the `waddle-server` pods.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImageSpec {
    /// Container image repository (default `ghcr.io/waddle-social/waddle`).
    #[serde(default = "default_image_repository")]
    pub repository: String,
    /// Mutable image tag (ignored when `digest` is provided).
    #[serde(default = "default_image_tag")]
    pub tag: String,
    /// Optional immutable `sha256:...` digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Container image pull policy.
    #[serde(default)]
    pub pull_policy: PullPolicy,
    /// Names of image pull secrets in the same namespace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_secrets: Vec<String>,
}

impl Default for ImageSpec {
    fn default() -> Self {
        Self {
            repository: default_image_repository(),
            tag: default_image_tag(),
            digest: None,
            pull_policy: PullPolicy::default(),
            pull_secrets: Vec::new(),
        }
    }
}

/// XMPP domain and endpoint configuration.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct XmppSpec {
    /// Enable XMPP subsystem (`WADDLE_XMPP_ENABLED`).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Canonical XMPP domain (`WADDLE_XMPP_DOMAIN`).
    #[x_kube(validation = "size(self) > 0")]
    pub domain: String,
    /// Trusted public RFC 7395 WebSocket endpoint (`WADDLE_XMPP_PUBLIC_WEBSOCKET_URL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_websocket_url: Option<String>,
    /// Optional same-namespace TLS Secret (`tls.crt`, `tls.key`) mounted at `/etc/waddle/tls`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_secret_name: Option<String>,
    /// Enable native SCRAM authentication (`WADDLE_NATIVE_AUTH_ENABLED`).
    #[serde(default = "default_true")]
    pub native_auth_enabled: bool,
    /// Enable in-band account registration (`WADDLE_REGISTRATION_ENABLED`).
    #[serde(default)]
    pub registration_enabled: bool,
}

/// ADR-0017 multi-replica libp2p clustering settings.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClusteringSpec {
    /// Enable multi-replica clustering (`WADDLE_CLUSTERING_ENABLED`).
    #[serde(default)]
    pub enabled: bool,
    /// TCP port exposed for swarm traffic (default `7900`).
    #[serde(default = "default_swarm_port")]
    pub swarm_port: u16,
    /// libp2p listen multiaddrs (defaults to `["/ip4/0.0.0.0/tcp/7900"]`).
    #[serde(default = "default_listen_addrs")]
    pub listen_addrs: Vec<String>,
    /// Optional explicit bootstrap peers (`host:port`); empty uses the headless Service.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bootstrap_peers: Vec<String>,
    /// Number of enrolled Ed25519 keys in `WADDLE_CLUSTERING_KEYPAIR_POOL`.
    #[serde(default)]
    pub enrolled_key_count: i32,
    /// Extra keypair headroom above `replicas + maxSurge` (default `1`).
    #[serde(default = "default_keypair_headroom")]
    pub keypair_headroom: i32,
    /// Claim-release drain budget in seconds (`WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS`).
    #[serde(default = "default_claim_release_budget_secs")]
    pub claim_release_budget_seconds: i64,
}

impl Default for ClusteringSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            swarm_port: default_swarm_port(),
            listen_addrs: default_listen_addrs(),
            bootstrap_peers: Vec::new(),
            enrolled_key_count: 0,
            keypair_headroom: default_keypair_headroom(),
            claim_release_budget_seconds: default_claim_release_budget_secs(),
        }
    }
}

/// Deployment lineage identity configuration (`WADDLE_DEPLOYMENT_UUID`, `WADDLE_DB_LINEAGE_ACTION`).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentIdentitySpec {
    /// Stable lower-case RFC-4122 UUID bound into `_lineage`. Required in ESO mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    /// Explicit one-shot lineage action (`"enroll"` or `"adopt=<uuid>[,<uuid>...]"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_action: Option<String>,
}

/// S3-compatible object storage configuration for file uploads.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct S3StorageSpec {
    /// S3 endpoint URL (`WADDLE_S3_ENDPOINT`).
    #[x_kube(validation = "size(self) > 0")]
    pub endpoint: String,
    /// Bucket name (`WADDLE_S3_BUCKET`).
    #[x_kube(validation = "size(self) > 0")]
    pub bucket: String,
}

/// PVC access mode.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PersistenceAccessMode {
    /// Single-node read-write mount.
    #[default]
    ReadWriteOnce,
    /// Multi-node read-write mount.
    ReadWriteMany,
    /// Multi-node read-only mount.
    ReadOnlyMany,
}

impl PersistenceAccessMode {
    /// Returns the Kubernetes PersistentVolumeAccessMode string.
    #[must_use]
    pub const fn as_k8s_str(self) -> &'static str {
        match self {
            Self::ReadWriteOnce => "ReadWriteOnce",
            Self::ReadWriteMany => "ReadWriteMany",
            Self::ReadOnlyMany => "ReadOnlyMany",
        }
    }
}

/// Persistent volume settings for `/var/lib/waddle`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PersistenceSpec {
    /// Create or mount a PersistentVolumeClaim (`false` uses `emptyDir`).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Use an existing same-namespace PVC instead of creating one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_claim: Option<String>,
    /// StorageClass name for created PVCs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_class_name: Option<String>,
    /// Requested PVC access modes.
    #[serde(default = "default_access_modes")]
    pub access_modes: Vec<PersistenceAccessMode>,
    /// Explicitly permit `RollingUpdate` or `replicas > 1` with `ReadWriteOnce`.
    #[serde(default)]
    pub allow_unsafe_rwo_scale: bool,
    /// Storage capacity request (e.g. `"10Gi"`).
    #[serde(default = "default_pvc_size")]
    pub size: String,
    /// Mount path inside the server container.
    #[serde(default = "default_mount_path")]
    pub mount_path: String,
    /// Subdirectory beneath `mountPath` used for `WADDLE_UPLOAD_DIR`.
    #[serde(default = "default_upload_sub_path")]
    pub upload_sub_path: String,
}

impl Default for PersistenceSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            existing_claim: None,
            storage_class_name: None,
            access_modes: default_access_modes(),
            allow_unsafe_rwo_scale: false,
            size: default_pvc_size(),
            mount_path: default_mount_path(),
            upload_sub_path: default_upload_sub_path(),
        }
    }
}

/// Service type for optional S2S federation listener.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum S2sServiceType {
    /// Cluster-internal IP service.
    ClusterIP,
    /// NodePort service for external S2S ingress.
    #[default]
    NodePort,
    /// Cloud LoadBalancer service.
    LoadBalancer,
}

impl S2sServiceType {
    /// Returns the Kubernetes Service `type` string.
    #[must_use]
    pub const fn as_k8s_str(self) -> &'static str {
        match self {
            Self::ClusterIP => "ClusterIP",
            Self::NodePort => "NodePort",
            Self::LoadBalancer => "LoadBalancer",
        }
    }
}

/// Server-to-server (XMPP S2S) federation configuration.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct S2sSpec {
    /// Enable S2S federation (`WADDLE_S2S_ENABLED`).
    #[serde(default)]
    pub enabled: bool,
    /// Same-namespace ConfigMap holding `s2s.toml`, mounted at `/etc/waddle/s2s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_map_name: Option<String>,
    /// S2S TCP port (default `5269`).
    #[serde(default = "default_s2s_port")]
    pub port: i32,
    /// Kubernetes Service type for S2S traffic.
    #[serde(default)]
    pub service_type: S2sServiceType,
    /// Optional fixed NodePort when `serviceType` is `NodePort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_port: Option<i32>,
    /// Optional external IPs bound to the S2S Service.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_ips: Vec<String>,
}

impl Default for S2sSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            config_map_name: None,
            port: default_s2s_port(),
            service_type: S2sServiceType::default(),
            node_port: None,
            external_ips: Vec::new(),
        }
    }
}

/// OpenTelemetry export configuration.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySpec {
    /// OTLP collector endpoint (`OTEL_EXPORTER_OTLP_ENDPOINT`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_endpoint: Option<String>,
    /// OTLP protocol (`OTEL_EXPORTER_OTLP_PROTOCOL`, e.g. `"grpc"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_protocol: Option<String>,
    /// Logical service name (`OTEL_SERVICE_NAME`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Service version (`OTEL_SERVICE_VERSION`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_version: Option<String>,
    /// Deployment environment resource attribute (`deployment.environment=...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_environment: Option<String>,
    /// OTel trace sampler (`OTEL_TRACES_SAMPLER`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traces_sampler: Option<String>,
    /// OTel trace sampler argument (`OTEL_TRACES_SAMPLER_ARG`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traces_sampler_arg: Option<String>,
}

/// Ordered shutdown timing configuration matching `waddle-server::shutdown_deadline`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShutdownSpec {
    /// Native `lifecycle.preStop.sleep` duration in seconds (default `5`).
    #[serde(default = "default_pre_stop_sleep_secs")]
    pub pre_stop_sleep_seconds: i64,
    /// Server drain deadline in seconds (`WADDLE_DRAIN_TIMEOUT_SECS`, default `30`).
    #[serde(default = "default_drain_timeout_secs")]
    pub drain_timeout_seconds: i64,
    /// Pod `terminationGracePeriodSeconds`. Computed from the shutdown floor when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination_grace_period_seconds: Option<i64>,
}

impl Default for ShutdownSpec {
    fn default() -> Self {
        Self {
            pre_stop_sleep_seconds: default_pre_stop_sleep_secs(),
            drain_timeout_seconds: default_drain_timeout_secs(),
            termination_grace_period_seconds: None,
        }
    }
}

/// RFC 7395 WebSocket keepalive settings (`WADDLE_WS_KEEPALIVE_*`).
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WsKeepaliveSpec {
    /// Ping interval in seconds (`1..=120`, default `45`).
    #[serde(default = "default_ws_interval_secs")]
    pub interval_secs: u64,
    /// Consecutive missed pongs before closing (`1..=10`, default `2`).
    #[serde(default = "default_ws_miss_limit")]
    pub miss_limit: u64,
}

impl Default for WsKeepaliveSpec {
    fn default() -> Self {
        Self {
            interval_secs: default_ws_interval_secs(),
            miss_limit: default_ws_miss_limit(),
        }
    }
}

/// Reference to a Kubernetes Gateway API `Gateway` resource.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRefSpec {
    /// `Gateway` object name (e.g. `waddle-gateway`).
    #[x_kube(validation = "size(self) > 0")]
    pub name: String,
    /// Namespace containing the `Gateway` (e.g. `default`).
    #[x_kube(validation = "size(self) > 0")]
    pub namespace: String,
}

/// Durable ingress authority transaction settings (`WADDLE_INGRESS_*`) and optional
/// per-tenant Gateway API / DNS / TLS routing configuration.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IngressSpec {
    /// Ingress DB pool size (`1..=32`, default `4`).
    #[serde(default = "default_ingress_pool_size")]
    pub pool_size: u32,
    /// Ingress DB retry attempts (`1..=32`, default `5`).
    #[serde(default = "default_ingress_retry_attempts")]
    pub retry_attempts: u32,
    /// Enable operator-managed per-tenant Gateway API, DNS, and TLS reconciliation.
    #[serde(default)]
    pub enabled: bool,
    /// Target Gateway API `Gateway` used for tenant `HTTPRoute`s and BYO listeners.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_ref: Option<GatewayRefSpec>,
    /// Managed wildcard base DNS domain for `ManagedSubdomain` tenants (e.g. `waddle.social`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_base_domain: Option<String>,
    /// Static HTTPS wildcard listener `sectionName` on `gatewayRef` for managed subdomains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_wildcard_https_section: Option<String>,
    /// Static HTTP wildcard listener `sectionName` on `gatewayRef` for managed subdomain redirects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_wildcard_http_section: Option<String>,
    /// Static HTTP catch-all listener `sectionName` on `gatewayRef` for BYO HTTP-01 and redirects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byo_http_section: Option<String>,
    /// Static target IP addresses written into `DNSEndpoint` for `ManagedSubdomain` tenants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 16), inner(length(max = 45)))]
    pub target_ips: Vec<String>,
    /// `cert-manager` `ClusterIssuer` name used for `CustomDomain` tenant `Certificate`s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_cluster_issuer: Option<String>,
    /// Additional reserved single-label subdomains under `managedBaseDomain` (merged with
    /// the always-enforced built-in reserved floor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 256), inner(length(max = 63)))]
    pub reserved_subdomains: Vec<String>,
}

impl Default for IngressSpec {
    fn default() -> Self {
        Self {
            pool_size: default_ingress_pool_size(),
            retry_attempts: default_ingress_retry_attempts(),
            enabled: false,
            gateway_ref: None,
            managed_base_domain: None,
            managed_wildcard_https_section: None,
            managed_wildcard_http_section: None,
            byo_http_section: None,
            target_ips: Vec::new(),
            tls_cluster_issuer: None,
            reserved_subdomains: Vec::new(),
        }
    }
}

/// Focused CPU/memory resource quantities to avoid inflating the CRD OpenAPI schema.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSpec {
    /// CPU quantity string (e.g. `"100m"` or `"1"`).
    pub cpu: String,
    /// Memory quantity string (e.g. `"512Mi"` or `"4Gi"`).
    pub memory: String,
}

/// Container compute resource requests and limits.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRequirementsSpec {
    /// Guaranteed resource requests.
    #[serde(default = "default_resource_requests")]
    pub requests: ResourceSpec,
    /// Maximum resource limits.
    #[serde(default = "default_resource_limits")]
    pub limits: ResourceSpec,
}

impl Default for ResourceRequirementsSpec {
    fn default() -> Self {
        Self {
            requests: default_resource_requests(),
            limits: default_resource_limits(),
        }
    }
}

/// HTTP probe configuration.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HttpProbeSpec {
    /// HTTP probe path.
    pub path: String,
    /// Initial delay before first probe.
    pub initial_delay_seconds: i32,
    /// Interval between probes.
    pub period_seconds: i32,
    /// Probe timeout.
    pub timeout_seconds: i32,
    /// Failures before marking unhealthy.
    pub failure_threshold: i32,
}

/// Liveness and readiness probes for `waddle-server`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProbesSpec {
    /// Liveness probe (defaults to `/health`).
    #[serde(default = "default_liveness_probe")]
    pub liveness: HttpProbeSpec,
    /// Readiness probe (defaults to `/ready`).
    #[serde(default = "default_readiness_probe")]
    pub readiness: HttpProbeSpec,
}

impl Default for ProbesSpec {
    fn default() -> Self {
        Self {
            liveness: default_liveness_probe(),
            readiness: default_readiness_probe(),
        }
    }
}

/// Primary Kubernetes Service type.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServiceType {
    /// Cluster-internal service.
    #[default]
    ClusterIP,
    /// NodePort service.
    NodePort,
    /// Cloud LoadBalancer service.
    LoadBalancer,
}

impl ServiceType {
    /// Returns the Kubernetes Service `type` string.
    #[must_use]
    pub const fn as_k8s_str(self) -> &'static str {
        match self {
            Self::ClusterIP => "ClusterIP",
            Self::NodePort => "NodePort",
            Self::LoadBalancer => "LoadBalancer",
        }
    }
}

/// Primary HTTP/WebSocket Service settings.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSpec {
    /// Kubernetes Service type.
    #[serde(default)]
    pub kind: ServiceType,
    /// HTTP/WebSocket service port (default `3000`).
    #[serde(default = "default_http_port")]
    pub http_port: i32,
    /// Optional Service metadata annotations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

impl Default for ServiceSpec {
    fn default() -> Self {
        Self {
            kind: ServiceType::default(),
            http_port: default_http_port(),
            annotations: BTreeMap::new(),
        }
    }
}

/// PodDisruptionBudget configuration when clustering is enabled.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PodDisruptionBudgetSpec {
    /// Create a PodDisruptionBudget when clustering is enabled (default `true`).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum unavailable pods (default `1`).
    #[serde(default = "default_pdb_max_unavailable")]
    pub max_unavailable: i32,
}

impl Default for PodDisruptionBudgetSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            max_unavailable: default_pdb_max_unavailable(),
        }
    }
}

/// NetworkPolicy configuration when clustering is enabled.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicySpec {
    /// Create a swarm NetworkPolicy when clustering is enabled (default `true`).
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for NetworkPolicySpec {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Preferred pod anti-affinity configuration.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PodAntiAffinitySpec {
    /// Enable preferred pod anti-affinity across nodes (default `true`).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Scheduling preference weight (`1..=100`, default `50`).
    #[serde(default = "default_anti_affinity_weight")]
    pub weight: i32,
    /// Topology key (default `kubernetes.io/hostname`).
    #[serde(default = "default_topology_key")]
    pub topology_key: String,
}

impl Default for PodAntiAffinitySpec {
    fn default() -> Self {
        Self {
            enabled: true,
            weight: default_anti_affinity_weight(),
            topology_key: default_topology_key(),
        }
    }
}

fn default_image_repository() -> String {
    "ghcr.io/waddle-social/waddle".to_owned()
}

fn default_image_tag() -> String {
    "main".to_owned()
}

const fn default_true() -> bool {
    true
}

const fn default_swarm_port() -> u16 {
    7900
}

fn default_listen_addrs() -> Vec<String> {
    vec!["/ip4/0.0.0.0/tcp/7900".to_owned()]
}

const fn default_keypair_headroom() -> i32 {
    1
}

const fn default_claim_release_budget_secs() -> i64 {
    5
}

fn default_access_modes() -> Vec<PersistenceAccessMode> {
    vec![PersistenceAccessMode::ReadWriteOnce]
}

fn default_pvc_size() -> String {
    "10Gi".to_owned()
}

fn default_mount_path() -> String {
    "/var/lib/waddle".to_owned()
}

fn default_upload_sub_path() -> String {
    "uploads".to_owned()
}

const fn default_s2s_port() -> i32 {
    5269
}

const fn default_pre_stop_sleep_secs() -> i64 {
    5
}

const fn default_drain_timeout_secs() -> i64 {
    30
}

const fn default_ws_interval_secs() -> u64 {
    45
}

const fn default_ws_miss_limit() -> u64 {
    2
}

const fn default_ingress_pool_size() -> u32 {
    4
}

const fn default_ingress_retry_attempts() -> u32 {
    5
}

fn default_resource_requests() -> ResourceSpec {
    ResourceSpec {
        cpu: "100m".to_owned(),
        memory: "256Mi".to_owned(),
    }
}

fn default_resource_limits() -> ResourceSpec {
    ResourceSpec {
        cpu: "500m".to_owned(),
        memory: "512Mi".to_owned(),
    }
}

fn default_liveness_probe() -> HttpProbeSpec {
    HttpProbeSpec {
        path: "/health".to_owned(),
        initial_delay_seconds: 15,
        period_seconds: 10,
        timeout_seconds: 2,
        failure_threshold: 3,
    }
}

fn default_readiness_probe() -> HttpProbeSpec {
    HttpProbeSpec {
        path: "/ready".to_owned(),
        initial_delay_seconds: 5,
        period_seconds: 10,
        timeout_seconds: 2,
        failure_threshold: 3,
    }
}

const fn default_http_port() -> i32 {
    3000
}

const fn default_pdb_max_unavailable() -> i32 {
    1
}

const fn default_anti_affinity_weight() -> i32 {
    50
}

fn default_topology_key() -> String {
    "kubernetes.io/hostname".to_owned()
}
