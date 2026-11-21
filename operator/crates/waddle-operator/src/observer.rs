use std::collections::BTreeMap;
use std::str::FromStr;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Secret, Service};
use k8s_openapi::api::discovery::v1::EndpointSlice;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition as KubeCondition;
use kube::api::{Api, DynamicObject, ListParams};
use kube::Client;
use waddle_operator_crd::{
    ContentDigest, EsoApiVersion, PostgresSourceMode, ResourceName, RestrictedRuntimeSpec,
    SecretProviderMode, SecretStoreKind, SpiceDbSourceMode, UpdateStrategyKind, ValidatedSpec,
    WaddleServer, WaddleServerSpec,
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

    let (postgres_health, postgres_binding) =
        observe_postgres(client, ns, ws, policy.capabilities.has_cnpg).await?;

    let spicedb_key_in_runtime_secret = spicedb_expects_key_in_runtime_secret(ws);

    let secret_obs = observe_secrets(
        client,
        ws,
        ns,
        policy,
        live_deployment.as_ref(),
        spicedb_key_in_runtime_secret,
    )
    .await?;

    let (spicedb_health, spicedb_binding) = observe_spicedb(
        client,
        ws,
        ns,
        policy.capabilities.has_spicedb,
        &secret_obs.target_secret_name,
    )
    .await?;

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

async fn observe_postgres(
    client: &Client,
    ns: &str,
    ws: &WaddleServer,
    known_has_cnpg: bool,
) -> Result<(DependencyHealth, Option<PostgresBinding>), OperatorError> {
    let pg = &ws.spec.postgres;
    let restricted = pg.restricted_runtime.as_ref().filter(|r| r.enabled);

    match pg.mode {
        PostgresSourceMode::CloudNativePg => {
            let Some(cnpg) = &pg.cnpg else {
                return Ok((
                    DependencyHealth::NotReady {
                        reason: "CnpgSpecMissing".to_owned(),
                        detail: "postgres.mode is CloudNativePg without spec.postgres.cnpg"
                            .to_owned(),
                    },
                    None,
                ));
            };

            let ar = external_crds::api_resource_cnpg();
            let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), ns, &ar);
            let cluster_obj = match api.get(&cnpg.cluster_name).await {
                Ok(obj) => obj,
                Err(kube::Error::Api(status)) if status.code == 404 => {
                    if is_missing_crd_status(&status.message)
                        || (!known_has_cnpg
                            && !has_group_version_live(client, "postgresql.cnpg.io", "v1").await)
                    {
                        return Ok((
                            DependencyHealth::CrdNotInstalled {
                                api_group: "postgresql.cnpg.io".to_owned(),
                            },
                            None,
                        ));
                    }
                    return Ok((
                        DependencyHealth::Missing {
                            reason: "CnpgClusterNotFound".to_owned(),
                            detail: format!(
                                "CloudNativePG Cluster {} not found in namespace {ns}",
                                cnpg.cluster_name
                            ),
                        },
                        None,
                    ));
                }
                Err(source) => {
                    return Err(OperatorError::Kube {
                        context: "get CloudNativePG Cluster",
                        source,
                    });
                }
            };

            let cluster_view: CnpgClusterView = serde_json::from_value(cluster_obj.data)
                .map_err(|err| OperatorError::InvalidClusterState(err.to_string()))?;
            let health = external_crds::evaluate_cnpg_health(
                &cluster_view,
                &cnpg.cluster_name,
                cnpg.require_healthy,
            );
            if !health.is_ready() {
                return Ok((health, None));
            }

            if let Some(rr) = restricted {
                if let Some(blocker) = observe_restricted_runtime(client, ns, rr).await? {
                    return Ok((blocker, None));
                }
                return Ok((
                    health,
                    Some(PostgresBinding::RestrictedRuntime {
                        spec: Box::new(rr.clone()),
                    }),
                ));
            }

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
                ));
            };

            let data = secret.data.unwrap_or_default();
            if has_nonempty_key(&data, &cnpg.uri_key) {
                return Ok((
                    health,
                    Some(PostgresBinding::UriSecret {
                        secret_name,
                        uri_key: cnpg.uri_key.clone(),
                    }),
                ));
            }
            if has_nonempty_key(&data, &cnpg.username_key)
                && has_nonempty_key(&data, &cnpg.password_key)
            {
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
            ))
        }
        PostgresSourceMode::External => {
            let Some(ext) = &pg.external else {
                return Ok((
                    DependencyHealth::NotReady {
                        reason: "ExternalPostgresSpecMissing".to_owned(),
                        detail: "postgres.mode is External without spec.postgres.external"
                            .to_owned(),
                    },
                    None,
                ));
            };

            if let Some(svc_name) = &ext.service_name {
                if let Some(blocker) =
                    check_service_endpoints(client, ns, svc_name, "Postgres").await?
                {
                    return Ok((blocker, None));
                }
            }

            if let Some(rr) = restricted {
                if let Some(blocker) = observe_restricted_runtime(client, ns, rr).await? {
                    return Ok((blocker, None));
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
                ));
            }

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
            ))
        }
    }
}

