//! The clock a replica measures its revocation-fence lease on.
//!
//! A lease is valid until a deadline taken on this clock, and a decision's
//! ticket is current only while that deadline lies ahead. The clock has to
//! keep counting while the host is suspended: a host that sleeps through
//! the end of its lease must find the lease ended when it wakes, since
//! PostgreSQL ended it on its own clock meanwhile and a revoke may have
//! stopped waiting for the replica. `std::time::Instant` is
//! `CLOCK_MONOTONIC` on Linux, which stops during a suspend, so the lease
//! is measured on `CLOCK_BOOTTIME` instead, which does not.
//!
//! Other platforms have no `CLOCK_BOOTTIME`. There the lease is measured on
//! `std::time::Instant`, and [`suspend_counted`] says so: a replica that
//! runs there and is suspended in the middle of a lease can act on a
//! decision after a revoke was acknowledged without it. Production replicas
//! run on Linux; the fence warns once when it starts elsewhere.

use std::time::Duration;

/// A reading of the boot-time clock: the time since an arbitrary origin
/// fixed while the host runs, counting time it spent suspended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct BootInstant(Duration);

impl BootInstant {
    /// The clock now, or `None` when it cannot be read. A caller that
    /// cannot read the clock decides nothing under a lease.
    pub(super) fn now() -> Option<Self> {
        read_clock().map(Self)
    }

    /// The reading `duration` after this one, if it can be represented.
    pub(super) fn checked_add(self, duration: Duration) -> Option<Self> {
        self.0.checked_add(duration).map(Self)
    }
}

/// Whether the lease clock of this platform keeps counting while the host
/// is suspended.
pub(super) const fn suspend_counted() -> bool {
    cfg!(target_os = "linux")
}

#[cfg(target_os = "linux")]
fn read_clock() -> Option<Duration> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable `timespec` that lives for the whole
    // call, and `CLOCK_BOOTTIME` is a clock the Linux kernel defines (since
    // 2.6.39). `clock_gettime` writes only into `now` and reports failure
    // through its return value, which is checked before `now` is read.
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
