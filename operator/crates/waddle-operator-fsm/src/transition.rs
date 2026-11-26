use std::marker::PhantomData;

use crate::command::{EventNotice, EventSeverity, ReconcileCommand, RequeuePolicy};
use crate::condition::{compute_conditions, ConditionRecord};
use crate::observation::{ObservedWorld, SecretSourceKind, WorkloadObservation};
use crate::phase::{
    Degraded, Failed, Initializing, Pending, Phase, PhaseState, Provisioning, Ready, Reconciling,
    Terminating,
};

const MAX_EVALUATE_HOPS: usize = 5;

/// Compile-time proof that transitioning from state `Self` to `Next` is a legal FSM edge.
pub trait CanTransitionTo<Next: PhaseState>: PhaseState {}

// Legal outgoing edges from Initializing:
impl CanTransitionTo<Pending> for Initializing {}
impl CanTransitionTo<Failed> for Initializing {}
impl CanTransitionTo<Terminating> for Initializing {}

// Legal outgoing edges from Pending (pre-deployment bootstrap):
impl CanTransitionTo<Provisioning> for Pending {}
impl CanTransitionTo<Reconciling> for Pending {}
impl CanTransitionTo<Failed> for Pending {}
impl CanTransitionTo<Terminating> for Pending {}

// Legal outgoing edges from Provisioning:
impl CanTransitionTo<Reconciling> for Provisioning {}
impl CanTransitionTo<Pending> for Provisioning {}
impl CanTransitionTo<Failed> for Provisioning {}
impl CanTransitionTo<Terminating> for Provisioning {}

// Legal outgoing edges from Reconciling:
// Note: `Reconciling -> Pending` is taken ONLY before a Deployment exists.
impl CanTransitionTo<Ready> for Reconciling {}
impl CanTransitionTo<Degraded> for Reconciling {}
impl CanTransitionTo<Pending> for Reconciling {}
impl CanTransitionTo<Terminating> for Reconciling {}

// Legal outgoing edges from Ready (never regresses to Pending or Failed):
impl CanTransitionTo<Reconciling> for Ready {}
impl CanTransitionTo<Degraded> for Ready {}
impl CanTransitionTo<Terminating> for Ready {}

// Legal outgoing edges from Degraded (never regresses to Pending or Failed):
impl CanTransitionTo<Reconciling> for Degraded {}
impl CanTransitionTo<Ready> for Degraded {}
impl CanTransitionTo<Terminating> for Degraded {}

// Legal outgoing edges from Failed:
impl CanTransitionTo<Pending> for Failed {}
impl CanTransitionTo<Terminating> for Failed {}

// Terminating has NO outgoing edges.

/// Typestate wrapper enforcing valid transitions for `WaddleServer` at compile time.
///
/// # Examples
///
/// Legal bootstrap flow compiles cleanly:
/// ```
/// use waddle_operator_fsm::{Initializing, Pending, Provisioning, Ready, Reconciling, StateMachine};
///
/// let sm = StateMachine::<Initializing>::new()
///     .transition::<Pending>()
///     .transition::<Provisioning>()
///     .transition::<Reconciling>()
///     .transition::<Ready>();
/// assert_eq!(sm.phase(), waddle_operator_fsm::Phase::Ready);
/// ```
///
/// Illegal shortcuts are rejected at compile time:
/// ```compile_fail
/// use waddle_operator_fsm::{Initializing, Ready, StateMachine};
/// let _ = StateMachine::<Initializing>::new().transition::<Ready>();
/// ```
/// ```compile_fail
/// use waddle_operator_fsm::{Pending, Ready, StateMachine};
/// let _ = StateMachine::<Pending>::new().transition::<Ready>();
/// ```
/// ```compile_fail
/// use waddle_operator_fsm::{Pending, Ready, StateMachine};
/// let sm = StateMachine::<Ready>::from_phase_marker();
/// let _ = sm.transition::<Pending>();
/// ```
/// ```compile_fail
/// use waddle_operator_fsm::{Degraded, Pending, StateMachine};
/// let sm = StateMachine::<Degraded>::from_phase_marker();
/// let _ = sm.transition::<Pending>();
/// ```
/// ```compile_fail
/// use waddle_operator_fsm::{Pending, StateMachine, Terminating};
/// let sm = StateMachine::<Terminating>::from_phase_marker();
/// let _ = sm.transition::<Pending>();
/// ```
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateMachine<S: PhaseState> {
    _state: PhantomData<S>,
}

