use std::collections::BTreeMap;
use std::str::FromStr;

use k8s_openapi::api::apps::v1::{Deployment, RollingUpdateDeployment};
use k8s_openapi::api::core::v1::{
    ConfigMap, Container, EnvVar, EnvVarSource, PersistentVolumeClaim, PodSecurityContext,
    PodTemplateSpec, Probe, ResourceRequirements, Secret, SecurityContext, Service, ServiceAccount,
    Volume, VolumeMount,
};
use k8s_openapi::api::discovery::v1::EndpointSlice;
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition as KubeCondition;
use kube::api::{Api, DynamicObject, ListParams};
use kube::{Client, ResourceExt};
use waddle_operator_crd::{
    CloudNativePgSource, ContentDigest, EsoApiVersion, ExternalPostgresSource, ExternalSecretsSpec,
    ExternalSpiceDbSource, PostgresSourceMode, ResourceName, RestrictedRuntimeSpec,
    SecretProviderMode, SecretStoreKind, SpiceDbOperatorSource, SpiceDbSourceMode,
    UpdateStrategyKind, ValidatedSpec, WaddleServer, WaddleServerSpec,
};
use waddle_operator_fsm::{
    ConditionKind, ConditionRecord, ConditionStatus, DependencyHealth, ObservedWorld,
    SecretSourceKind, SecretsObservation, SensitiveKeyRotation, SpecValidationState,
    WorkloadObservation,
};

use crate::bindings::{PostgresBinding, ResolvedRuntimeBindings, SpiceDbBinding};
use crate::error::OperatorError;
use crate::external_crds::{
    self, CnpgClusterView, DiscoveredCapabilities, DiscoveredEsoVersion, ExternalSecretView,
    SpiceDbClusterView,
};
use crate::resources::{
    self, ANNOTATION_OCCUPANT_ID_FINGERPRINT, ANNOTATION_SESSION_KEY_FINGERPRINT,
    ANNOTATION_SPEC_HASH, KEY_DEPLOYMENT_UUID, KEY_OCCUPANT_ID_SECRET, KEY_SESSION_KEY,
    KEY_SPICEDB_PRESHARED_KEY,
};
use crate::yolo;

/// Runtime security policy and discovered cluster capabilities.
#[derive(Debug, Clone, Default)]
pub struct OperatorPolicy {
    /// Whether `--enable-yolo-mode` was explicitly opted in.
    pub enable_yolo_mode: bool,
    /// Allowlisted `ClusterSecretStore` names for operator-managed `ExternalSecret`s.
    pub allowed_cluster_secret_stores: Vec<String>,
    /// Third-party CRD groups discovered on startup.
    pub capabilities: DiscoveredCapabilities,
}

/// Complete observed state plus non-secret runtime bindings needed by command execution.
#[derive(Debug, Clone)]
pub struct WorldSnapshot {
    /// Pure snapshot consumed by `waddle_operator_fsm::evaluate`.
    pub world: ObservedWorld,
    /// Pre-validated spec invariants when `spec_validation` passed.
    pub validated: Option<ValidatedSpec>,
    /// Resolved Kubernetes resource bindings when dependencies and secrets can be wired.
    pub bindings: Option<ResolvedRuntimeBindings>,
    /// Non-secret deployment UUID discovered from spec or YOLO secret.
    pub resolved_deployment_uuid: Option<String>,
}

/// Observes the same-namespace dependencies, secrets, and workload state of a `WaddleServer`.
///
/// # Errors
/// Returns [`OperatorError`] only on unexpected Kubernetes API failures or missing metadata.
/// Missing third-party CRDs (`404`) and missing resources are captured inside `ObservedWorld`.
pub async fn observe_world(
    client: &Client,
    ws: &WaddleServer,
    policy: &OperatorPolicy,
) -> Result<WorldSnapshot, OperatorError> {
    let name = resources::workload_name(ws)?;
    let ns = resources::workload_namespace(ws)?;

    let live_deployment = Api::<Deployment>::namespaced(client.clone(), ns)
        .get_opt(name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get live Deployment",
            source,
        })?;

    let (validated, spec_validation) = match waddle_operator_crd::validate_spec(&ws.spec) {
        Ok(v) => (Some(v), SpecValidationState::Valid),
        Err(report) => {
            let existing_conditions = ws
                .status
                .as_ref()
                .map(|s| parse_existing_conditions(&s.conditions))
                .unwrap_or_default();
            let workload =
                observe_workload(client, ns, ws, None, None, live_deployment.as_ref()).await?;
            let world = ObservedWorld {
                generation: ws.metadata.generation.unwrap_or(1),
                deleting: ws.metadata.deletion_timestamp.is_some(),
                spec_validation: SpecValidationState::Invalid {
                    summary: report.summary(),
                },
                postgres: DependencyHealth::NotReady {
                    reason: "SpecInvalid".to_owned(),
                    detail: "Specification invalid; PostgreSQL lookup skipped".to_owned(),
                },
                spicedb: DependencyHealth::NotReady {
                    reason: "SpecInvalid".to_owned(),
                    detail: "Specification invalid; SpiceDB lookup skipped".to_owned(),
                },
                secrets: SecretsObservation {
                    source: spec_secret_source_kind(&ws.spec),
                    yolo_allowed_by_operator: policy.enable_yolo_mode,
                    cluster_store_allowed: true,
                    external_secret_synced: None,
                    external_secret_error: Some(
                        "Specification invalid; secret lookup skipped".to_owned(),
                    ),
                    managed_external_secret_drifted: false,
                    target_secret_exists: false,
                    missing_keys: Vec::new(),
                    invalid_keys: Vec::new(),
                    checksum: None,
                    sensitive_key_rotated: None,
                },
                workload,
                existing_conditions,
                now_rfc3339: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            };
            return Ok(WorldSnapshot {
                world,
                validated: None,
                bindings: None,
                resolved_deployment_uuid: None,
            });
        }
    };

    let (postgres_health, postgres_binding, pg_digest) =
        observe_postgres(client, ns, ws, policy.capabilities.has_cnpg).await?;

    let spicedb_key_in_runtime_secret = spicedb_expects_key_in_runtime_secret(ws);

    let mut secret_obs = observe_secrets(
        client,
        ws,
        ns,
        policy,
        live_deployment.as_ref(),
        spicedb_key_in_runtime_secret,
    )
    .await?;

    let (spicedb_health, spicedb_binding, sp_digest) = observe_spicedb(
        client,
        ws,
        ns,
        policy.capabilities.has_spicedb,
        &secret_obs.target_secret_name,
    )
    .await?;

    let mut digest_parts = secret_obs.component_digests;
    if let Some(digest) = pg_digest {
        digest_parts.insert("postgres".to_owned(), digest);
    }
    if let Some(digest) = sp_digest {
        digest_parts.insert("spicedb".to_owned(), digest);
    }
    secret_obs.observation.checksum = yolo::combine_digests(&digest_parts);

    let bindings = if let (Some(pg), Some(spicedb)) = (postgres_binding, spicedb_binding) {
        Some(ResolvedRuntimeBindings {
            postgres: pg,
            spicedb,
            runtime_secret_name: secret_obs.target_secret_name,
            extra_secret_refs: collect_extra_secret_refs(ws),
            clustering_keypool_secret_name: ws
                .spec
                .secrets
                .external_secrets
                .as_ref()
                .and_then(|e| e.clustering_keypool_secret_name.clone()),
            deployment_uuid: ws.spec.deployment.uuid.clone(),
            lineage_action: resolve_lineage_action(ws),
            secrets_checksum: secret_obs.observation.checksum.clone(),
            session_key_fingerprint: secret_obs.session_key_fingerprint,
            occupant_id_fingerprint: secret_obs.occupant_id_fingerprint,
            eso_api_version: secret_obs.eso_api_version,
        })
    } else {
        None
    };

    let workload = observe_workload(
        client,
        ns,
        ws,
        validated.as_ref(),
        bindings.as_ref(),
        live_deployment.as_ref(),
    )
    .await?;

    let existing_conditions = ws
        .status
        .as_ref()
        .map(|s| parse_existing_conditions(&s.conditions))
        .unwrap_or_default();

    let world = ObservedWorld {
        generation: ws.metadata.generation.unwrap_or(1),
        deleting: ws.metadata.deletion_timestamp.is_some(),
        spec_validation,
        postgres: postgres_health,
        spicedb: spicedb_health,
        secrets: secret_obs.observation,
        workload,
        existing_conditions,
        now_rfc3339: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };

    Ok(WorldSnapshot {
        world,
        validated,
        bindings,
        resolved_deployment_uuid: secret_obs.resolved_deployment_uuid,
    })
}

type PostgresObservation = (
    DependencyHealth,
    Option<PostgresBinding>,
    Option<ContentDigest>,
);

async fn observe_postgres(
    client: &Client,
    ns: &str,
    ws: &WaddleServer,
    known_has_cnpg: bool,
) -> Result<PostgresObservation, OperatorError> {
    let pg = &ws.spec.postgres;
    let restricted = pg.restricted_runtime.as_ref().filter(|r| r.enabled);

    match pg.mode {
        PostgresSourceMode::CloudNativePg => {
            observe_cnpg_postgres(client, ns, pg.cnpg.as_ref(), restricted, known_has_cnpg).await
        }
        PostgresSourceMode::External => {
            observe_external_postgres(client, ns, pg.external.as_ref(), restricted).await
        }
    }
}

async fn observe_cnpg_postgres(
    client: &Client,
    ns: &str,
    cnpg: Option<&CloudNativePgSource>,
    restricted: Option<&RestrictedRuntimeSpec>,
    known_has_cnpg: bool,
) -> Result<PostgresObservation, OperatorError> {
    let Some(cnpg) = cnpg else {
        return Ok((
            DependencyHealth::NotReady {
                reason: "CnpgSpecMissing".to_owned(),
                detail: "postgres.mode is CloudNativePg without spec.postgres.cnpg".to_owned(),
            },
            None,
            None,
        ));
    };

    let cluster_view =
        match fetch_cnpg_cluster(client, ns, &cnpg.cluster_name, known_has_cnpg).await? {
            Ok(view) => view,
            Err(health) => return Ok((health, None, None)),
        };

    let health = external_crds::evaluate_cnpg_health(
        &cluster_view,
        &cnpg.cluster_name,
        cnpg.require_healthy,
    );
    if !health.is_ready() {
        return Ok((health, None, None));
    }

    if let Some(rr) = restricted {
        let (blocker, rr_digest) = observe_restricted_runtime(client, ns, rr).await?;
        if let Some(b) = blocker {
            return Ok((b, None, None));
        }
        return Ok((
            health,
            Some(PostgresBinding::RestrictedRuntime {
                spec: Box::new(rr.clone()),
            }),
            rr_digest,
        ));
    }

    resolve_cnpg_secret_binding(client, ns, cnpg, &cluster_view, health).await
}

async fn fetch_cnpg_cluster(
    client: &Client,
    ns: &str,
    cluster_name: &str,
    known_has_cnpg: bool,
) -> Result<Result<CnpgClusterView, DependencyHealth>, OperatorError> {
    let ar = external_crds::api_resource_cnpg();
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), ns, &ar);
    let cluster_obj = match api.get(cluster_name).await {
        Ok(obj) => obj,
        Err(kube::Error::Api(status)) if status.code == 404 => {
            if is_missing_crd_status(&status.message)
                || (!known_has_cnpg
                    && !has_group_version_live(client, "postgresql.cnpg.io", "v1").await)
            {
                return Ok(Err(DependencyHealth::CrdNotInstalled {
                    api_group: "postgresql.cnpg.io".to_owned(),
                }));
            }
            return Ok(Err(DependencyHealth::Missing {
                reason: "CnpgClusterNotFound".to_owned(),
                detail: format!("CloudNativePG Cluster {cluster_name} not found in namespace {ns}"),
            }));
        }
        Err(source) => {
            return Err(OperatorError::Kube {
                context: "get CloudNativePG Cluster",
                source,
            });
        }
    };

    let view: CnpgClusterView = serde_json::from_value(cluster_obj.data)
        .map_err(|err| OperatorError::InvalidClusterState(err.to_string()))?;
    Ok(Ok(view))
}

