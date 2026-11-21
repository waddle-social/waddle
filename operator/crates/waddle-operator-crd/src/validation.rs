use std::collections::BTreeSet;
use std::fmt;

use url::Url;

use crate::dependencies::{
    PostgresDependencySpec, PostgresSourceMode, RestrictedRuntimeSpec, SpiceDbDependencySpec,
    SpiceDbSourceMode,
};
use crate::identifiers::{
    DeploymentUuid, GitSha, ImageDigest, PgIdentifier, ResourceName, XmppDomain,
};
use crate::secrets::{SecretProviderMode, SecretsSpec};
use crate::server_config::{
    PersistenceAccessMode, UpdateStrategyKind, SHUTDOWN_KILL_MARGIN_SECS,
    SHUTDOWN_RETIRE_BUDGET_SECS, SHUTDOWN_TERMINAL_SWEEP_BUDGET_SECS,
};
use crate::waddle_server::WaddleServerSpec;

const MAX_WAIT_SECS: u64 = 300;
const WS_KEEPALIVE_MAX_INTERVAL_SECS: u64 = 120;
const WS_KEEPALIVE_MAX_MISS_LIMIT: u64 = 10;
const MAX_INGRESS_KNOB: u32 = 32;

const RESERVED_OPERATOR_ENV_KEYS: &[&str] = &[
    "WADDLE_SESSION_KEY",
    "WADDLE_OCCUPANT_ID_SECRET",
    "WADDLE_DATABASE_URL",
    "WADDLE_SPICEDB_PRESHARED_KEY",
    "WADDLE_CLUSTERING_KEYPAIR_POOL",
    "WADDLE_DB_RUNTIME_ATTESTATION",
    "WADDLE_RUNTIME_DB_PASSWORD",
    "WADDLE_DB_MIGRATOR_LOGIN",
    "WADDLE_XMPP_MAM_SCHEMA_OWNER",
    "WADDLE_XMPP_MAM_CLAIM_SCHEMA",
];

/// Individual rule failure returned by [`validate_spec`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValidationViolation {
    /// Replica count cannot be negative.
    NegativeReplicas {
        /// Supplied replica count.
        replicas: i32,
    },
    /// Multi-replica deployments require clustering enabled.
    MultiReplicaRequiresClustering {
        /// Supplied replica count.
        replicas: i32,
    },
    /// YOLO secret generation is restricted to single-replica non-clustered deployments.
    YoloForbidsClusteringOrMultiReplica,
    /// `terminationGracePeriodSeconds` is below the required ordered-shutdown floor.
    TerminationGraceBelowFloor {
        /// Configured grace period.
        configured: i64,
        /// Minimum safe floor in seconds.
        minimum_floor: i64,
    },
    /// Negative shutdown or claim-release timer.
    NegativeShutdownDuration {
        /// Field name.
        field: &'static str,
    },
    /// `RollingUpdate` with an RWO PVC can deadlock on Multi-Attach.
    RollingUpdateWithRwoPvc,
    /// `replicas > 1` with an RWO PVC can deadlock scheduling/attachment.
    MultiReplicaWithRwoPvc,
    /// Clustering requires a valid `deployment.uuid`.
    ClusteringRequiresDeploymentUuid,
    /// Clustering requires S3 or `ReadWriteMany` persistence.
    ClusteringRequiresSharedStorage,
    /// Clustering requires at least one listen multiaddr.
    ClusteringRequiresListenAddrs,
    /// Enrolled clustering keypool count is below `replicas + maxSurge + headroom`.
    InsufficientClusteringKeypool {
        /// Configured `enrolledKeyCount`.
        enrolled: i32,
        /// Minimum required key count.
        required: i32,
    },
    /// Non-YOLO deployments require `deployment.uuid` for DB lineage attestation.
    DeploymentUuidRequiredInEsoMode,
    /// Invalid `deployment.uuid` format.
    InvalidDeploymentUuid,
    /// Invalid `deployment.lineageAction` syntax.
    InvalidLineageAction,
    /// `deployment.lineageAction` requires `deployment.uuid`.
    LineageActionRequiresDeploymentUuid,
    /// `deployment.lineageAction` is prohibited with `restrictedRuntime`.
    LineageActionForbiddenInRestrictedRuntime,
    /// Invalid `restrictedRuntime` setting.
    InvalidRestrictedRuntime {
        /// Description of the invalid field.
        detail: String,
    },
    /// Invalid `image.digest` or zero digest.
    InvalidImageDigest,
    /// `baseUrl` must be an `http://` or `https://` URL with host.
    InvalidBaseUrl,
    /// `xmpp.domain` failed canonical domain validation.
    InvalidXmppDomain,
    /// `xmpp.publicWebsocketUrl` must be `ws://` or `wss://` without userinfo/query/fragment.
    InvalidPublicWebsocketUrl,
    /// Invalid Kubernetes resource name reference.
    InvalidResourceName {
        /// Field path.
        field: &'static str,
    },
    /// Discriminated dependency or secrets struct is inconsistent with its `mode`.
    InconsistentDiscriminatedSpec {
        /// Section name.
        section: &'static str,
        /// Explanation.
        detail: &'static str,
    },
    /// `s3` endpoint and bucket must both be non-empty.
    IncompleteS3Config,
    /// `s2s.enabled` requires `s2s.configMapName`.
    S2sRequiresConfigMap,
    /// Numeric configuration field is outside the range accepted by `waddle-server`.
    OutOfRange {
        /// Field name.
        field: &'static str,
        /// Minimum inclusive bound.
        min: u64,
        /// Maximum inclusive bound.
        max: u64,
        /// Supplied value.
        actual: u64,
    },
    /// `extraEnv` attempted to overwrite an operator-managed key.
    ReservedExtraEnvKey {
        /// Colliding key name.
        key: String,
    },
}

