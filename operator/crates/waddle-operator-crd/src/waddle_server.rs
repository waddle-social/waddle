use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{CustomResource, KubeSchema};
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use crate::dependencies::{AuthzSpec, BrokerSpec, ExtensionsSpec, StorageSpec};
use crate::secrets::{SecretProviderMode, SecretsSpec};
use crate::server_config::{
    ClusteringSpec, DeploymentIdentitySpec, ImageSpec, IngressSpec, NetworkPolicySpec,
    PersistenceSpec, PodAntiAffinitySpec, PodDisruptionBudgetSpec, ProbesSpec,
    ResourceRequirementsSpec, S2sSpec, S3StorageSpec, ServerMode, ServiceSpec, ShutdownSpec,
    TelemetrySpec, UpdateStrategyKind, WsKeepaliveSpec, XmppSpec,
};

/// Declarative specification for a `WaddleServer` deployment.
#[derive(CustomResource, KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[kube(
    group = "waddle.social",
    version = "v1alpha1",
    kind = "WaddleServer",
    plural = "waddleservers",
    namespaced,
    doc = "Declarative deployment and lifecycle specification for a Waddle XMPP server.",
    status = "WaddleServerStatus",
    derive = "PartialEq",
    cel
)]
#[kube(shortname = "waddle")]
#[kube(shortname = "ws")]
#[kube(scale(
    spec_replicas_path = ".spec.replicas",
    status_replicas_path = ".status.replicas"
))]
#[kube(
    printcolumn = r#"{"name":"Phase","type":"string","description":"Current lifecycle state machine phase","jsonPath":".status.phase"}"#
)]
#[kube(
    printcolumn = r#"{"name":"Ready","type":"string","description":"Ready replicas over desired replicas","jsonPath":".status.readySummary"}"#
)]
#[kube(
    printcolumn = r#"{"name":"Secrets","type":"string","description":"Configured secret provisioning mode","jsonPath":".status.secretsMode"}"#
)]
#[kube(
    printcolumn = r#"{"name":"Storage","type":"string","description":"Observed storage dependency readiness","jsonPath":".status.dependencies.storage.phase"}"#
)]
#[kube(
    printcolumn = r#"{"name":"Authz","type":"string","description":"Observed authorization dependency readiness","jsonPath":".status.dependencies.authz.phase"}"#
)]
#[kube(printcolumn(
    name = "Age",
    type_ = "date",
    json_path = ".metadata.creationTimestamp"
))]
#[serde(rename_all = "camelCase")]
#[x_kube(
    cel,
    validation = "!has(self.replicas) || self.replicas >= 0",
    validation = "!has(self.replicas) || self.replicas <= 1 || self.clustering.enabled",
    validation = "self.secrets.mode != 'Yolo' || ((!has(self.replicas) || self.replicas <= 1) && !self.clustering.enabled)",
    validation = "!self.clustering.enabled || has(self.s3) || self.persistence.accessModes.exists(m, m == 'ReadWriteMany')",
    validation = "!self.clustering.enabled || (has(self.deployment) && has(self.deployment.uuid) && size(self.deployment.uuid) > 0)",
    validation = "!(self.updateStrategy == 'RollingUpdate' && self.persistence.enabled && self.persistence.accessModes.exists(m, m == 'ReadWriteOnce') && !self.persistence.allowUnsafeRwoScale)",
    validation = "self.storage.backend != 'Turso' || ((!has(self.replicas) || self.replicas <= 1) && !self.clustering.enabled && self.updateStrategy == 'Recreate')"
)]
pub struct WaddleServerSpec {
    /// Desired replica count. Omit to allow an external HPA to manage replicas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<i32>,

    /// Server runtime mode (`HomeServer` or `Standalone`).
    #[serde(default)]
    pub mode: ServerMode,

    /// Public base HTTP URL (`WADDLE_BASE_URL`).
    #[x_kube(validation = "size(self) > 0")]
    pub base_url: String,

    /// Container image specification.
    #[serde(default)]
    pub image: ImageSpec,

    /// XMPP endpoint and domain settings.
    pub xmpp: XmppSpec,

    /// Pluggable storage backend (`Turso` local per-tenant DBs or external `Postgres`).
    pub storage: StorageSpec,

    /// Pluggable authorization backend (`OpenFga` or `SpiceDb`).
    pub authz: AuthzSpec,

    /// Optional credential custody and inference egress broker configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker: Option<BrokerSpec>,

    /// Optional server extensions runtime configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<ExtensionsSpec>,

    /// Runtime secret provisioning configuration (ESO or YOLO mode).
    pub secrets: SecretsSpec,

    /// Stable deployment lineage UUID and optional one-shot action.
    #[serde(default)]
    pub deployment: DeploymentIdentitySpec,

    /// ADR-0017 horizontal clustering configuration.
    #[serde(default)]
    pub clustering: ClusteringSpec,

    /// Deployment rollout strategy (defaults to `Recreate` for RWO PVC safety).
    #[serde(default)]
    pub update_strategy: UpdateStrategyKind,

    /// Ordered graceful shutdown timings.
    #[serde(default)]
    pub shutdown: ShutdownSpec,

    /// Optional S3-compatible blob storage for uploads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3: Option<S3StorageSpec>,

    /// Persistent volume settings for `/var/lib/waddle`.
    #[serde(default)]
    pub persistence: PersistenceSpec,

    /// Server-to-server XMPP federation configuration.
    #[serde(default)]
    pub s2s: S2sSpec,

    /// OpenTelemetry tracing and metrics configuration.
    #[serde(default)]
    pub telemetry: TelemetrySpec,

