//! Linux CLOCK_MONOTONIC is shared across these native processes. The deadline
//! in evidence is the exact absolute deadline armed in timerfd, not a duration
//! re-started after a barrier, process preemption, or filesystem write.
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::unix::AsyncFd;

const NANOS_PER_SECOND: u64 = 1_000_000_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MonotonicNanos(u64);
impl MonotonicNanos {
    pub fn now() -> Result<Self> {
        let mut value = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: value is a valid writable timespec; CLOCK_MONOTONIC has no
        // process-local epoch or wall-clock adjustment.
        ensure!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } == 0,
            "monotonic clock read failed"
        );
        ensure!(
            (0..NANOS_PER_SECOND as i64).contains(&value.tv_nsec),
            "invalid clock nanoseconds"
        );
        let seconds = u64::try_from(value.tv_sec).context("negative monotonic clock")?;
        Ok(Self(
            seconds
                .checked_mul(NANOS_PER_SECOND)
                .and_then(|n| n.checked_add(value.tv_nsec as u64))
                .context("monotonic clock overflow")?,
        ))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deadline {
    pub armed: MonotonicNanos,
    pub expires: MonotonicNanos,
}
impl Deadline {
    pub fn after(duration: Duration) -> Result<Self> {
        let armed = MonotonicNanos::now()?;
        Self::from_start(armed, duration)
    }
    fn from_start(armed: MonotonicNanos, duration: Duration) -> Result<Self> {
        let nanos = u64::try_from(duration.as_nanos()).context("deadline duration overflow")?;
        ensure!(nanos > 0, "zero deadline duration");
        Ok(Self {
            armed,
            expires: MonotonicNanos(armed.0.checked_add(nanos).context("deadline overflow")?),
        })
    }
    pub fn precedes_expiry(self, observation: MonotonicNanos, elapsed: MonotonicNanos) -> bool {
        self.armed <= observation && observation < self.expires && self.expires <= elapsed
    }
    pub async fn elapsed(self) -> Result<MonotonicNanos> {
        // SAFETY: timerfd_create takes no pointers and gives us one owned fd.
        let fd = unsafe {
            libc::timerfd_create(
                libc::CLOCK_MONOTONIC,
                libc::TFD_NONBLOCK | libc::TFD_CLOEXEC,
            )
        };
        ensure!(fd >= 0, "monotonic timer creation failed");
        // SAFETY: the successful create above returned a fresh descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let value = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                tv_sec: i64::try_from(self.expires.0 / NANOS_PER_SECOND)
                    .context("deadline seconds overflow")?,
                tv_nsec: (self.expires.0 % NANOS_PER_SECOND) as i64,
            },
        };
        // SAFETY: fd remains owned, value is initialized, old_value is unused.
        ensure!(
            unsafe {
                libc::timerfd_settime(
                    fd.as_raw_fd(),
                    libc::TFD_TIMER_ABSTIME,
                    &value,
                    std::ptr::null_mut(),
                )
            } == 0,
            "absolute monotonic timer setup failed"
        );
        // SAFETY: OwnedFd keeps this exact descriptor open for the entire
        // AsyncFd lifetime; nothing else closes or replaces it.
        let timer = unsafe { AsyncFd::register(fd) }?;
        loop {
            let mut ready = timer.readable().await?;
            let read = ready.try_io(|fd| {
                let mut expirations = 0u64;
                // SAFETY: the buffer is writable for exactly sizeof(u64).
                let read = unsafe {
                    libc::read(
                        fd.as_raw_fd(),
                        (&mut expirations as *mut u64).cast(),
                        std::mem::size_of::<u64>(),
                    )
                };
                if read < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if read != std::mem::size_of::<u64>() as isize || expirations == 0 {
                    return Err(std::io::Error::other("invalid timer expiration"));
                }
                Ok(())
            });
            match read {
                Ok(result) => {
                    result?;
                    let observed = MonotonicNanos::now()?;
                    ensure!(
                        observed >= self.expires,
                        "timer expired before absolute deadline"
                    );
                    return Ok(observed);
                }
                Err(_) => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_ordering_and_overflow_fail_closed() {
        let deadline =
            Deadline::from_start(MonotonicNanos(10), Duration::from_nanos(10)).expect("deadline");
        assert!(deadline.precedes_expiry(MonotonicNanos(19), MonotonicNanos(20)));
        assert!(!deadline.precedes_expiry(MonotonicNanos(20), MonotonicNanos(21)));
        assert!(!deadline.precedes_expiry(MonotonicNanos(9), MonotonicNanos(21)));
        assert!(!deadline.precedes_expiry(MonotonicNanos(11), MonotonicNanos(19)));
        assert!(Deadline::from_start(MonotonicNanos(u64::MAX), Duration::from_nanos(1)).is_err());
        assert!(Deadline::from_start(MonotonicNanos(0), Duration::MAX).is_err());
    }
    #[tokio::test]
    async fn delayed_poll_keeps_the_original_absolute_deadline() {
        let deadline = Deadline::after(Duration::from_millis(300)).expect("deadline");
        tokio::time::sleep(Duration::from_millis(350)).await;
        let elapsed = tokio::time::timeout(Duration::from_millis(100), deadline.elapsed())
            .await
            .expect("already due timer")
            .expect("clock");
        assert!(elapsed >= deadline.expires);
    }
}