impl fmt::Display for ValidationViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NegativeReplicas { replicas } => {
                write!(f, "spec.replicas ({replicas}) must be >= 0")
            }
            Self::MultiReplicaRequiresClustering { replicas } => write!(
                f,
                "spec.replicas ({replicas}) > 1 requires spec.clustering.enabled=true"
            ),
            Self::YoloForbidsClusteringOrMultiReplica => write!(
                f,
                "Yolo secret mode only supports single-replica non-clustered deployments (replicas <= 1 and clustering.enabled=false)"
            ),
            Self::TerminationGraceBelowFloor {
                configured,
                minimum_floor,
            } => write!(
                f,
                "spec.shutdown.terminationGracePeriodSeconds ({configured}) must be >= {minimum_floor}"
            ),
            Self::NegativeShutdownDuration { field } => {
                write!(f, "{field} must be >= 0")
            }
            Self::RollingUpdateWithRwoPvc => write!(
                f,
                "spec.updateStrategy=RollingUpdate with a ReadWriteOnce PVC can deadlock on Multi-Attach; use Recreate, ReadWriteMany, or persistence.allowUnsafeRwoScale=true"
            ),
            Self::MultiReplicaWithRwoPvc => write!(
                f,
                "spec.replicas > 1 with a ReadWriteOnce PVC is unschedulable; use ReadWriteMany, disable persistence, or set persistence.allowUnsafeRwoScale=true"
            ),
            Self::ClusteringRequiresDeploymentUuid => {
                write!(f, "spec.clustering.enabled=true requires spec.deployment.uuid")
            }
            Self::ClusteringRequiresSharedStorage => write!(
                f,
                "spec.clustering.enabled=true requires spec.s3 or persistence.accessModes containing ReadWriteMany"
            ),
            Self::ClusteringRequiresListenAddrs => {
                write!(f, "spec.clustering.enabled=true requires at least one listenAddrs entry")
            }
            Self::InsufficientClusteringKeypool { enrolled, required } => write!(
                f,
                "spec.clustering.enrolledKeyCount ({enrolled}) must be >= {required} (replicas + maxSurge + keypairHeadroom)"
            ),
            Self::DeploymentUuidRequiredInEsoMode => {
                write!(f, "spec.deployment.uuid is required in ExternalSecrets mode")
            }
            Self::InvalidDeploymentUuid => {
                write!(f, "spec.deployment.uuid must be a lower-case RFC-4122 UUID")
            }
            Self::InvalidLineageAction => write!(
                f,
                "spec.deployment.lineageAction must be 'enroll' or 'adopt=<uuid>[,<uuid>...]'"
            ),
            Self::LineageActionRequiresDeploymentUuid => write!(
                f,
                "spec.deployment.lineageAction requires spec.deployment.uuid"
            ),
            Self::LineageActionForbiddenInRestrictedRuntime => write!(
                f,
                "spec.deployment.lineageAction cannot be set when restrictedRuntime.enabled=true"
            ),
            Self::InvalidRestrictedRuntime { detail } => {
                write!(f, "spec.postgres.restrictedRuntime invalid: {detail}")
            }
            Self::InvalidImageDigest => {
                write!(f, "spec.image.digest must match sha256:<64 lower-case hex> and not be zero")
            }
            Self::InvalidBaseUrl => {
                write!(f, "spec.baseUrl must be a valid http:// or https:// URL with a host")
            }
            Self::InvalidXmppDomain => {
                write!(f, "spec.xmpp.domain must be a valid canonical lower-case DNS domain")
            }
            Self::InvalidPublicWebsocketUrl => write!(
                f,
                "spec.xmpp.publicWebsocketUrl must use ws:// or wss:// with host and without userinfo/query/fragment"
            ),
            Self::InvalidResourceName { field } => {
                write!(f, "{field} must be a valid lower-case Kubernetes resource name")
            }
            Self::InconsistentDiscriminatedSpec { section, detail } => {
                write!(f, "{section}: {detail}")
            }
            Self::IncompleteS3Config => {
                write!(f, "spec.s3.endpoint and spec.s3.bucket must both be non-empty")
            }
            Self::S2sRequiresConfigMap => {
                write!(f, "spec.s2s.enabled=true requires spec.s2s.configMapName")
            }
            Self::OutOfRange {
                field,
                min,
                max,
                actual,
            } => write!(f, "{field} ({actual}) must be between {min} and {max}"),
            Self::ReservedExtraEnvKey { key } => write!(
                f,
                "spec.extraEnv cannot override operator-managed variable {key}"
            ),
        }
    }
}

/// Aggregated validation errors for a `WaddleServerSpec`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid WaddleServer specification ({} violation(s))", .violations.len())]
pub struct ValidationReport {
    violations: Vec<ValidationViolation>,
}

impl ValidationReport {
    /// Returns the ordered list of spec validation violations.
    #[must_use]
    pub fn violations(&self) -> &[ValidationViolation] {
        &self.violations
    }

