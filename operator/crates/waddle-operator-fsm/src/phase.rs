use std::fmt;

use waddle_operator_crd::WaddleServerPhase;

/// Dynamic representation of the `WaddleServer` lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Initial state before the first observation pass.
    Initializing,
    /// Waiting for external Postgres and/or SpiceDB dependencies before creating workloads.
    Pending,
    /// Provisioning YOLO secrets or managed `ExternalSecret` before workload creation.
    Provisioning,
    /// Applying workload manifests and waiting for rollout convergence.
    Reconciling,
    /// Server workload and all dependencies are healthy and available.
    Ready,
    /// Live workload exists, but dependencies, secrets, spec, or pods are unhealthy.
    Degraded,
    /// Pre-deployment validation or policy failure; no workload created.
    Failed,
    /// Resource has a deletion timestamp and is tearing down.
    Terminating,
}

impl Phase {
    /// Returns the human-readable Kubernetes status phase name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initializing => "Initializing",
            Self::Pending => "Pending",
            Self::Provisioning => "Provisioning",
            Self::Reconciling => "Reconciling",
            Self::Ready => "Ready",
            Self::Degraded => "Degraded",
            Self::Failed => "Failed",
            Self::Terminating => "Terminating",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<Phase> for WaddleServerPhase {
    fn from(phase: Phase) -> Self {
        match phase {
            Phase::Initializing => Self::Initializing,
            Phase::Pending => Self::Pending,
            Phase::Provisioning => Self::Provisioning,
            Phase::Reconciling => Self::Reconciling,
            Phase::Ready => Self::Ready,
            Phase::Degraded => Self::Degraded,
            Phase::Failed => Self::Failed,
            Phase::Terminating => Self::Terminating,
        }
    }
}

impl From<WaddleServerPhase> for Phase {
    fn from(phase: WaddleServerPhase) -> Self {
        match phase {
            WaddleServerPhase::Initializing => Self::Initializing,
            WaddleServerPhase::Pending => Self::Pending,
            WaddleServerPhase::Provisioning => Self::Provisioning,
            WaddleServerPhase::Reconciling => Self::Reconciling,
            WaddleServerPhase::Ready => Self::Ready,
            WaddleServerPhase::Degraded => Self::Degraded,
            WaddleServerPhase::Failed => Self::Failed,
            WaddleServerPhase::Terminating => Self::Terminating,
        }
    }
}

mod private {
    pub trait Sealed {}
}

/// Sealed trait implemented by zero-sized typestate markers representing each [`Phase`].
pub trait PhaseState: private::Sealed {
    /// Dynamic [`Phase`] value corresponding to this compile-time state marker.
    const PHASE: Phase;
}

/// Initial state marker before dependency inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Initializing;
/// Pre-deployment waiting state marker until Postgres and SpiceDB are ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pending;
/// Secret provisioning state marker (YOLO generation or managed ESO sync).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provisioning;
/// Workload apply and rollout convergence state marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reconciling;
/// Healthy steady-state marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ready;
/// Degraded live-workload state marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Degraded;
/// Pre-deployment validation/policy error state marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failed;
/// Terminal deletion state marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminating;

impl private::Sealed for Initializing {}
impl private::Sealed for Pending {}
impl private::Sealed for Provisioning {}
impl private::Sealed for Reconciling {}
impl private::Sealed for Ready {}
impl private::Sealed for Degraded {}
impl private::Sealed for Failed {}
impl private::Sealed for Terminating {}

impl PhaseState for Initializing {
    const PHASE: Phase = Phase::Initializing;
}
impl PhaseState for Pending {
    const PHASE: Phase = Phase::Pending;
}
impl PhaseState for Provisioning {
    const PHASE: Phase = Phase::Provisioning;
}
impl PhaseState for Reconciling {
    const PHASE: Phase = Phase::Reconciling;
}
impl PhaseState for Ready {
    const PHASE: Phase = Phase::Ready;
}
impl PhaseState for Degraded {
    const PHASE: Phase = Phase::Degraded;
}
impl PhaseState for Failed {
    const PHASE: Phase = Phase::Failed;
}
impl PhaseState for Terminating {
    const PHASE: Phase = Phase::Terminating;
}
