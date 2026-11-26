use waddle_operator_crd::{ContentDigest, DependencyHealthPhase, DependencyStatusSummary};

/// Observed health of an external dependency (Postgres or SpiceDB).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyHealth {
    /// Referenced cluster, service, or secret does not exist yet.
    Missing {
        /// Machine-readable reason.
        reason: String,
        /// Human-readable detail.
        detail: String,
    },
    /// Required external CRD API group is not installed in the cluster.
    CrdNotInstalled {
        /// Missing API group (e.g. `postgresql.cnpg.io` or `authzed.com`).
        api_group: String,
    },
    /// Resource exists, but health conditions, migrations, or ready endpoints are incomplete.
    NotReady {
        /// Machine-readable reason.
        reason: String,
        /// Human-readable detail.
        detail: String,
    },
    /// Dependency exists and satisfies readiness requirements.
    Ready {
        /// Non-secret summary hint (e.g. endpoint URL or cluster name).
        endpoint_hint: String,
    },
}

impl DependencyHealth {
    /// Returns `true` when the dependency is ready for `waddle-server`.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    /// Returns the coarse CRD status phase.
    #[must_use]
    pub const fn phase(&self) -> DependencyHealthPhase {
        match self {
            Self::Missing { .. } => DependencyHealthPhase::Missing,
            Self::CrdNotInstalled { .. } => DependencyHealthPhase::CrdNotInstalled,
            Self::NotReady { .. } => DependencyHealthPhase::NotReady,
            Self::Ready { .. } => DependencyHealthPhase::Ready,
        }
    }

    /// Converts this observation into the non-secret CRD status summary.
    #[must_use]
    pub fn to_status_summary(&self) -> DependencyStatusSummary {
        match self {
            Self::Missing { reason, detail } => DependencyStatusSummary {
                phase: DependencyHealthPhase::Missing,
                reason: reason.clone(),
                message: Some(detail.clone()),
            },
            Self::CrdNotInstalled { api_group } => DependencyStatusSummary {
                phase: DependencyHealthPhase::CrdNotInstalled,
                reason: "CrdNotInstalled".to_owned(),
                message: Some(format!("CRD API group {api_group} is not installed")),
            },
            Self::NotReady { reason, detail } => DependencyStatusSummary {
                phase: DependencyHealthPhase::NotReady,
                reason: reason.clone(),
                message: Some(detail.clone()),
            },
            Self::Ready { endpoint_hint } => DependencyStatusSummary {
                phase: DependencyHealthPhase::Ready,
                reason: "Ready".to_owned(),
                message: Some(endpoint_hint.clone()),
            },
        }
    }
}

/// Which secret provisioning mechanism the resource uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSourceKind {
    /// Pre-existing ESO `ExternalSecret` not created by this operator.
    ExternalSecretsUnmanaged,
    /// Operator-owned `ExternalSecret` backed by a `SecretStore` or `ClusterSecretStore`.
    ExternalSecretsManaged,
    /// Single-node YOLO secret generation.
    Yolo,
}

/// Identifies whether a rotation hazard key changed after initial workload creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensitiveKeyRotation {
    /// `WADDLE_SESSION_KEY` changed (breaks sealed VAPID keys in Postgres).
    SessionKey,
    /// `WADDLE_OCCUPANT_ID_SECRET` changed (breaks XEP-0421 occupant-id continuity).
    OccupantIdSecret,
    /// Both `WADDLE_SESSION_KEY` and `WADDLE_OCCUPANT_ID_SECRET` changed.
    Both,
}

impl SensitiveKeyRotation {
    /// Human-readable description naming only environment variable identifiers.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::SessionKey => {
                "WADDLE_SESSION_KEY changed after initial boot; sealed VAPID keys in Postgres may fail to decrypt"
            }
            Self::OccupantIdSecret => {
                "WADDLE_OCCUPANT_ID_SECRET changed after initial boot; XEP-0421 occupant IDs will change across rooms"
            }
            Self::Both => {
                "WADDLE_SESSION_KEY and WADDLE_OCCUPANT_ID_SECRET changed after initial boot; VAPID decryption and XEP-0421 occupant IDs will break"
            }
        }
    }
}