    /// Renders concise, semicolon-delimited diagnostic messages safe for CRD status/events.
    #[must_use]
    pub fn summary(&self) -> String {
        self.violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Normalized values derived from a validated [`WaddleServerSpec`].
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSpec {
    /// Effective desired replica count (`1` when omitted for HPA).
    pub effective_replicas: i32,
    /// Enforced pod `terminationGracePeriodSeconds`.
    pub termination_grace_period_seconds: i64,
    /// Parsed deployment UUID if supplied in spec.
    pub deployment_uuid: Option<DeploymentUuid>,
    /// Canonical XMPP domain.
    pub xmpp_domain: XmppDomain,
}

/// Validates a [`WaddleServerSpec`] against all server startup and Kubernetes invariants.
///
/// # Errors
/// Returns a [`ValidationReport`] containing every detected violation when invalid.
pub fn validate_spec(spec: &WaddleServerSpec) -> Result<ValidatedSpec, ValidationReport> {
    let mut violations = Vec::new();
    let effective_replicas = spec.effective_replicas();

    validate_replicas_and_clustering(spec, effective_replicas, &mut violations);
    let termination_grace_period_seconds = validate_shutdown_floor(spec, &mut violations);
    validate_persistence_and_strategy(spec, effective_replicas, &mut violations);
    let deployment_uuid = validate_deployment_identity(spec, &mut violations);
    validate_image_and_urls(spec, &mut violations);
    let xmpp_domain = validate_xmpp(spec, &mut violations);
    validate_postgres(&spec.postgres, &mut violations);
    validate_spicedb(&spec.spicedb, &mut violations);
    validate_secrets(&spec.secrets, &mut violations);
    validate_numeric_knobs(spec, &mut violations);
    validate_s3_and_s2s_and_env(spec, &mut violations);

    if !violations.is_empty() {
        return Err(ValidationReport { violations });
    }

    Ok(ValidatedSpec {
        effective_replicas,
        termination_grace_period_seconds,
        deployment_uuid,
        xmpp_domain: xmpp_domain.unwrap_or_else(|| {
            // Unreachable when violations is empty; domain was validated above.
            XmppDomain::try_new("localhost").unwrap_or_else(|_| unreachable!())
        }),
    })
}

fn validate_replicas_and_clustering(
    spec: &WaddleServerSpec,
    effective_replicas: i32,
    violations: &mut Vec<ValidationViolation>,
) {
    if let Some(replicas) = spec.replicas {
        if replicas < 0 {
            violations.push(ValidationViolation::NegativeReplicas { replicas });
        }
    }

    if effective_replicas > 1 && !spec.clustering.enabled {
        violations.push(ValidationViolation::MultiReplicaRequiresClustering {
            replicas: effective_replicas,
        });
    }

    if spec.secrets.mode == SecretProviderMode::Yolo
        && (effective_replicas > 1 || spec.clustering.enabled)
    {
        violations.push(ValidationViolation::YoloForbidsClusteringOrMultiReplica);
    }

    if spec.clustering.enabled {
        if spec
            .deployment
            .uuid
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_none()
        {
            violations.push(ValidationViolation::ClusteringRequiresDeploymentUuid);
        }

        let has_s3 = spec
            .s3
            .as_ref()
            .is_some_and(|s3| !s3.endpoint.trim().is_empty() && !s3.bucket.trim().is_empty());
        let has_rwx = spec
            .persistence
            .access_modes
            .contains(&PersistenceAccessMode::ReadWriteMany);
        if !has_s3 && !has_rwx {
            violations.push(ValidationViolation::ClusteringRequiresSharedStorage);
        }

        if spec.clustering.listen_addrs.is_empty() {
            violations.push(ValidationViolation::ClusteringRequiresListenAddrs);
        }

        if spec.secrets.mode == SecretProviderMode::ExternalSecrets {
            let max_surge = match spec.update_strategy {
                UpdateStrategyKind::RollingUpdate => 1,
                UpdateStrategyKind::Recreate => 0,
            };
            let required = effective_replicas
                .saturating_add(max_surge)
                .saturating_add(spec.clustering.keypair_headroom);
            if spec.clustering.enrolled_key_count < required {
                violations.push(ValidationViolation::InsufficientClusteringKeypool {
                    enrolled: spec.clustering.enrolled_key_count,
                    required,
                });
            }
        }
    }
}

fn validate_shutdown_floor(
    spec: &WaddleServerSpec,
    violations: &mut Vec<ValidationViolation>,
) -> i64 {
    if spec.shutdown.pre_stop_sleep_seconds < 0 {
        violations.push(ValidationViolation::NegativeShutdownDuration {
            field: "spec.shutdown.preStopSleepSeconds",
        });
    }
    if spec.shutdown.drain_timeout_seconds < 0 {
        violations.push(ValidationViolation::NegativeShutdownDuration {
            field: "spec.shutdown.drainTimeoutSeconds",
        });
    }
    if spec.clustering.claim_release_budget_seconds < 0 {
        violations.push(ValidationViolation::NegativeShutdownDuration {
            field: "spec.clustering.claimReleaseBudgetSeconds",
        });
    }

    let floor = spec
        .shutdown
        .pre_stop_sleep_seconds
        .max(0)
        .saturating_add(spec.shutdown.drain_timeout_seconds.max(0))
        .saturating_add(spec.clustering.claim_release_budget_seconds.max(0))
        .saturating_add(SHUTDOWN_RETIRE_BUDGET_SECS)
        .saturating_add(SHUTDOWN_TERMINAL_SWEEP_BUDGET_SECS)
        .saturating_add(SHUTDOWN_KILL_MARGIN_SECS);

    match spec.shutdown.termination_grace_period_seconds {
        Some(configured) if configured < floor => {
            violations.push(ValidationViolation::TerminationGraceBelowFloor {
                configured,
                minimum_floor: floor,
            });
            floor
        }
        Some(configured) => configured,
        None => floor,
    }
}

fn validate_persistence_and_strategy(
    spec: &WaddleServerSpec,
    effective_replicas: i32,
    violations: &mut Vec<ValidationViolation>,
) {
    let has_rwo = spec
        .persistence
        .access_modes
        .contains(&PersistenceAccessMode::ReadWriteOnce);
    if spec.persistence.enabled && has_rwo && !spec.persistence.allow_unsafe_rwo_scale {
        if spec.update_strategy == UpdateStrategyKind::RollingUpdate {
            violations.push(ValidationViolation::RollingUpdateWithRwoPvc);
        }
        if effective_replicas > 1 {
            violations.push(ValidationViolation::MultiReplicaWithRwoPvc);
        }
    }
    if let Some(claim) = spec.persistence.existing_claim.as_deref() {
        validate_resource_name(claim, "spec.persistence.existingClaim", violations);
    }
}

fn validate_deployment_identity(
    spec: &WaddleServerSpec,
    violations: &mut Vec<ValidationViolation>,
) -> Option<DeploymentUuid> {
    let parsed_uuid = match spec.deployment.uuid.as_deref() {
        Some(raw) if !raw.trim().is_empty() => match DeploymentUuid::parse_exact(raw) {
            Ok(uuid) => Some(uuid),
            Err(_) => {
                violations.push(ValidationViolation::InvalidDeploymentUuid);
                None
            }
        },
        _ => {
            if spec.secrets.mode == SecretProviderMode::ExternalSecrets {
                violations.push(ValidationViolation::DeploymentUuidRequiredInEsoMode);
            }
            None
        }
    };

    if let Some(raw_action) = spec.deployment.lineage_action.as_deref() {
        let trimmed = raw_action.trim();
        if trimmed.is_empty() {
            violations.push(ValidationViolation::InvalidLineageAction);
        } else {
            if parsed_uuid.is_none() {
                violations.push(ValidationViolation::LineageActionRequiresDeploymentUuid);
            }
            let restricted = spec
                .postgres
                .restricted_runtime
                .as_ref()
                .is_some_and(|r| r.enabled);
            if restricted {
                violations.push(ValidationViolation::LineageActionForbiddenInRestrictedRuntime);
            }
            if trimmed != "enroll" {
                let valid_adopt = trimmed.strip_prefix("adopt=").is_some_and(|list| {
                    !list.is_empty()
                        && list
                            .split(',')
                            .all(|entry| DeploymentUuid::parse_exact(entry.trim()).is_ok())
                });
                if !valid_adopt {
                    violations.push(ValidationViolation::InvalidLineageAction);
                }
            }
        }
    }

    parsed_uuid
}

fn validate_image_and_urls(spec: &WaddleServerSpec, violations: &mut Vec<ValidationViolation>) {
    if let Some(digest) = spec.image.digest.as_deref() {
        if ImageDigest::try_new(digest).is_err() {
            violations.push(ValidationViolation::InvalidImageDigest);
        }
    }
    let valid = Url::parse(spec.base_url.trim())
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some());
    if !valid {
        violations.push(ValidationViolation::InvalidBaseUrl);
    }
}

fn validate_xmpp(
    spec: &WaddleServerSpec,
    violations: &mut Vec<ValidationViolation>,
) -> Option<XmppDomain> {
    let domain = match XmppDomain::try_new(spec.xmpp.domain.trim()) {
        Ok(domain) => Some(domain),
        Err(_) => {
            violations.push(ValidationViolation::InvalidXmppDomain);
            None
        }
    };

    if let Some(ws_raw) = spec.xmpp.public_websocket_url.as_deref() {
        let valid = Url::parse(ws_raw.trim()).is_ok_and(|url| {
            matches!(url.scheme(), "ws" | "wss")
                && url.host().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
        });
        if !valid {
            violations.push(ValidationViolation::InvalidPublicWebsocketUrl);
        }
    }

    if let Some(tls_secret) = spec.xmpp.tls_secret_name.as_deref() {
        validate_resource_name(tls_secret, "spec.xmpp.tlsSecretName", violations);
    }

    domain
}

fn validate_postgres(postgres: &PostgresDependencySpec, violations: &mut Vec<ValidationViolation>) {
    match (postgres.mode, &postgres.cnpg, &postgres.external) {
        (PostgresSourceMode::CloudNativePg, Some(cnpg), None) => {
            validate_resource_name(
                &cnpg.cluster_name,
                "spec.postgres.cnpg.clusterName",
                violations,
            );
            if let Some(secret) = cnpg.secret_name.as_deref() {
                validate_resource_name(secret, "spec.postgres.cnpg.secretName", violations);
            }
        }
        (PostgresSourceMode::External, None, Some(ext)) => {
            validate_resource_name(
                &ext.secret_name,
                "spec.postgres.external.secretName",
                violations,
            );
            if let Some(svc) = ext.service_name.as_deref() {
                validate_resource_name(svc, "spec.postgres.external.serviceName", violations);
            }
        }
        _ => violations.push(ValidationViolation::InconsistentDiscriminatedSpec {
            section: "spec.postgres",
            detail: "mode must match the populated cnpg or external block exclusively",
        }),
    }

    if let Some(timeout) = postgres.acquire_timeout_secs {
        if !(1..=MAX_WAIT_SECS).contains(&timeout) {
            violations.push(ValidationViolation::OutOfRange {
                field: "spec.postgres.acquireTimeoutSecs",
                min: 1,
                max: MAX_WAIT_SECS,
                actual: timeout,
            });
        }
    }

    if let Some(restricted) = &postgres.restricted_runtime {
        if restricted.enabled {
            validate_restricted_runtime(restricted, violations);
        }
    }
}

fn validate_restricted_runtime(
    restricted: &RestrictedRuntimeSpec,
    violations: &mut Vec<ValidationViolation>,
) {
    if GitSha::try_new(&restricted.release_git_sha).is_err() {
        violations.push(ValidationViolation::InvalidRestrictedRuntime {
            detail: "releaseGitSha must be a 40-character lower-case hexadecimal SHA".to_owned(),
        });
    }
    let pg_checks = [
        ("schema", &restricted.schema),
        ("databaseName", &restricted.database_name),
        ("roles.migratorLogin", &restricted.roles.migrator_login),
        ("roles.schemaOwner", &restricted.roles.schema_owner),
        ("roles.runtimeLogin", &restricted.roles.runtime_login),
        (
            "roles.attestorInstallerLogin",
            &restricted.roles.attestor_installer_login,
        ),
        (
            "roles.attestorDefinerRole",
            &restricted.roles.attestor_definer_role,
        ),
        ("roles.attestorSchema", &restricted.roles.attestor_schema),
    ];
    for (field, value) in pg_checks {
        if PgIdentifier::try_new(value).is_err() {
            violations.push(ValidationViolation::InvalidRestrictedRuntime {
                detail: format!("{field} must be a lower-case PostgreSQL identifier"),
            });
        }
    }
    if restricted.database_host.trim().is_empty() {
        violations.push(ValidationViolation::InvalidRestrictedRuntime {
            detail: "databaseHost must not be empty".to_owned(),
        });
    }
    validate_resource_name(
        &restricted.ca_secret_name,
        "spec.postgres.restrictedRuntime.caSecretName",
        violations,
    );
    validate_resource_name(
        &restricted.runtime_credential_secret,
        "spec.postgres.restrictedRuntime.runtimeCredentialSecret",
        violations,
    );
    validate_resource_name(
        &restricted.handoff_config_map_name,
        "spec.postgres.restrictedRuntime.handoffConfigMapName",
        violations,
    );
}

fn validate_spicedb(spicedb: &SpiceDbDependencySpec, violations: &mut Vec<ValidationViolation>) {
    match (spicedb.mode, &spicedb.operator, &spicedb.external) {
        (SpiceDbSourceMode::SpiceDbOperator, Some(op), None) => {
            validate_resource_name(
                &op.cluster_name,
                "spec.spicedb.operator.clusterName",
                violations,
            );
            if let Some(svc) = op.service_name.as_deref() {
                validate_resource_name(svc, "spec.spicedb.operator.serviceName", violations);
            }
            if let Some(secret_ref) = &op.preshared_key_secret_ref {
                validate_resource_name(
                    &secret_ref.name,
                    "spec.spicedb.operator.presharedKeySecretRef.name",
                    violations,
                );
            }
        }
        (SpiceDbSourceMode::External, None, Some(ext)) => {
            if Url::parse(ext.endpoint.trim()).is_err() {
                violations.push(ValidationViolation::InconsistentDiscriminatedSpec {
                    section: "spec.spicedb.external.endpoint",
                    detail: "endpoint must be a valid URL",
                });
            }
            if let Some(svc) = ext.service_name.as_deref() {
                validate_resource_name(svc, "spec.spicedb.external.serviceName", violations);
            }
            if let Some(secret_ref) = &ext.preshared_key_secret_ref {
                validate_resource_name(
                    &secret_ref.name,
                    "spec.spicedb.external.presharedKeySecretRef.name",
                    violations,
                );
            }
        }
        _ => violations.push(ValidationViolation::InconsistentDiscriminatedSpec {
            section: "spec.spicedb",
            detail: "mode must match the populated operator or external block exclusively",
        }),
    }
}

fn validate_secrets(secrets: &SecretsSpec, violations: &mut Vec<ValidationViolation>) {
    match (secrets.mode, &secrets.external_secrets, &secrets.yolo) {
        (SecretProviderMode::ExternalSecrets, Some(eso), None) => {
            match (&eso.external_secret_name, &eso.managed) {
                (Some(name), None) => {
                    validate_resource_name(
                        name,
                        "spec.secrets.externalSecrets.externalSecretName",
                        violations,
                    );
                }
                (None, Some(managed)) => {
                    validate_resource_name(
                        &managed.secret_store_ref.name,
                        "spec.secrets.externalSecrets.managed.secretStoreRef.name",
                        violations,
                    );
                    if let Some(target) = managed.target_secret_name.as_deref() {
                        validate_resource_name(
                            target,
                            "spec.secrets.externalSecrets.managed.targetSecretName",
                            violations,
                        );
                    }
                }
                _ => violations.push(ValidationViolation::InconsistentDiscriminatedSpec {
                    section: "spec.secrets.externalSecrets",
                    detail: "exactly one of externalSecretName or managed must be set",
                }),
            }
            if let Some(pool_secret) = eso.clustering_keypool_secret_name.as_deref() {
                validate_resource_name(
                    pool_secret,
                    "spec.secrets.externalSecrets.clusteringKeypoolSecretName",
                    violations,
                );
            }
            if let Some(apns) = &eso.apns_secret_ref {
                validate_resource_name(
                    &apns.secret_name,
                    "spec.secrets.externalSecrets.apnsSecretRef.secretName",
                    violations,
                );
            }
            for extra in &eso.extra_secret_refs {
                validate_resource_name(
                    extra,
                    "spec.secrets.externalSecrets.extraSecretRefs",
                    violations,
                );
            }
        }
        (SecretProviderMode::Yolo, None, maybe_yolo) => {
            if let Some(yolo) = maybe_yolo {
                if let Some(name) = yolo.secret_name.as_deref() {
                    validate_resource_name(name, "spec.secrets.yolo.secretName", violations);
                }
                for extra in &yolo.extra_secret_refs {
                    validate_resource_name(extra, "spec.secrets.yolo.extraSecretRefs", violations);
                }
            }
        }
        _ => violations.push(ValidationViolation::InconsistentDiscriminatedSpec {
            section: "spec.secrets",
            detail: "mode must match the populated externalSecrets or yolo block exclusively",
        }),
    }
}

fn validate_numeric_knobs(spec: &WaddleServerSpec, violations: &mut Vec<ValidationViolation>) {
    if !(1..=WS_KEEPALIVE_MAX_INTERVAL_SECS).contains(&spec.ws_keepalive.interval_secs) {
        violations.push(ValidationViolation::OutOfRange {
            field: "spec.wsKeepalive.intervalSecs",
            min: 1,
            max: WS_KEEPALIVE_MAX_INTERVAL_SECS,
            actual: spec.ws_keepalive.interval_secs,
        });
    }
    if !(1..=WS_KEEPALIVE_MAX_MISS_LIMIT).contains(&spec.ws_keepalive.miss_limit) {
        violations.push(ValidationViolation::OutOfRange {
            field: "spec.wsKeepalive.missLimit",
            min: 1,
            max: WS_KEEPALIVE_MAX_MISS_LIMIT,
            actual: spec.ws_keepalive.miss_limit,
        });
    }
    if !(1..=MAX_INGRESS_KNOB).contains(&spec.ingress.pool_size) {
        violations.push(ValidationViolation::OutOfRange {
            field: "spec.ingress.poolSize",
            min: 1,
            max: u64::from(MAX_INGRESS_KNOB),
            actual: u64::from(spec.ingress.pool_size),
        });
    }
    if !(1..=MAX_INGRESS_KNOB).contains(&spec.ingress.retry_attempts) {
        violations.push(ValidationViolation::OutOfRange {
            field: "spec.ingress.retryAttempts",
            min: 1,
            max: u64::from(MAX_INGRESS_KNOB),
            actual: u64::from(spec.ingress.retry_attempts),
        });
    }
}

fn validate_s3_and_s2s_and_env(spec: &WaddleServerSpec, violations: &mut Vec<ValidationViolation>) {
    if let Some(s3) = &spec.s3 {
        if s3.endpoint.trim().is_empty() || s3.bucket.trim().is_empty() {
            violations.push(ValidationViolation::IncompleteS3Config);
        }
    }

    if spec.s2s.enabled {
        match spec.s2s.config_map_name.as_deref() {
            Some(cm) => validate_resource_name(cm, "spec.s2s.configMapName", violations),
            None => violations.push(ValidationViolation::S2sRequiresConfigMap),
        }
    }

    let reserved: BTreeSet<&str> = RESERVED_OPERATOR_ENV_KEYS.iter().copied().collect();
    for key in spec.extra_env.keys() {
        if reserved.contains(key.as_str()) {
            violations.push(ValidationViolation::ReservedExtraEnvKey { key: key.clone() });
        }
    }
}

fn validate_resource_name(
    raw: &str,
    field: &'static str,
    violations: &mut Vec<ValidationViolation>,
) {
    if ResourceName::try_new(raw).is_err() {
        violations.push(ValidationViolation::InvalidResourceName { field });
    }
}

#[cfg(test)]
mod tests {
    use kube::CustomResourceExt;
    use serde_json::Value;

