/// Errors returned when driving the `WaddleServer` state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TransitionError {
    /// Safety bound exceeded while converging multi-hop state transitions.
    #[error("state machine exceeded maximum convergence hops ({max_hops})")]
    MaxHopsExceeded {
        /// Maximum hops permitted in a single evaluation pass.
        max_hops: usize,
    },
}
