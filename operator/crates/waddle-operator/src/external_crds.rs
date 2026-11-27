use kube::api::ApiResource;
use kube::discovery::Scope;
use kube::Client;
use serde::Deserialize;
use waddle_operator_fsm::DependencyHealth;

/// Discovered ESO API version supported by the target cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveredEsoVersion {
    /// `external-secrets.io/v1`.
    V1,
    /// `external-secrets.io/v1beta1`.
    V1Beta1,
}

impl DiscoveredEsoVersion {
    /// Returns the Kubernetes `apiVersion` string.
    #[must_use]
    pub const fn api_version(self) -> &'static str {
        match self {
            Self::V1 => "external-secrets.io/v1",
            Self::V1Beta1 => "external-secrets.io/v1beta1",
        }
    }

    /// Returns the Kubernetes version segment (`v1` or `v1beta1`).
    #[must_use]
    pub const fn version_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V1Beta1 => "v1beta1",
        }
    }
}

/// Optional third-party CRDs discovered on the cluster at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiscoveredCapabilities {
    /// Whether `postgresql.cnpg.io/v1` `Cluster` is served.
    pub has_cnpg: bool,
    /// Whether `authzed.com/v1alpha1` `SpiceDBCluster` is served.
    pub has_spicedb: bool,
    /// Preferred served version of `external-secrets.io` `ExternalSecret`.
    pub eso_version: Option<DiscoveredEsoVersion>,
    /// Whether `cert-manager.io/v1` `Certificate` is served.
    pub has_cert_manager: bool,
}

/// Discovers installed dependency CRDs (`postgresql.cnpg.io`, `authzed.com`, `external-secrets.io`,
/// `cert-manager.io`).
///
/// Missing groups are treated gracefully as absent capabilities so the controller can run even
/// when optional CRDs are not installed yet.
pub async fn discover_capabilities(client: &Client) -> DiscoveredCapabilities {
    let has_cnpg = has_group_version(client, "postgresql.cnpg.io", "v1").await;
    let has_spicedb = has_group_version(client, "authzed.com", "v1alpha1").await;
    let eso_version = if has_group_version(client, "external-secrets.io", "v1").await {
        Some(DiscoveredEsoVersion::V1)
    } else if has_group_version(client, "external-secrets.io", "v1beta1").await {
        Some(DiscoveredEsoVersion::V1Beta1)
    } else {
        None
    };
    let has_cert_manager = has_group_version(client, "cert-manager.io", "v1").await;

    DiscoveredCapabilities {
        has_cnpg,
        has_spicedb,
        eso_version,
        has_cert_manager,
    }
}

async fn has_group_version(client: &Client, group: &str, version: &str) -> bool {
    let gv = format!("{group}/{version}");
    match client.list_api_group_resources(&gv).await {
        Ok(list) => !list.resources.is_empty(),
        Err(_) => false,
    }
}

/// Dynamic [`ApiResource`] descriptor for CloudNativePG `Cluster` (`postgresql.cnpg.io/v1`).
#[must_use]
pub fn api_resource_cnpg() -> ApiResource {
    ApiResource {
        group: "postgresql.cnpg.io".to_owned(),
        version: "v1".to_owned(),
        api_version: "postgresql.cnpg.io/v1".to_owned(),
        kind: "Cluster".to_owned(),
        plural: "clusters".to_owned(),
    }
}

/// Dynamic [`ApiResource`] descriptor for AuthZed `SpiceDBCluster` (`authzed.com/v1alpha1`).
#[must_use]
pub fn api_resource_spicedb() -> ApiResource {
    ApiResource {
        group: "authzed.com".to_owned(),
        version: "v1alpha1".to_owned(),
        api_version: "authzed.com/v1alpha1".to_owned(),
        kind: "SpiceDBCluster".to_owned(),
        plural: "spicedbclusters".to_owned(),
    }
}

/// Dynamic [`ApiResource`] descriptor for ESO `ExternalSecret`.
#[must_use]
pub fn api_resource_eso(version: DiscoveredEsoVersion) -> ApiResource {
    ApiResource {
        group: "external-secrets.io".to_owned(),
        version: version.version_str().to_owned(),
        api_version: version.api_version().to_owned(),
        kind: "ExternalSecret".to_owned(),
        plural: "externalsecrets".to_owned(),
    }
}

/// Dynamic [`ApiResource`] descriptor for `cert-manager.io/v1` `Certificate`.
#[must_use]
pub fn api_resource_certificate() -> ApiResource {
    ApiResource {
        group: "cert-manager.io".to_owned(),
        version: "v1".to_owned(),
        api_version: "cert-manager.io/v1".to_owned(),
        kind: "Certificate".to_owned(),
        plural: "certificates".to_owned(),
    }
}

/// Returns the scope constant used for dynamic namespaced watchers.
#[must_use]
pub const fn namespaced_scope() -> Scope {
    Scope::Namespaced
}

