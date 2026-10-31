//! One enclosing graceful-shutdown deadline (#2046 W1.10).
//!
//! SIGTERM establishes one budget for HTTP/Q6 and the process's wait for
//! clustering. The normal process-shutdown path derives stage limits from
//! it, in this order:
//!
//! 1. connection drain (at most half of `drain`) and the Q6 SM drain, both
//!    inside `drain` (`WADDLE_DRAIN_TIMEOUT_SECS`);
//! 2. the clustering room drain, inside `claim_release`
//!    (`WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS`), in parallel with 1;
//! 3. identity retirement: the `disable()` barrier, exact-owner demotion and
//!    `retire_after_shutdown`, inside [`SHUTDOWN_RETIRE_BUDGET`];
//! 4. the local terminal sweep and terminal departure write, inside
//!    [`SHUTDOWN_TERMINAL_SWEEP_BUDGET`].
//!
//! The enclosing total is the sum of all four, so stage 1 and 2 running in
//! parallel only ever leave more time for 3 and 4. A stage is clamped to
//! what remains of the enclosing deadline after the reservations of the
//! stages that follow it. Clustering terminal fencing started before process
//! stop can retain unbounded publication or demotion waits; those waits do not
//! acquire a deadline retroactively. The lifecycle stops awaiting clustering
//! at the enclosing deadline; this does not prove the task completed or cancel
//! it. The detached task may continue until the runtime is torn down, with
//! unfinished ownership left to lease expiry rather than assumed retired.
//!
//! This deadline bounds those waits, not physical process exit: telemetry
//! shutdown and runtime teardown follow them.
//!
//! The Helm chart's `terminationGracePeriodSeconds` floor
//! (`charts/waddle-server/templates/validations.yaml`) is
//! `preStopSleepSeconds` + the same four terms + a kill margin; its
//! `$retireBudget` and `$terminalSweepBudget` mirror the two constants
//! below, and `scripts/test_chart_shutdown_budget.py` checks they agree.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::time::Instant;

/// Reserved for identity retirement after the drains: the `disable()`
/// authority barrier, exact-owner demotion and the
/// `retire_after_shutdown` write that makes the node's remaining claims
/// reclaimable. Chart mirror: `$retireBudget` in `validations.yaml`.
pub const SHUTDOWN_RETIRE_BUDGET: Duration = Duration::from_secs(5);

/// Reserved for the local terminal sweep and the terminal departure write
/// after retirement. Chart mirror: `$terminalSweepBudget` in
/// `validations.yaml`.
pub const SHUTDOWN_TERMINAL_SWEEP_BUDGET: Duration = Duration::from_secs(2);

/// The configured per-stage budgets that add up to the enclosing deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownBudget {
    drain: Duration,
    claim_release: Duration,
}

impl ShutdownBudget {
    /// `drain` bounds the connection drain plus the Q6 SM drain;
    /// `claim_release` bounds the clustering room drain.
    pub fn new(drain: Duration, claim_release: Duration) -> Self {
        Self {
            drain,
            claim_release,
        }
    }

    pub fn drain(&self) -> Duration {
        self.drain
    }

    pub fn claim_release(&self) -> Duration {
        self.claim_release
    }

    /// Time reserved after the drain phase for retirement and the terminal
    /// sweep.
    pub fn tail(&self) -> Duration {
        SHUTDOWN_RETIRE_BUDGET + SHUTDOWN_TERMINAL_SWEEP_BUDGET
    }

    /// The enclosing budget from SIGTERM to the end of the last stage.
    pub fn total(&self) -> Duration {
        self.drain + self.claim_release + self.tail()
    }
}

/// The process-wide shutdown deadline. Clones share one start instant.
///
/// Nothing here starts the clock implicitly: only [`Self::start`] latches
/// it (the first caller wins), and every stage computation lives on the
/// [`StartedShutdown`] it returns. Code that may run before SIGTERM reads
/// [`Self::started`], which never latches.
#[derive(Debug, Clone)]
pub struct ShutdownDeadline {
    budget: ShutdownBudget,
    started: Arc<OnceLock<Instant>>,
}

impl ShutdownDeadline {
    pub fn new(budget: ShutdownBudget) -> Self {
        Self {
            budget,
            started: Arc::new(OnceLock::new()),
        }
    }

    pub fn budget(&self) -> ShutdownBudget {
        self.budget
    }

    /// Latch the shutdown start (first caller wins). Call only once the
    /// process stop token has fired.
    pub fn start(&self) -> StartedShutdown {
        StartedShutdown {
            budget: self.budget,
            start: *self.started.get_or_init(Instant::now),
        }
    }

    /// The shutdown already in progress, if any. Never latches.
    pub fn started(&self) -> Option<StartedShutdown> {
        self.started.get().map(|start| StartedShutdown {
            budget: self.budget,
            start: *start,
        })
    }

