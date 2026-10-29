//! Bounded, content-free supervisor diagnostics. No process output, arguments,
//! environment, paths, or credential values are captured in these causes.
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum StopPhase {
    ProcessGroup,
    LeaderReap,
    Postgres,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ProcessFault {
    ProcessStat {
        pid: u32,
        errno: Option<i32>,
    },
    ObserveExit {
        errno: Option<i32>,
    },
    Signal {
        errno: Option<i32>,
    },
    Deadline {
        phase: StopPhase,
        postgres: Option<crate::postgres_diagnostics::PostgresSnapshot>,
    },
    PostgresExit {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Io {
        errno: Option<i32>,
    },
    ReapNotReady,
    Invariant,
    LatchUnavailable,
    #[cfg(test)]
    InjectedStop,
    #[cfg(test)]
    InjectedPostSpawn,
    #[cfg(test)]
    InjectedShutdown,
}

impl fmt::Display for ProcessFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl Error for ProcessFault {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum CleanupService {
    SpiceDb,
    Postgres,
    Worker,
    Initdb,
    Migration,
    Version,
    Fixture,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CleanupFailure {
    pub service: CleanupService,
    pub cause: ProcessFault,
}

impl CleanupFailure {
    pub fn capture(service: CleanupService, error: &anyhow::Error) -> Self {
        let cause = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ProcessFault>().cloned())
            .or_else(|| {
                error.chain().find_map(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .map(|error| ProcessFault::Io {
                            errno: error.raw_os_error(),
                        })
                })
            })
            .unwrap_or(ProcessFault::Invariant);
        Self { service, cause }
    }
}
impl fmt::Display for CleanupFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl Error for CleanupFailure {}

/// Shared by every process allowed to access a private fixture. Cleanup failure
/// survives local handle Drop and successful later retries. Poison is unsafe.
#[derive(Clone, Default)]
pub struct CleanupLatch(Arc<Mutex<Option<CleanupFailure>>>);

impl CleanupLatch {
    pub fn record(&self, failure: CleanupFailure) {
        if let Ok(mut first) = self.0.lock() {
            first.get_or_insert(failure);
        }
    }
    pub fn failure(&self) -> Option<CleanupFailure> {
        match self.0.lock() {
            Ok(first) => first.clone(),
            Err(_) => Some(CleanupFailure {
                service: CleanupService::Fixture,
                cause: ProcessFault::LatchUnavailable,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn poisoned_latch_is_cleanup_uncertainty() {
        let latch = CleanupLatch::default();
        let poison = latch.clone();
        let outcome = std::thread::spawn(move || {
            let _guard = poison.0.lock().expect("initial latch lock");
            panic!("intentional latch poison");
        })
        .join();
        assert!(outcome.is_err());
        assert_eq!(
            latch.failure(),
            Some(CleanupFailure {
                service: CleanupService::Fixture,
                cause: ProcessFault::LatchUnavailable
            })
        );
    }
}
