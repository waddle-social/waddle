use crate::observation::{
    DependencyHealth, ObservedWorld, SecretSourceKind, SpecValidationState, WorkloadObservation,
};
use crate::phase::Phase;

/// Canonical condition types published on `WaddleServer.status.conditions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConditionKind {
    /// Overall operational readiness of the `WaddleServer`.
    Ready,
    /// Spec and operator-policy validation result.
    SpecValid,
    /// PostgreSQL dependency readiness.
    PostgresReady,
    /// SpiceDB dependency readiness.
    SpiceDbReady,
    /// Runtime secrets readiness.
    SecretsReady,
    /// Waddle `Deployment` rollout and pod availability.
    WorkloadReady,
    /// Non-fatal security or operational posture warning (e.g. YOLO mode active).
    ConfigurationWarning,
}

impl ConditionKind {
    /// Returns the Kubernetes condition `type` string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::SpecValid => "SpecValid",
            Self::PostgresReady => "PostgresReady",
            Self::SpiceDbReady => "SpiceDbReady",
            Self::SecretsReady => "SecretsReady",
            Self::WorkloadReady => "WorkloadReady",
            Self::ConfigurationWarning => "ConfigurationWarning",
        }
    }

    /// Parses a Kubernetes condition `type` string if recognized.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "Ready" => Some(Self::Ready),
            "SpecValid" => Some(Self::SpecValid),
            "PostgresReady" => Some(Self::PostgresReady),
            "SpiceDbReady" => Some(Self::SpiceDbReady),
            "SecretsReady" => Some(Self::SecretsReady),
            "WorkloadReady" => Some(Self::WorkloadReady),
            "ConfigurationWarning" => Some(Self::ConfigurationWarning),
            _ => None,
        }
    }
}

/// Standard Kubernetes condition status values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConditionStatus {
    /// Condition holds.
    True,
    /// Condition does not hold.
    False,
    /// Condition could not be determined.
    Unknown,
}

impl ConditionStatus {
    /// Returns the Kubernetes wire string (`"True"`, `"False"`, or `"Unknown"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::True => "True",
            Self::False => "False",
            Self::Unknown => "Unknown",
        }
    }

    /// Parses a Kubernetes condition status string.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "True" => Self::True,
            "False" => Self::False,
            _ => Self::Unknown,
        }
    }
}

/// Pure status condition entry with `lastTransitionTime` preservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionRecord {
    /// Condition category.
    pub kind: ConditionKind,
    /// Current status (`True`, `False`, or `Unknown`).
    pub status: ConditionStatus,
    /// Short `PascalCase` reason.
    pub reason: String,
    /// Human-readable message.
    pub message: String,
    /// Observed resource generation.
    pub observed_generation: i64,
    /// RFC-3339 timestamp of the last status change.
    pub last_transition_time: String,
}

/// Computes the canonical condition set for `(phase, world)`, preserving `lastTransitionTime`
/// whenever a condition's `status` has not changed.
#[must_use]
pub fn compute_conditions(phase: Phase, world: &ObservedWorld) -> Vec<ConditionRecord> {
    let mut records = Vec::with_capacity(7);

    records.push(build_ready_condition(phase, world));
    records.push(build_spec_valid_condition(world));
    records.push(build_dependency_condition(
        ConditionKind::PostgresReady,
        &world.postgres,
        world,
    ));
    records.push(build_dependency_condition(
        ConditionKind::SpiceDbReady,
        &world.spicedb,
        world,
    ));
    records.push(build_secrets_condition(world));
    records.push(build_workload_condition(world));

    if matches!(world.secrets.source, SecretSourceKind::Yolo) {
        records.push(with_preserved_transition_time(
            ConditionKind::ConfigurationWarning,
            ConditionStatus::True,
            "YoloModeEnabled",
            "YOLO secret generation is active; this mode is restricted to single-node dev/test deployments",
            world,
        ));
    }

    records
}

fn build_ready_condition(phase: Phase, world: &ObservedWorld) -> ConditionRecord {
    let (status, reason, message) = match phase {
        Phase::Ready => (
            ConditionStatus::True,
            "Ready",
            "WaddleServer dependencies, secrets, and workload are ready".to_owned(),
        ),
        Phase::Pending => (
            ConditionStatus::False,
            "DependenciesPending",
            pending_summary(world),
        ),
        Phase::Provisioning => (
            ConditionStatus::False,
            "ProvisioningSecrets",
            "Waiting for runtime secrets to be provisioned and synced".to_owned(),
        ),
        Phase::Reconciling => (
            ConditionStatus::False,
            "Reconciling",
            "Workload rollout is in progress".to_owned(),
        ),
        Phase::Degraded => (ConditionStatus::False, "Degraded", degraded_summary(world)),
        Phase::Failed => (
            ConditionStatus::False,
            world
                .policy_or_spec_blocker()
                .map_or("Failed", |(reason, _)| reason),
            world.policy_or_spec_blocker().map_or_else(
                || "Pre-deployment validation failed".to_owned(),
                |(_, msg)| msg,
            ),
        ),
        Phase::Terminating => (
            ConditionStatus::False,
            "Terminating",
            "WaddleServer is terminating".to_owned(),
        ),
        Phase::Initializing => (
            ConditionStatus::Unknown,
            "Initializing",
            "Evaluating initial WaddleServer state".to_owned(),
        ),
    };

    with_preserved_transition_time(ConditionKind::Ready, status, reason, &message, world)
}