impl Default for StateMachine<Initializing> {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine<Initializing> {
    /// Creates a new state machine in the [`Initializing`] state.
    pub const fn new() -> Self {
        Self {
            _state: PhantomData,
        }
    }
}

impl<S: PhaseState> StateMachine<S> {
    /// Reconstructs a typed handle for a state marker (used when resuming from persisted status).
    pub const fn from_phase_marker() -> Self {
        Self {
            _state: PhantomData,
        }
    }

    /// Returns the dynamic [`Phase`] corresponding to `S`.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        S::PHASE
    }

    /// Transitions to `Next`, statically checked via [`CanTransitionTo`].
    pub const fn transition<Next: PhaseState>(self) -> StateMachine<Next>
    where
        S: CanTransitionTo<Next>,
    {
        StateMachine {
            _state: PhantomData,
        }
    }
}

/// Pure reconciliation decision returned by [`evaluate`].
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionOutcome {
    /// Phase persisted prior to this evaluation.
    pub from_phase: Phase,
    /// Converged phase after evaluating the current [`ObservedWorld`].
    pub to_phase: Phase,
    /// Sequence of legal typestate hops taken in this pass.
    pub transition_trace: Vec<(Phase, Phase)>,
    /// Declarative side-effect commands to execute.
    pub commands: Vec<ReconcileCommand>,
    /// Canonically ordered status conditions preserving `lastTransitionTime`.
    pub conditions: Vec<ConditionRecord>,
    /// Recommended controller requeue interval.
    pub requeue: RequeuePolicy,
}

enum MachineCursor {
    Initializing(StateMachine<Initializing>),
    Pending(StateMachine<Pending>),
    Provisioning(StateMachine<Provisioning>),
    Reconciling(StateMachine<Reconciling>),
    Ready(StateMachine<Ready>),
    Degraded(StateMachine<Degraded>),
    Failed(StateMachine<Failed>),
    Terminating(StateMachine<Terminating>),
}

impl MachineCursor {
    const fn from_phase(phase: Phase) -> Self {
        match phase {
            Phase::Initializing => Self::Initializing(StateMachine::<Initializing>::new()),
            Phase::Pending => Self::Pending(StateMachine::<Pending>::from_phase_marker()),
            Phase::Provisioning => {
                Self::Provisioning(StateMachine::<Provisioning>::from_phase_marker())
            }
            Phase::Reconciling => {
                Self::Reconciling(StateMachine::<Reconciling>::from_phase_marker())
            }
            Phase::Ready => Self::Ready(StateMachine::<Ready>::from_phase_marker()),
            Phase::Degraded => Self::Degraded(StateMachine::<Degraded>::from_phase_marker()),
            Phase::Failed => Self::Failed(StateMachine::<Failed>::from_phase_marker()),
            Phase::Terminating => {
                Self::Terminating(StateMachine::<Terminating>::from_phase_marker())
            }
        }
    }

    const fn phase(&self) -> Phase {
        match self {
            Self::Initializing(_) => Phase::Initializing,
            Self::Pending(_) => Phase::Pending,
            Self::Provisioning(_) => Phase::Provisioning,
            Self::Reconciling(_) => Phase::Reconciling,
            Self::Ready(_) => Phase::Ready,
            Self::Degraded(_) => Phase::Degraded,
            Self::Failed(_) => Phase::Failed,
            Self::Terminating(_) => Phase::Terminating,
        }
    }
}

enum StepResult {
    Advance(MachineCursor),
    Settled {
        cursor: MachineCursor,
        commands: Vec<ReconcileCommand>,
        requeue: RequeuePolicy,
    },
}