async fn resolve_cnpg_secret_binding(
    client: &Client,
    ns: &str,
    cnpg: &CloudNativePgSource,
    cluster_view: &CnpgClusterView,
    health: DependencyHealth,
) -> Result<PostgresObservation, OperatorError> {
    let secret_name = cnpg
        .secret_name
        .clone()
        .unwrap_or_else(|| cluster_view.resolved_secret_name(&cnpg.cluster_name));
    if !is_safe_resource_name(&secret_name) {
        return Ok((
            DependencyHealth::NotReady {
                reason: "InvalidPostgresSecretName".to_owned(),
                detail: format!(
                    "Resolved CNPG credential Secret name '{secret_name}' is not a valid Kubernetes resource name"
                ),
            },
            None,
            None,
        ));
    }
    let Some(secret) = Api::<Secret>::namespaced(client.clone(), ns)
        .get_opt(&secret_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get CNPG credentials Secret",
            source,
        })?
    else {
        return Ok((
            DependencyHealth::NotReady {
                reason: "PostgresSecretMissing".to_owned(),
                detail: format!("PostgreSQL credential Secret {secret_name} not found"),
            },
            None,
            None,
        ));
    };

    let data = secret.data.unwrap_or_default();
    let raw = to_raw_byte_map(Some(&data));
    let pg_digest = yolo::compute_consumed_secret_digest(
        &raw,
        &[&cnpg.uri_key, &cnpg.username_key, &cnpg.password_key],
    );
    if has_nonempty_key(&data, &cnpg.uri_key) {
        return Ok((
            health,
            Some(PostgresBinding::UriSecret {
                secret_name,
                uri_key: cnpg.uri_key.clone(),
            }),
            pg_digest,
        ));
    }
    if has_nonempty_key(&data, &cnpg.username_key) && has_nonempty_key(&data, &cnpg.password_key) {
        return Ok((
            health,
            Some(PostgresBinding::BasicAuthSecret {
                secret_name,
                username_key: cnpg.username_key.clone(),
                password_key: cnpg.password_key.clone(),
                host: format!("{}-rw", cnpg.cluster_name),
                port: 5432,
                database: cluster_view.resolved_database_name().to_owned(),
            }),
            pg_digest,
        ));
    }

    Ok((
        DependencyHealth::NotReady {
            reason: "PostgresSecretKeysMissing".to_owned(),
            detail: format!(
                "Secret {secret_name} lacks non-empty '{}' or '{}'/'{}'",
                cnpg.uri_key, cnpg.username_key, cnpg.password_key
            ),
        },
        None,
        None,
    ))
}

async fn observe_external_postgres(
    client: &Client,
    ns: &str,
    ext: Option<&ExternalPostgresSource>,
    restricted: Option<&RestrictedRuntimeSpec>,
) -> Result<PostgresObservation, OperatorError> {
    let Some(ext) = ext else {
        return Ok((
            DependencyHealth::NotReady {
                reason: "ExternalPostgresSpecMissing".to_owned(),
                detail: "postgres.mode is External without spec.postgres.external".to_owned(),
            },
            None,
            None,
        ));
    };

    if let Some(svc_name) = &ext.service_name {
        if let Some(blocker) = check_service_endpoints(client, ns, svc_name, "Postgres").await? {
            return Ok((blocker, None, None));
        }
    }

    if let Some(rr) = restricted {
        let (blocker, rr_digest) = observe_restricted_runtime(client, ns, rr).await?;
        if let Some(b) = blocker {
            return Ok((b, None, None));
        }
        return Ok((
            DependencyHealth::Ready {
                endpoint_hint: format!(
                    "{}:{}/{}",
                    rr.database_host, rr.database_port, rr.database_name
                ),
            },
            Some(PostgresBinding::RestrictedRuntime {
                spec: Box::new(rr.clone()),
            }),
            rr_digest,
        ));
    }

    let Some(secret) = Api::<Secret>::namespaced(client.clone(), ns)
        .get_opt(&ext.secret_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get external Postgres Secret",
            source,
        })?
    else {
        return Ok((
            DependencyHealth::Missing {
                reason: "PostgresSecretMissing".to_owned(),
                detail: format!(
                    "PostgreSQL Secret {} not found in namespace {ns}",
                    ext.secret_name
                ),
            },
            None,
            None,
        ));
    };

    let data = secret.data.unwrap_or_default();
    if !has_nonempty_key(&data, &ext.uri_key) {
        return Ok((
            DependencyHealth::NotReady {
                reason: "PostgresUriKeyMissing".to_owned(),
                detail: format!(
                    "PostgreSQL Secret {} is missing non-empty key '{}'",
                    ext.secret_name, ext.uri_key
                ),
            },
            None,
            None,
        ));
    }

    let pg_digest =
        yolo::compute_consumed_secret_digest(&to_raw_byte_map(Some(&data)), &[&ext.uri_key]);
    let hint = ext.service_name.as_ref().map_or_else(
        || format!("secret/{}", ext.secret_name),
        |svc| format!("svc/{svc}"),
    );
    Ok((
        DependencyHealth::Ready {
            endpoint_hint: hint,
        },
        Some(PostgresBinding::UriSecret {
            secret_name: ext.secret_name.clone(),
            uri_key: ext.uri_key.clone(),
        }),
        pg_digest,
    ))
}

async fn observe_restricted_runtime(
    client: &Client,
    ns: &str,
    rr: &RestrictedRuntimeSpec,
) -> Result<(Option<DependencyHealth>, Option<ContentDigest>), OperatorError> {
    let secrets = Api::<Secret>::namespaced(client.clone(), ns);
    let Some(ca_secret) = secrets
        .get_opt(&rr.ca_secret_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get restricted runtime CA Secret",
            source,
        })?
    else {
        return Ok((
            Some(DependencyHealth::Missing {
                reason: "PostgresCaSecretMissing".to_owned(),
                detail: format!(
                    "Restricted runtime CA Secret {} not found",
                    rr.ca_secret_name
                ),
            }),
            None,
        ));
    };
    let ca_data = ca_secret.data.unwrap_or_default();
    if !has_nonempty_key(&ca_data, &rr.ca_secret_key) {
        return Ok((
            Some(DependencyHealth::NotReady {
                reason: "PostgresCaKeyMissing".to_owned(),
                detail: format!(
                    "CA Secret {} lacks non-empty key '{}'",
                    rr.ca_secret_name, rr.ca_secret_key
                ),
            }),
            None,
        ));
    }

    let Some(cred_secret) = secrets
        .get_opt(&rr.runtime_credential_secret)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get restricted runtime credential Secret",
            source,
        })?
    else {
        return Ok((
            Some(DependencyHealth::Missing {
                reason: "PostgresRuntimeCredentialMissing".to_owned(),
                detail: format!(
                    "Restricted runtime credential Secret {} not found",
                    rr.runtime_credential_secret
                ),
            }),
            None,
        ));
    };
    let cred_data = cred_secret.data.unwrap_or_default();
    if !has_nonempty_key(&cred_data, &rr.runtime_credential_key) {
        return Ok((
            Some(DependencyHealth::NotReady {
                reason: "PostgresRuntimeCredentialKeyMissing".to_owned(),
                detail: format!(
                    "Credential Secret {} lacks non-empty key '{}'",
                    rr.runtime_credential_secret, rr.runtime_credential_key
                ),
            }),
            None,
        ));
    }

    let Some(handoff_cm) = Api::<ConfigMap>::namespaced(client.clone(), ns)
        .get_opt(&rr.handoff_config_map_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get restricted runtime handoff ConfigMap",
            source,
        })?
    else {
        return Ok((
            Some(DependencyHealth::Missing {
                reason: "PostgresHandoffMissing".to_owned(),
                detail: format!(
                    "Restricted runtime handoff ConfigMap {} not found",
                    rr.handoff_config_map_name
                ),
            }),
            None,
        ));
    };
    let handoff_val = handoff_cm
        .data
        .as_ref()
        .and_then(|d| d.get("handoff.json"))
        .filter(|v| !v.trim().is_empty());
    let Some(handoff_json) = handoff_val else {
        return Ok((
            Some(DependencyHealth::NotReady {
                reason: "PostgresHandoffKeyMissing".to_owned(),
                detail: format!(
                    "Handoff ConfigMap {} lacks non-empty 'handoff.json'",
                    rr.handoff_config_map_name
                ),
            }),
            None,
        ));
    };

    let mut parts = BTreeMap::new();
    if let Some(d) = ca_data
        .get(&rr.ca_secret_key)
        .and_then(|v| yolo::compute_single_key_digest(&v.0))
    {
        parts.insert("ca".to_owned(), d);
    }
    if let Some(d) = cred_data
        .get(&rr.runtime_credential_key)
        .and_then(|v| yolo::compute_single_key_digest(&v.0))
    {
        parts.insert("cred".to_owned(), d);
    }
    if let Some(d) = yolo::compute_single_key_digest(handoff_json.as_bytes()) {
        parts.insert("handoff".to_owned(), d);
    }

    Ok((None, yolo::combine_digests(&parts)))
}

type SpiceDbObservation = (
    DependencyHealth,
    Option<SpiceDbBinding>,
    Option<ContentDigest>,
);

async fn observe_spicedb(
    client: &Client,
    ws: &WaddleServer,
    ns: &str,
    known_has_spicedb: bool,
    runtime_secret_name: &str,
) -> Result<SpiceDbObservation, OperatorError> {
    let sp = &ws.spec.spicedb;
    match sp.mode {
        SpiceDbSourceMode::SpiceDbOperator => {
            observe_spicedb_operator(
                client,
                ws,
                ns,
                sp.operator.as_ref(),
                known_has_spicedb,
                runtime_secret_name,
            )
            .await
        }
        SpiceDbSourceMode::External => {
            observe_external_spicedb(client, ns, sp.external.as_ref(), runtime_secret_name).await
        }
    }
}

async fn observe_spicedb_operator(
    client: &Client,
    ws: &WaddleServer,
    ns: &str,
    op: Option<&SpiceDbOperatorSource>,
    known_has_spicedb: bool,
    runtime_secret_name: &str,
) -> Result<SpiceDbObservation, OperatorError> {
    let Some(op) = op else {
        return Ok((
            DependencyHealth::NotReady {
                reason: "SpiceDbSpecMissing".to_owned(),
                detail: "spicedb.mode is SpiceDbOperator without spec.spicedb.operator".to_owned(),
            },
            None,
            None,
        ));
    };

    let view = match fetch_spicedb_cluster(client, ns, &op.cluster_name, known_has_spicedb).await? {
        Ok(view) => view,
        Err(health) => return Ok((health, None, None)),
    };

    let svc_name = op.service_name.as_deref().unwrap_or(&op.cluster_name);
    let ready_endpoints = count_ready_endpoints(client, ns, svc_name).await?;
    let insecure = op.insecure.unwrap_or_else(|| !view.has_tls());
    let scheme = if insecure { "http" } else { "https" };
    let endpoint = format!("{scheme}://{svc_name}:{}", op.port);

    let health =
        external_crds::evaluate_spicedb_health(&view, &endpoint, ready_endpoints, op.require_ready);
    if !health.is_ready() {
        return Ok((health, None, None));
    }

    let (pk_secret_name, pk_secret_key, sp_digest) =
        match resolve_spicedb_operator_key(client, ws, ns, op, &view, runtime_secret_name).await? {
            Ok(resolved) => resolved,
            Err(blocker) => return Ok((blocker, None, None)),
        };

    Ok((
        health,
        Some(SpiceDbBinding {
            endpoint,
            insecure,
            preshared_key_secret_name: pk_secret_name,
            preshared_key_secret_key: pk_secret_key,
        }),
        sp_digest,
    ))
}

