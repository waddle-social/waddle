//! A once-a-minute limit on one repeated warning.
//!
//! A refusal that every request of a surface can hit (the store cannot be
//! read, the enrolled set has not loaded) would otherwise log once per
//! request. Only the kind of the failure is logged, never an identifier.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// At most one warning per [`RateLimitedWarning::INTERVAL`].
pub(crate) struct RateLimitedWarning {
    /// Milliseconds after the process's first warning check at which the
    /// next warning may be logged.
    next_ms: AtomicU64,
}

impl RateLimitedWarning {
    const INTERVAL: Duration = Duration::from_secs(60);

    pub(crate) const fn new() -> Self {
        Self {
            next_ms: AtomicU64::new(0),
        }
    }

    /// Whether the warning may be logged now. A `true` answer reserves the
    /// interval, so concurrent callers log it once.
    pub(crate) fn due(&self) -> bool {
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        let now = u64::try_from(ORIGIN.get_or_init(Instant::now).elapsed().as_millis())
            .unwrap_or(u64::MAX);
        let due = self.next_ms.load(Ordering::Relaxed);
        let next =
            now.saturating_add(u64::try_from(Self::INTERVAL.as_millis()).unwrap_or(u64::MAX));
        now >= due
            && self
                .next_ms
                .compare_exchange(due, next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
    }
}
