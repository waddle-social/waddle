use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec, DeploymentStrategy};
use k8s_openapi::api::core::v1::{
    Affinity, Capabilities, ConfigMapEnvSource, ConfigMapKeySelector, ConfigMapVolumeSource,
    Container, ContainerPort, EmptyDirVolumeSource, EnvFromSource, EnvVar, EnvVarSource,
    HTTPGetAction, KeyToPath, Lifecycle, LifecycleHandler, LocalObjectReference,
    ObjectFieldSelector, PersistentVolumeClaimVolumeSource, PodAffinityTerm, PodAntiAffinity,
    PodSecurityContext, PodSpec, PodTemplateSpec, Probe, ResourceRequirements, SecretEnvSource,
    SecretKeySelector, SecretVolumeSource, SecurityContext, SleepAction, Volume, VolumeMount,
    WeightedPodAffinityTerm,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::ObjectMeta;
use waddle_operator_crd::{
    ContentDigest, SecretProviderMode, UpdateStrategyKind, ValidatedSpec, WaddleServer,
};

use crate::bindings::{PostgresBinding, ResolvedRuntimeBindings};
use crate::error::OperatorError;
use crate::resources::labels;
use crate::resources::secret;

/// Annotation key storing the desired workload spec + config + secret digest.
pub const ANNOTATION_SPEC_HASH: &str = "waddle.social/spec-hash";
/// Annotation key storing the fingerprinted `WADDLE_SESSION_KEY` digest for rotation warnings.
pub const ANNOTATION_SESSION_KEY_FINGERPRINT: &str = "waddle.social/session-key-fingerprint";
/// Annotation key storing the fingerprinted `WADDLE_OCCUPANT_ID_SECRET` digest.
pub const ANNOTATION_OCCUPANT_ID_FINGERPRINT: &str = "waddle.social/occupant-id-fingerprint";

/// Builds the Waddle server `Deployment` matching `waddle-server` Helm chart invariants.
///
/// When `ws.spec.replicas` is `None`, `Deployment.spec.replicas` is left `None` so an external
/// HPA can manage replica scaling without SSA conflicts.
///
/// # Errors
/// Returns [`OperatorError`] if metadata is missing or spec hashing fails.
pub fn build_deployment(
    ws: &WaddleServer,
    validated: &ValidatedSpec,
    bindings: &ResolvedRuntimeBindings,
    config_digest: &ContentDigest,
) -> Result<Deployment, OperatorError> {
    let name = labels::workload_name(ws)?.to_owned();
    let owner = labels::owner_reference(ws)?;
    let obj_labels = labels::common_labels(ws)?;
    let selector = labels::selector_labels(ws)?;
    let spec = &ws.spec;

    let mut pod_labels = selector.clone();
    for (k, v) in &spec.pod_labels {
        pod_labels.insert(k.clone(), v.clone());
    }

    let mut pod_annotations = spec.pod_annotations.clone();
    pod_annotations.insert(
        "waddle.social/config-checksum".to_owned(),
        config_digest.as_str().to_owned(),
    );
    if let Some(secret_digest) = &bindings.secrets_checksum {
        pod_annotations.insert(
            "waddle.social/secrets-checksum".to_owned(),
            secret_digest.as_str().to_owned(),
        );
    }
    if let Some(fp) = &bindings.session_key_fingerprint {
        pod_annotations.insert(
            ANNOTATION_SESSION_KEY_FINGERPRINT.to_owned(),
            fp.as_str().to_owned(),
        );
    }
    if let Some(fp) = &bindings.occupant_id_fingerprint {
        pod_annotations.insert(
            ANNOTATION_OCCUPANT_ID_FINGERPRINT.to_owned(),
            fp.as_str().to_owned(),
        );
    }

    let ports = build_ports(ws);
    let env_from = build_env_from(ws, bindings)?;
    let env = build_env(ws, bindings)?;
    let (volumes, volume_mounts) = build_storage_and_mounts(ws, bindings)?;

    let container = Container {
        name: "waddle-server".to_owned(),
        image: Some(build_image_ref(ws)),
        image_pull_policy: Some(spec.image.pull_policy.as_k8s_str().to_owned()),
        security_context: Some(SecurityContext {
            run_as_non_root: Some(true),
            run_as_user: Some(1000),
            run_as_group: Some(1000),
            allow_privilege_escalation: Some(false),
            read_only_root_filesystem: Some(false),
            capabilities: Some(Capabilities {
                drop: Some(vec!["ALL".to_owned()]),
                ..Capabilities::default()
            }),
            ..SecurityContext::default()
        }),
        lifecycle: (spec.shutdown.pre_stop_sleep_seconds > 0).then(|| Lifecycle {
            pre_stop: Some(LifecycleHandler {
                sleep: Some(SleepAction {
                    seconds: spec.shutdown.pre_stop_sleep_seconds,
                }),
                ..LifecycleHandler::default()
            }),
            ..Lifecycle::default()
        }),
        ports: Some(ports),
        env_from: Some(env_from),
        env: Some(env),
        liveness_probe: Some(build_probe(&spec.probes.liveness)),
        readiness_probe: Some(build_probe(&spec.probes.readiness)),
        resources: Some(build_resources(&spec.resources)),
        volume_mounts: Some(volume_mounts),
        ..Container::default()
    };

    let image_pull_secrets = (!spec.image.pull_secrets.is_empty()).then(|| {
        spec.image
            .pull_secrets
            .iter()
            .map(|secret_name| LocalObjectReference {
                name: secret_name.clone(),
            })
            .collect()
    });

    let affinity = spec.pod_anti_affinity.enabled.then(|| Affinity {
        pod_anti_affinity: Some(PodAntiAffinity {
            preferred_during_scheduling_ignored_during_execution: Some(vec![
                WeightedPodAffinityTerm {
                    weight: spec.pod_anti_affinity.weight,
                    pod_affinity_term: PodAffinityTerm {
                        label_selector: Some(LabelSelector {
                            match_labels: Some(selector.clone()),
                            ..LabelSelector::default()
                        }),
                        topology_key: spec.pod_anti_affinity.topology_key.clone(),
                        ..PodAffinityTerm::default()
                    },
                },
            ]),
            ..PodAntiAffinity::default()
        }),
        ..Affinity::default()
    });

    let strategy_type = match spec.update_strategy {
        UpdateStrategyKind::Recreate => "Recreate",
        UpdateStrategyKind::RollingUpdate => "RollingUpdate",
    };

    let deploy_spec = DeploymentSpec {
        replicas: spec.replicas,
        strategy: Some(DeploymentStrategy {
            type_: Some(strategy_type.to_owned()),
            ..DeploymentStrategy::default()
        }),
        selector: LabelSelector {
            match_labels: Some(selector),
            ..LabelSelector::default()
        },
        template: PodTemplateSpec {
            metadata: Some(ObjectMeta {
                labels: Some(pod_labels),
                annotations: Some(pod_annotations),
                ..ObjectMeta::default()
            }),
            spec: Some(PodSpec {
                service_account_name: Some(name.clone()),
                termination_grace_period_seconds: Some(validated.termination_grace_period_seconds),
                image_pull_secrets,
                security_context: Some(PodSecurityContext {
                    fs_group: Some(1000),
                    ..PodSecurityContext::default()
                }),
                containers: vec![container],
                volumes: Some(volumes),
                node_selector: (!spec.node_selector.is_empty()).then(|| spec.node_selector.clone()),
                affinity,
                ..PodSpec::default()
            }),
        },
        ..DeploymentSpec::default()
    };

    let spec_hash = compute_workload_hash(ws, &deploy_spec)?;
    let mut deploy_annotations = BTreeMap::new();
    deploy_annotations.insert(
        ANNOTATION_SPEC_HASH.to_owned(),
        spec_hash.as_str().to_owned(),
    );

    Ok(Deployment {
        metadata: ObjectMeta {
            name: Some(name),
            labels: Some(obj_labels),
            annotations: Some(deploy_annotations),
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        spec: Some(deploy_spec),
        ..Deployment::default()
    })
}

fn build_image_ref(ws: &WaddleServer) -> String {
    match ws
        .spec
        .image
        .digest
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(digest) => format!("{}@{digest}", ws.spec.image.repository),
        None => format!("{}:{}", ws.spec.image.repository, ws.spec.image.tag),
    }
}

fn build_ports(ws: &WaddleServer) -> Vec<ContainerPort> {
    let mut ports = vec![ContainerPort {
        name: Some("http".to_owned()),
        container_port: ws.spec.service.http_port,
        protocol: Some("TCP".to_owned()),
        ..ContainerPort::default()
    }];
    if ws.spec.clustering.enabled {
        ports.push(ContainerPort {
            name: Some("swarm".to_owned()),
            container_port: i32::from(ws.spec.clustering.swarm_port),
            protocol: Some("TCP".to_owned()),
            ..ContainerPort::default()
        });
    }
    if ws.spec.s2s.enabled {
        ports.push(ContainerPort {
            name: Some("xmpp-s2s".to_owned()),
            container_port: ws.spec.s2s.port,
            protocol: Some("TCP".to_owned()),
            ..ContainerPort::default()
        });
    }
    ports
}

fn build_env_from(
    ws: &WaddleServer,
    bindings: &ResolvedRuntimeBindings,
) -> Result<Vec<EnvFromSource>, OperatorError> {
    let mut sources = vec![EnvFromSource {
        config_map_ref: Some(ConfigMapEnvSource {
            name: labels::config_map_name(ws)?,
            optional: Some(false),
        }),
        ..EnvFromSource::default()
    }];

    if ws.spec.secrets.mode == SecretProviderMode::ExternalSecrets {
        sources.push(EnvFromSource {
            secret_ref: Some(SecretEnvSource {
                name: bindings.runtime_secret_name.clone(),
                optional: Some(false),
            }),
            ..EnvFromSource::default()
        });
    }

    for extra_secret in &bindings.extra_secret_refs {
        sources.push(EnvFromSource {
            secret_ref: Some(SecretEnvSource {
                name: extra_secret.clone(),
                optional: Some(false),
            }),
            ..EnvFromSource::default()
        });
    }

    Ok(sources)
}

fn build_env(
    ws: &WaddleServer,
    bindings: &ResolvedRuntimeBindings,
) -> Result<Vec<EnvVar>, OperatorError> {
    let mut env = vec![
        secret_env_var(
            secret::KEY_SESSION_KEY,
            &bindings.runtime_secret_name,
            secret::KEY_SESSION_KEY,
        ),
        secret_env_var(
            secret::KEY_OCCUPANT_ID_SECRET,
            &bindings.runtime_secret_name,
            secret::KEY_OCCUPANT_ID_SECRET,
        ),
        secret_env_var(
            secret::KEY_SPICEDB_PRESHARED_KEY,
            &bindings.spicedb.preshared_key_secret_name,
            &bindings.spicedb.preshared_key_secret_key,
        ),
        EnvVar {
            name: "WADDLE_CLUSTERING_POD_TEMPLATE_HASH".to_owned(),
            value_from: Some(EnvVarSource {
                field_ref: Some(ObjectFieldSelector {
                    field_path: "metadata.labels['pod-template-hash']".to_owned(),
                    ..ObjectFieldSelector::default()
                }),
                ..EnvVarSource::default()
            }),
            ..EnvVar::default()
        },
        EnvVar {
            name: "WADDLE_UPLOAD_DIR".to_owned(),
            value: Some(format!(
                "{}/{}",
                ws.spec.persistence.mount_path, ws.spec.persistence.upload_sub_path
            )),
            ..EnvVar::default()
        },
    ];

    if ws.spec.secrets.mode == SecretProviderMode::Yolo && ws.spec.deployment.uuid.is_none() {
        env.push(secret_env_var(
            secret::KEY_DEPLOYMENT_UUID,
            &bindings.runtime_secret_name,
            secret::KEY_DEPLOYMENT_UUID,
        ));
    }

    append_postgres_env(&mut env, &bindings.postgres);

    if let Some(pool_secret) = &bindings.clustering_keypool_secret_name {
        env.push(secret_env_var(
            "WADDLE_CLUSTERING_KEYPAIR_POOL",
            pool_secret,
            "WADDLE_CLUSTERING_KEYPAIR_POOL",
        ));
    }

    if ws.spec.xmpp.enabled && ws.spec.xmpp.tls_secret_name.is_some() {
        env.push(plain_env_var(
            "WADDLE_XMPP_TLS_CERT",
            "/etc/waddle/tls/tls.crt",
        ));
        env.push(plain_env_var(
            "WADDLE_XMPP_TLS_KEY",
            "/etc/waddle/tls/tls.key",
        ));
    }

    if let Some(eso) = &ws.spec.secrets.external_secrets {
        if let Some(apns) = &eso.apns_secret_ref {
            env.push(plain_env_var(
                "WADDLE_APNS_KEY_PATH",
                "/var/run/secrets/waddle-apns/AuthKey.p8",
            ));
            env.push(plain_env_var("WADDLE_APNS_TEAM_ID", &apns.team_id));
            env.push(plain_env_var("WADDLE_APNS_KEY_ID", &apns.key_id));
            env.push(plain_env_var("WADDLE_APNS_BUNDLE_ID", &apns.bundle_id));
        }
    }

    Ok(env)
}

fn append_postgres_env(env: &mut Vec<EnvVar>, binding: &PostgresBinding) {
    match binding {
        PostgresBinding::UriSecret {
            secret_name,
            uri_key,
        } => {
            env.push(secret_env_var("WADDLE_DATABASE_URL", secret_name, uri_key));
        }
        PostgresBinding::BasicAuthSecret {
            secret_name,
            username_key,
            password_key,
            host,
            port,
            database,
        } => {
            env.push(secret_env_var(
                "WADDLE_DB_USERNAME",
                secret_name,
                username_key,
            ));
            env.push(secret_env_var(
                "WADDLE_DB_PASSWORD",
                secret_name,
                password_key,
            ));
            env.push(plain_env_var(
                "WADDLE_DATABASE_URL",
                &format!("postgresql://$(WADDLE_DB_USERNAME):$(WADDLE_DB_PASSWORD)@{host}:{port}/{database}"),
            ));
        }
        PostgresBinding::RestrictedRuntime { spec } => {
            env.push(secret_env_var(
                "WADDLE_RUNTIME_DB_PASSWORD",
                &spec.runtime_credential_secret,
                &spec.runtime_credential_key,
            ));
            env.push(plain_env_var(
                "WADDLE_DATABASE_URL",
                &format!(
                    "postgresql://{}:$(WADDLE_RUNTIME_DB_PASSWORD)@{}:{}/{}?sslmode=verify-full&sslrootcert=/etc/waddle/postgres-ca/{}&options=-c%20search_path%3D{}",
                    spec.roles.runtime_login,
                    spec.database_host,
                    spec.database_port,
                    spec.database_name,
                    spec.ca_secret_key,
                    spec.schema,
                ),
            ));
            env.push(EnvVar {
                name: "WADDLE_DB_RUNTIME_ATTESTATION".to_owned(),
                value_from: Some(EnvVarSource {
                    config_map_key_ref: Some(ConfigMapKeySelector {
                        name: spec.handoff_config_map_name.clone(),
                        key: "handoff.json".to_owned(),
                        optional: Some(false),
                    }),
                    ..EnvVarSource::default()
                }),
                ..EnvVar::default()
            });
            env.push(plain_env_var("WADDLE_GIT_SHA", &spec.release_git_sha));
            env.push(plain_env_var(
                "WADDLE_DB_MIGRATOR_LOGIN",
                &spec.roles.migrator_login,
            ));
            env.push(plain_env_var(
                "WADDLE_XMPP_MAM_SCHEMA_OWNER",
                &spec.roles.schema_owner,
            ));
            env.push(plain_env_var("WADDLE_XMPP_MAM_CLAIM_SCHEMA", &spec.schema));
        }
    }
}

fn build_storage_and_mounts(
    ws: &WaddleServer,
    bindings: &ResolvedRuntimeBindings,
) -> Result<(Vec<Volume>, Vec<VolumeMount>), OperatorError> {
    let data_volume = if ws.spec.persistence.enabled {
        Volume {
            name: "data".to_owned(),
            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                claim_name: labels::pvc_name(ws)?,
                ..PersistentVolumeClaimVolumeSource::default()
            }),
            ..Volume::default()
        }
    } else {
        Volume {
            name: "data".to_owned(),
            empty_dir: Some(EmptyDirVolumeSource::default()),
            ..Volume::default()
        }
    };

    let mut volumes = vec![data_volume];
    let mut mounts = vec![VolumeMount {
        name: "data".to_owned(),
        mount_path: ws.spec.persistence.mount_path.clone(),
        ..VolumeMount::default()
    }];

    if ws.spec.xmpp.enabled {
        if let Some(tls_secret) = &ws.spec.xmpp.tls_secret_name {
            volumes.push(Volume {
                name: "xmpp-tls".to_owned(),
                secret: Some(SecretVolumeSource {
                    secret_name: Some(tls_secret.clone()),
                    ..SecretVolumeSource::default()
                }),
                ..Volume::default()
            });
            mounts.push(VolumeMount {
                name: "xmpp-tls".to_owned(),
                mount_path: "/etc/waddle/tls".to_owned(),
                read_only: Some(true),
                ..VolumeMount::default()
            });
        }
    }

    if let PostgresBinding::RestrictedRuntime { spec } = &bindings.postgres {
        volumes.push(Volume {
            name: "postgres-ca".to_owned(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(spec.ca_secret_name.clone()),
                ..SecretVolumeSource::default()
            }),
            ..Volume::default()
        });
        mounts.push(VolumeMount {
            name: "postgres-ca".to_owned(),
            mount_path: "/etc/waddle/postgres-ca".to_owned(),
            read_only: Some(true),
            ..VolumeMount::default()
        });
    }

    if let Some(eso) = &ws.spec.secrets.external_secrets {
        if let Some(apns) = &eso.apns_secret_ref {
            volumes.push(Volume {
                name: "apns-auth-key".to_owned(),
                secret: Some(SecretVolumeSource {
                    secret_name: Some(apns.secret_name.clone()),
                    default_mode: Some(0o440),
                    items: Some(vec![KeyToPath {
                        key: apns.secret_key.clone(),
                        path: "AuthKey.p8".to_owned(),
                        ..KeyToPath::default()
                    }]),
                    ..SecretVolumeSource::default()
                }),
                ..Volume::default()
            });
            mounts.push(VolumeMount {
                name: "apns-auth-key".to_owned(),
                mount_path: "/var/run/secrets/waddle-apns".to_owned(),
                read_only: Some(true),
                ..VolumeMount::default()
            });
        }
    }

    if ws.spec.s2s.enabled {
        if let Some(cm_name) = &ws.spec.s2s.config_map_name {
            volumes.push(Volume {
                name: "s2s-config".to_owned(),
                config_map: Some(ConfigMapVolumeSource {
                    name: cm_name.clone(),
                    ..ConfigMapVolumeSource::default()
                }),
                ..Volume::default()
            });
            mounts.push(VolumeMount {
                name: "s2s-config".to_owned(),
                mount_path: "/etc/waddle/s2s".to_owned(),
                read_only: Some(true),
                ..VolumeMount::default()
            });
        }
    }

    Ok((volumes, mounts))
}