    use crate::dependencies::{
        CloudNativePgSource, ExternalPostgresSource, ExternalSpiceDbSource, PostgresDependencySpec,
        PostgresSourceMode, RestrictedRuntimeRolesSpec, RestrictedRuntimeSpec,
        SpiceDbDependencySpec, SpiceDbOperatorSource, SpiceDbSourceMode,
    };
    use crate::secrets::{
        EsoApiVersion, ExternalSecretsSpec, SecretProviderMode, SecretsSpec, YoloSecretsSpec,
    };
    use crate::server_config::{
        ClusteringSpec, DeploymentIdentitySpec, ImageSpec, PersistenceAccessMode, PersistenceSpec,
        S2sSpec, S3StorageSpec, ShutdownSpec, UpdateStrategyKind, WsKeepaliveSpec, XmppSpec,
    };
    use crate::waddle_server::{WaddleServer, WaddleServerSpec};

    use super::*;

    const SAMPLE_UUID: &str = "b90424f8-22ca-40bb-a24e-551d171c7b84";

    fn valid_eso_spec() -> WaddleServerSpec {
        WaddleServerSpec {
            replicas: Some(1),
            mode: crate::server_config::ServerMode::HomeServer,
            base_url: "https://xmpp.waddle.social".to_owned(),
            image: ImageSpec::default(),
            xmpp: XmppSpec {
                enabled: true,
                domain: "waddle.social".to_owned(),
                public_websocket_url: Some("wss://xmpp.waddle.social/ws".to_owned()),
                tls_secret_name: None,
                native_auth_enabled: true,
                registration_enabled: false,
            },
            postgres: PostgresDependencySpec {
                mode: PostgresSourceMode::CloudNativePg,
                cnpg: Some(CloudNativePgSource {
                    cluster_name: "postgresql".to_owned(),
                    secret_name: None,
                    uri_key: "uri".to_owned(),
                    username_key: "username".to_owned(),
                    password_key: "password".to_owned(),
                    require_healthy: true,
                }),
                external: None,
                pool_size: 10,
                control_plane_pool_size: 4,
                acquire_timeout_secs: None,
                restricted_runtime: None,
            },
            spicedb: SpiceDbDependencySpec {
                mode: SpiceDbSourceMode::SpiceDbOperator,
                operator: Some(SpiceDbOperatorSource {
                    cluster_name: "spicedb".to_owned(),
                    service_name: None,
                    port: 50051,
                    insecure: None,
                    preshared_key_secret_ref: None,
                    require_ready: true,
                }),
                external: None,
            },
            secrets: SecretsSpec {
                mode: SecretProviderMode::ExternalSecrets,
                external_secrets: Some(ExternalSecretsSpec {
                    external_secret_name: Some("waddle-runtime-secrets".to_owned()),
                    managed: None,
                    api_version: EsoApiVersion::Auto,
                    clustering_keypool_secret_name: None,
                    extra_secret_refs: Vec::new(),
                    apns_secret_ref: None,
                }),
                yolo: None,
            },
            deployment: DeploymentIdentitySpec {
                uuid: Some(SAMPLE_UUID.to_owned()),
                lineage_action: None,
            },
            clustering: ClusteringSpec::default(),
            update_strategy: UpdateStrategyKind::Recreate,
            shutdown: ShutdownSpec::default(),
            s3: None,
            persistence: PersistenceSpec::default(),
            s2s: S2sSpec::default(),
            telemetry: crate::server_config::TelemetrySpec::default(),
            ws_keepalive: WsKeepaliveSpec::default(),
            ingress: crate::server_config::IngressSpec::default(),
            server_owner_localparts: Vec::new(),
            cors_origins: None,
            rust_log: "info".to_owned(),
            extra_env: std::collections::BTreeMap::new(),
            service: crate::server_config::ServiceSpec::default(),
            probes: crate::server_config::ProbesSpec::default(),
            resources: crate::server_config::ResourceRequirementsSpec::default(),
            pdb: crate::server_config::PodDisruptionBudgetSpec::default(),
            network_policy: crate::server_config::NetworkPolicySpec::default(),
            pod_anti_affinity: crate::server_config::PodAntiAffinitySpec::default(),
            node_selector: std::collections::BTreeMap::new(),
            pod_annotations: std::collections::BTreeMap::new(),
            pod_labels: std::collections::BTreeMap::new(),
        }
    }