async fn observe_restricted_runtime(
    client: &Client,
    ns: &str,
    rr: &RestrictedRuntimeSpec,
) -> Result<Option<DependencyHealth>, OperatorError> {
    let secrets = Api::<Secret>::namespaced(client.clone(), ns);
    let Some(ca_secret) = secrets
        .get_opt(&rr.ca_secret_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get restricted runtime CA Secret",
            source,
        })?
    else {
        return Ok(Some(DependencyHealth::Missing {
            reason: "PostgresCaSecretMissing".to_owned(),
            detail: format!(
                "Restricted runtime CA Secret {} not found",
                rr.ca_secret_name
            ),
        }));
    };
    if !has_nonempty_key(&ca_secret.data.unwrap_or_default(), &rr.ca_secret_key) {
        return Ok(Some(DependencyHealth::NotReady {
            reason: "PostgresCaKeyMissing".to_owned(),
            detail: format!(
                "CA Secret {} lacks non-empty key '{}'",
                rr.ca_secret_name, rr.ca_secret_key
            ),
        }));
    }

    let Some(cred_secret) = secrets
        .get_opt(&rr.runtime_credential_secret)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get restricted runtime credential Secret",
            source,
        })?
    else {
        return Ok(Some(DependencyHealth::Missing {
            reason: "PostgresRuntimeCredentialMissing".to_owned(),
            detail: format!(
                "Restricted runtime credential Secret {} not found",
                rr.runtime_credential_secret
            ),
        }));
    };
    if !has_nonempty_key(
        &cred_secret.data.unwrap_or_default(),
        &rr.runtime_credential_key,
    ) {
        return Ok(Some(DependencyHealth::NotReady {
            reason: "PostgresRuntimeCredentialKeyMissing".to_owned(),
            detail: format!(
                "Credential Secret {} lacks non-empty key '{}'",
                rr.runtime_credential_secret, rr.runtime_credential_key
            ),
        }));
    }

    let Some(handoff_cm) = Api::<ConfigMap>::namespaced(client.clone(), ns)
        .get_opt(&rr.handoff_config_map_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get restricted runtime handoff ConfigMap",
            source,
        })?
    else {
        return Ok(Some(DependencyHealth::Missing {
            reason: "PostgresHandoffMissing".to_owned(),
            detail: format!(
                "Restricted runtime handoff ConfigMap {} not found",
                rr.handoff_config_map_name
            ),
        }));
    };
    let has_handoff = handoff_cm
        .data
        .as_ref()
        .and_then(|d| d.get("handoff.json"))
        .is_some_and(|v| !v.trim().is_empty());
    if !has_handoff {
        return Ok(Some(DependencyHealth::NotReady {
            reason: "PostgresHandoffKeyMissing".to_owned(),
            detail: format!(
                "Handoff ConfigMap {} lacks non-empty 'handoff.json'",
                rr.handoff_config_map_name
            ),
        }));
    }

    Ok(None)
}