fn build_probe(spec: &waddle_operator_crd::HttpProbeSpec) -> Probe {
    Probe {
        http_get: Some(HTTPGetAction {
            path: Some(spec.path.clone()),
            port: IntOrString::String("http".to_owned()),
            ..HTTPGetAction::default()
        }),
        initial_delay_seconds: Some(spec.initial_delay_seconds),
        period_seconds: Some(spec.period_seconds),
        timeout_seconds: Some(spec.timeout_seconds),
        failure_threshold: Some(spec.failure_threshold),
        ..Probe::default()
    }
}

fn build_resources(spec: &waddle_operator_crd::ResourceRequirementsSpec) -> ResourceRequirements {
    let mut requests = BTreeMap::new();
    requests.insert("cpu".to_owned(), Quantity(spec.requests.cpu.clone()));
    requests.insert("memory".to_owned(), Quantity(spec.requests.memory.clone()));

    let mut limits = BTreeMap::new();
    limits.insert("cpu".to_owned(), Quantity(spec.limits.cpu.clone()));
    limits.insert("memory".to_owned(), Quantity(spec.limits.memory.clone()));

    ResourceRequirements {
        requests: Some(requests),
        limits: Some(limits),
        ..ResourceRequirements::default()
    }
}

fn secret_env_var(name: &str, secret_name: &str, key: &str) -> EnvVar {
    EnvVar {
        name: name.to_owned(),
        value_from: Some(EnvVarSource {
            secret_key_ref: Some(SecretKeySelector {
                name: secret_name.to_owned(),
                key: key.to_owned(),
                optional: Some(false),
            }),
            ..EnvVarSource::default()
        }),
        ..EnvVar::default()
    }
}