async fn fetch_spicedb_cluster(
    client: &Client,
    ns: &str,
    cluster_name: &str,
    known_has_spicedb: bool,
) -> Result<Result<SpiceDbClusterView, DependencyHealth>, OperatorError> {
    let ar = external_crds::api_resource_spicedb();
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), ns, &ar);
    let cluster_obj = match api.get(cluster_name).await {
        Ok(obj) => obj,
        Err(kube::Error::Api(status)) if status.code == 404 => {
            if is_missing_crd_status(&status.message)
                || (!known_has_spicedb
                    && !has_group_version_live(client, "authzed.com", "v1alpha1").await)
            {
                return Ok(Err(DependencyHealth::CrdNotInstalled {
                    api_group: "authzed.com".to_owned(),
                }));
            }
            return Ok(Err(DependencyHealth::Missing {
                reason: "SpiceDbClusterNotFound".to_owned(),
                detail: format!("SpiceDBCluster {cluster_name} not found in namespace {ns}"),
            }));
        }
        Err(source) => {
            return Err(OperatorError::Kube {
                context: "get SpiceDBCluster",
                source,
            });
        }
    };

    let mut view: SpiceDbClusterView = serde_json::from_value(cluster_obj.data)
        .map_err(|err| OperatorError::InvalidClusterState(err.to_string()))?;
    // DynamicObject stores object metadata in `cluster_obj.metadata` rather than `data`.
    if view.metadata.generation.is_none() {
        view.metadata.generation = cluster_obj.metadata.generation;
    }
    Ok(Ok(view))
}

async fn resolve_spicedb_operator_key(
    client: &Client,
    ws: &WaddleServer,
    ns: &str,
    op: &SpiceDbOperatorSource,
    view: &SpiceDbClusterView,
    runtime_secret_name: &str,
) -> Result<Result<(String, String, Option<ContentDigest>), DependencyHealth>, OperatorError> {
    if let Some(explicit) = &op.preshared_key_secret_ref {
        let (blocker, digest) =
            verify_secret_key_present(client, ns, &explicit.name, &explicit.key, "SpiceDB").await?;
        if let Some(b) = blocker {
            return Ok(Err(b));
        }
        return Ok(Ok((explicit.name.clone(), explicit.key.clone(), digest)));
    }
    if ws.spec.secrets.mode != SecretProviderMode::Yolo {
        return Ok(Ok((
            runtime_secret_name.to_owned(),
            KEY_SPICEDB_PRESHARED_KEY.to_owned(),
            None,
        )));
    }

    let Some(cluster_secret) = view
        .spec
        .secret_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(Err(DependencyHealth::NotReady {
            reason: "SpiceDbSecretNameMissing".to_owned(),
            detail: format!(
                "SpiceDBCluster {} has no spec.secretName; set presharedKeySecretRef",
                op.cluster_name
            ),
        }));
    };
    if !is_safe_resource_name(cluster_secret) {
        return Ok(Err(DependencyHealth::NotReady {
            reason: "InvalidSpiceDbSecretName".to_owned(),
            detail: format!(
                "SpiceDBCluster.spec.secretName '{cluster_secret}' is not a valid Kubernetes resource name"
            ),
        }));
    }
    let (blocker, digest) =
        verify_secret_key_present(client, ns, cluster_secret, "preshared_key", "SpiceDB").await?;
    if let Some(b) = blocker {
        return Ok(Err(b));
    }
    Ok(Ok((
        cluster_secret.to_owned(),
        "preshared_key".to_owned(),
        digest,
    )))
}

async fn observe_external_spicedb(
    client: &Client,
    ns: &str,
    ext: Option<&ExternalSpiceDbSource>,
    runtime_secret_name: &str,
) -> Result<SpiceDbObservation, OperatorError> {
    let Some(ext) = ext else {
        return Ok((
            DependencyHealth::NotReady {
                reason: "ExternalSpiceDbSpecMissing".to_owned(),
                detail: "spicedb.mode is External without spec.spicedb.external".to_owned(),
            },
            None,
            None,
        ));
    };

    if let Some(svc_name) = &ext.service_name {
        if let Some(blocker) = check_service_endpoints(client, ns, svc_name, "SpiceDB").await? {
            return Ok((blocker, None, None));
        }
    }

    let (pk_secret_name, pk_secret_key, sp_digest) = if let Some(explicit) =
        &ext.preshared_key_secret_ref
    {
        let (blocker, digest) =
            verify_secret_key_present(client, ns, &explicit.name, &explicit.key, "SpiceDB").await?;
        if let Some(b) = blocker {
            return Ok((b, None, None));
        }
        (explicit.name.clone(), explicit.key.clone(), digest)
    } else {
        (
            runtime_secret_name.to_owned(),
            KEY_SPICEDB_PRESHARED_KEY.to_owned(),
            None,
        )
    };

    Ok((
        DependencyHealth::Ready {
            endpoint_hint: ext.endpoint.clone(),
        },
        Some(SpiceDbBinding {
            endpoint: ext.endpoint.clone(),
            insecure: ext.insecure,
            preshared_key_secret_name: pk_secret_name,
            preshared_key_secret_key: pk_secret_key,
        }),
        sp_digest,
    ))
}

struct SecretObservationBundle {
    observation: SecretsObservation,
    target_secret_name: String,
    component_digests: BTreeMap<String, ContentDigest>,
    session_key_fingerprint: Option<ContentDigest>,
    occupant_id_fingerprint: Option<ContentDigest>,
    eso_api_version: Option<DiscoveredEsoVersion>,
    resolved_deployment_uuid: Option<String>,
}

async fn observe_secrets(
    client: &Client,
    ws: &WaddleServer,
    ns: &str,
    policy: &OperatorPolicy,
    live_deployment: Option<&Deployment>,
    spicedb_key_in_runtime_secret: bool,
) -> Result<SecretObservationBundle, OperatorError> {
    let mut bundle = match ws.spec.secrets.mode {
        SecretProviderMode::Yolo => {
            observe_yolo_secrets(
                client,
                ws,
                ns,
                policy,
                live_deployment,
                spicedb_key_in_runtime_secret,
            )
            .await?
        }
        SecretProviderMode::ExternalSecrets => {
            observe_eso_secrets(
                client,
                ws,
                ns,
                policy,
                live_deployment,
                spicedb_key_in_runtime_secret,
            )
            .await?
        }
    };

    if let Some(runtime_digest) = bundle.observation.checksum.take() {
        bundle
            .component_digests
            .insert("runtime".to_owned(), runtime_digest);
    }
    collect_companion_secret_digests(client, ns, ws, &mut bundle.component_digests).await?;
    bundle.observation.checksum = yolo::combine_digests(&bundle.component_digests);
    Ok(bundle)
}

async fn observe_yolo_secrets(
    client: &Client,
    ws: &WaddleServer,
    ns: &str,
    policy: &OperatorPolicy,
    live_deployment: Option<&Deployment>,
    spicedb_key_in_runtime_secret: bool,
) -> Result<SecretObservationBundle, OperatorError> {
    let target_secret_name = resources::yolo_secret_name(ws)?;
    let secret = Api::<Secret>::namespaced(client.clone(), ns)
        .get_opt(&target_secret_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get YOLO runtime Secret",
            source,
        })?;

    let mut missing_keys = Vec::new();
    let mut invalid_keys = Vec::new();
    let mut resolved_deployment_uuid = ws.spec.deployment.uuid.clone();
    let mut checksum = None;
    let mut session_fp = None;
    let mut occupant_fp = None;

    let target_secret_exists = if let Some(sec) = secret {
        let raw_entries = to_raw_byte_map(sec.data.as_ref());
        check_min_length_key(
            &raw_entries,
            KEY_SESSION_KEY,
            32,
            &mut missing_keys,
            &mut invalid_keys,
        );
        check_min_length_key(
            &raw_entries,
            KEY_OCCUPANT_ID_SECRET,
            32,
            &mut missing_keys,
            &mut invalid_keys,
        );
        check_nonempty_key(&raw_entries, KEY_DEPLOYMENT_UUID, &mut missing_keys);
        if spicedb_key_in_runtime_secret {
            check_nonempty_key(&raw_entries, KEY_SPICEDB_PRESHARED_KEY, &mut missing_keys);
        }
        if ws.spec.s3.is_some() {
            check_nonempty_key(&raw_entries, "WADDLE_S3_ACCESS_KEY_ID", &mut missing_keys);
            check_nonempty_key(
                &raw_entries,
                "WADDLE_S3_SECRET_ACCESS_KEY",
                &mut missing_keys,
            );
        }

        if resolved_deployment_uuid.is_none() {
            resolved_deployment_uuid = raw_entries
                .get(KEY_DEPLOYMENT_UUID)
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned);
        }

        session_fp = raw_entries
            .get(KEY_SESSION_KEY)
            .and_then(|b| yolo::compute_single_key_digest(b));
        occupant_fp = raw_entries
            .get(KEY_OCCUPANT_ID_SECRET)
            .and_then(|b| yolo::compute_single_key_digest(b));
        checksum = yolo::compute_consumed_secret_digest(
            &raw_entries,
            &[
                KEY_SESSION_KEY,
                KEY_OCCUPANT_ID_SECRET,
                KEY_DEPLOYMENT_UUID,
                KEY_SPICEDB_PRESHARED_KEY,
                "WADDLE_S3_ACCESS_KEY_ID",
                "WADDLE_S3_SECRET_ACCESS_KEY",
            ],
        );
        true
    } else {
        false
    };

    let sensitive_key_rotated =
        detect_sensitive_key_rotation(live_deployment, session_fp.as_ref(), occupant_fp.as_ref());

    Ok(SecretObservationBundle {
        observation: SecretsObservation {
            source: SecretSourceKind::Yolo,
            yolo_allowed_by_operator: policy.enable_yolo_mode,
            cluster_store_allowed: true,
            external_secret_synced: None,
            external_secret_error: None,
            managed_external_secret_drifted: false,
            target_secret_exists,
            missing_keys,
            invalid_keys,
            checksum,
            sensitive_key_rotated,
        },
        target_secret_name,
        component_digests: BTreeMap::new(),
        session_key_fingerprint: session_fp,
        occupant_id_fingerprint: occupant_fp,
        eso_api_version: None,
        resolved_deployment_uuid,
    })
}

struct EsoObjectObservation {
    target_secret_name: String,
    synced: Option<bool>,
    error: Option<String>,
    managed_drifted: bool,
}

struct TargetSecretInspection {
    exists: bool,
    missing_keys: Vec<String>,
    invalid_keys: Vec<String>,
    checksum: Option<ContentDigest>,
    session_fp: Option<ContentDigest>,
    occupant_fp: Option<ContentDigest>,
}

