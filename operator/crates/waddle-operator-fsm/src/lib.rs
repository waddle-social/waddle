//! Pure, zero-I/O typestate and convergent reconciliation state machine
//! for `WaddleServer` resources.

mod command;
mod condition;
mod error;
mod observation;
mod phase;
mod transition;

pub use command::{EventNotice, EventSeverity, ReconcileCommand, RequeuePolicy};
pub use condition::{compute_conditions, ConditionKind, ConditionRecord, ConditionStatus};
pub use error::TransitionError;
pub use observation::{
    DependencyHealth, ObservedWorld, SecretSourceKind, SecretsObservation, SensitiveKeyRotation,
    SpecValidationState, WorkloadObservation,
};
pub use phase::{
    Degraded, Failed, Initializing, Pending, Phase, PhaseState, Provisioning, Ready, Reconciling,
    Terminating,
};
pub use transition::{evaluate, CanTransitionTo, StateMachine, TransitionOutcome};