fn build_spec_valid_condition(world: &ObservedWorld) -> ConditionRecord {
    match (&world.spec_validation, world.secrets.policy_violation()) {
        (SpecValidationState::Invalid { summary }, _) => with_preserved_transition_time(
            ConditionKind::SpecValid,
            ConditionStatus::False,
            "SpecInvalid",
            summary,
            world,
        ),
        (SpecValidationState::Valid, Some((reason, msg))) => with_preserved_transition_time(
            ConditionKind::SpecValid,
            ConditionStatus::False,
            reason,
            msg,
            world,
        ),
        (SpecValidationState::Valid, None) => with_preserved_transition_time(
            ConditionKind::SpecValid,
            ConditionStatus::True,
            "SpecValid",
            "WaddleServer specification and operator security policy checks passed",
            world,
        ),
    }
}

fn build_dependency_condition(
    kind: ConditionKind,
    health: &DependencyHealth,
    world: &ObservedWorld,
) -> ConditionRecord {
    let summary = health.to_status_summary();
    let status = if health.is_ready() {
        ConditionStatus::True
    } else {
        ConditionStatus::False
    };
    let msg = summary
        .message
        .as_deref()
        .unwrap_or("Dependency status unknown");
    with_preserved_transition_time(kind, status, &summary.reason, msg, world)
}

fn build_secrets_condition(world: &ObservedWorld) -> ConditionRecord {
    let summary = world.secrets.to_status_summary();
    let status = if world.secrets.is_ready() {
        ConditionStatus::True
    } else {
        ConditionStatus::False
    };
    let msg = summary
        .message
        .as_deref()
        .unwrap_or("Secret readiness unknown");
    with_preserved_transition_time(
        ConditionKind::SecretsReady,
        status,
        &summary.reason,
        msg,
        world,
    )
}

fn build_workload_condition(world: &ObservedWorld) -> ConditionRecord {
    let (status, reason, message) = match &world.workload {
        WorkloadObservation::NotCreated => (
            ConditionStatus::False,
            "DeploymentNotCreated",
            "Server Deployment has not been created yet".to_owned(),
        ),
        WorkloadObservation::RolloutInProgress {
            desired_replicas,
            ready_replicas,
            ..
        } => (
            ConditionStatus::False,
            "RolloutInProgress",
            format!("Deployment rollout in progress ({ready_replicas}/{desired_replicas} ready)"),
        ),
        WorkloadObservation::ProgressDeadlineExceeded { message, .. } => (
            ConditionStatus::False,
            "ProgressDeadlineExceeded",
            message.clone(),
        ),
        WorkloadObservation::Available {
            desired_replicas,
            ready_replicas,
            drift_detected,
            ..
        } => {
            if *drift_detected {
                (
                    ConditionStatus::False,
                    "ManifestDriftDetected",
                    "Owned Kubernetes manifests differ from desired spec".to_owned(),
                )
            } else {
                (
                    ConditionStatus::True,
                    "WorkloadAvailable",
                    format!("Deployment available ({ready_replicas}/{desired_replicas} ready)"),
                )
            }
        }
        WorkloadObservation::DegradedReplicas {
            desired_replicas,
            available_replicas,
            ..
        } => (
            ConditionStatus::False,
            "InsufficientAvailableReplicas",
            format!("Only {available_replicas}/{desired_replicas} desired pods are available"),
        ),
    };

    with_preserved_transition_time(
        ConditionKind::WorkloadReady,
        status,
        reason,
        &message,
        world,
    )
}

fn pending_summary(world: &ObservedWorld) -> String {
    let mut waiting = Vec::new();
    if !world.postgres.is_ready() {
        waiting.push("Postgres");
    }
    if !world.spicedb.is_ready() {
        waiting.push("SpiceDB");
    }
    if !world.secrets.is_ready() {
        waiting.push("Secrets");
    }
    if waiting.is_empty() {
        "Waiting for dependencies".to_owned()
    } else {
        format!("Waiting for {}", waiting.join(", "))
    }
}

fn degraded_summary(world: &ObservedWorld) -> String {
    if let Some((_, msg)) = world.policy_or_spec_blocker() {
        return msg;
    }
    if !world.postgres.is_ready() {
        return world
            .postgres
            .to_status_summary()
            .message
            .unwrap_or_else(|| "Postgres is not ready".to_owned());
    }
    if !world.spicedb.is_ready() {
        return world
            .spicedb
            .to_status_summary()
            .message
            .unwrap_or_else(|| "SpiceDB is not ready".to_owned());
    }
    if !world.secrets.is_ready() {
        return world
            .secrets
            .to_status_summary()
            .message
            .unwrap_or_else(|| "Runtime secrets are not ready".to_owned());
    }
    match &world.workload {
        WorkloadObservation::ProgressDeadlineExceeded { message, .. } => message.clone(),
        WorkloadObservation::DegradedReplicas {
            desired_replicas,
            available_replicas,
            ..
        } => format!("Deployment degraded ({available_replicas}/{desired_replicas} available)"),
        _ => "WaddleServer is degraded".to_owned(),
    }
}

fn with_preserved_transition_time(
    kind: ConditionKind,
    status: ConditionStatus,
    reason: &str,
    message: &str,
    world: &ObservedWorld,
) -> ConditionRecord {
    let last_transition_time = world
        .existing_conditions
        .iter()
        .find(|existing| existing.kind == kind && existing.status == status)
        .map_or_else(
            || world.now_rfc3339.clone(),
            |existing| existing.last_transition_time.clone(),
        );

    ConditionRecord {
        kind,
        status,
        reason: reason.to_owned(),
        message: message.to_owned(),
        observed_generation: world.generation,
        last_transition_time,
    }
}