async fn observe_spicedb(
    client: &Client,
    ws: &WaddleServer,
    ns: &str,
    known_has_spicedb: bool,
    runtime_secret_name: &str,
) -> Result<(DependencyHealth, Option<SpiceDbBinding>), OperatorError> {
    let sp = &ws.spec.spicedb;
    match sp.mode {
        SpiceDbSourceMode::SpiceDbOperator => {
            let Some(op) = &sp.operator else {
                return Ok((
                    DependencyHealth::NotReady {
                        reason: "SpiceDbSpecMissing".to_owned(),
                        detail: "spicedb.mode is SpiceDbOperator without spec.spicedb.operator"
                            .to_owned(),
                    },
                    None,
                ));
            };

            let ar = external_crds::api_resource_spicedb();
            let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), ns, &ar);
            let cluster_obj = match api.get(&op.cluster_name).await {
                Ok(obj) => obj,
                Err(kube::Error::Api(status)) if status.code == 404 => {
                    if is_missing_crd_status(&status.message)
                        || (!known_has_spicedb
                            && !has_group_version_live(client, "authzed.com", "v1alpha1").await)
                    {
                        return Ok((
                            DependencyHealth::CrdNotInstalled {
                                api_group: "authzed.com".to_owned(),
                            },
                            None,
                        ));
                    }
                    return Ok((
                        DependencyHealth::Missing {
                            reason: "SpiceDbClusterNotFound".to_owned(),
                            detail: format!(
                                "SpiceDBCluster {} not found in namespace {ns}",
                                op.cluster_name
                            ),
                        },
                        None,
                    ));
                }
                Err(source) => {
                    return Err(OperatorError::Kube {
                        context: "get SpiceDBCluster",
                        source,
                    });
                }
            };

            let view: SpiceDbClusterView = serde_json::from_value(cluster_obj.data)
                .map_err(|err| OperatorError::InvalidClusterState(err.to_string()))?;

            let svc_name = op.service_name.as_deref().unwrap_or(&op.cluster_name);
            let ready_endpoints = count_ready_endpoints(client, ns, svc_name).await?;
            let insecure = op.insecure.unwrap_or_else(|| !view.has_tls());
            let scheme = if insecure { "http" } else { "https" };
            let endpoint = format!("{scheme}://{svc_name}:{}", op.port);

            let health = external_crds::evaluate_spicedb_health(
                &view,
                &endpoint,
                ready_endpoints,
                op.require_ready,
            );
            if !health.is_ready() {
                return Ok((health, None));
            }

            let (pk_secret_name, pk_secret_key) = if let Some(explicit) =
                &op.preshared_key_secret_ref
            {
                if let Some(blocker) =
                    verify_secret_key_present(client, ns, &explicit.name, &explicit.key, "SpiceDB")
                        .await?
                {
                    return Ok((blocker, None));
                }
                (explicit.name.clone(), explicit.key.clone())
            } else if ws.spec.secrets.mode == SecretProviderMode::Yolo {
                let Some(cluster_secret) = view
                    .spec
                    .secret_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    return Ok((
                            DependencyHealth::NotReady {
                                reason: "SpiceDbSecretNameMissing".to_owned(),
                                detail: format!(
                                    "SpiceDBCluster {} has no spec.secretName; set presharedKeySecretRef",
                                    op.cluster_name
                                ),
                            },
                            None,
                        ));
                };
                if !is_safe_resource_name(cluster_secret) {
                    return Ok((
                        DependencyHealth::NotReady {
                            reason: "InvalidSpiceDbSecretName".to_owned(),
                            detail: format!(
                                "SpiceDBCluster.spec.secretName '{cluster_secret}' is not a valid Kubernetes resource name"
                            ),
                        },
                        None,
                    ));
                }
                if let Some(blocker) = verify_secret_key_present(
                    client,
                    ns,
                    cluster_secret,
                    "preshared_key",
                    "SpiceDB",
                )
                .await?
                {
                    return Ok((blocker, None));
                }
                (cluster_secret.to_owned(), "preshared_key".to_owned())
            } else {
                (
                    runtime_secret_name.to_owned(),
                    KEY_SPICEDB_PRESHARED_KEY.to_owned(),
                )
            };

            Ok((
                health,
                Some(SpiceDbBinding {
                    endpoint,
                    insecure,
                    preshared_key_secret_name: pk_secret_name,
                    preshared_key_secret_key: pk_secret_key,
                }),
            ))
        }
        SpiceDbSourceMode::External => {
            let Some(ext) = &sp.external else {
                return Ok((
                    DependencyHealth::NotReady {
                        reason: "ExternalSpiceDbSpecMissing".to_owned(),
                        detail: "spicedb.mode is External without spec.spicedb.external".to_owned(),
                    },
                    None,
                ));
            };

            if let Some(svc_name) = &ext.service_name {
                if let Some(blocker) =
                    check_service_endpoints(client, ns, svc_name, "SpiceDB").await?
                {
                    return Ok((blocker, None));
                }
            }

            let (pk_secret_name, pk_secret_key) = if let Some(explicit) =
                &ext.preshared_key_secret_ref
            {
                if let Some(blocker) =
                    verify_secret_key_present(client, ns, &explicit.name, &explicit.key, "SpiceDB")
                        .await?
                {
                    return Ok((blocker, None));
                }
                (explicit.name.clone(), explicit.key.clone())
            } else {
                (
                    runtime_secret_name.to_owned(),
                    KEY_SPICEDB_PRESHARED_KEY.to_owned(),
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
            ))
        }
    }
}