async fn observe_eso_secrets(
    client: &Client,
    ws: &WaddleServer,
    ns: &str,
    policy: &OperatorPolicy,
    live_deployment: Option<&Deployment>,
    spicedb_key_in_runtime_secret: bool,
) -> Result<SecretObservationBundle, OperatorError> {
    let Some(eso) = &ws.spec.secrets.external_secrets else {
        return missing_eso_spec_bundle(ws, policy);
    };

    let is_managed = eso.managed.is_some();
    let source_kind = if is_managed {
        SecretSourceKind::ExternalSecretsManaged
    } else {
        SecretSourceKind::ExternalSecretsUnmanaged
    };
    let cluster_store_allowed = is_eso_cluster_store_allowed(eso, policy);
    let eso_version =
        resolve_eso_version(client, eso.api_version, policy.capabilities.eso_version).await;

    let mut es_obs =
        observe_external_secret_object(client, ws, ns, eso, eso_version, is_managed).await?;

    let secret = if is_safe_resource_name(&es_obs.target_secret_name) {
        Api::<Secret>::namespaced(client.clone(), ns)
            .get_opt(&es_obs.target_secret_name)
            .await
            .map_err(|source| OperatorError::Kube {
                context: "get ESO target runtime Secret",
                source,
            })?
    } else {
        es_obs.synced = Some(false);
        es_obs.error = Some(format!(
            "ExternalSecret spec.target.name '{}' is not a valid Kubernetes resource name",
            es_obs.target_secret_name
        ));
        None
    };

    let mut inspection = inspect_eso_target_secret(
        client,
        ws,
        ns,
        eso,
        secret.as_ref(),
        spicedb_key_in_runtime_secret,
    )
    .await?;
    check_eso_apns_secret(client, ns, eso, &mut inspection.missing_keys).await?;

    let sensitive_key_rotated = detect_sensitive_key_rotation(
        live_deployment,
        inspection.session_fp.as_ref(),
        inspection.occupant_fp.as_ref(),
    );

    Ok(SecretObservationBundle {
        observation: SecretsObservation {
            source: source_kind,
            yolo_allowed_by_operator: policy.enable_yolo_mode,
            cluster_store_allowed,
            external_secret_synced: es_obs.synced,
            external_secret_error: es_obs.error,
            managed_external_secret_drifted: es_obs.managed_drifted,
            target_secret_exists: inspection.exists,
            missing_keys: inspection.missing_keys,
            invalid_keys: inspection.invalid_keys,
            checksum: inspection.checksum,
            sensitive_key_rotated,
        },
        target_secret_name: es_obs.target_secret_name,
        component_digests: BTreeMap::new(),
        session_key_fingerprint: inspection.session_fp,
        occupant_id_fingerprint: inspection.occupant_fp,
        eso_api_version: eso_version,
        resolved_deployment_uuid: ws.spec.deployment.uuid.clone(),
    })
}

fn missing_eso_spec_bundle(
    ws: &WaddleServer,
    policy: &OperatorPolicy,
) -> Result<SecretObservationBundle, OperatorError> {
    let fallback_target = resources::managed_target_secret_name(ws)?;
    Ok(SecretObservationBundle {
        observation: SecretsObservation {
            source: SecretSourceKind::ExternalSecretsUnmanaged,
            yolo_allowed_by_operator: policy.enable_yolo_mode,
            cluster_store_allowed: true,
            external_secret_synced: Some(false),
            external_secret_error: Some(
                "secrets.mode is ExternalSecrets without spec.secrets.externalSecrets".to_owned(),
            ),
            managed_external_secret_drifted: false,
            target_secret_exists: false,
            missing_keys: vec![],
            invalid_keys: vec![],
            checksum: None,
            sensitive_key_rotated: None,
        },
        target_secret_name: fallback_target,
        component_digests: BTreeMap::new(),
        session_key_fingerprint: None,
        occupant_id_fingerprint: None,
        eso_api_version: None,
        resolved_deployment_uuid: ws.spec.deployment.uuid.clone(),
    })
}

fn is_eso_cluster_store_allowed(eso: &ExternalSecretsSpec, policy: &OperatorPolicy) -> bool {
    match &eso.managed {
        Some(managed) if managed.secret_store_ref.kind == SecretStoreKind::ClusterSecretStore => {
            policy
                .allowed_cluster_secret_stores
                .iter()
                .any(|allowed| allowed == &managed.secret_store_ref.name)
        }
        _ => true,
    }
}

async fn observe_external_secret_object(
    client: &Client,
    ws: &WaddleServer,
    ns: &str,
    eso: &ExternalSecretsSpec,
    eso_version: Option<DiscoveredEsoVersion>,
    is_managed: bool,
) -> Result<EsoObjectObservation, OperatorError> {
    let mut target_secret_name = expected_runtime_secret_name(ws)?;
    let Some(version) = eso_version else {
        return Ok(EsoObjectObservation {
            target_secret_name,
            synced: Some(false),
            error: Some(
                "External Secrets Operator CRD (external-secrets.io) is not installed".to_owned(),
            ),
            managed_drifted: is_managed,
        });
    };

    let es_name = if is_managed {
        resources::managed_external_secret_name(ws)?
    } else {
        eso.external_secret_name
            .clone()
            .unwrap_or_else(|| target_secret_name.clone())
    };
    let ar = external_crds::api_resource_eso(version);
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), ns, &ar);

    match api.get(&es_name).await {
        Ok(live_es) => {
            let view: ExternalSecretView = serde_json::from_value(live_es.data.clone())
                .map_err(|err| OperatorError::InvalidClusterState(err.to_string()))?;
            if !is_managed {
                if let Some(target_name) = view
                    .spec
                    .target
                    .as_ref()
                    .and_then(|t| t.name.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    target_secret_name = target_name.to_owned();
                }
            }
            let (synced, error) = external_crds::evaluate_eso_status(&view);
            let managed_drifted = if is_managed {
                let desired_es = resources::build_managed_external_secret(ws, version)?;
                resources::managed_external_secret_drifted(&live_es, &desired_es)
            } else {
                false
            };
            Ok(EsoObjectObservation {
                target_secret_name,
                synced,
                error,
                managed_drifted,
            })
        }
        Err(kube::Error::Api(status)) if status.code == 404 => {
            let error = if is_missing_crd_status(&status.message)
                || !has_group_version_live(client, "external-secrets.io", version.version_str())
                    .await
            {
                format!(
                    "ExternalSecret CRD {} is not installed",
                    version.api_version()
                )
            } else {
                format!("ExternalSecret {es_name} not found in namespace {ns}")
            };
            Ok(EsoObjectObservation {
                target_secret_name,
                synced: Some(false),
                error: Some(error),
                managed_drifted: is_managed,
            })
        }
        Err(source) => Err(OperatorError::Kube {
            context: "get ExternalSecret",
            source,
        }),
    }
}

async fn inspect_eso_target_secret(
    client: &Client,
    ws: &WaddleServer,
    ns: &str,
    eso: &ExternalSecretsSpec,
    secret: Option<&Secret>,
    spicedb_key_in_runtime_secret: bool,
) -> Result<TargetSecretInspection, OperatorError> {
    let Some(sec) = secret else {
        return Ok(TargetSecretInspection {
            exists: false,
            missing_keys: Vec::new(),
            invalid_keys: Vec::new(),
            checksum: None,
            session_fp: None,
            occupant_fp: None,
        });
    };

    let mut missing_keys = Vec::new();
    let mut invalid_keys = Vec::new();
    let raw_entries = to_raw_byte_map(sec.data.as_ref());
    check_min_length_key(
        &raw_entries,
        KEY_SESSION_KEY,
        32,
        &mut missing_keys,
        &mut invalid_keys,
    );
    check_min_length_key(
        &raw_entries,
        KEY_OCCUPANT_ID_SECRET,
        32,
        &mut missing_keys,
        &mut invalid_keys,
    );
    if spicedb_key_in_runtime_secret {
        check_nonempty_key(&raw_entries, KEY_SPICEDB_PRESHARED_KEY, &mut missing_keys);
    }
    if ws.spec.s3.is_some() {
        check_nonempty_key(&raw_entries, "WADDLE_S3_ACCESS_KEY_ID", &mut missing_keys);
        check_nonempty_key(
            &raw_entries,
            "WADDLE_S3_SECRET_ACCESS_KEY",
            &mut missing_keys,
        );
    }

    let mut digest_keys: Vec<String> = vec![
        KEY_SESSION_KEY.to_owned(),
        KEY_OCCUPANT_ID_SECRET.to_owned(),
        KEY_SPICEDB_PRESHARED_KEY.to_owned(),
        "WADDLE_S3_ACCESS_KEY_ID".to_owned(),
        "WADDLE_S3_SECRET_ACCESS_KEY".to_owned(),
        "WADDLE_AUTH_PROVIDERS_JSON".to_owned(),
    ];
    if eso.clustering_keypool_secret_name.is_none() {
        digest_keys.push("WADDLE_CLUSTERING_KEYPAIR_POOL".to_owned());
    }
    if let Some(managed) = &eso.managed {
        for extra in &managed.remote_refs.extra_data {
            digest_keys.push(extra.secret_key.clone());
        }
    }
    let key_refs: Vec<&str> = digest_keys.iter().map(String::as_str).collect();
    let checksum = yolo::compute_consumed_secret_digest(&raw_entries, &key_refs);
    let session_fp = raw_entries
        .get(KEY_SESSION_KEY)
        .and_then(|b| yolo::compute_single_key_digest(b));
    let occupant_fp = raw_entries
        .get(KEY_OCCUPANT_ID_SECRET)
        .and_then(|b| yolo::compute_single_key_digest(b));

    if ws.spec.clustering.enabled {
        validate_eso_clustering_keypool(
            client,
            ws,
            ns,
            eso,
            &raw_entries,
            &mut missing_keys,
            &mut invalid_keys,
        )
        .await?;
    }

    Ok(TargetSecretInspection {
        exists: true,
        missing_keys,
        invalid_keys,
        checksum,
        session_fp,
        occupant_fp,
    })
}

async fn validate_eso_clustering_keypool(
    client: &Client,
    ws: &WaddleServer,
    ns: &str,
    eso: &ExternalSecretsSpec,
    raw_entries: &BTreeMap<String, Vec<u8>>,
    missing_keys: &mut Vec<String>,
    invalid_keys: &mut Vec<String>,
) -> Result<(), OperatorError> {
    let max_surge = match ws.spec.update_strategy {
        UpdateStrategyKind::RollingUpdate => 1,
        UpdateStrategyKind::Recreate => 0,
    };
    let min_required = ws
        .spec
        .effective_replicas()
        .saturating_add(max_surge)
        .saturating_add(ws.spec.clustering.keypair_headroom);
    let required_count = usize::try_from(min_required.max(1)).unwrap_or(1);

    let Some(pool_secret_name) = &eso.clustering_keypool_secret_name else {
        validate_keypool_in_map(raw_entries, required_count, missing_keys, invalid_keys);
        return Ok(());
    };

    let pool_secret = Api::<Secret>::namespaced(client.clone(), ns)
        .get_opt(pool_secret_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get clustering keypool Secret",
            source,
        })?;
    match pool_secret.and_then(|s| s.data) {
        Some(pool_data) => validate_keypool_in_map(
            &to_raw_byte_map(Some(&pool_data)),
            required_count,
            missing_keys,
            invalid_keys,
        ),
        None => missing_keys.push(format!("{pool_secret_name}/WADDLE_CLUSTERING_KEYPAIR_POOL")),
    }
    Ok(())
}

async fn check_eso_apns_secret(
    client: &Client,
    ns: &str,
    eso: &ExternalSecretsSpec,
    missing_keys: &mut Vec<String>,
) -> Result<(), OperatorError> {
    let Some(apns) = &eso.apns_secret_ref else {
        return Ok(());
    };
    let apns_sec = Api::<Secret>::namespaced(client.clone(), ns)
        .get_opt(&apns.secret_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get APNs Secret",
            source,
        })?;
    match apns_sec.and_then(|s| s.data) {
        Some(data) if has_nonempty_key(&data, &apns.secret_key) => {}
        _ => missing_keys.push(format!("{}/{}", apns.secret_name, apns.secret_key)),
    }
    Ok(())
}

