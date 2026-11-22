use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use waddle_operator_fsm::Phase;

/// In-memory Prometheus metrics exported on `GET /metrics`.
#[derive(Debug, Default)]
pub struct OperatorMetrics {
    reconciliations_total: AtomicU64,
    reconcile_errors_total: AtomicU64,
    phase_transitions_total: AtomicU64,
    resource_phases: Mutex<BTreeMap<(String, String), Phase>>,
}

impl OperatorMetrics {
    /// Creates an initialized metrics registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Increments the total reconciliations counter.
    pub fn record_reconcile(&self) {
        self.reconciliations_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Increments the reconciler errors counter.
    pub fn record_error(&self) {
        self.reconcile_errors_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Updates tracked phase for `(namespace, name)` and records transitions.
    pub fn record_phase(&self, namespace: &str, name: &str, from: Phase, to: Phase) {
        if from != to {
            self.phase_transitions_total.fetch_add(1, Ordering::Relaxed);
        }
        let mut guard = self
            .resource_phases
            .lock()
            .expect("resource_phases mutex poisoned");
        guard.insert((namespace.to_owned(), name.to_owned()), to);
    }

    /// Removes a deleted resource from the tracked phase gauge map.
    pub fn remove_resource(&self, namespace: &str, name: &str) {
        let mut guard = self
            .resource_phases
            .lock()
            .expect("resource_phases mutex poisoned");
        guard.remove(&(namespace.to_owned(), name.to_owned()));
    }

    /// Renders metrics in Prometheus text exposition format.
    #[must_use]
    pub fn render_prometheus(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# HELP waddle_operator_reconciliations_total Total WaddleServer reconciliations"
        );
        let _ = writeln!(out, "# TYPE waddle_operator_reconciliations_total counter");
        let _ = writeln!(
            out,
            "waddle_operator_reconciliations_total {}",
            self.reconciliations_total.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "# HELP waddle_operator_reconcile_errors_total Total WaddleServer reconcile errors"
        );
        let _ = writeln!(out, "# TYPE waddle_operator_reconcile_errors_total counter");
        let _ = writeln!(
            out,
            "waddle_operator_reconcile_errors_total {}",
            self.reconcile_errors_total.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "# HELP waddle_operator_phase_transitions_total Total WaddleServer phase transitions"
        );
        let _ = writeln!(
            out,
            "# TYPE waddle_operator_phase_transitions_total counter"
        );
        let _ = writeln!(
            out,
            "waddle_operator_phase_transitions_total {}",
            self.phase_transitions_total.load(Ordering::Relaxed)
        );

        let all_phases = [
            Phase::Initializing,
            Phase::Pending,
            Phase::Provisioning,
            Phase::Reconciling,
            Phase::Ready,
            Phase::Degraded,
            Phase::Failed,
            Phase::Terminating,
        ];
        let mut counts = [0u64; 8];
        let guard = self
            .resource_phases
            .lock()
            .expect("resource_phases mutex poisoned");
        for phase in guard.values() {
            if let Some(idx) = all_phases.iter().position(|p| p == phase) {
                counts[idx] += 1;
            }
        }
        drop(guard);
        let _ = writeln!(
            out,
            "# HELP waddle_operator_resources_by_phase Current WaddleServer count per phase"
        );
        let _ = writeln!(out, "# TYPE waddle_operator_resources_by_phase gauge");
        for (phase, count) in all_phases.iter().zip(counts) {
            let _ = writeln!(
                out,
                "waddle_operator_resources_by_phase{{phase=\"{}\"}} {count}",
                phase.as_str()
            );
        }

        out
    }
}