    fn walk_schema_rejecting_non_structural(node: &Value, path: &str) {
        if let Some(obj) = node.as_object() {
            assert!(
                !obj.contains_key("oneOf"),
                "CRD schema at {path} must not contain non-structural oneOf"
            );
            assert!(
                !obj.contains_key("anyOf"),
                "CRD schema at {path} must not contain anyOf"
            );
            assert_ne!(
                obj.get("additionalProperties").and_then(Value::as_bool),
                Some(false),
                "CRD schema at {path} must not emit additionalProperties: false"
            );
            for (k, v) in obj {
                walk_schema_rejecting_non_structural(v, &format!("{path}.{k}"));
            }
        } else if let Some(arr) = node.as_array() {
            for (idx, item) in arr.iter().enumerate() {
                walk_schema_rejecting_non_structural(item, &format!("{path}[{idx}]"));
            }
        }
    }

    #[test]
    fn crd_schema_is_structural_and_under_etcd_size_budget() {
        let crd = WaddleServer::crd();
        let json_val = serde_json::to_value(&crd).expect("CRD serializes to JSON");
        walk_schema_rejecting_non_structural(&json_val, "$");

        let yaml = serde_yaml_ng::to_string(&crd).expect("CRD serializes to YAML");
        assert!(
            yaml.len() < 200 * 1024,
            "CRD YAML size ({} bytes) exceeds 200 KiB budget",
            yaml.len()
        );
    }