async fn observe_workload(
    client: &Client,
    ns: &str,
    ws: &WaddleServer,
    validated: Option<&ValidatedSpec>,
    bindings: Option<&ResolvedRuntimeBindings>,
    live_deployment: Option<&Deployment>,
) -> Result<WorkloadObservation, OperatorError> {
    let Some(dep) = live_deployment else {
        return Ok(WorkloadObservation::NotCreated);
    };

    let drift_detected = if let (Some(val), Some(b)) = (validated, bindings) {
        workload_has_drifted(client, ns, ws, val, b, dep).await?
    } else {
        false
    };

    let desired_replicas = ws.spec.replicas.unwrap_or_else(|| {
        dep.spec
            .as_ref()
            .and_then(|s| s.replicas)
            .unwrap_or_else(|| ws.spec.effective_replicas())
    });
    let dep_gen = dep.metadata.generation.unwrap_or(0);
    let Some(status) = dep.status.as_ref() else {
        return Ok(WorkloadObservation::RolloutInProgress {
            desired_replicas,
            updated_replicas: 0,
            ready_replicas: 0,
            available_replicas: 0,
            drift_detected,
        });
    };

    let obs_gen = status.observed_generation.unwrap_or(0);
    let updated_replicas = status.updated_replicas.unwrap_or(0);
    let ready_replicas = status.ready_replicas.unwrap_or(0);
    let available_replicas = status.available_replicas.unwrap_or(0);
    let conditions = status.conditions.as_deref().unwrap_or(&[]);

    if obs_gen < dep_gen {
        return Ok(WorkloadObservation::RolloutInProgress {
            desired_replicas,
            updated_replicas,
            ready_replicas,
            available_replicas,
            drift_detected,
        });
    }

    if let Some(failed) = conditions.iter().find(|cond| {
        cond.type_ == "Progressing"
            && cond.status == "False"
            && cond.reason.as_deref() == Some("ProgressDeadlineExceeded")
    }) {
        return Ok(WorkloadObservation::ProgressDeadlineExceeded {
            desired_replicas,
            ready_replicas,
            message: failed
                .message
                .clone()
                .unwrap_or_else(|| "Deployment exceeded progress deadline".to_owned()),
            drift_detected,
        });
    }

    let available_false = conditions
        .iter()
        .any(|cond| cond.type_ == "Available" && cond.status == "False");
    if obs_gen >= dep_gen
        && updated_replicas >= desired_replicas
        && ready_replicas >= desired_replicas
        && available_replicas >= desired_replicas
        && !available_false
    {
        return Ok(WorkloadObservation::Available {
            desired_replicas,
            updated_replicas,
            ready_replicas,
            available_replicas,
            drift_detected,
        });
    }

    let progressing = conditions.iter().find(|cond| cond.type_ == "Progressing");
    let rollout_converged_earlier = progressing.is_some_and(|cond| {
        cond.status == "True" && cond.reason.as_deref() == Some("NewReplicaSetAvailable")
    });
    if obs_gen < dep_gen || (updated_replicas < desired_replicas && !rollout_converged_earlier) {
        return Ok(WorkloadObservation::RolloutInProgress {
            desired_replicas,
            updated_replicas,
            ready_replicas,
            available_replicas,
            drift_detected,
        });
    }

    let actively_progressing = progressing.is_some_and(|cond| cond.status == "True");
    if actively_progressing && !rollout_converged_earlier {
        return Ok(WorkloadObservation::RolloutInProgress {
            desired_replicas,
            updated_replicas,
            ready_replicas,
            available_replicas,
            drift_detected,
        });
    }

    Ok(WorkloadObservation::DegradedReplicas {
        desired_replicas,
        ready_replicas,
        available_replicas,
        drift_detected,
    })
}

async fn resolve_eso_version(
    client: &Client,
    preference: EsoApiVersion,
    discovered: Option<DiscoveredEsoVersion>,
) -> Option<DiscoveredEsoVersion> {
    match preference {
        EsoApiVersion::V1 => Some(DiscoveredEsoVersion::V1),
        EsoApiVersion::V1Beta1 => Some(DiscoveredEsoVersion::V1Beta1),
        EsoApiVersion::Auto => {
            if discovered.is_some() {
                return discovered;
            }
            if has_group_version_live(client, "external-secrets.io", "v1").await {
                Some(DiscoveredEsoVersion::V1)
            } else if has_group_version_live(client, "external-secrets.io", "v1beta1").await {
                Some(DiscoveredEsoVersion::V1Beta1)
            } else {
                None
            }
        }
    }
}

async fn check_service_endpoints(
    client: &Client,
    ns: &str,
    svc_name: &str,
    label: &str,
) -> Result<Option<DependencyHealth>, OperatorError> {
    let svc = Api::<Service>::namespaced(client.clone(), ns)
        .get_opt(svc_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get dependency Service",
            source,
        })?;
    if svc.is_none() {
        return Ok(Some(DependencyHealth::Missing {
            reason: format!("{label}ServiceMissing"),
            detail: format!("{label} Service {svc_name} not found in namespace {ns}"),
        }));
    }
    if count_ready_endpoints(client, ns, svc_name).await? == 0 {
        return Ok(Some(DependencyHealth::NotReady {
            reason: format!("{label}EndpointsNotReady"),
            detail: format!("{label} Service {svc_name} has 0 ready endpoints"),
        }));
    }
    Ok(None)
}

async fn count_ready_endpoints(
    client: &Client,
    ns: &str,
    service_name: &str,
) -> Result<usize, OperatorError> {
    let slices = Api::<EndpointSlice>::namespaced(client.clone(), ns)
        .list(&ListParams::default().labels(&format!("kubernetes.io/service-name={service_name}")))
        .await
        .map_err(|source| OperatorError::Kube {
            context: "list EndpointSlices",
            source,
        })?;

    let mut ready_count = 0usize;
    for slice in slices.items {
        for ep in slice.endpoints {
            let is_ready = ep
                .conditions
                .as_ref()
                .and_then(|c| c.ready)
                .unwrap_or(false);
            if is_ready && !ep.addresses.is_empty() {
                ready_count += 1;
            }
        }
    }
    Ok(ready_count)
}

async fn verify_secret_key_present(
    client: &Client,
    ns: &str,
    secret_name: &str,
    key: &str,
    label: &str,
) -> Result<(Option<DependencyHealth>, Option<ContentDigest>), OperatorError> {
    let sec = Api::<Secret>::namespaced(client.clone(), ns)
        .get_opt(secret_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get dependency Secret",
            source,
        })?;
    let Some(secret) = sec else {
        return Ok((
            Some(DependencyHealth::NotReady {
                reason: format!("{label}SecretMissing"),
                detail: format!("{label} Secret {secret_name} not found in namespace {ns}"),
            }),
            None,
        ));
    };
    let data = secret.data.unwrap_or_default();
    if !has_nonempty_key(&data, key) {
        return Ok((
            Some(DependencyHealth::NotReady {
                reason: format!("{label}SecretKeyMissing"),
                detail: format!("{label} Secret {secret_name} lacks non-empty key '{key}'"),
            }),
            None,
        ));
    }
    let digest = data
        .get(key)
        .and_then(|val| yolo::compute_single_key_digest(&val.0));
    Ok((None, digest))
}

async fn collect_companion_secret_digests(
    client: &Client,
    ns: &str,
    ws: &WaddleServer,
    parts: &mut BTreeMap<String, ContentDigest>,
) -> Result<(), OperatorError> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), ns);
    collect_xmpp_tls_digest(&secrets, ws, parts).await?;
    if let Some(eso) = &ws.spec.secrets.external_secrets {
        collect_eso_companion_digests(&secrets, ws, eso, parts).await?;
    }
    collect_extra_secret_digests(&secrets, ws, parts).await?;
    Ok(())
}

async fn insert_secret_keys_digest(
    secrets: &Api<Secret>,
    secret_name: &str,
    context: &'static str,
    keys: &[&str],
    part_key: &str,
    parts: &mut BTreeMap<String, ContentDigest>,
) -> Result<(), OperatorError> {
    if !is_safe_resource_name(secret_name) {
        return Ok(());
    }
    let Some(sec) = secrets
        .get_opt(secret_name)
        .await
        .map_err(|source| OperatorError::Kube { context, source })?
    else {
        return Ok(());
    };
    let raw = to_raw_byte_map(sec.data.as_ref());
    if let Some(digest) = yolo::compute_consumed_secret_digest(&raw, keys) {
        parts.insert(part_key.to_owned(), digest);
    }
    Ok(())
}

async fn collect_xmpp_tls_digest(
    secrets: &Api<Secret>,
    ws: &WaddleServer,
    parts: &mut BTreeMap<String, ContentDigest>,
) -> Result<(), OperatorError> {
    if !ws.spec.xmpp.enabled {
        return Ok(());
    }
    let Some(tls_secret) = ws.spec.xmpp.tls_secret_name.as_deref() else {
        return Ok(());
    };
    insert_secret_keys_digest(
        secrets,
        tls_secret,
        "get XMPP TLS Secret for checksum",
        &["tls.crt", "tls.key"],
        "xmpp_tls",
        parts,
    )
    .await
}

async fn collect_eso_companion_digests(
    secrets: &Api<Secret>,
    ws: &WaddleServer,
    eso: &ExternalSecretsSpec,
    parts: &mut BTreeMap<String, ContentDigest>,
) -> Result<(), OperatorError> {
    if let Some(apns) = &eso.apns_secret_ref {
        insert_secret_keys_digest(
            secrets,
            &apns.secret_name,
            "get APNs Secret for checksum",
            &[&apns.secret_key],
            "apns",
            parts,
        )
        .await?;
    }
    if ws.spec.clustering.enabled {
        if let Some(pool_name) = eso.clustering_keypool_secret_name.as_deref() {
            insert_secret_keys_digest(
                secrets,
                pool_name,
                "get keypool Secret for checksum",
                &["WADDLE_CLUSTERING_KEYPAIR_POOL"],
                "keypool",
                parts,
            )
            .await?;
        }
    }
    Ok(())
}

async fn collect_extra_secret_digests(
    secrets: &Api<Secret>,
    ws: &WaddleServer,
    parts: &mut BTreeMap<String, ContentDigest>,
) -> Result<(), OperatorError> {
    for extra_name in collect_extra_secret_refs(ws) {
        if !is_safe_resource_name(&extra_name) {
            continue;
        }
        let Some(sec) =
            secrets
                .get_opt(&extra_name)
                .await
                .map_err(|source| OperatorError::Kube {
                    context: "get extra envFrom Secret for checksum",
                    source,
                })?
        else {
            continue;
        };
        let raw = to_raw_byte_map(sec.data.as_ref());
        let key_refs: Vec<&str> = raw.keys().map(String::as_str).collect();
        if let Some(d) = yolo::compute_consumed_secret_digest(&raw, &key_refs) {
            parts.insert(format!("extra:{extra_name}"), d);
        }
    }
    Ok(())
}

