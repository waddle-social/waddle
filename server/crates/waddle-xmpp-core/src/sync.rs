//! Fail-stop handling for poisoned in-process state.
//!
//! A poisoned lock may expose a partially completed state transition. Infallible
//! APIs cannot report that uncertainty, and a panic in a detached async task may
//! leave the rest of the process serving traffic. These paths terminate the
//! process so its service supervisor can restart it from durable state.

/// Terminate the process without inspecting or recovering a poisoned guard.
///
/// Use as the error callback to [`Result::unwrap_or_else`] only where the API
/// cannot propagate a typed failure. Aborting also stops sibling tasks: ordinary
/// unwinding would allow detached workers or their supervisors to disappear
/// while the process continued advertising healthy authority.
///
/// The diagnostic does not print protected state.
/// This function never returns, including when invoked inside a Tokio task.
pub fn abort_on_poison<T>(_poisoned: std::sync::PoisonError<T>) -> T {
    use std::io::Write;

    let _ = writeln!(
        std::io::stderr().lock(),
        "fatal invariant: poisoned lock; terminating process without recovering state",
    );
    std::process::abort()
}