/// Evaluates `world` starting from `current`, stepping through the typestate graph to convergence.
///
/// This function is pure, deterministic, performs zero I/O, and never inspects raw secret values.
pub fn evaluate(current: Phase, world: &ObservedWorld) -> TransitionOutcome {
    let mut cursor = MachineCursor::from_phase(current);
    let mut trace = Vec::new();

    let (commands, requeue) = {
        let mut settled = None;
        for _ in 0..MAX_EVALUATE_HOPS {
            let before = cursor.phase();
            match step_once(cursor, world) {
                StepResult::Advance(next) => {
                    let after = next.phase();
                    trace.push((before, after));
                    cursor = next;
                }
                StepResult::Settled {
                    cursor: final_cursor,
                    commands,
                    requeue,
                } => {
                    cursor = final_cursor;
                    settled = Some((commands, requeue));
                    break;
                }
            }
        }
        settled.unwrap_or_else(|| (Vec::new(), RequeuePolicy::AfterSeconds(15)))
    };

    let to_phase = cursor.phase();
    let mut all_commands = commands;

    if world.workload.exists() {
        if let Some(rotation) = world.secrets.sensitive_key_rotated {
            all_commands.push(ReconcileCommand::EmitEvent(EventNotice {
                severity: EventSeverity::Warning,
                reason: "SensitiveSecretRotated".to_owned(),
                message: rotation.describe().to_owned(),
            }));
        }
    }

    if current != to_phase {
        let path = trace
            .iter()
            .map(|(from, to)| format!("{from}->{to}"))
            .collect::<Vec<_>>()
            .join(", ");
        let severity = match to_phase {
            Phase::Degraded | Phase::Failed => EventSeverity::Warning,
            _ => EventSeverity::Normal,
        };
        all_commands.push(ReconcileCommand::EmitEvent(EventNotice {
            severity,
            reason: "PhaseTransition".to_owned(),
            message: format!("Transitioned to {to_phase} ({path})"),
        }));
    }

    let conditions = compute_conditions(to_phase, world);

    TransitionOutcome {
        from_phase: current,
        to_phase,
        transition_trace: trace,
        commands: all_commands,
        conditions,
        requeue,
    }
}

fn step_once(cursor: MachineCursor, world: &ObservedWorld) -> StepResult {
    match cursor {
        MachineCursor::Terminating(m) => StepResult::Settled {
            cursor: MachineCursor::Terminating(m),
            commands: Vec::new(),
            requeue: RequeuePolicy::None,
        },
        MachineCursor::Initializing(m) => {
            if world.deleting {
                StepResult::Advance(MachineCursor::Terminating(m.transition::<Terminating>()))
            } else if world.policy_or_spec_blocker().is_some() && !world.workload.exists() {
                StepResult::Advance(MachineCursor::Failed(m.transition::<Failed>()))
            } else {
                StepResult::Advance(MachineCursor::Pending(m.transition::<Pending>()))
            }
        }
        MachineCursor::Failed(m) => {
            if world.deleting {
                StepResult::Advance(MachineCursor::Terminating(m.transition::<Terminating>()))
            } else if world.policy_or_spec_blocker().is_some() {
                StepResult::Settled {
                    cursor: MachineCursor::Failed(m),
                    commands: Vec::new(),
                    requeue: RequeuePolicy::AfterSeconds(30),
                }
            } else {
                StepResult::Advance(MachineCursor::Pending(m.transition::<Pending>()))
            }
        }
        MachineCursor::Pending(m) => step_pending(m, world),
        MachineCursor::Provisioning(m) => step_provisioning(m, world),
        MachineCursor::Reconciling(m) => step_reconciling(m, world),
        MachineCursor::Ready(m) => step_ready(m, world),
        MachineCursor::Degraded(m) => step_degraded(m, world),
    }
}

fn step_pending(m: StateMachine<Pending>, world: &ObservedWorld) -> StepResult {
    if world.deleting {
        return StepResult::Advance(MachineCursor::Terminating(m.transition::<Terminating>()));
    }
    if world.policy_or_spec_blocker().is_some() && !world.workload.exists() {
        return StepResult::Advance(MachineCursor::Failed(m.transition::<Failed>()));
    }
    if world.workload.exists() {
        // Recover cleanly after an empty/corrupted status without tearing down live pods.
        return StepResult::Advance(MachineCursor::Reconciling(m.transition::<Reconciling>()));
    }
    if !world.deps_ready() {
        return StepResult::Settled {
            cursor: MachineCursor::Pending(m),
            commands: Vec::new(),
            requeue: RequeuePolicy::AfterSeconds(15),
        };
    }
    if world.secrets.is_ready() && !world.secrets.managed_external_secret_drifted {
        return StepResult::Advance(MachineCursor::Reconciling(m.transition::<Reconciling>()));
    }
    match world.secrets.source {
        SecretSourceKind::Yolo | SecretSourceKind::ExternalSecretsManaged => {
            StepResult::Advance(MachineCursor::Provisioning(m.transition::<Provisioning>()))
        }
        SecretSourceKind::ExternalSecretsUnmanaged => StepResult::Settled {
            cursor: MachineCursor::Pending(m),
            commands: Vec::new(),
            requeue: RequeuePolicy::AfterSeconds(10),
        },
    }
}

