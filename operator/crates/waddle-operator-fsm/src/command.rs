/// Severity of a Kubernetes Event emitted by the reconciler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSeverity {
    /// Normal operational event.
    Normal,
    /// Warning event highlighting risk or failure.
    Warning,
}

/// Declarative Kubernetes Event request emitted by the pure state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventNotice {
    /// Event severity (`Normal` or `Warning`).
    pub severity: EventSeverity,
    /// `PascalCase` Kubernetes event reason.
    pub reason: String,
    /// Human-readable message (MUST never contain secret values).
    pub message: String,
}

/// Declarative side-effect instruction produced by [`crate::transition::evaluate`].
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileCommand {
    /// Create (never overwrite existing keys) the `<name>-yolo-secrets` Secret.
    EnsureYoloSecret,
    /// Server-Side Apply the operator-managed ESO `ExternalSecret`.
    ApplyManagedExternalSecret,
    /// Server-Side Apply owned server `ConfigMap`, `ServiceAccount`, `PVC`, `Service`s,
    /// `Deployment`, and clustering `PodDisruptionBudget` / `NetworkPolicy`.
    ApplyWorkload,
    /// Emit a Kubernetes Event on the `WaddleServer` resource.
    EmitEvent(EventNotice),
}

/// Requeue cadence chosen by the state machine.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequeuePolicy {
    /// Do not schedule a timer requeue (used in `Terminating`).
    None,
    /// Requeue after the given number of seconds.
    AfterSeconds(u64),
}
