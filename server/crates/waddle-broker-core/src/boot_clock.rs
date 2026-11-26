//! Suspend-aware monotonic boot-time clock (`CLOCK_BOOTTIME` on Linux).
//!
//! A lease is valid until a deadline measured on this clock. Unlike
//! `std::time::Instant` (`CLOCK_MONOTONIC` on Linux, which pauses while the
//! host is suspended), `CLOCK_BOOTTIME` keeps advancing across host suspend
//! so a replica that sleeps past its lease expiry immediately self-fences on
//! wake without touching storage or provider dispatch.
//!
//! On non-Linux platforms where `CLOCK_BOOTTIME` is unavailable, this falls
//! back to `std::time::Instant` and [`suspend_counted`] returns `false`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// A reading of the boot-time clock: elapsed time since an arbitrary origin
/// fixed while the host runs, counting time spent suspended on Linux.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BootInstant(Duration);

impl BootInstant {
    /// Current boot-time reading, or `None` if the OS clock cannot be read.
    /// Callers that cannot read the clock must fail closed.
    pub fn now() -> Option<Self> {
        read_clock().map(Self)
    }

    /// Construct a `BootInstant` from a raw `Duration` since the clock origin.
    pub const fn from_raw_duration(duration: Duration) -> Self {
        Self(duration)
    }

    /// Raw elapsed duration since the clock origin.
    pub const fn as_raw_duration(self) -> Duration {
        self.0
    }

    /// Reading `duration` after this instant, if representable.
    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        self.0.checked_add(duration).map(Self)
    }

    /// Reading `duration` before this instant, if representable.
    pub fn checked_sub(self, duration: Duration) -> Option<Self> {
        self.0.checked_sub(duration).map(Self)
    }

    /// Elapsed duration since `earlier`, saturating at zero if `self < earlier`.
    pub fn saturating_duration_since(self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }
}

/// Whether the platform boot clock continues counting while the host is suspended.
pub const fn suspend_counted() -> bool {
    cfg!(target_os = "linux")
}

/// Injectable boot-clock abstraction so lease guards and drain budgets can use
/// `SystemBootClock` in production and `FakeBootClock` in deterministic tests.
pub trait BootClock: Send + Sync {
    fn now(&self) -> Option<BootInstant>;
    fn suspend_counted(&self) -> bool;
}

/// Production boot clock backed by `CLOCK_BOOTTIME` on Linux.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemBootClock;

impl BootClock for SystemBootClock {
    fn now(&self) -> Option<BootInstant> {
        BootInstant::now()
    }

    fn suspend_counted(&self) -> bool {
        suspend_counted()
    }
}

/// Deterministic controllable boot clock for testing lease expiry, suspend
/// jumps, and `WADDLE_DRAIN_TIMEOUT_SECS` budgets without sleeping.
#[derive(Debug)]
pub struct FakeBootClock {
    nanos: AtomicU64,
    available: AtomicBool,
    suspend_aware: bool,
}

impl FakeBootClock {
    pub fn new(initial: Duration) -> Self {
        let initial_nanos = u64::try_from(initial.as_nanos()).unwrap_or(u64::MAX);
        Self {
            nanos: AtomicU64::new(initial_nanos),
            available: AtomicBool::new(true),
            suspend_aware: true,
        }
    }

    /// Advance the clock by `delta` (e.g., to simulate normal elapsed time or
    /// host suspend while `std::time::Instant` barely moves).
    pub fn advance(&self, delta: Duration) {
        let delta_nanos = u64::try_from(delta.as_nanos()).unwrap_or(u64::MAX);
        self.nanos
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                Some(cur.saturating_add(delta_nanos))
            })
            .ok();
    }

    /// Alias for [`Self::advance`] emphasizing host suspend simulation.
    pub fn advance_suspend(&self, delta: Duration) {
        self.advance(delta);
    }

    /// Simulate clock read failure (`now()` returns `None`) to test fail-closed paths.
    pub fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::SeqCst);
    }
}

impl BootClock for FakeBootClock {
    fn now(&self) -> Option<BootInstant> {
        if !self.available.load(Ordering::SeqCst) {
            return None;
        }
        Some(BootInstant::from_raw_duration(Duration::from_nanos(
            self.nanos.load(Ordering::SeqCst),
        )))
    }

    fn suspend_counted(&self) -> bool {
        self.suspend_aware
    }
}

#[cfg(target_os = "linux")]
fn read_clock() -> Option<Duration> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable `timespec` that lives for the entire
    // call, and `CLOCK_BOOTTIME` is a standard Linux kernel clock (since 2.6.39).
    // `clock_gettime` writes only into `now` and signals failure via its return
    // value, which is checked before `now` is read.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut now) };
    if status != 0 {
        return None;
    }
    let seconds = u64::try_from(now.tv_sec).ok()?;
    let nanoseconds = u32::try_from(now.tv_nsec)
        .ok()
        .filter(|nanoseconds| *nanoseconds < 1_000_000_000)?;
    Some(Duration::new(seconds, nanoseconds))
}

#[cfg(not(target_os = "linux"))]
fn read_clock() -> Option<Duration> {
    use std::sync::OnceLock;
    use std::time::Instant;

    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    Some(ORIGIN.get_or_init(Instant::now).elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_boot_clock_reads_monotonically() {
        let clock = SystemBootClock;
        let t1 = clock.now().expect("boot clock should read");
        let t2 = clock.now().expect("boot clock should read");
        assert!(t2 >= t1);
        assert_eq!(clock.suspend_counted(), cfg!(target_os = "linux"));
    }

    #[test]
    fn fake_boot_clock_simulates_suspend_and_fail_closed() {
        let clock = FakeBootClock::new(Duration::from_secs(100));
        let start = clock.now().expect("available");
        let deadline = start
            .checked_add(Duration::from_secs(5))
            .expect("deadline fits");

        clock.advance_suspend(Duration::from_secs(6));
        let woken = clock.now().expect("available");
        assert!(woken >= deadline);
        assert_eq!(
            woken.saturating_duration_since(start),
            Duration::from_secs(6)
        );

        clock.set_available(false);
        assert_eq!(clock.now(), None);
    }
}