fn step_provisioning(m: StateMachine<Provisioning>, world: &ObservedWorld) -> StepResult {
    if world.deleting {
        return StepResult::Advance(MachineCursor::Terminating(m.transition::<Terminating>()));
    }
    if world.policy_or_spec_blocker().is_some() {
        if world.workload.exists() {
            return StepResult::Advance(MachineCursor::Reconciling(m.transition::<Reconciling>()));
        }
        return StepResult::Advance(MachineCursor::Failed(m.transition::<Failed>()));
    }
    if !world.deps_ready() {
        if world.workload.exists() {
            return StepResult::Advance(MachineCursor::Reconciling(m.transition::<Reconciling>()));
        }
        return StepResult::Advance(MachineCursor::Pending(m.transition::<Pending>()));
    }
    if world.secrets.is_ready() && !world.secrets.managed_external_secret_drifted {
        return StepResult::Advance(MachineCursor::Reconciling(m.transition::<Reconciling>()));
    }
    let mut commands = Vec::new();
    match world.secrets.source {
        SecretSourceKind::Yolo => commands.push(ReconcileCommand::EnsureYoloSecret),
        SecretSourceKind::ExternalSecretsManaged => {
            commands.push(ReconcileCommand::ApplyManagedExternalSecret);
        }
        SecretSourceKind::ExternalSecretsUnmanaged => {
            return StepResult::Advance(MachineCursor::Pending(m.transition::<Pending>()));
        }
    }
    StepResult::Settled {
        cursor: MachineCursor::Provisioning(m),
        commands,
        requeue: RequeuePolicy::AfterSeconds(5),
    }
}

fn step_reconciling(m: StateMachine<Reconciling>, world: &ObservedWorld) -> StepResult {
    if world.deleting {
        return StepResult::Advance(MachineCursor::Terminating(m.transition::<Terminating>()));
    }
    if world.policy_or_spec_blocker().is_some() || !world.deps_ready() || !world.secrets.is_ready()
    {
        if !world.workload.exists() {
            return StepResult::Advance(MachineCursor::Pending(m.transition::<Pending>()));
        }
        return StepResult::Advance(MachineCursor::Degraded(m.transition::<Degraded>()));
    }

    let mut commands = Vec::new();
    if world.secrets.source == SecretSourceKind::ExternalSecretsManaged
        && world.secrets.managed_external_secret_drifted
    {
        commands.push(ReconcileCommand::ApplyManagedExternalSecret);
    }

    match &world.workload {
        WorkloadObservation::NotCreated => {
            commands.push(ReconcileCommand::ApplyWorkload);
            StepResult::Settled {
                cursor: MachineCursor::Reconciling(m),
                commands,
                requeue: RequeuePolicy::AfterSeconds(5),
            }
        }
        WorkloadObservation::RolloutInProgress { drift_detected, .. } => {
            if *drift_detected {
                commands.push(ReconcileCommand::ApplyWorkload);
            }
            StepResult::Settled {
                cursor: MachineCursor::Reconciling(m),
                commands,
                requeue: RequeuePolicy::AfterSeconds(5),
            }
        }
        WorkloadObservation::ProgressDeadlineExceeded { drift_detected, .. }
        | WorkloadObservation::DegradedReplicas { drift_detected, .. } => {
            if *drift_detected {
                commands.push(ReconcileCommand::ApplyWorkload);
                StepResult::Settled {
                    cursor: MachineCursor::Reconciling(m),
                    commands,
                    requeue: RequeuePolicy::AfterSeconds(5),
                }
            } else {
                StepResult::Advance(MachineCursor::Degraded(m.transition::<Degraded>()))
            }
        }
        WorkloadObservation::Available { drift_detected, .. } => {
            if *drift_detected {
                commands.push(ReconcileCommand::ApplyWorkload);
                StepResult::Settled {
                    cursor: MachineCursor::Reconciling(m),
                    commands,
                    requeue: RequeuePolicy::AfterSeconds(5),
                }
            } else {
                StepResult::Advance(MachineCursor::Ready(m.transition::<Ready>()))
            }
        }
    }
}

