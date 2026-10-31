//! Process-progress signal behind the `/health` liveness probe.
//!
//! Liveness answers one question: is this process still making progress on
//! its async runtime? A restart is the remedy only for a wedged process, so
//! the signal must never depend on anything a restart cannot fix — in
//! particular not on the database, whose outages must leave pods live and
//! unready (`/ready`) rather than restart every replica at once.
//!
//! A small heartbeat task records a tick on the runtime's timer every
//! [`RUNTIME_PROGRESS_TICK`]. `/health` fails once no tick has landed for
//! [`RUNTIME_PROGRESS_STALL_LIMIT`]: the timer driver or the scheduler has
//! stopped polling ordinary tasks, which no amount of waiting fixes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use tokio::time::{Instant, MissedTickBehavior};

/// Period of the runtime-progress heartbeat.
pub(crate) const RUNTIME_PROGRESS_TICK: Duration = Duration::from_secs(1);

/// Longest gap between heartbeat ticks that liveness still reports healthy.
/// Ten missed ticks is far outside any scheduling delay a busy but working
/// runtime shows (CPU throttling delays a tick by milliseconds), yet well
/// inside the kubelet's restart window (`failureThreshold` × `periodSeconds`
/// = 30 s with the chart defaults).
pub(crate) const RUNTIME_PROGRESS_STALL_LIMIT: Duration = Duration::from_secs(10);

/// Last time the runtime-progress heartbeat ran, as milliseconds since
/// construction.
#[derive(Debug)]
pub(crate) struct RuntimeProgress {
    origin: Instant,
    last_tick_ms: AtomicU64,
}

impl RuntimeProgress {
    pub(crate) fn new() -> Self {
        Self {
            origin: Instant::now(),
            last_tick_ms: AtomicU64::new(0),
        }
    }

    /// Record that the runtime polled the heartbeat now.
    pub(crate) fn record(&self) {
        let elapsed = Instant::now().saturating_duration_since(self.origin);
        let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        self.last_tick_ms.fetch_max(elapsed_ms, Ordering::Relaxed);
    }

    /// Time since the last recorded tick (or since construction, before the
    /// first one).
    pub(crate) fn stalled_for(&self) -> Duration {
        let last_tick =
            self.origin + Duration::from_millis(self.last_tick_ms.load(Ordering::Relaxed));
        Instant::now().saturating_duration_since(last_tick)
    }
}

/// Start the heartbeat that feeds [`RuntimeProgress`]. The first tick is
/// recorded synchronously, so a long startup before the HTTP listener serves
/// never reads as a stall. The task ends when the progress record is dropped.
pub(crate) fn spawn_runtime_progress_heartbeat(progress: &Arc<RuntimeProgress>) {
    progress.record();
    let progress: Weak<RuntimeProgress> = Arc::downgrade(progress);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(RUNTIME_PROGRESS_TICK);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let Some(progress) = progress.upgrade() else {
                return;
            };
            progress.record();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn fresh_progress_is_not_stalled() {
        let progress = RuntimeProgress::new();
        assert!(progress.stalled_for() <= RUNTIME_PROGRESS_STALL_LIMIT);
        assert_eq!(progress.stalled_for(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn missing_heartbeat_reads_as_stalled_after_the_limit() {
        let progress = RuntimeProgress::new();
        tokio::time::advance(RUNTIME_PROGRESS_STALL_LIMIT).await;
        assert!(
            progress.stalled_for() <= RUNTIME_PROGRESS_STALL_LIMIT,
            "the limit itself is still live"
        );
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(progress.stalled_for() > RUNTIME_PROGRESS_STALL_LIMIT);
    }

    #[tokio::test(start_paused = true)]
    async fn running_heartbeat_keeps_progress_fresh() {
        let progress = Arc::new(RuntimeProgress::new());
        spawn_runtime_progress_heartbeat(&progress);
        // Sleeping lets the paused clock auto-advance through many ticks,
        // exactly as a working runtime would poll the heartbeat.
        tokio::time::sleep(RUNTIME_PROGRESS_STALL_LIMIT * 6).await;
        assert!(progress.stalled_for() <= RUNTIME_PROGRESS_TICK);
        assert!(progress.stalled_for() <= RUNTIME_PROGRESS_STALL_LIMIT);
    }

    #[tokio::test(start_paused = true)]
    async fn spawning_the_heartbeat_records_progress_immediately() {
        let progress = Arc::new(RuntimeProgress::new());
        tokio::time::advance(RUNTIME_PROGRESS_STALL_LIMIT * 3).await;
        assert!(progress.stalled_for() > RUNTIME_PROGRESS_STALL_LIMIT);
        spawn_runtime_progress_heartbeat(&progress);
        assert_eq!(progress.stalled_for(), Duration::ZERO);
    }
}