    /// RFC 7395 WebSocket keepalive settings.
    #[serde(default)]
    pub ws_keepalive: WsKeepaliveSpec,

    /// Durable ingress authority transaction limits.
    #[serde(default)]
    pub ingress: IngressSpec,

    /// Localparts granted initial server owner privileges (`WADDLE_SERVER_OWNER_LOCALPARTS`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub server_owner_localparts: Vec<String>,

    /// Allowed CORS origins (`WADDLE_CORS_ORIGINS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cors_origins: Option<String>,

    /// Rust `tracing` log filter (`RUST_LOG`).
    #[serde(default = "default_rust_log")]
    pub rust_log: String,

    /// Additional non-sensitive environment variables placed in the server `ConfigMap`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_env: BTreeMap<String, String>,

    /// Primary HTTP/WebSocket Service settings.
    #[serde(default)]
    pub service: ServiceSpec,

    /// Container liveness and readiness probe configuration.
    #[serde(default)]
    pub probes: ProbesSpec,

    /// Compute resource requests and limits.
    #[serde(default)]
    pub resources: ResourceRequirementsSpec,

    /// PodDisruptionBudget settings (applied when clustering is enabled).
    #[serde(default)]
    pub pdb: PodDisruptionBudgetSpec,

    /// NetworkPolicy settings (applied when clustering is enabled).
    #[serde(default)]
    pub network_policy: NetworkPolicySpec,

    /// Pod anti-affinity settings.
    #[serde(default)]
    pub pod_anti_affinity: PodAntiAffinitySpec,

    /// Optional node selector labels applied to server pods.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub node_selector: BTreeMap<String, String>,

    /// Optional annotations applied to server pods.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pod_annotations: BTreeMap<String, String>,

    /// Optional extra labels applied to server pods.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pod_labels: BTreeMap<String, String>,
}

impl WaddleServerSpec {
    /// Returns the effective replica count (`1` when omitted for HPA management).
    #[must_use]
    pub fn effective_replicas(&self) -> i32 {
        self.replicas.unwrap_or(1)
    }
}

/// Lifecycle state machine phase reported in `WaddleServer.status.phase`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WaddleServerPhase {
    /// Initial admission before first dependency evaluation.
    #[default]
    Initializing,
    /// Waiting for storage and/or authorization dependencies (or CRDs) before deployment.
    Pending,
    /// Creating YOLO secret or operator-managed `ExternalSecret` and waiting for sync.
    Provisioning,
    /// Applying owned Kubernetes workload resources and waiting for rollout convergence.
    Reconciling,
    /// All dependencies, secrets, and server replicas are healthy and available.
    Ready,
    /// Live workload exists, but dependencies, secrets, spec, or replicas are degraded.
    Degraded,
    /// Pre-deployment validation or security policy failure; no workload created.
    Failed,
    /// Resource has a deletion timestamp and is tearing down.
    Terminating,
}

/// Health summary phase for an observed dependency or secret source.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DependencyHealthPhase {
    /// Target resource does not exist yet.
    #[default]
    Missing,
    /// Required external CRD API group is not installed in the cluster.
    CrdNotInstalled,
    /// Resource exists, but readiness conditions or endpoints are not yet healthy.
    NotReady,
    /// Dependency is healthy and ready for consumption.
    Ready,
}

/// Observed state of a single dependency or secret source.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DependencyStatusSummary {
    /// Coarse health classification.
    pub phase: DependencyHealthPhase,
    /// Short machine-readable reason.
    pub reason: String,
    /// Optional human-readable diagnostic message (never contains secret values).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Aggregated dependency status block in `WaddleServer.status.dependencies`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DependenciesStatus {
    /// Observed storage backend readiness (`Turso` local storage or external `Postgres`).
    pub storage: DependencyStatusSummary,
    /// Observed authorization backend readiness (`OpenFga` or `SpiceDb`).
    pub authz: DependencyStatusSummary,
    /// Observed runtime secrets readiness.
    pub secrets: DependencyStatusSummary,
}

/// Observed status subresource for `WaddleServer`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WaddleServerStatus {
    /// Current state machine phase.
    pub phase: WaddleServerPhase,
    /// Latest `.metadata.generation` processed by the controller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// Current status replica count (used by the `/scale` subresource).
    pub replicas: i32,
    /// Number of ready server pods.
    pub ready_replicas: i32,
    /// Number of updated server pods.
    pub updated_replicas: i32,
    /// Compact `ready/desired` summary for `kubectl get` printer output.
    pub ready_summary: String,
    /// Effective secret provisioning mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets_mode: Option<SecretProviderMode>,
    /// Resolved non-secret `WADDLE_DEPLOYMENT_UUID` bound to this deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_uuid: Option<String>,
    /// Summary of dependency and secret health.
    #[serde(default)]
    pub dependencies: DependenciesStatus,
    /// Standard Kubernetes status conditions keyed by `type`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "conditions_schema")]
    pub conditions: Vec<Condition>,
}

fn default_rust_log() -> String {
    "info".to_owned()
}

pub(crate) fn conditions_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "array",
        "x-kubernetes-list-type": "map",
        "x-kubernetes-list-map-keys": ["type"],
        "items": {
            "type": "object",
            "required": ["type", "status", "reason", "message", "lastTransitionTime"],
            "properties": {
                "lastTransitionTime": { "type": "string", "format": "date-time" },
                "message": { "type": "string" },
                "observedGeneration": { "type": "integer", "format": "int64" },
                "reason": { "type": "string" },
                "status": { "type": "string" },
                "type": { "type": "string" }
            }
        }
    })
}