fn plain_env_var(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: name.to_owned(),
        value: Some(value.to_owned()),
        ..EnvVar::default()
    }
}

/// Computes a deterministic SHA-256 hex digest of a `DeploymentSpec` and companion workload specs.
///
/// Covers `DeploymentSpec` (with `replicas: ws.spec.replicas`, keeping HPA scale changes free of
/// drift) together with `service`, `pdb`, `network_policy`, `s2s`, and `persistence` so edits to
/// any owned child manifest trigger workload reconciliation.
///
/// # Errors
/// Returns [`OperatorError::Serialization`] if JSON serialization fails.
pub fn compute_workload_hash(
    ws: &WaddleServer,
    spec: &DeploymentSpec,
) -> Result<ContentDigest, OperatorError> {
    use sha2::Digest;

    let payload = (
        spec,
        &ws.spec.service,
        &ws.spec.pdb,
        &ws.spec.network_policy,
        &ws.spec.s2s,
        &ws.spec.persistence,
    );
    let bytes = serde_json::to_vec(&payload)
        .map_err(|err| OperatorError::Serialization(err.to_string()))?;
    let mut hasher = sha2::Sha256::new();
    hasher.update(&bytes);
    let hex_digest = hex::encode(hasher.finalize());
    ContentDigest::try_new(&hex_digest).map_err(|err| OperatorError::Serialization(err.to_string()))
}