    #[test]
    fn cel_accepts_valid_spec_and_enforces_immutable_secret_mode() {
        let valid = WaddleServer::new("prod", valid_eso_spec());
        assert!(
            valid.validate_cel().is_ok(),
            "valid ESO spec should pass CEL: {:?}",
            valid.validate_cel()
        );

        let mut flipped = valid.clone();
        flipped.spec.secrets = SecretsSpec {
            mode: SecretProviderMode::Yolo,
            external_secrets: None,
            yolo: Some(YoloSecretsSpec::default()),
        };
        assert!(
            flipped.validate_cel_update(&valid).is_err(),
            "changing secrets.mode must be rejected by CEL immutable rule"
        );
    }

    #[test]
    fn cel_rejects_yolo_with_clustering_and_rolling_update_with_rwo() {
        let mut yolo_cluster = valid_eso_spec();
        yolo_cluster.secrets = SecretsSpec {
            mode: SecretProviderMode::Yolo,
            external_secrets: None,
            yolo: Some(YoloSecretsSpec::default()),
        };
        yolo_cluster.clustering.enabled = true;
        yolo_cluster.s3 = Some(S3StorageSpec {
            endpoint: "https://r2.example.com".to_owned(),
            bucket: "waddle".to_owned(),
        });
        assert!(
            WaddleServer::new("bad-yolo", yolo_cluster)
                .validate_cel()
                .is_err(),
            "YOLO + clustering must be rejected by CEL"
        );

        let mut rwo_rolling = valid_eso_spec();
        rwo_rolling.update_strategy = UpdateStrategyKind::RollingUpdate;
        assert!(
            WaddleServer::new("bad-rwo", rwo_rolling)
                .validate_cel()
                .is_err(),
            "RollingUpdate with RWO PVC must be rejected by CEL"
        );
    }