/// Non-secret metadata summarizing the observed state of runtime secrets.
///
/// Raw secret bytes MUST never enter this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretsObservation {
    /// Selected provisioning mechanism.
    pub source: SecretSourceKind,
    /// Whether the operator process enabled `--enable-yolo-mode`.
    pub yolo_allowed_by_operator: bool,
    /// Whether the referenced `ClusterSecretStore` (if any) is permitted by operator policy.
    pub cluster_store_allowed: bool,
    /// Observed ESO `ExternalSecret` `Ready` condition (`None` for YOLO mode).
    pub external_secret_synced: Option<bool>,
    /// Optional human-readable ESO sync failure message.
    pub external_secret_error: Option<String>,
    /// Whether an operator-managed `ExternalSecret` differs from the desired spec.
    pub managed_external_secret_drifted: bool,
    /// Whether the target Kubernetes `Secret` exists.
    pub target_secret_exists: bool,
    /// Names of required keys missing from the target `Secret`.
    pub missing_keys: Vec<String>,
    /// Names of keys failing format or minimum-length checks.
    pub invalid_keys: Vec<String>,
    /// Non-secret SHA-256 digest of consumed secret values for rollout tracking.
    pub checksum: Option<ContentDigest>,
    /// Indicates if a non-rotatable sensitive key changed compared to the live deployment.
    pub sensitive_key_rotated: Option<SensitiveKeyRotation>,
}

impl SecretsObservation {
    /// Returns `true` if operator security policy forbids this secret configuration.
    #[must_use]
    pub const fn policy_violation(&self) -> Option<(&'static str, &'static str)> {
        if matches!(self.source, SecretSourceKind::Yolo) && !self.yolo_allowed_by_operator {
            return Some((
                "YoloModeDisabled",
                "Yolo secret mode is disabled on this operator instance (--enable-yolo-mode=false)",
            ));
        }
        if matches!(self.source, SecretSourceKind::ExternalSecretsManaged)
            && !self.cluster_store_allowed
        {
            return Some((
                "ClusterSecretStoreForbidden",
                "Referenced ClusterSecretStore is not in the operator's allowed-cluster-secret-stores list",
            ));
        }
        None
    }

    /// Returns `true` when secrets exist, ESO is synced (if applicable), and all keys pass validation.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        if self.policy_violation().is_some() || !self.target_secret_exists {
            return false;
        }
        if !self.missing_keys.is_empty() || !self.invalid_keys.is_empty() {
            return false;
        }
        match self.source {
            SecretSourceKind::ExternalSecretsUnmanaged
            | SecretSourceKind::ExternalSecretsManaged => self.external_secret_synced == Some(true),
            SecretSourceKind::Yolo => true,
        }
    }

    /// Summarizes secret readiness for `WaddleServerStatus.dependencies.secrets`.
    #[must_use]
    pub fn to_status_summary(&self) -> DependencyStatusSummary {
        if let Some((reason, detail)) = self.policy_violation() {
            return DependencyStatusSummary {
                phase: DependencyHealthPhase::NotReady,
                reason: reason.to_owned(),
                message: Some(detail.to_owned()),
            };
        }
        if !self.target_secret_exists {
            let reason = if self.external_secret_error.is_some() {
                "ExternalSecretSyncFailed"
            } else {
                "SecretMissing"
            };
            let message = self
                .external_secret_error
                .clone()
                .unwrap_or_else(|| "Target runtime Secret does not exist yet".to_owned());
            return DependencyStatusSummary {
                phase: DependencyHealthPhase::Missing,
                reason: reason.to_owned(),
                message: Some(message),
            };
        }
        if matches!(
            self.source,
            SecretSourceKind::ExternalSecretsUnmanaged | SecretSourceKind::ExternalSecretsManaged
        ) && self.external_secret_synced != Some(true)
        {
            return DependencyStatusSummary {
                phase: DependencyHealthPhase::NotReady,
                reason: "ExternalSecretNotSynced".to_owned(),
                message: self
                    .external_secret_error
                    .clone()
                    .or_else(|| Some("Waiting for ExternalSecret Ready=True".to_owned())),
            };
        }
        if !self.missing_keys.is_empty() || !self.invalid_keys.is_empty() {
            return DependencyStatusSummary {
                phase: DependencyHealthPhase::NotReady,
                reason: "SecretKeysIncomplete".to_owned(),
                message: Some(format!(
                    "missing=[{}], invalid=[{}]",
                    self.missing_keys.join(", "),
                    self.invalid_keys.join(", ")
                )),
            };
        }
        DependencyStatusSummary {
            phase: DependencyHealthPhase::Ready,
            reason: "Ready".to_owned(),
            message: Some("All required runtime secret keys present and valid".to_owned()),
        }
    }
}