/// Shared Kubernetes condition projection for third-party CRDs.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExternalCondition {
    /// Condition type string.
    #[serde(rename = "type")]
    pub kind: String,
    /// Status string (`"True"`, `"False"`, `"Unknown"`).
    pub status: String,
    /// Optional reason string.
    #[serde(default)]
    pub reason: Option<String>,
    /// Optional human-readable message.
    #[serde(default)]
    pub message: Option<String>,
}

/// Minimal typed projection of CloudNativePG `postgresql.cnpg.io/v1` `Cluster`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CnpgClusterView {
    /// Spec excerpt.
    #[serde(default)]
    pub spec: CnpgSpecView,
    /// Status excerpt.
    #[serde(default)]
    pub status: CnpgStatusView,
}

/// CNPG `spec` projection.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CnpgSpecView {
    /// Bootstrap configuration.
    #[serde(default)]
    pub bootstrap: Option<CnpgBootstrapView>,
}

/// CNPG `spec.bootstrap` projection.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CnpgBootstrapView {
    /// InitDB configuration.
    #[serde(default)]
    pub initdb: Option<CnpgInitDbView>,
}

/// CNPG `spec.bootstrap.initdb` projection.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CnpgInitDbView {
    /// Database name created at bootstrap.
    #[serde(default)]
    pub database: Option<String>,
    /// Explicit secret override holding credentials.
    #[serde(default)]
    pub secret: Option<CnpgLocalName>,
}

/// Named reference inside CNPG spec.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CnpgLocalName {
    /// Secret name.
    pub name: String,
}

/// CNPG `status` projection.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CnpgStatusView {
    /// Number of ready instances.
    #[serde(default)]
    pub ready_instances: i32,
    /// Pod name of current primary instance.
    #[serde(default)]
    pub current_primary: Option<String>,
    /// Human-readable phase summary.
    #[serde(default)]
    pub phase: Option<String>,
    /// Reported status conditions.
    #[serde(default)]
    pub conditions: Vec<ExternalCondition>,
}

impl CnpgClusterView {
    /// Resolves the CNPG application secret name (`spec.bootstrap.initdb.secret.name` or `<cluster>-app`).
    #[must_use]
    pub fn resolved_secret_name(&self, cluster_name: &str) -> String {
        self.spec
            .bootstrap
            .as_ref()
            .and_then(|b| b.initdb.as_ref())
            .and_then(|i| i.secret.as_ref())
            .map(|s| s.name.trim())
            .filter(|s| !s.is_empty())
            .map_or_else(|| format!("{cluster_name}-app"), str::to_owned)
    }

    /// Resolves the database name (`spec.bootstrap.initdb.database` or `"app"`).
    #[must_use]
    pub fn resolved_database_name(&self) -> &str {
        self.spec
            .bootstrap
            .as_ref()
            .and_then(|b| b.initdb.as_ref())
            .and_then(|i| i.database.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("app")
    }
}

/// Evaluates whether a CNPG `Cluster` is ready to serve Waddle.
#[must_use]
pub fn evaluate_cnpg_health(
    cluster: &CnpgClusterView,
    cluster_name: &str,
    require_healthy: bool,
) -> DependencyHealth {
    let endpoint_hint = format!("{cluster_name}-rw");
    if !require_healthy {
        return DependencyHealth::Ready { endpoint_hint };
    }

    let ready_condition = cluster
        .status
        .conditions
        .iter()
        .find(|cond| cond.kind == "Ready");
    let has_ready_true = ready_condition.is_some_and(|cond| cond.status == "True");
    let has_primary = cluster
        .status
        .current_primary
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty());

    if has_ready_true && cluster.status.ready_instances >= 1 && has_primary {
        return DependencyHealth::Ready { endpoint_hint };
    }

    let detail = ready_condition
        .and_then(|cond| cond.message.clone())
        .or_else(|| cluster.status.phase.clone())
        .unwrap_or_else(|| {
            format!(
                "readyInstances={}, hasPrimary={has_primary}",
                cluster.status.ready_instances
            )
        });
    let reason = ready_condition
        .and_then(|cond| cond.reason.clone())
        .unwrap_or_else(|| "ClusterNotReady".to_owned());

    DependencyHealth::NotReady { reason, detail }
}

/// Minimal typed projection of AuthZed `authzed.com/v1alpha1` `SpiceDBCluster`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpiceDbClusterView {
    /// Object metadata excerpt.
    #[serde(default)]
    pub metadata: SpiceDbMetadataView,
    /// Spec excerpt.
    #[serde(default)]
    pub spec: SpiceDbSpecView,
    /// Status excerpt.
    #[serde(default)]
    pub status: SpiceDbStatusView,
}

/// SpiceDB metadata projection.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SpiceDbMetadataView {
    /// Resource generation.
    #[serde(default)]
    pub generation: Option<i64>,
}

/// SpiceDB `spec` projection.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpiceDbSpecView {
    /// Secret name storing `preshared_key`.
    #[serde(default)]
    pub secret_name: Option<String>,
    /// Typed datastore / TLS config excerpt.
    #[serde(default)]
    pub config: SpiceDbConfigView,
}