    #[test]
    fn validates_default_eso_spec_and_computes_shutdown_floor() {
        let validated = validate_spec(&valid_eso_spec()).expect("default ESO spec is valid");
        assert_eq!(validated.effective_replicas, 1);
        assert_eq!(
            validated.termination_grace_period_seconds, 52,
            "default shutdown floor must equal 5 + 30 + 5 + 5 + 2 + 5 = 52"
        );
    }

    #[test]
    fn rejects_shutdown_grace_below_floor() {
        let mut spec = valid_eso_spec();
        spec.shutdown.termination_grace_period_seconds = Some(51);
        let err = validate_spec(&spec).expect_err("grace below 52s must fail");
        assert!(
            err.violations()
                .contains(&ValidationViolation::TerminationGraceBelowFloor {
                    configured: 51,
                    minimum_floor: 52,
                }),
            "unexpected violations: {:?}",
            err.violations()
        );
    }

    #[test]
    fn rejects_yolo_clustering_and_multi_replica() {
        let mut spec = valid_eso_spec();
        spec.secrets = SecretsSpec {
            mode: SecretProviderMode::Yolo,
            external_secrets: None,
            yolo: Some(YoloSecretsSpec::default()),
        };
        spec.replicas = Some(2);
        let err = validate_spec(&spec).expect_err("YOLO multi-replica must fail");
        assert!(err
            .violations()
            .contains(&ValidationViolation::YoloForbidsClusteringOrMultiReplica));
    }