/// Observed state of the owned Waddle `Deployment` workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkloadObservation {
    /// No `Deployment` exists yet in the cluster.
    NotCreated,
    /// `Deployment` exists and is actively rolling out (including `Recreate` zero-pod gap).
    RolloutInProgress {
        /// Desired replicas from the spec/deployment.
        desired_replicas: i32,
        /// Pods updated to the current template.
        updated_replicas: i32,
        /// Ready pods.
        ready_replicas: i32,
        /// Available pods.
        available_replicas: i32,
        /// Whether owned manifests differ from the desired spec/checksums.
        drift_detected: bool,
    },
    /// Kubernetes reported `Progressing=False` (`ProgressDeadlineExceeded`).
    ProgressDeadlineExceeded {
        /// Desired replicas.
        desired_replicas: i32,
        /// Ready replicas.
        ready_replicas: i32,
        /// Condition message from Deployment.
        message: String,
        /// Whether owned manifests differ from the desired spec/checksums.
        drift_detected: bool,
    },
    /// Deployment converged on the current generation with all desired replicas available.
    Available {
        /// Desired replicas.
        desired_replicas: i32,
        /// Updated replicas.
        updated_replicas: i32,
        /// Ready replicas.
        ready_replicas: i32,
        /// Available replicas.
        available_replicas: i32,
        /// Whether owned manifests differ from the desired spec/checksums.
        drift_detected: bool,
    },
    /// Deployment finished observing the current generation but has fewer available pods than desired.
    DegradedReplicas {
        /// Desired replicas.
        desired_replicas: i32,
        /// Ready replicas.
        ready_replicas: i32,
        /// Available replicas.
        available_replicas: i32,
        /// Whether owned manifests differ from the desired spec/checksums.
        drift_detected: bool,
    },
}

impl WorkloadObservation {
    /// Returns `true` if a `Deployment` already exists in the cluster.
    #[must_use]
    pub const fn exists(&self) -> bool {
        !matches!(self, Self::NotCreated)
    }

    /// Returns `true` if owned workload manifests need to be re-applied.
    #[must_use]
    pub const fn drift_detected(&self) -> bool {
        match self {
            Self::NotCreated => true,
            Self::RolloutInProgress { drift_detected, .. }
            | Self::ProgressDeadlineExceeded { drift_detected, .. }
            | Self::Available { drift_detected, .. }
            | Self::DegradedReplicas { drift_detected, .. } => *drift_detected,
        }
    }

    /// Returns `(desired, ready, updated)` replica counts for CRD status reporting.
    #[must_use]
    pub const fn replica_counts(&self) -> (i32, i32, i32) {
        match self {
            Self::NotCreated => (0, 0, 0),
            Self::RolloutInProgress {
                desired_replicas,
                updated_replicas,
                ready_replicas,
                ..
            } => (*desired_replicas, *ready_replicas, *updated_replicas),
            Self::ProgressDeadlineExceeded {
                desired_replicas,
                ready_replicas,
                ..
            } => (*desired_replicas, *ready_replicas, 0),
            Self::Available {
                desired_replicas,
                updated_replicas,
                ready_replicas,
                ..
            } => (*desired_replicas, *ready_replicas, *updated_replicas),
            Self::DegradedReplicas {
                desired_replicas,
                ready_replicas,
                ..
            } => (*desired_replicas, *ready_replicas, *desired_replicas),
        }
    }
}

/// Result of pure `WaddleServerSpec` validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecValidationState {
    /// Spec passes all server and Kubernetes validation rules.
    Valid,
    /// Spec violates one or more rules.
    Invalid {
        /// Concise diagnostic summary.
        summary: String,
    },
}

/// Complete, pure snapshot of the cluster world passed into [`crate::transition::evaluate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedWorld {
    /// `.metadata.generation` of the `WaddleServer` resource.
    pub generation: i64,
    /// Whether `metadata.deletionTimestamp` is set.
    pub deleting: bool,
    /// Result of pure spec validation.
    pub spec_validation: SpecValidationState,
    /// Observed storage backend health (`Turso` or `Postgres`).
    pub storage: DependencyHealth,
    /// Observed authorization backend health (`OpenFga` or `SpiceDb`).
    pub authz: DependencyHealth,
    /// Observed secret metadata (no raw secret values).
    pub secrets: SecretsObservation,
    /// Observed workload state.
    pub workload: WorkloadObservation,
    /// Existing status conditions used to preserve `lastTransitionTime`.
    pub existing_conditions: Vec<crate::condition::ConditionRecord>,
    /// Current UTC timestamp in RFC-3339 format used only when condition status changes.
    pub now_rfc3339: String,
}

impl ObservedWorld {
    /// Returns an optional `(reason, message)` pair if spec or operator security policy fails.
    #[must_use]
    pub fn policy_or_spec_blocker(&self) -> Option<(&'static str, String)> {
        if let SpecValidationState::Invalid { summary } = &self.spec_validation {
            return Some(("SpecInvalid", summary.clone()));
        }
        if let Some((reason, msg)) = self.secrets.policy_violation() {
            return Some((reason, msg.to_owned()));
        }
        None
    }

    /// Returns `true` when storage and authorization dependencies are both healthy.
    #[must_use]
    pub const fn deps_ready(&self) -> bool {
        self.storage.is_ready() && self.authz.is_ready()
    }
}