fn step_ready(m: StateMachine<Ready>, world: &ObservedWorld) -> StepResult {
    if world.deleting {
        return StepResult::Advance(MachineCursor::Terminating(m.transition::<Terminating>()));
    }
    if world.policy_or_spec_blocker().is_some() || !world.deps_ready() || !world.secrets.is_ready()
    {
        return StepResult::Advance(MachineCursor::Degraded(m.transition::<Degraded>()));
    }

    let mut commands = Vec::new();
    if world.secrets.source == SecretSourceKind::ExternalSecretsManaged
        && world.secrets.managed_external_secret_drifted
    {
        commands.push(ReconcileCommand::ApplyManagedExternalSecret);
    }

    match &world.workload {
        WorkloadObservation::Available {
            drift_detected: false,
            ..
        } => StepResult::Settled {
            cursor: MachineCursor::Ready(m),
            commands,
            requeue: RequeuePolicy::AfterSeconds(300),
        },
        WorkloadObservation::NotCreated
        | WorkloadObservation::RolloutInProgress { .. }
        | WorkloadObservation::Available {
            drift_detected: true,
            ..
        } => StepResult::Advance(MachineCursor::Reconciling(m.transition::<Reconciling>())),
        WorkloadObservation::ProgressDeadlineExceeded { drift_detected, .. }
        | WorkloadObservation::DegradedReplicas { drift_detected, .. } => {
            if *drift_detected {
                StepResult::Advance(MachineCursor::Reconciling(m.transition::<Reconciling>()))
            } else {
                StepResult::Advance(MachineCursor::Degraded(m.transition::<Degraded>()))
            }
        }
    }
}