async fn workload_has_drifted(
    client: &Client,
    ns: &str,
    ws: &WaddleServer,
    validated: &ValidatedSpec,
    bindings: &ResolvedRuntimeBindings,
    live_dep: &Deployment,
) -> Result<bool, OperatorError> {
    let name = resources::workload_name(ws)?;
    let owner = resources::owner_reference(ws)?;

    let desired_cm = resources::build_config_map(ws, bindings)?;
    let cm_digest = resources::config_map_digest(&desired_cm)?;
    let desired_dep = resources::build_deployment(ws, validated, bindings, &cm_digest)?;

    if deployment_drifted(ws, live_dep, &desired_dep) {
        return Ok(true);
    }

    let sa_exists = Api::<ServiceAccount>::namespaced(client.clone(), ns)
        .get_opt(name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get ServiceAccount for drift detection",
            source,
        })?
        .is_some();
    if !sa_exists {
        return Ok(true);
    }

    let cm_name = resources::config_map_name(ws)?;
    let Some(live_cm) = Api::<ConfigMap>::namespaced(client.clone(), ns)
        .get_opt(&cm_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get ConfigMap for drift detection",
            source,
        })?
    else {
        return Ok(true);
    };
    if config_map_drifted(&live_cm, &desired_cm) {
        return Ok(true);
    }

    let svc_api: Api<Service> = Api::namespaced(client.clone(), ns);
    let desired_http_svc = resources::build_http_service(ws)?;
    let Some(live_http_svc) =
        svc_api
            .get_opt(name)
            .await
            .map_err(|source| OperatorError::Kube {
                context: "get HTTP Service for drift detection",
                source,
            })?
    else {
        return Ok(true);
    };
    if service_drifted(&live_http_svc, &desired_http_svc) {
        return Ok(true);
    }

    if resources::build_pvc(ws)?.is_some() {
        let pvc_name = resources::pvc_name(ws)?;
        let pvc_exists = Api::<PersistentVolumeClaim>::namespaced(client.clone(), ns)
            .get_opt(&pvc_name)
            .await
            .map_err(|source| OperatorError::Kube {
                context: "get PersistentVolumeClaim for drift detection",
                source,
            })?
            .is_some();
        if !pvc_exists {
            return Ok(true);
        }
    }

    let headless_name = resources::headless_service_name(ws)?;
    let live_headless =
        svc_api
            .get_opt(&headless_name)
            .await
            .map_err(|source| OperatorError::Kube {
                context: "get headless Service for drift detection",
                source,
            })?;
    match (resources::build_headless_service(ws)?, live_headless) {
        (Some(_), None) => return Ok(true),
        (Some(desired), Some(live)) if service_drifted(&live, &desired) => return Ok(true),
        (None, Some(live)) if owned_by_uid(&live, &owner.uid) => return Ok(true),
        _ => {}
    }

    let s2s_name = resources::s2s_service_name(ws)?;
    let live_s2s = svc_api
        .get_opt(&s2s_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get S2S Service for drift detection",
            source,
        })?;
    match (resources::build_s2s_service(ws)?, live_s2s) {
        (Some(_), None) => return Ok(true),
        (Some(desired), Some(live)) if service_drifted(&live, &desired) => return Ok(true),
        (None, Some(live)) if owned_by_uid(&live, &owner.uid) => return Ok(true),
        _ => {}
    }

    let live_pdb = Api::<PodDisruptionBudget>::namespaced(client.clone(), ns)
        .get_opt(name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get PodDisruptionBudget for drift detection",
            source,
        })?;
    match (resources::build_pdb(ws)?, live_pdb) {
        (Some(_), None) => return Ok(true),
        (Some(desired), Some(live)) if pdb_drifted(&live, &desired) => return Ok(true),
        (None, Some(live)) if owned_by_uid(&live, &owner.uid) => return Ok(true),
        _ => {}
    }

    let np_name = resources::network_policy_name(ws)?;
    let live_np = Api::<NetworkPolicy>::namespaced(client.clone(), ns)
        .get_opt(&np_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get NetworkPolicy for drift detection",
            source,
        })?;
    match (resources::build_network_policy(ws)?, live_np) {
        (Some(_), None) => return Ok(true),
        (Some(desired), Some(live)) if network_policy_drifted(&live, &desired) => return Ok(true),
        (None, Some(live)) if owned_by_uid(&live, &owner.uid) => return Ok(true),
        _ => {}
    }

    Ok(false)
}

fn owned_by_uid<K: ResourceExt>(obj: &K, uid: &str) -> bool {
    obj.owner_references().iter().any(|owner| owner.uid == uid)
}

fn config_map_drifted(live: &ConfigMap, desired: &ConfigMap) -> bool {
    !map_subset_matches(live.data.as_ref(), desired.data.as_ref())
        || !map_subset_matches(live.binary_data.as_ref(), desired.binary_data.as_ref())
}

fn map_subset_matches<V: PartialEq>(
    live: Option<&BTreeMap<String, V>>,
    desired: Option<&BTreeMap<String, V>>,
) -> bool {
    let Some(d) = desired else {
        return true;
    };
    let Some(l) = live else {
        return d.is_empty();
    };
    d.iter().all(|(k, v)| l.get(k) == Some(v))
}

fn deployment_drifted(ws: &WaddleServer, live: &Deployment, desired: &Deployment) -> bool {
    let desired_hash = desired
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(ANNOTATION_SPEC_HASH));
    let live_hash = live
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(ANNOTATION_SPEC_HASH));
    if desired_hash != live_hash {
        return true;
    }

    let (Some(live_spec), Some(desired_spec)) = (&live.spec, &desired.spec) else {
        return true;
    };
    if ws.spec.replicas.is_some() && live_spec.replicas != desired_spec.replicas {
        return true;
    }

    if deployment_strategy_drifted(live_spec.strategy.as_ref(), desired_spec.strategy.as_ref()) {
        return true;
    }

    pod_template_drifted(&live_spec.template, &desired_spec.template)
}

fn deployment_strategy_drifted(
    live: Option<&k8s_openapi::api::apps::v1::DeploymentStrategy>,
    desired: Option<&k8s_openapi::api::apps::v1::DeploymentStrategy>,
) -> bool {
    let Some(ds) = desired else {
        return false;
    };
    let Some(ls) = live else {
        return true;
    };
    if ds.type_.is_some() && ls.type_ != ds.type_ {
        return true;
    }
    rolling_update_drifted(ls.rolling_update.as_ref(), ds.rolling_update.as_ref())
}

fn rolling_update_drifted(
    live: Option<&RollingUpdateDeployment>,
    desired: Option<&RollingUpdateDeployment>,
) -> bool {
    let Some(d_ru) = desired else {
        return false;
    };
    let Some(l_ru) = live else {
        return true;
    };
    (d_ru.max_surge.is_some() && l_ru.max_surge != d_ru.max_surge)
        || (d_ru.max_unavailable.is_some() && l_ru.max_unavailable != d_ru.max_unavailable)
}

fn pod_template_drifted(live_tpl: &PodTemplateSpec, desired_tpl: &PodTemplateSpec) -> bool {
    let live_meta = live_tpl.metadata.as_ref();
    let desired_meta = desired_tpl.metadata.as_ref();
    if !map_subset_matches(
        live_meta.and_then(|m| m.labels.as_ref()),
        desired_meta.and_then(|m| m.labels.as_ref()),
    ) {
        return true;
    }
    if !map_subset_matches(
        live_meta.and_then(|m| m.annotations.as_ref()),
        desired_meta.and_then(|m| m.annotations.as_ref()),
    ) {
        return true;
    }

    let (Some(l_ps), Some(d_ps)) = (&live_tpl.spec, &desired_tpl.spec) else {
        return true;
    };
    if d_ps.service_account_name.is_some() && l_ps.service_account_name != d_ps.service_account_name
    {
        return true;
    }
    if d_ps.termination_grace_period_seconds.is_some()
        && l_ps.termination_grace_period_seconds != d_ps.termination_grace_period_seconds
    {
        return true;
    }
    if pod_security_context_drifted(
        l_ps.security_context.as_ref(),
        d_ps.security_context.as_ref(),
    ) {
        return true;
    }
    if !map_subset_matches(l_ps.node_selector.as_ref(), d_ps.node_selector.as_ref()) {
        return true;
    }
    if d_ps.affinity.is_some() && l_ps.affinity != d_ps.affinity {
        return true;
    }
    if let Some(d_ips) = &d_ps.image_pull_secrets {
        let l_ips = l_ps.image_pull_secrets.as_deref().unwrap_or(&[]);
        if d_ips
            .iter()
            .any(|d_ref| !l_ips.iter().any(|l_ref| l_ref.name == d_ref.name))
        {
            return true;
        }
    }
    if volumes_drifted(
        l_ps.volumes.as_deref().unwrap_or(&[]),
        d_ps.volumes.as_deref().unwrap_or(&[]),
    ) {
        return true;
    }

    d_ps.containers.iter().any(|d_c| {
        let Some(l_c) = l_ps.containers.iter().find(|c| c.name == d_c.name) else {
            return true;
        };
        container_drifted(l_c, d_c)
    })
}

fn pod_security_context_drifted(
    live: Option<&PodSecurityContext>,
    desired: Option<&PodSecurityContext>,
) -> bool {
    let Some(d_psc) = desired else {
        return false;
    };
    let Some(l_psc) = live else {
        return true;
    };
    (d_psc.fs_group.is_some() && l_psc.fs_group != d_psc.fs_group)
        || (d_psc.run_as_non_root.is_some() && l_psc.run_as_non_root != d_psc.run_as_non_root)
        || (d_psc.run_as_user.is_some() && l_psc.run_as_user != d_psc.run_as_user)
        || (d_psc.run_as_group.is_some() && l_psc.run_as_group != d_psc.run_as_group)
}

fn volumes_drifted(live: &[Volume], desired: &[Volume]) -> bool {
    desired.iter().any(|d_vol| {
        let Some(l_vol) = live.iter().find(|v| v.name == d_vol.name) else {
            return true;
        };
        single_volume_drifted(l_vol, d_vol)
    })
}

fn single_volume_drifted(l_vol: &Volume, d_vol: &Volume) -> bool {
    if let Some(d_sec) = &d_vol.secret {
        let Some(l_sec) = &l_vol.secret else {
            return true;
        };
        if (d_sec.secret_name.is_some() && l_sec.secret_name != d_sec.secret_name)
            || (d_sec.default_mode.is_some() && l_sec.default_mode != d_sec.default_mode)
            || (d_sec.items.is_some() && l_sec.items != d_sec.items)
        {
            return true;
        }
    }
    if let Some(d_cm) = &d_vol.config_map {
        let Some(l_cm) = &l_vol.config_map else {
            return true;
        };
        if l_cm.name != d_cm.name
            || (d_cm.default_mode.is_some() && l_cm.default_mode != d_cm.default_mode)
            || (d_cm.items.is_some() && l_cm.items != d_cm.items)
        {
            return true;
        }
    }
    if let Some(d_pvc) = &d_vol.persistent_volume_claim {
        let Some(l_pvc) = &l_vol.persistent_volume_claim else {
            return true;
        };
        if l_pvc.claim_name != d_pvc.claim_name
            || (d_pvc.read_only.is_some() && l_pvc.read_only != d_pvc.read_only)
        {
            return true;
        }
    }
    d_vol.empty_dir.is_some() && l_vol.empty_dir.is_none()
}

fn container_drifted(live: &Container, desired: &Container) -> bool {
    if live.image != desired.image
        || (desired.image_pull_policy.is_some()
            && live.image_pull_policy != desired.image_pull_policy)
        || container_security_context_drifted(
            live.security_context.as_ref(),
            desired.security_context.as_ref(),
        )
        || pre_stop_drifted(live.lifecycle.as_ref(), desired.lifecycle.as_ref())
        || ports_drifted(
            live.ports.as_deref().unwrap_or(&[]),
            desired.ports.as_deref().unwrap_or(&[]),
        )
        || env_from_drifted(
            live.env_from.as_deref().unwrap_or(&[]),
            desired.env_from.as_deref().unwrap_or(&[]),
        )
        || env_vars_drifted(
            live.env.as_deref().unwrap_or(&[]),
            desired.env.as_deref().unwrap_or(&[]),
        )
        || probe_drifted(
            live.liveness_probe.as_ref(),
            desired.liveness_probe.as_ref(),
        )
        || probe_drifted(
            live.readiness_probe.as_ref(),
            desired.readiness_probe.as_ref(),
        )
        || resources_drifted(live.resources.as_ref(), desired.resources.as_ref())
        || volume_mounts_drifted(
            live.volume_mounts.as_deref().unwrap_or(&[]),
            desired.volume_mounts.as_deref().unwrap_or(&[]),
        )
    {
        return true;
    }

    false
}