    /// Latch an explicit start, as if shutdown had begun at `started`.
    #[cfg(test)]
    pub(crate) fn start_at(&self, started: Instant) {
        let _ = self.started.set(started);
    }
}

/// A shutdown that has begun: the latched start plus the stage budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartedShutdown {
    budget: ShutdownBudget,
    start: Instant,
}

impl StartedShutdown {
    pub fn start(&self) -> Instant {
        self.start
    }

    pub fn budget(&self) -> ShutdownBudget {
        self.budget
    }

    /// The enclosing deadline.
    pub fn deadline(&self) -> Instant {
        self.start + self.budget.total()
    }

    /// The enclosing deadline less `reserve` for the stages after the
    /// caller, never earlier than the start.
    pub fn deadline_before(&self, reserve: Duration) -> Instant {
        self.start + self.budget.total().saturating_sub(reserve)
    }

    /// End of the connection + Q6 SM drain stage.
    pub fn drain_deadline(&self) -> Instant {
        self.start + self.budget.drain
    }

    /// End of the clustering room drain stage.
    pub fn claim_release_deadline(&self) -> Instant {
        self.start + self.budget.claim_release
    }

    /// End of the whole drain phase. The SM and room drains run in
    /// parallel, so it is the later of the two plus `margin`, but never
    /// later than the point that keeps the retirement and terminal-sweep
    /// reservations intact.
    pub fn drain_phase_deadline(&self, margin: Duration) -> Instant {
        let parallel = self.start + self.budget.drain.max(self.budget.claim_release) + margin;
        parallel.min(self.deadline_before(self.budget.tail()))
    }

    /// Time left until the enclosing deadline.
    pub fn remaining(&self) -> Duration {
        self.deadline().saturating_duration_since(Instant::now())
    }

    /// A stage's budget: its own `cap`, clamped to what remains of the
    /// enclosing deadline after `reserve` for the stages that follow it.
    pub fn stage_budget(&self, cap: Duration, reserve: Duration) -> Duration {
        cap.min(self.remaining().saturating_sub(reserve))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deadline(drain_secs: u64, claim_release_secs: u64) -> ShutdownDeadline {
        ShutdownDeadline::new(ShutdownBudget::new(
            Duration::from_secs(drain_secs),
            Duration::from_secs(claim_release_secs),
        ))
    }

    #[test]
    fn total_adds_every_stage() {
        // Chart defaults: drain 30 s, claim release 5 s, retire 5 s,
        // terminal sweep 2 s. preStop 5 s + 42 s + kill margin 5 s = 52 s.
        assert_eq!(deadline(30, 5).budget().total(), Duration::from_secs(42));
    }

    #[tokio::test(start_paused = true)]
    async fn first_observer_latches_the_shared_start() {
        let deadline = deadline(30, 5);
        let clone = deadline.clone();
        let _ = deadline.budget().total();
        assert_eq!(deadline.started(), None, "reading the budget never latches");
        let started = clone.start();
        tokio::time::advance(Duration::from_secs(3)).await;
        assert_eq!(deadline.started(), Some(started));
        assert_eq!(deadline.start(), started);
        assert_eq!(
            started.deadline(),
            started.start() + Duration::from_secs(42)
        );
        assert_eq!(started.remaining(), Duration::from_secs(39));
    }

    #[tokio::test(start_paused = true)]
    async fn drain_phase_keeps_the_tail_reservation() {
        let deadline = deadline(30, 5).start();
        let start = deadline.start();
        let margin = Duration::from_millis(100);
        assert_eq!(
            deadline.drain_phase_deadline(margin),
            start + Duration::from_secs(30) + margin
        );

        // A near-zero room budget must not let the margin eat into the
        // retirement reservation.
        let tight = ShutdownDeadline::new(ShutdownBudget::new(
            Duration::from_secs(30),
            Duration::from_millis(10),
        ))
        .start();
        let start = tight.start();
        assert_eq!(
            tight.drain_phase_deadline(margin),
            start + Duration::from_millis(30_010)
        );
        assert_eq!(
            tight.deadline() - tight.drain_phase_deadline(margin),
            tight.budget().tail()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stage_budget_never_spends_a_later_stages_reservation() {
        let deadline = deadline(30, 5).start();
        tokio::time::advance(Duration::from_secs(36)).await;
        // 6 s left: a 10 s control-plane cap keeps 2 s for the sweep.
        assert_eq!(
            deadline.stage_budget(Duration::from_secs(10), SHUTDOWN_TERMINAL_SWEEP_BUDGET),
            Duration::from_secs(4)
        );
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(
            deadline.stage_budget(Duration::from_secs(10), Duration::ZERO),
            Duration::ZERO
        );
    }
}
