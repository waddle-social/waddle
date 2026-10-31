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
    SpawnClosed,
    OutstandingChildren {
        count: u64,
    },
    ChildPermitLost,
    CountOverflow,
    CountUnderflow,
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

/// One lock linearizes reservation, close, and first-failure reporting. Never
/// held across spawning, waiting, stopping children, or filesystem operations.
#[derive(Default)]
struct State {
    closed: bool,
    outstanding: u64,
    first_failure: Option<CleanupFailure>,
}
#[derive(Clone, Default)]
pub(crate) struct CleanupLatch(Arc<Mutex<State>>);

impl CleanupLatch {
    #[cfg(test)]
    pub(crate) fn outstanding(&self) -> u64 {
        self.0.lock().expect("test gate").outstanding
    }
    #[cfg(test)]
    pub(crate) fn poison(&self) {
        let gate = self.clone();
        assert!(std::thread::spawn(move || {
            let _lock = gate.0.lock().expect("initial gate");
            panic!("intentional gate poison");
        })
        .join()
        .is_err());
    }
    pub(crate) fn record(&self, failure: CleanupFailure) {
        if let Ok(mut state) = self.0.lock() {
            state.first_failure.get_or_insert(failure);
        }
    }
    pub(crate) fn failure(&self) -> Option<CleanupFailure> {
        match self.0.lock() {
            Ok(state) => state.first_failure.clone(),
            Err(_) => Some(CleanupFailure {
                service: CleanupService::Fixture,
                cause: ProcessFault::LatchUnavailable,
            }),
        }
    }
    pub(crate) fn reserve(&self, service: CleanupService) -> anyhow::Result<PreSpawnPermit> {
        let mut state = self.0.lock().map_err(|_| ProcessFault::LatchUnavailable)?;
        if state.closed {
            return Err(ProcessFault::SpawnClosed.into());
        }
        match state.outstanding.checked_add(1) {
            Some(count) => state.outstanding = count,
            None => {
                state.first_failure.get_or_insert(CleanupFailure {
                    service,
                    cause: ProcessFault::CountOverflow,
                });
                return Err(ProcessFault::CountOverflow.into());
            }
        }
        Ok(PreSpawnPermit {
            cleanup: self.clone(),
            service,
            rollback: true,
        })
    }
    fn release(&self, service: CleanupService) -> anyhow::Result<()> {
        let mut state = self.0.lock().map_err(|_| ProcessFault::LatchUnavailable)?;
        match state.outstanding.checked_sub(1) {
            Some(count) => state.outstanding = count,
            None => {
                state.first_failure.get_or_insert(CleanupFailure {
                    service,
                    cause: ProcessFault::CountUnderflow,
                });
                return Err(ProcessFault::CountUnderflow.into());
            }
        }
        Ok(())
    }
    pub(crate) fn close(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.closed = true;
        }
    }
    pub(crate) fn require_no_children(&self) {
        if let Ok(mut state) = self.0.lock() {
            let count = state.outstanding;
            if count != 0 {
                state.first_failure.get_or_insert(CleanupFailure {
                    service: CleanupService::Fixture,
                    cause: ProcessFault::OutstandingChildren { count },
                });
            }
        }
    }
}

/// Only this pre-spawn state may roll its count back in Drop. It is consumed
/// infallibly into Process immediately after a successful OS spawn.
pub(crate) struct PreSpawnPermit {
    cleanup: CleanupLatch,
    service: CleanupService,
    rollback: bool,
}
impl PreSpawnPermit {
    pub(crate) fn activate(mut self) -> LivePermit {
        self.rollback = false;
        LivePermit {
            cleanup: self.cleanup.clone(),
            service: self.service,
            released: false,
        }
    }
}
impl Drop for PreSpawnPermit {
    fn drop(&mut self) {
        if self.rollback {
            if let Err(error) = self.cleanup.release(self.service) {
                self.cleanup
                    .record(CleanupFailure::capture(self.service, &error));
            }
        }
    }
}

/// A lost LIVE permit stays counted forever. Field Drop is not evidence that
/// the child's process group or PostgreSQL backends stopped.
pub(crate) struct LivePermit {
    cleanup: CleanupLatch,
    service: CleanupService,
    released: bool,
}
impl LivePermit {
    pub(crate) fn release_verified(&mut self) -> anyhow::Result<()> {
        if !self.released {
            self.cleanup.release(self.service)?;
            self.released = true;
        }
        Ok(())
    }
}
impl Drop for LivePermit {
    fn drop(&mut self) {
        if !self.released {
            self.cleanup.record(CleanupFailure {
                service: self.service,
                cause: ProcessFault::ChildPermitLost,
            });
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
    #[test]
    fn permits_fail_closed_on_overflow_underflow_and_lost_live_ownership() {
        let gate = CleanupLatch::default();
        gate.0.lock().expect("gate").outstanding = u64::MAX;
        assert!(gate.reserve(CleanupService::Worker).is_err());
        assert_eq!(
            gate.failure().expect("failure").cause,
            ProcessFault::CountOverflow
        );
        assert_eq!(gate.outstanding(), u64::MAX);
        let gate = CleanupLatch::default();
        assert!(gate.release(CleanupService::Worker).is_err());
        assert_eq!(
            gate.failure().expect("failure").cause,
            ProcessFault::CountUnderflow
        );
        let gate = CleanupLatch::default();
        let pre = gate.reserve(CleanupService::Worker).expect("reserve");
        assert_eq!(gate.outstanding(), 1);
        drop(pre);
        assert_eq!(gate.outstanding(), 0);
        let live = gate
            .reserve(CleanupService::Worker)
            .expect("reserve")
            .activate();
        drop(live);
        assert_eq!(gate.outstanding(), 1);
        assert_eq!(
            gate.failure().expect("failure").cause,
            ProcessFault::ChildPermitLost
        );
    }
    #[test]
    fn verified_release_is_idempotent_and_close_never_reopens() {
        let gate = CleanupLatch::default();
        let mut live = gate
            .reserve(CleanupService::Worker)
            .expect("reserve")
            .activate();
        gate.close();
        live.release_verified().expect("verified release");
        live.release_verified().expect("idempotent release");
        drop(live);
        gate.require_no_children();
        assert_eq!(gate.outstanding(), 0);
        assert!(gate.failure().is_none());
        assert!(gate.reserve(CleanupService::Worker).is_err());
    }
}