fn container_security_context_drifted(
    live: Option<&SecurityContext>,
    desired: Option<&SecurityContext>,
) -> bool {
    let Some(d_sc) = desired else {
        return false;
    };
    let Some(l_sc) = live else {
        return true;
    };
    if (d_sc.run_as_non_root.is_some() && l_sc.run_as_non_root != d_sc.run_as_non_root)
        || (d_sc.run_as_user.is_some() && l_sc.run_as_user != d_sc.run_as_user)
        || (d_sc.run_as_group.is_some() && l_sc.run_as_group != d_sc.run_as_group)
        || (d_sc.allow_privilege_escalation.is_some()
            && l_sc.allow_privilege_escalation != d_sc.allow_privilege_escalation)
        || (d_sc.read_only_root_filesystem.is_some()
            && l_sc.read_only_root_filesystem != d_sc.read_only_root_filesystem)
    {
        return true;
    }

    if let Some(d_drop) = d_sc.capabilities.as_ref().and_then(|c| c.drop.as_ref()) {
        let Some(l_drop) = l_sc.capabilities.as_ref().and_then(|c| c.drop.as_ref()) else {
            return true;
        };
        if d_drop.iter().any(|cap| !l_drop.contains(cap)) {
            return true;
        }
    }

    false
}

fn pre_stop_drifted(
    live: Option<&k8s_openapi::api::core::v1::Lifecycle>,
    desired: Option<&k8s_openapi::api::core::v1::Lifecycle>,
) -> bool {
    let Some(d_pre) = desired.and_then(|l| l.pre_stop.as_ref()) else {
        return false;
    };
    let Some(l_pre) = live.and_then(|l| l.pre_stop.as_ref()) else {
        return true;
    };
    (d_pre.sleep.is_some() && l_pre.sleep != d_pre.sleep)
        || (d_pre.exec.is_some() && l_pre.exec != d_pre.exec)
        || (d_pre.http_get.is_some() && l_pre.http_get != d_pre.http_get)
}

fn ports_drifted(
    live: &[k8s_openapi::api::core::v1::ContainerPort],
    desired: &[k8s_openapi::api::core::v1::ContainerPort],
) -> bool {
    desired.iter().any(|d_p| {
        !live.iter().any(|l_p| {
            l_p.name == d_p.name
                && l_p.container_port == d_p.container_port
                && (d_p.protocol.is_none() || l_p.protocol == d_p.protocol)
        })
    })
}

fn env_from_drifted(
    live: &[k8s_openapi::api::core::v1::EnvFromSource],
    desired: &[k8s_openapi::api::core::v1::EnvFromSource],
) -> bool {
    desired.iter().any(|d_ef| {
        !live.iter().any(|l_ef| {
            let prefix_ok =
                l_ef.prefix.as_deref().unwrap_or("") == d_ef.prefix.as_deref().unwrap_or("");
            let cm_ok = match (&l_ef.config_map_ref, &d_ef.config_map_ref) {
                (_, None) => true,
                (None, Some(_)) => false,
                (Some(lr), Some(dr)) => {
                    lr.name == dr.name && (dr.optional.is_none() || lr.optional == dr.optional)
                }
            };
            let sec_ok = match (&l_ef.secret_ref, &d_ef.secret_ref) {
                (_, None) => true,
                (None, Some(_)) => false,
                (Some(lr), Some(dr)) => {
                    lr.name == dr.name && (dr.optional.is_none() || lr.optional == dr.optional)
                }
            };
            prefix_ok
                && cm_ok
                && sec_ok
                && (d_ef.config_map_ref.is_some() || l_ef.config_map_ref.is_none())
                && (d_ef.secret_ref.is_some() || l_ef.secret_ref.is_none())
        })
    })
}

fn env_vars_drifted(live: &[EnvVar], desired: &[EnvVar]) -> bool {
    desired.iter().any(|d_e| {
        let Some(l_e) = live.iter().find(|e| e.name == d_e.name) else {
            return true;
        };
        if d_e.value_from.is_none() {
            l_e.value.as_deref().unwrap_or("") != d_e.value.as_deref().unwrap_or("")
                || l_e.value_from.is_some()
        } else {
            l_e.value.is_some()
                || env_value_from_drifted(l_e.value_from.as_ref(), d_e.value_from.as_ref())
        }
    })
}

fn env_value_from_drifted(live: Option<&EnvVarSource>, desired: Option<&EnvVarSource>) -> bool {
    let Some(d_vf) = desired else {
        return live.is_some();
    };
    let Some(l_vf) = live else {
        return true;
    };
    let skr_ok = match (&l_vf.secret_key_ref, &d_vf.secret_key_ref) {
        (None, None) => true,
        (Some(lr), Some(dr)) => {
            lr.name == dr.name
                && lr.key == dr.key
                && (dr.optional.is_none() || lr.optional == dr.optional)
        }
        _ => false,
    };
    let cmkr_ok = match (&l_vf.config_map_key_ref, &d_vf.config_map_key_ref) {
        (None, None) => true,
        (Some(lr), Some(dr)) => {
            lr.name == dr.name
                && lr.key == dr.key
                && (dr.optional.is_none() || lr.optional == dr.optional)
        }
        _ => false,
    };
    let fr_ok = match (&l_vf.field_ref, &d_vf.field_ref) {
        (None, None) => true,
        (Some(lr), Some(dr)) => lr.field_path == dr.field_path,
        _ => false,
    };
    let rfr_ok = match (&l_vf.resource_field_ref, &d_vf.resource_field_ref) {
        (None, None) => true,
        (Some(lr), Some(dr)) => {
            lr.resource == dr.resource && lr.container_name == dr.container_name
        }
        _ => false,
    };
    !(skr_ok && cmkr_ok && fr_ok && rfr_ok)
}

fn probe_drifted(live: Option<&Probe>, desired: Option<&Probe>) -> bool {
    let Some(d_p) = desired else {
        return false;
    };
    let Some(l_p) = live else {
        return true;
    };
    if (d_p.initial_delay_seconds.is_some()
        && l_p.initial_delay_seconds != d_p.initial_delay_seconds)
        || (d_p.period_seconds.is_some() && l_p.period_seconds != d_p.period_seconds)
        || (d_p.timeout_seconds.is_some() && l_p.timeout_seconds != d_p.timeout_seconds)
        || (d_p.success_threshold.is_some() && l_p.success_threshold != d_p.success_threshold)
        || (d_p.failure_threshold.is_some() && l_p.failure_threshold != d_p.failure_threshold)
    {
        return true;
    }
    if let Some(d_http) = &d_p.http_get {
        let Some(l_http) = &l_p.http_get else {
            return true;
        };
        if l_http.path != d_http.path
            || l_http.port != d_http.port
            || (d_http.scheme.is_some() && l_http.scheme != d_http.scheme)
        {
            return true;
        }
    }
    if let Some(d_tcp) = &d_p.tcp_socket {
        let Some(l_tcp) = &l_p.tcp_socket else {
            return true;
        };
        if l_tcp.port != d_tcp.port {
            return true;
        }
    }
    false
}

fn resources_drifted(
    live: Option<&ResourceRequirements>,
    desired: Option<&ResourceRequirements>,
) -> bool {
    let Some(d_res) = desired else {
        return false;
    };
    let Some(l_res) = live else {
        return true;
    };
    !resource_map_matches(l_res.requests.as_ref(), d_res.requests.as_ref())
        || !resource_map_matches(l_res.limits.as_ref(), d_res.limits.as_ref())
}

fn resource_map_matches(
    live: Option<&BTreeMap<String, Quantity>>,
    desired: Option<&BTreeMap<String, Quantity>>,
) -> bool {
    let Some(d) = desired else {
        return true;
    };
    let Some(l) = live else {
        return d.is_empty();
    };
    d.iter()
        .all(|(k, d_q)| l.get(k).is_some_and(|l_q| quantities_equal(l_q, d_q)))
}

fn quantities_equal(live: &Quantity, desired: &Quantity) -> bool {
    if live.0 == desired.0 {
        return true;
    }
    match (
        parse_quantity_nano(&live.0),
        parse_quantity_nano(&desired.0),
    ) {
        (Some(l), Some(d)) => l == d,
        _ => false,
    }
}

fn parse_quantity_nano(raw: &str) -> Option<i128> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, unsigned) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = s.strip_prefix('+') {
        (false, rest)
    } else {
        (false, s)
    };

    let (num_part, binary_mul, suffix_exp10) = split_quantity_suffix(unsigned)?;
    let (mantissa, sci_exp10) = match num_part.rsplit_once(['e', 'E']) {
        Some((m, exp_str)) => (m, exp_str.parse::<i32>().ok()?),
        None => (num_part, 0),
    };
    let (digits, frac_digits) = parse_decimal_digits(mantissa)?;

    let total_exp10 = 9i32
        .checked_sub(frac_digits)?
        .checked_add(sci_exp10)?
        .checked_add(suffix_exp10)?;
    let scaled = digits.checked_mul(binary_mul)?;
    let nano = if total_exp10 >= 0 {
        let pow = 10i128.checked_pow(u32::try_from(total_exp10).ok()?)?;
        scaled.checked_mul(pow)?
    } else {
        let div = 10i128.checked_pow(u32::try_from(-total_exp10).ok()?)?;
        if scaled % div != 0 {
            return None;
        }
        scaled / div
    };
    if neg {
        nano.checked_neg()
    } else {
        Some(nano)
    }
}

fn split_quantity_suffix(input: &str) -> Option<(&str, i128, i32)> {
    const BINARY_SUFFIXES: [(&str, u32); 6] = [
        ("Ki", 1),
        ("Mi", 2),
        ("Gi", 3),
        ("Ti", 4),
        ("Pi", 5),
        ("Ei", 6),
    ];
    for (suffix, exp) in BINARY_SUFFIXES {
        if let Some(num) = input.strip_suffix(suffix) {
            return Some((num, 1_024i128.checked_pow(exp)?, 0));
        }
    }

    const DECIMAL_SUFFIXES: [(&str, i32); 10] = [
        ("n", -9),
        ("u", -6),
        ("m", -3),
        ("k", 3),
        ("M", 6),
        ("G", 9),
        ("T", 12),
        ("P", 15),
        ("E", 18),
        ("", 0),
    ];
    for (suffix, exp10) in DECIMAL_SUFFIXES {
        if let Some(num) = input.strip_suffix(suffix) {
            return Some((num, 1, exp10));
        }
    }

    None
}

fn parse_decimal_digits(mantissa: &str) -> Option<(i128, i32)> {
    let (int_str, frac_str) = match mantissa.split_once('.') {
        Some((i, f)) => {
            if f.contains('.') {
                return None;
            }
            (i, f)
        }
        None => (mantissa, ""),
    };
    if int_str.is_empty() && frac_str.is_empty() {
        return None;
    }

    let trimmed_frac = frac_str.trim_end_matches('0');
    let mut acc = 0i128;
    for ch in int_str.bytes().chain(trimmed_frac.bytes()) {
        if !ch.is_ascii_digit() {
            return None;
        }
        acc = acc.checked_mul(10)?.checked_add(i128::from(ch - b'0'))?;
    }
    if trimmed_frac.len() < frac_str.len() {
        for ch in frac_str[trimmed_frac.len()..].bytes() {
            if ch != b'0' {
                return None;
            }
        }
    }
    let frac_len = i32::try_from(trimmed_frac.len()).ok()?;
    Some((acc, frac_len))
}