    #[test]
    fn validates_clustering_keypool_headroom_and_shared_storage() {
        let mut spec = valid_eso_spec();
        spec.replicas = Some(2);
        spec.clustering.enabled = true;
        spec.clustering.enrolled_key_count = 3; // needs 2 + 1 (RollingUpdate) + 1 = 4
        spec.update_strategy = UpdateStrategyKind::RollingUpdate;
        spec.persistence.enabled = false;
        spec.s3 = Some(S3StorageSpec {
            endpoint: "https://r2.example.com".to_owned(),
            bucket: "waddle".to_owned(),
        });
        let err = validate_spec(&spec).expect_err("insufficient keypool must fail");
        assert!(err
            .violations()
            .contains(&ValidationViolation::InsufficientClusteringKeypool {
                enrolled: 3,
                required: 4
            }));

        spec.clustering.enrolled_key_count = 4;
        assert!(
            validate_spec(&spec).is_ok(),
            "clustered spec with 4 keys and S3 should validate"
        );
    }

    #[test]
    fn validates_restricted_runtime_and_forbids_lineage_action() {
        let mut spec = valid_eso_spec();
        spec.deployment.lineage_action = Some("enroll".to_owned());
        spec.postgres.restricted_runtime = Some(RestrictedRuntimeSpec {
            enabled: true,
            release_git_sha: "badsha".to_owned(),
            schema: "public".to_owned(),
            database_host: "postgresql-rw".to_owned(),
            database_port: 5432,
            database_name: "waddle".to_owned(),
            ca_secret_name: "postgresql-ca".to_owned(),
            ca_secret_key: "ca.crt".to_owned(),
            runtime_credential_secret: "waddle-runtime-db".to_owned(),
            runtime_credential_key: "password".to_owned(),
            handoff_config_map_name: "waddle-server-runtime-handoff".to_owned(),
            roles: RestrictedRuntimeRolesSpec {
                migrator_login: "waddle_migrator".to_owned(),
                schema_owner: "waddle_owner".to_owned(),
                runtime_login: "waddle_runtime".to_owned(),
                attestor_installer_login: "waddle_attestor_installer".to_owned(),
                attestor_definer_role: "waddle_attestor_definer".to_owned(),
                attestor_schema: "waddle_attestor".to_owned(),
            },
        });
        let err = validate_spec(&spec).expect_err("bad sha + lineage action must fail");
        assert!(err
            .violations()
            .contains(&ValidationViolation::LineageActionForbiddenInRestrictedRuntime));
        assert!(err
            .violations()
            .iter()
            .any(|v| matches!(v, ValidationViolation::InvalidRestrictedRuntime { .. })));
    }

    #[test]
    fn rejects_reserved_extra_env_and_bad_keepalive_and_zero_digest() {
        let mut spec = valid_eso_spec();
        spec.extra_env
            .insert("WADDLE_SESSION_KEY".to_owned(), "oops".to_owned());
        spec.ws_keepalive.interval_secs = 121;
        spec.image.digest = Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
        );
        spec.persistence.access_modes = vec![PersistenceAccessMode::ReadWriteMany];
        spec.update_strategy = UpdateStrategyKind::RollingUpdate;

        let err = validate_spec(&spec).expect_err("reserved key and out of range must fail");
        assert!(err
            .violations()
            .contains(&ValidationViolation::ReservedExtraEnvKey {
                key: "WADDLE_SESSION_KEY".to_owned()
            }));
        assert!(err.violations().contains(&ValidationViolation::OutOfRange {
            field: "spec.wsKeepalive.intervalSecs",
            min: 1,
            max: 120,
            actual: 121
        }));
        assert!(err
            .violations()
            .contains(&ValidationViolation::InvalidImageDigest));
    }

    #[test]
    fn allows_yolo_single_node_without_deployment_uuid_in_spec() {
        let mut spec = valid_eso_spec();
        spec.deployment.uuid = None;
        spec.secrets = SecretsSpec {
            mode: SecretProviderMode::Yolo,
            external_secrets: None,
            yolo: Some(YoloSecretsSpec::default()),
        };
        spec.postgres = PostgresDependencySpec {
            mode: PostgresSourceMode::External,
            cnpg: None,
            external: Some(ExternalPostgresSource {
                secret_name: "pg-secret".to_owned(),
                uri_key: "uri".to_owned(),
                service_name: Some("pg-rw".to_owned()),
            }),
            pool_size: 10,
            control_plane_pool_size: 4,
            acquire_timeout_secs: Some(30),
            restricted_runtime: None,
        };
        spec.spicedb = SpiceDbDependencySpec {
            mode: SpiceDbSourceMode::External,
            operator: None,
            external: Some(ExternalSpiceDbSource {
                endpoint: "http://spicedb:50051".to_owned(),
                insecure: true,
                service_name: Some("spicedb".to_owned()),
                preshared_key_secret_ref: None,
            }),
        };
        assert!(
            validate_spec(&spec).is_ok(),
            "YOLO single node with external postgres/spicedb should validate"
        );
    }
}