/// SpiceDB `spec.config` projection.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpiceDbConfigView {
    /// Datastore engine (`postgres`, `cockroachdb`, `memory`, etc.).
    #[serde(default)]
    pub datastore_engine: Option<String>,
    /// Optional TLS Secret name (when absent, gRPC is plaintext `http://`).
    #[serde(default)]
    pub tls_secret_name: Option<String>,
}

/// SpiceDB `status` projection.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpiceDbStatusView {
    /// Observed generation processed by `spicedb-operator`.
    #[serde(default)]
    pub observed_generation: Option<i64>,
    /// Current migration hash.
    #[serde(default)]
    pub current_migration_hash: Option<String>,
    /// Target migration hash.
    #[serde(default)]
    pub target_migration_hash: Option<String>,
    /// Active conditions reported by `spicedb-operator`.
    #[serde(default)]
    pub conditions: Vec<ExternalCondition>,
}

impl SpiceDbClusterView {
    /// Returns whether TLS is configured on the `SpiceDBCluster`.
    #[must_use]
    pub fn has_tls(&self) -> bool {
        self.spec
            .config
            .tls_secret_name
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
    }
}

/// Evaluates whether an AuthZed `SpiceDBCluster` is ready and serving endpoints.
#[must_use]
pub fn evaluate_spicedb_health(
    cluster: &SpiceDbClusterView,
    endpoint_hint: &str,
    ready_endpoints: usize,
    require_ready: bool,
) -> DependencyHealth {
    if !require_ready {
        return DependencyHealth::Ready {
            endpoint_hint: endpoint_hint.to_owned(),
        };
    }

    if let (Some(gen), Some(obs)) = (
        cluster.metadata.generation,
        cluster.status.observed_generation,
    ) {
        if obs < gen {
            return DependencyHealth::NotReady {
                reason: "GenerationPending".to_owned(),
                detail: format!(
                    "SpiceDBCluster observedGeneration ({obs}) lags generation ({gen})"
                ),
            };
        }
    }

    let is_memory = cluster
        .spec
        .config
        .datastore_engine
        .as_deref()
        .is_some_and(|engine| engine.eq_ignore_ascii_case("memory"));

    let migrations_aligned = if is_memory {
        cluster.status.current_migration_hash == cluster.status.target_migration_hash
    } else {
        match (
            cluster
                .status
                .current_migration_hash
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty()),
            cluster
                .status
                .target_migration_hash
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty()),
        ) {
            (Some(current), Some(target)) => current == target,
            _ => false,
        }
    };

    if !migrations_aligned {
        return DependencyHealth::NotReady {
            reason: "MigrationsPending".to_owned(),
            detail: "SpiceDB datastore migrations have not converged".to_owned(),
        };
    }

    for cond in &cluster.status.conditions {
        if cond.status != "True" {
            continue;
        }
        let non_blocking = cond.kind == "ConfigurationWarning"
            || (cond.kind == "RollingDeployment" && ready_endpoints >= 1);
        if !non_blocking {
            return DependencyHealth::NotReady {
                reason: cond.kind.clone(),
                detail: cond
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("SpiceDBCluster condition {} is True", cond.kind)),
            };
        }
    }

    if ready_endpoints == 0 {
        return DependencyHealth::NotReady {
            reason: "NoReadyEndpoints".to_owned(),
            detail: "SpiceDB Service has 0 ready endpoints".to_owned(),
        };
    }

    DependencyHealth::Ready {
        endpoint_hint: endpoint_hint.to_owned(),
    }
}

/// Minimal typed projection of ESO `ExternalSecret` (`v1` or `v1beta1`).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSecretView {
    /// Spec excerpt.
    #[serde(default)]
    pub spec: ExternalSecretSpecView,
    /// Status excerpt.
    #[serde(default)]
    pub status: ExternalSecretStatusView,
}

/// ESO `ExternalSecret.spec` projection.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSecretSpecView {
    /// Target secret specification.
    #[serde(default)]
    pub target: Option<ExternalSecretTargetView>,
}

/// ESO `ExternalSecret.spec.target` projection.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExternalSecretTargetView {
    /// Target `Secret` name.
    #[serde(default)]
    pub name: Option<String>,
}

/// ESO `ExternalSecret.status` projection.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSecretStatusView {
    /// Conditions reported by ESO.
    #[serde(default)]
    pub conditions: Vec<ExternalCondition>,
}

/// Evaluates whether an ESO `ExternalSecret` reports `Ready=True`.
#[must_use]
pub fn evaluate_eso_status(es: &ExternalSecretView) -> (Option<bool>, Option<String>) {
    let Some(ready) = es
        .status
        .conditions
        .iter()
        .find(|cond| cond.kind == "Ready")
    else {
        return (
            Some(false),
            Some("Waiting for ExternalSecret Ready condition".to_owned()),
        );
    };
    if ready.status == "True" {
        (Some(true), None)
    } else {
        let msg = ready
            .message
            .clone()
            .or_else(|| ready.reason.clone())
            .unwrap_or_else(|| "ExternalSecret Ready is not True".to_owned());
        (Some(false), Some(msg))
    }
}