fn volume_mounts_drifted(live: &[VolumeMount], desired: &[VolumeMount]) -> bool {
    desired.iter().any(|d_vm| {
        let Some(l_vm) = live
            .iter()
            .find(|vm| vm.name == d_vm.name && vm.mount_path == d_vm.mount_path)
        else {
            return true;
        };
        (d_vm.read_only.is_some() && l_vm.read_only != d_vm.read_only)
            || (d_vm.sub_path.is_some() && l_vm.sub_path != d_vm.sub_path)
    })
}

fn service_drifted(live: &Service, desired: &Service) -> bool {
    let (Some(live_spec), Some(desired_spec)) = (&live.spec, &desired.spec) else {
        return true;
    };
    if live_spec.type_ != desired_spec.type_ || live_spec.selector != desired_spec.selector {
        return true;
    }
    if desired_spec.cluster_ip.as_deref() == Some("None")
        && live_spec.cluster_ip != desired_spec.cluster_ip
    {
        return true;
    }
    if desired_spec.publish_not_ready_addresses.is_some()
        && live_spec.publish_not_ready_addresses != desired_spec.publish_not_ready_addresses
    {
        return true;
    }
    if desired_spec.external_ips.is_some() && live_spec.external_ips != desired_spec.external_ips {
        return true;
    }

    let live_ports = live_spec.ports.as_deref().unwrap_or(&[]);
    let desired_ports = desired_spec.ports.as_deref().unwrap_or(&[]);
    if live_ports.len() != desired_ports.len() {
        return true;
    }
    live_ports.iter().zip(desired_ports.iter()).any(|(lp, dp)| {
        lp.name != dp.name
            || lp.port != dp.port
            || lp.target_port != dp.target_port
            || lp.protocol != dp.protocol
            || (dp.node_port.is_some() && lp.node_port != dp.node_port)
    })
}

fn pdb_drifted(live: &PodDisruptionBudget, desired: &PodDisruptionBudget) -> bool {
    let (Some(live_spec), Some(desired_spec)) = (&live.spec, &desired.spec) else {
        return true;
    };
    live_spec.max_unavailable != desired_spec.max_unavailable
        || live_spec.selector != desired_spec.selector
}

fn network_policy_drifted(live: &NetworkPolicy, desired: &NetworkPolicy) -> bool {
    let (Some(live_spec), Some(desired_spec)) = (&live.spec, &desired.spec) else {
        return true;
    };
    live_spec.pod_selector != desired_spec.pod_selector
        || live_spec.policy_types != desired_spec.policy_types
        || live_spec.ingress != desired_spec.ingress
}

async fn has_group_version_live(client: &Client, group: &str, version: &str) -> bool {
    let gv = format!("{group}/{version}");
    match client.list_api_group_resources(&gv).await {
        Ok(list) => !list.resources.is_empty(),
        Err(_) => false,
    }
}

fn is_missing_crd_status(message: &str) -> bool {
    message.contains("could not find the requested resource")
        || message.contains("no matches for kind")
}

fn has_nonempty_key(data: &BTreeMap<String, k8s_openapi::ByteString>, key: &str) -> bool {
    data.get(key)
        .is_some_and(|val| !val.0.is_empty() && !val.0.iter().all(u8::is_ascii_whitespace))
}

fn expected_runtime_secret_name(ws: &WaddleServer) -> Result<String, OperatorError> {
    match ws.spec.secrets.mode {
        SecretProviderMode::Yolo => resources::yolo_secret_name(ws),
        SecretProviderMode::ExternalSecrets => {
            if let Some(eso) = &ws.spec.secrets.external_secrets {
                if eso.managed.is_some() {
                    return resources::managed_target_secret_name(ws);
                }
                if let Some(name) = &eso.external_secret_name {
                    return Ok(name.clone());
                }
            }
            resources::managed_target_secret_name(ws)
        }
    }
}

fn spicedb_expects_key_in_runtime_secret(ws: &WaddleServer) -> bool {
    match ws.spec.spicedb.mode {
        SpiceDbSourceMode::SpiceDbOperator => {
            if ws.spec.secrets.mode == SecretProviderMode::Yolo {
                return false;
            }
            ws.spec
                .spicedb
                .operator
                .as_ref()
                .is_none_or(|op| op.preshared_key_secret_ref.is_none())
        }
        SpiceDbSourceMode::External => ws
            .spec
            .spicedb
            .external
            .as_ref()
            .is_none_or(|ext| ext.preshared_key_secret_ref.is_none()),
    }
}

fn collect_extra_secret_refs(ws: &WaddleServer) -> Vec<String> {
    match ws.spec.secrets.mode {
        SecretProviderMode::Yolo => ws
            .spec
            .secrets
            .yolo
            .as_ref()
            .map(|y| y.extra_secret_refs.clone())
            .unwrap_or_default(),
        SecretProviderMode::ExternalSecrets => ws
            .spec
            .secrets
            .external_secrets
            .as_ref()
            .map(|e| e.extra_secret_refs.clone())
            .unwrap_or_default(),
    }
}

fn resolve_lineage_action(ws: &WaddleServer) -> Option<String> {
    if ws
        .spec
        .postgres
        .restricted_runtime
        .as_ref()
        .is_some_and(|rr| rr.enabled)
    {
        return None;
    }
    if let Some(explicit) = ws
        .spec
        .deployment
        .lineage_action
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Some(explicit.to_owned());
    }
    let yolo_enroll = ws.spec.secrets.mode == SecretProviderMode::Yolo
        && ws
            .spec
            .secrets
            .yolo
            .as_ref()
            .is_none_or(|y| y.enroll_lineage);
    yolo_enroll.then(|| "enroll".to_owned())
}

fn spec_secret_source_kind(spec: &WaddleServerSpec) -> SecretSourceKind {
    match spec.secrets.mode {
        SecretProviderMode::Yolo => SecretSourceKind::Yolo,
        SecretProviderMode::ExternalSecrets => {
            if spec
                .secrets
                .external_secrets
                .as_ref()
                .is_some_and(|e| e.managed.is_some())
            {
                SecretSourceKind::ExternalSecretsManaged
            } else {
                SecretSourceKind::ExternalSecretsUnmanaged
            }
        }
    }
}

fn is_safe_resource_name(name: &str) -> bool {
    !name.contains("..") && ResourceName::try_new(name).is_ok()
}

fn to_raw_byte_map(
    data: Option<&BTreeMap<String, k8s_openapi::ByteString>>,
) -> BTreeMap<String, Vec<u8>> {
    let mut map = BTreeMap::new();
    if let Some(entries) = data {
        for (k, v) in entries {
            map.insert(k.clone(), v.0.clone());
        }
    }
    map
}

fn check_min_length_key(
    entries: &BTreeMap<String, Vec<u8>>,
    key: &str,
    min_len: usize,
    missing_keys: &mut Vec<String>,
    invalid_keys: &mut Vec<String>,
) {
    match entries.get(key) {
        None => missing_keys.push(key.to_owned()),
        Some(bytes) if bytes.is_empty() => missing_keys.push(key.to_owned()),
        Some(bytes) if bytes.len() < min_len => invalid_keys.push(key.to_owned()),
        Some(_) => {}
    }
}

fn check_nonempty_key(
    entries: &BTreeMap<String, Vec<u8>>,
    key: &str,
    missing_keys: &mut Vec<String>,
) {
    match entries.get(key) {
        Some(bytes) if !bytes.is_empty() && !bytes.iter().all(u8::is_ascii_whitespace) => {}
        _ => missing_keys.push(key.to_owned()),
    }
}

fn validate_keypool_in_map(
    entries: &BTreeMap<String, Vec<u8>>,
    min_count: usize,
    missing_keys: &mut Vec<String>,
    invalid_keys: &mut Vec<String>,
) {
    const KEY: &str = "WADDLE_CLUSTERING_KEYPAIR_POOL";
    let Some(raw_bytes) = entries.get(KEY) else {
        missing_keys.push(KEY.to_owned());
        return;
    };
    let Ok(raw_str) = std::str::from_utf8(raw_bytes) else {
        invalid_keys.push(KEY.to_owned());
        return;
    };
    if raw_str.trim().is_empty() {
        missing_keys.push(KEY.to_owned());
        return;
    }
    if yolo::validate_keypair_pool(raw_str, min_count).is_err() {
        invalid_keys.push(KEY.to_owned());
    }
}

fn detect_sensitive_key_rotation(
    live_deployment: Option<&Deployment>,
    current_session_fp: Option<&ContentDigest>,
    current_occupant_fp: Option<&ContentDigest>,
) -> Option<SensitiveKeyRotation> {
    let annotations = live_deployment
        .and_then(|d| d.spec.as_ref())
        .and_then(|s| s.template.metadata.as_ref())
        .and_then(|m| m.annotations.as_ref())?;

    let session_changed = match (
        annotations.get(ANNOTATION_SESSION_KEY_FINGERPRINT),
        current_session_fp,
    ) {
        (Some(prev), Some(curr)) => prev != curr.as_str(),
        _ => false,
    };

    let occupant_changed = match (
        annotations.get(ANNOTATION_OCCUPANT_ID_FINGERPRINT),
        current_occupant_fp,
    ) {
        (Some(prev), Some(curr)) => prev != curr.as_str(),
        _ => false,
    };

    match (session_changed, occupant_changed) {
        (true, true) => Some(SensitiveKeyRotation::Both),
        (true, false) => Some(SensitiveKeyRotation::SessionKey),
        (false, true) => Some(SensitiveKeyRotation::OccupantIdSecret),
        (false, false) => None,
    }
}

fn parse_existing_conditions(conditions: &[KubeCondition]) -> Vec<ConditionRecord> {
    conditions
        .iter()
        .filter_map(|cond| {
            let kind = ConditionKind::parse(&cond.type_)?;
            let timestamp =
                chrono::DateTime::<chrono::Utc>::from_str(&cond.last_transition_time.0.to_string())
                    .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                    .unwrap_or_else(|_| cond.last_transition_time.0.to_string());
            Some(ConditionRecord {
                kind,
                status: ConditionStatus::parse(&cond.status),
                reason: cond.reason.clone(),
                message: cond.message.clone(),
                observed_generation: cond.observed_generation.unwrap_or(0),
                last_transition_time: timestamp,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use k8s_openapi::apimachinery::pkg::api::resource::Quantity;

    use super::quantities_equal;

    #[test]
    fn quantities_equal_handles_kubernetes_canonical_equivalents() {
        assert!(quantities_equal(
            &Quantity("0.5".to_owned()),
            &Quantity("500m".to_owned()),
        ));
        assert!(quantities_equal(
            &Quantity("1000m".to_owned()),
            &Quantity("1".to_owned()),
        ));
        assert!(quantities_equal(
            &Quantity("1024Mi".to_owned()),
            &Quantity("1Gi".to_owned()),
        ));
        assert!(quantities_equal(
            &Quantity("1.5Gi".to_owned()),
            &Quantity("1536Mi".to_owned()),
        ));
        assert!(!quantities_equal(
            &Quantity("500m".to_owned()),
            &Quantity("600m".to_owned()),
        ));
        assert!(!quantities_equal(
            &Quantity("512Mi".to_owned()),
            &Quantity("1Gi".to_owned()),
        ));
    }
}