fn step_degraded(m: StateMachine<Degraded>, world: &ObservedWorld) -> StepResult {
    if world.deleting {
        return StepResult::Advance(MachineCursor::Terminating(m.transition::<Terminating>()));
    }
    if world.policy_or_spec_blocker().is_some() || !world.deps_ready() || !world.secrets.is_ready()
    {
        let mut commands = Vec::new();
        if world.policy_or_spec_blocker().is_none() && world.deps_ready() {
            match world.secrets.source {
                SecretSourceKind::Yolo => commands.push(ReconcileCommand::EnsureYoloSecret),
                SecretSourceKind::ExternalSecretsManaged => {
                    commands.push(ReconcileCommand::ApplyManagedExternalSecret);
                }
                SecretSourceKind::ExternalSecretsUnmanaged => {}
            }
        }
        return StepResult::Settled {
            cursor: MachineCursor::Degraded(m),
            commands,
            requeue: RequeuePolicy::AfterSeconds(15),
        };
    }

    match &world.workload {
        WorkloadObservation::Available {
            drift_detected: false,
            ..
        } => StepResult::Advance(MachineCursor::Ready(m.transition::<Ready>())),
        WorkloadObservation::NotCreated
        | WorkloadObservation::RolloutInProgress { .. }
        | WorkloadObservation::Available {
            drift_detected: true,
            ..
        } => StepResult::Advance(MachineCursor::Reconciling(m.transition::<Reconciling>())),
        WorkloadObservation::ProgressDeadlineExceeded { drift_detected, .. }
        | WorkloadObservation::DegradedReplicas { drift_detected, .. } => {
            if *drift_detected {
                StepResult::Advance(MachineCursor::Reconciling(m.transition::<Reconciling>()))
            } else {
                let mut commands = Vec::new();
                if world.secrets.source == SecretSourceKind::ExternalSecretsManaged
                    && world.secrets.managed_external_secret_drifted
                {
                    commands.push(ReconcileCommand::ApplyManagedExternalSecret);
                }
                StepResult::Settled {
                    cursor: MachineCursor::Degraded(m),
                    commands,
                    requeue: RequeuePolicy::AfterSeconds(20),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::arbitrary::any;
    use proptest::option;
    use proptest::strategy::{Just, Strategy};
    use proptest::{prop_assert, prop_assert_eq, prop_assert_ne, prop_oneof, proptest};
    use waddle_operator_crd::ContentDigest;

    use crate::condition::{ConditionKind, ConditionRecord, ConditionStatus};
    use crate::observation::{
        DependencyHealth, SecretsObservation, SensitiveKeyRotation, SpecValidationState,
    };

    use super::*;

    fn ready_pg() -> DependencyHealth {
        DependencyHealth::Ready {
            endpoint_hint: "postgresql-rw".to_owned(),
        }
    }

    fn ready_spicedb() -> DependencyHealth {
        DependencyHealth::Ready {
            endpoint_hint: "http://spicedb:50051".to_owned(),
        }
    }

    fn missing_pg() -> DependencyHealth {
        DependencyHealth::Missing {
            reason: "ClusterNotFound".to_owned(),
            detail: "CNPG Cluster postgresql not found".to_owned(),
        }
    }

    fn missing_spicedb() -> DependencyHealth {
        DependencyHealth::Missing {
            reason: "ClusterNotFound".to_owned(),
            detail: "SpiceDBCluster spicedb not found".to_owned(),
        }
    }

    fn sample_digest() -> ContentDigest {
        ContentDigest::try_new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .expect("valid hex digest")
    }

    fn eso_secrets_ready() -> SecretsObservation {
        SecretsObservation {
            source: SecretSourceKind::ExternalSecretsUnmanaged,
            yolo_allowed_by_operator: false,
            cluster_store_allowed: true,
            external_secret_synced: Some(true),
            external_secret_error: None,
            managed_external_secret_drifted: false,
            target_secret_exists: true,
            missing_keys: Vec::new(),
            invalid_keys: Vec::new(),
            checksum: Some(sample_digest()),
            sensitive_key_rotated: None,
        }
    }

    fn base_world() -> ObservedWorld {
        ObservedWorld {
            generation: 1,
            deleting: false,
            spec_validation: SpecValidationState::Valid,
            storage: ready_pg(),
            authz: ready_spicedb(),
            secrets: eso_secrets_ready(),
            workload: WorkloadObservation::NotCreated,
            existing_conditions: Vec::new(),
            now_rfc3339: "2026-11-21T20:00:00Z".to_owned(),
        }
    }

    #[test]
    fn enters_pending_until_both_storage_and_authz_exist_and_are_ready() {
        let mut world = base_world();
        world.storage = missing_pg();
        world.authz = missing_spicedb();

        let out = evaluate(Phase::Initializing, &world);
        assert_eq!(out.to_phase, Phase::Pending);
        assert!(
            out.commands
                .iter()
                .all(|cmd| !matches!(cmd, ReconcileCommand::ApplyWorkload)),
            "Pending must never emit ApplyWorkload: {:?}",
            out.commands
        );

        world.storage = ready_pg();
        let out2 = evaluate(Phase::Pending, &world);
        assert_eq!(
            out2.to_phase,
            Phase::Pending,
            "Must stay Pending while authz is missing"
        );
        assert!(out2.commands.is_empty());

        world.authz = ready_spicedb();
        let out3 = evaluate(Phase::Pending, &world);
        assert_eq!(out3.to_phase, Phase::Reconciling);
        assert!(out3.commands.contains(&ReconcileCommand::ApplyWorkload));
    }

    #[test]
    fn yolo_mode_provisions_before_reconciling_and_respects_disable_flag() {
        let mut world = base_world();
        world.secrets = SecretsObservation {
            source: SecretSourceKind::Yolo,
            yolo_allowed_by_operator: false,
            cluster_store_allowed: true,
            external_secret_synced: None,
            external_secret_error: None,
            managed_external_secret_drifted: false,
            target_secret_exists: false,
            missing_keys: vec!["WADDLE_SESSION_KEY".to_owned()],
            invalid_keys: Vec::new(),
            checksum: None,
            sensitive_key_rotated: None,
        };

        let denied = evaluate(Phase::Initializing, &world);
        assert_eq!(
            denied.to_phase,
            Phase::Failed,
            "YOLO disabled by operator flag must fail pre-deployment"
        );

        world.secrets.yolo_allowed_by_operator = true;
        let prov = evaluate(Phase::Pending, &world);
        assert_eq!(prov.to_phase, Phase::Provisioning);
        assert!(prov.commands.contains(&ReconcileCommand::EnsureYoloSecret));

        world.secrets.target_secret_exists = true;
        world.secrets.missing_keys.clear();
        world.secrets.checksum = Some(sample_digest());
        let rec = evaluate(Phase::Provisioning, &world);
        assert_eq!(rec.to_phase, Phase::Reconciling);
        assert!(rec.commands.contains(&ReconcileCommand::ApplyWorkload));
    }

    #[test]
    fn recreate_rollout_with_zero_ready_pods_does_not_flap_to_degraded() {
        let mut world = base_world();
        world.workload = WorkloadObservation::RolloutInProgress {
            desired_replicas: 1,
            updated_replicas: 0,
            ready_replicas: 0,
            available_replicas: 0,
            drift_detected: false,
        };
        let out = evaluate(Phase::Reconciling, &world);
        assert_eq!(
            out.to_phase,
            Phase::Reconciling,
            "Recreate rollout with 0 ready pods must remain Reconciling"
        );
    }

    #[test]
    fn ready_transitions_to_degraded_never_pending_on_dependency_loss_or_bad_spec_edit() {
        let mut world = base_world();
        world.workload = WorkloadObservation::Available {
            desired_replicas: 2,
            updated_replicas: 2,
            ready_replicas: 2,
            available_replicas: 2,
            drift_detected: false,
        };

        world.authz = missing_spicedb();
        let dep_lost = evaluate(Phase::Ready, &world);
        assert_eq!(dep_lost.to_phase, Phase::Degraded);
        assert!(
            !dep_lost.commands.contains(&ReconcileCommand::ApplyWorkload),
            "Must preserve last-known-good Deployment when dependency disappears"
        );

        world.authz = ready_spicedb();
        world.spec_validation = SpecValidationState::Invalid {
            summary: "bad edit".to_owned(),
        };
        let bad_edit = evaluate(Phase::Ready, &world);
        assert_eq!(bad_edit.to_phase, Phase::Degraded);
        assert!(
            !bad_edit.commands.contains(&ReconcileCommand::ApplyWorkload),
            "Invalid spec edit after Ready must not mutate live Deployment"
        );
    }

    #[test]
    fn preserves_last_transition_time_when_condition_status_unchanged_and_emits_rotation_warning() {
        let mut world = base_world();
        world.workload = WorkloadObservation::Available {
            desired_replicas: 1,
            updated_replicas: 1,
            ready_replicas: 1,
            available_replicas: 1,
            drift_detected: false,
        };
        world.existing_conditions = vec![ConditionRecord {
            kind: ConditionKind::Ready,
            status: ConditionStatus::True,
            reason: "Ready".to_owned(),
            message: "Old".to_owned(),
            observed_generation: 1,
            last_transition_time: "2026-11-21T19:00:00Z".to_owned(),
        }];
        world.secrets.sensitive_key_rotated = Some(SensitiveKeyRotation::SessionKey);

        let out = evaluate(Phase::Ready, &world);
        assert_eq!(out.to_phase, Phase::Ready);
        let ready_cond = out
            .conditions
            .iter()
            .find(|c| c.kind == ConditionKind::Ready)
            .expect("Ready condition present");
        assert_eq!(ready_cond.last_transition_time, "2026-11-21T19:00:00Z");
        assert!(out.commands.iter().any(|cmd| {
            matches!(
                cmd,
                ReconcileCommand::EmitEvent(EventNotice {
                    severity: EventSeverity::Warning,
                    reason,
                    ..
                }) if reason == "SensitiveSecretRotated"
            )
        }));
    }

    #[test]
    fn drifted_unhealthy_workload_settles_in_reconciling_without_oscillating() {
        for workload in [
            WorkloadObservation::ProgressDeadlineExceeded {
                desired_replicas: 1,
                ready_replicas: 0,
                message: "deadline".to_owned(),
                drift_detected: true,
            },
            WorkloadObservation::DegradedReplicas {
                desired_replicas: 1,
                ready_replicas: 0,
                available_replicas: 0,
                drift_detected: true,
            },
        ] {
            let mut world = base_world();
            world.workload = workload;
            let out = evaluate(Phase::Initializing, &world);
            assert_eq!(out.to_phase, Phase::Reconciling);
            assert!(out.commands.contains(&ReconcileCommand::ApplyWorkload));

            let steady = evaluate(out.to_phase, &world);
            assert_eq!(steady.to_phase, Phase::Reconciling);
            assert!(steady.transition_trace.is_empty());
        }

        let mut converged_degraded = base_world();
        converged_degraded.workload = WorkloadObservation::DegradedReplicas {
            desired_replicas: 1,
            ready_replicas: 0,
            available_replicas: 0,
            drift_detected: false,
        };
        let out = evaluate(Phase::Reconciling, &converged_degraded);
        assert_eq!(out.to_phase, Phase::Degraded);
    }

    fn arb_phase() -> impl Strategy<Value = Phase> {
        prop_oneof![
            Just(Phase::Initializing),
            Just(Phase::Pending),
            Just(Phase::Provisioning),
            Just(Phase::Reconciling),
            Just(Phase::Ready),
            Just(Phase::Degraded),
            Just(Phase::Failed),
            Just(Phase::Terminating),
        ]
    }

    fn arb_dep() -> impl Strategy<Value = DependencyHealth> {
        prop_oneof![
            Just(missing_pg()),
            Just(DependencyHealth::CrdNotInstalled {
                api_group: "postgresql.cnpg.io".to_owned()
            }),
            Just(DependencyHealth::NotReady {
                reason: "NotReady".to_owned(),
                detail: "booting".to_owned()
            }),
            Just(ready_pg()),
        ]
    }

    fn arb_source() -> impl Strategy<Value = SecretSourceKind> {
        prop_oneof![
            Just(SecretSourceKind::ExternalSecretsUnmanaged),
            Just(SecretSourceKind::ExternalSecretsManaged),
            Just(SecretSourceKind::Yolo),
        ]
    }

    fn arb_workload() -> impl Strategy<Value = WorkloadObservation> {
        prop_oneof![
            Just(WorkloadObservation::NotCreated),
            any::<bool>().prop_map(|drift| WorkloadObservation::RolloutInProgress {
                desired_replicas: 1,
                updated_replicas: 0,
                ready_replicas: 0,
                available_replicas: 0,
                drift_detected: drift,
            }),
            any::<bool>().prop_map(|drift| WorkloadObservation::ProgressDeadlineExceeded {
                desired_replicas: 1,
                ready_replicas: 0,
                message: "deadline".to_owned(),
                drift_detected: drift,
            }),
            any::<bool>().prop_map(|drift| WorkloadObservation::Available {
                desired_replicas: 1,
                updated_replicas: 1,
                ready_replicas: 1,
                available_replicas: 1,
                drift_detected: drift,
            }),
            any::<bool>().prop_map(|drift| WorkloadObservation::DegradedReplicas {
                desired_replicas: 1,
                ready_replicas: 0,
                available_replicas: 0,
                drift_detected: drift,
            }),
        ]
    }

    proptest! {
        #[test]
        fn fsm_invariants_hold_over_all_worlds(
            phase in arb_phase(),
            deleting in any::<bool>(),
            spec_valid in any::<bool>(),
            pg in arb_dep(),
            spicedb in arb_dep(),
            source in arb_source(),
            yolo_allowed in any::<bool>(),
            store_allowed in any::<bool>(),
            eso_synced in option::of(any::<bool>()),
            secret_exists in any::<bool>(),
            missing_key in any::<bool>(),
            managed_drift in any::<bool>(),
            workload in arb_workload(),
        ) {
            let world = ObservedWorld {
                generation: 3,
                deleting,
                spec_validation: if spec_valid {
                    SpecValidationState::Valid
                } else {
                    SpecValidationState::Invalid {
                        summary: "invalid".to_owned(),
                    }
                },
                storage: pg,
                authz: spicedb,
                secrets: SecretsObservation {
                    source,
                    yolo_allowed_by_operator: yolo_allowed,
                    cluster_store_allowed: store_allowed,
                    external_secret_synced: eso_synced,
                    external_secret_error: None,
                    managed_external_secret_drifted: managed_drift,
                    target_secret_exists: secret_exists,
                    missing_keys: if missing_key {
                        vec!["WADDLE_SESSION_KEY".to_owned()]
                    } else {
                        Vec::new()
                    },
                    invalid_keys: Vec::new(),
                    checksum: secret_exists.then(sample_digest),
                    sensitive_key_rotated: None,
                },
                workload,
                existing_conditions: Vec::new(),
                now_rfc3339: "2026-11-21T20:00:00Z".to_owned(),
            };

            let out = evaluate(phase, &world);

            // Invariant 1: Terminating is strictly terminal.
            if phase == Phase::Terminating || deleting {
                prop_assert_eq!(out.to_phase, Phase::Terminating);
                prop_assert!(!out.commands.contains(&ReconcileCommand::ApplyWorkload));
            }

            // Invariant 2: Ready is unreachable unless spec, deps, and secrets are all ready.
            if out.to_phase == Phase::Ready {
                prop_assert!(!world.deleting);
                prop_assert!(world.policy_or_spec_blocker().is_none());
                prop_assert!(world.deps_ready());
                prop_assert!(world.secrets.is_ready());
            }

            // Invariant 3: Once a workload exists, never regress to Pending or Failed.
            if world.workload.exists() && phase != Phase::Pending && phase != Phase::Initializing && phase != Phase::Provisioning && phase != Phase::Failed {
                prop_assert_ne!(out.to_phase, Phase::Pending);
                prop_assert_ne!(out.to_phase, Phase::Failed);
            }

            // Invariant 4: Evaluation converges in <= MAX_EVALUATE_HOPS.
            prop_assert!(out.transition_trace.len() <= MAX_EVALUATE_HOPS);

            // Invariant 5: Re-evaluating the converged phase with its own conditions produces zero transitions.
            let mut steady_world = world.clone();
            steady_world.existing_conditions = out.conditions.clone();
            let steady = evaluate(out.to_phase, &steady_world);
            prop_assert_eq!(steady.to_phase, out.to_phase);
            prop_assert!(steady.transition_trace.is_empty());
            prop_assert_eq!(steady.conditions, out.conditions);
        }
    }
}