struct SecretObservationBundle {
    observation: SecretsObservation,
    target_secret_name: String,
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
    match ws.spec.secrets.mode {
        SecretProviderMode::Yolo => {
            observe_yolo_secrets(
                client,
                ws,
                ns,
                policy,
                live_deployment,
                spicedb_key_in_runtime_secret,
            )
            .await
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
            .await
        }
    }
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
        session_key_fingerprint: session_fp,
        occupant_id_fingerprint: occupant_fp,
        eso_api_version: None,
        resolved_deployment_uuid,
    })
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
        let fallback_target = resources::managed_target_secret_name(ws)?;
        return Ok(SecretObservationBundle {
            observation: SecretsObservation {
                source: SecretSourceKind::ExternalSecretsUnmanaged,
                yolo_allowed_by_operator: policy.enable_yolo_mode,
                cluster_store_allowed: true,
                external_secret_synced: Some(false),
                external_secret_error: Some(
                    "secrets.mode is ExternalSecrets without spec.secrets.externalSecrets"
                        .to_owned(),
                ),
                managed_external_secret_drifted: false,
                target_secret_exists: false,
                missing_keys: vec![],
                invalid_keys: vec![],
                checksum: None,
                sensitive_key_rotated: None,
            },
            target_secret_name: fallback_target,
            session_key_fingerprint: None,
            occupant_id_fingerprint: None,
            eso_api_version: None,
            resolved_deployment_uuid: ws.spec.deployment.uuid.clone(),
        });
    };

    let is_managed = eso.managed.is_some();
    let source_kind = if is_managed {
        SecretSourceKind::ExternalSecretsManaged
    } else {
        SecretSourceKind::ExternalSecretsUnmanaged
    };

    let cluster_store_allowed = match &eso.managed {
        Some(managed) if managed.secret_store_ref.kind == SecretStoreKind::ClusterSecretStore => {
            policy
                .allowed_cluster_secret_stores
                .iter()
                .any(|allowed| allowed == &managed.secret_store_ref.name)
        }
        _ => true,
    };

    let eso_version =
        resolve_eso_version(client, eso.api_version, policy.capabilities.eso_version).await;
    let mut target_secret_name = expected_runtime_secret_name(ws)?;

    let mut external_secret_synced = Some(false);
    let mut external_secret_error;
    let mut managed_drifted = false;

    match eso_version {
        None => {
            external_secret_error = Some(
                "External Secrets Operator CRD (external-secrets.io) is not installed".to_owned(),
            );
            managed_drifted = is_managed;
        }
        Some(version) => {
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
                    let (synced, err_msg) = external_crds::evaluate_eso_status(&view);
                    external_secret_synced = synced;
                    external_secret_error = err_msg;

                    if is_managed {
                        let desired_es = resources::build_managed_external_secret(ws, version)?;
                        managed_drifted =
                            resources::managed_external_secret_drifted(&live_es, &desired_es);
                    }
                }
                Err(kube::Error::Api(status)) if status.code == 404 => {
                    if is_missing_crd_status(&status.message)
                        || !has_group_version_live(
                            client,
                            "external-secrets.io",
                            version.version_str(),
                        )
                        .await
                    {
                        external_secret_error = Some(format!(
                            "ExternalSecret CRD {} is not installed",
                            version.api_version()
                        ));
                    } else {
                        external_secret_error = Some(format!(
                            "ExternalSecret {es_name} not found in namespace {ns}"
                        ));
                    }
                    managed_drifted = is_managed;
                }
                Err(source) => {
                    return Err(OperatorError::Kube {
                        context: "get ExternalSecret",
                        source,
                    });
                }
            }
        }
    }

    let secret = if is_safe_resource_name(&target_secret_name) {
        Api::<Secret>::namespaced(client.clone(), ns)
            .get_opt(&target_secret_name)
            .await
            .map_err(|source| OperatorError::Kube {
                context: "get ESO target runtime Secret",
                source,
            })?
    } else {
        external_secret_synced = Some(false);
        external_secret_error = Some(format!(
            "ExternalSecret spec.target.name '{target_secret_name}' is not a valid Kubernetes resource name"
        ));
        None
    };

    let mut missing_keys = Vec::new();
    let mut invalid_keys = Vec::new();
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
        checksum = yolo::compute_consumed_secret_digest(&raw_entries, &key_refs);
        session_fp = raw_entries
            .get(KEY_SESSION_KEY)
            .and_then(|b| yolo::compute_single_key_digest(b));
        occupant_fp = raw_entries
            .get(KEY_OCCUPANT_ID_SECRET)
            .and_then(|b| yolo::compute_single_key_digest(b));

        if ws.spec.clustering.enabled {
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

            if let Some(pool_secret_name) = &eso.clustering_keypool_secret_name {
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
                        &mut missing_keys,
                        &mut invalid_keys,
                    ),
                    None => missing_keys
                        .push(format!("{pool_secret_name}/WADDLE_CLUSTERING_KEYPAIR_POOL")),
                }
            } else {
                validate_keypool_in_map(
                    &raw_entries,
                    required_count,
                    &mut missing_keys,
                    &mut invalid_keys,
                );
            }
        }
        true
    } else {
        false
    };

    if let Some(apns) = &eso.apns_secret_ref {
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
    }

    let sensitive_key_rotated =
        detect_sensitive_key_rotation(live_deployment, session_fp.as_ref(), occupant_fp.as_ref());

    Ok(SecretObservationBundle {
        observation: SecretsObservation {
            source: source_kind,
            yolo_allowed_by_operator: policy.enable_yolo_mode,
            cluster_store_allowed,
            external_secret_synced,
            external_secret_error,
            managed_external_secret_drifted: managed_drifted,
            target_secret_exists,
            missing_keys,
            invalid_keys,
            checksum,
            sensitive_key_rotated,
        },
        target_secret_name,
        session_key_fingerprint: session_fp,
        occupant_id_fingerprint: occupant_fp,
        eso_api_version: eso_version,
        resolved_deployment_uuid: ws.spec.deployment.uuid.clone(),
    })
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
        let desired_cm = resources::build_config_map(ws, b)?;
        let cm_digest = resources::config_map_digest(&desired_cm)?;
        let desired_dep = resources::build_deployment(ws, val, b, &cm_digest)?;
        let desired_hash = desired_dep
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(ANNOTATION_SPEC_HASH));
        let live_hash = dep
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(ANNOTATION_SPEC_HASH));

        let cm_missing = Api::<ConfigMap>::namespaced(client.clone(), ns)
            .get_opt(&resources::config_map_name(ws)?)
            .await
            .map_err(|source| OperatorError::Kube {
                context: "get ConfigMap for drift detection",
                source,
            })?
            .is_none();

        cm_missing || desired_hash != live_hash
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
) -> Result<Option<DependencyHealth>, OperatorError> {
    let sec = Api::<Secret>::namespaced(client.clone(), ns)
        .get_opt(secret_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get dependency Secret",
            source,
        })?;
    let Some(secret) = sec else {
        return Ok(Some(DependencyHealth::NotReady {
            reason: format!("{label}SecretMissing"),
            detail: format!("{label} Secret {secret_name} not found in namespace {ns}"),
        }));
    };
    if !has_nonempty_key(&secret.data.unwrap_or_default(), key) {
        return Ok(Some(DependencyHealth::NotReady {
            reason: format!("{label}SecretKeyMissing"),
            detail: format!("{label} Secret {secret_name} lacks non-empty key '{key}'"),
        }));
    }
    Ok(None)
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
