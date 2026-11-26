//! Pure-Rust bounded LRU FD pool accounting state machine for Turso per-tenant DBs.
//!
//! Formally verified for `open_count <= capacity <= rlimit_nofile - reserved_fds`
//! and never evicting a pinned active handle in `server/specs/verus/lru_fd_pool.rs`.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireAction {
    /// Handle was already open in the pool; pin count incremented.
    ReusedOpen,
    /// Pool had spare capacity; opened a new handle and pinned it.
    OpenedFresh,
    /// Pool was at capacity with an unpinned handle available; evicted one unpinned
    /// handle (with WAL checkpoint) and opened + pinned the requested handle.
    EvictedUnpinnedAndOpened,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FdPoolError {
    #[error("reserved_fds must be strictly less than rlimit_nofile")]
    InvalidRlimitBudget,
    #[error("pool capacity exceeds rlimit_nofile - reserved_fds")]
    CapacityExceedsRlimit,
    #[error("pool capacity must be at least 1")]
    ZeroCapacity,
    #[error("all open handles in the FD pool are currently pinned")]
    AllHandlesPinned,
    #[error("cannot pin more handles than are open")]
    PinExceedsOpen,
    #[error("cannot release pin when pinned_count is zero")]
    NoPinnedHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FdPoolAccounting {
    pub rlimit_nofile: usize,
    pub reserved_fds: usize,
    pub capacity: usize,
    pub open_count: usize,
    pub pinned_count: usize,
}

impl FdPoolAccounting {
    pub fn new(
        rlimit_nofile: usize,
        reserved_fds: usize,
        capacity: usize,
    ) -> Result<Self, FdPoolError> {
        if reserved_fds >= rlimit_nofile {
            return Err(FdPoolError::InvalidRlimitBudget);
        }
        if capacity == 0 {
            return Err(FdPoolError::ZeroCapacity);
        }
        if capacity > rlimit_nofile - reserved_fds {
            return Err(FdPoolError::CapacityExceedsRlimit);
        }
        Ok(Self {
            rlimit_nofile,
            reserved_fds,
            capacity,
            open_count: 0,
            pinned_count: 0,
        })
    }

    /// Accounts for acquiring a pinned handle for a tenant DB.
    ///
    /// - `already_open`: `true` if the tenant's DB handle is already resident in the pool.
    /// - Never exceeds `capacity` and never evicts a pinned handle.
    pub fn acquire_open(&mut self, already_open: bool) -> Result<AcquireAction, FdPoolError> {
        if already_open {
            if self.pinned_count >= self.open_count {
                return Err(FdPoolError::PinExceedsOpen);
            }
            self.pinned_count += 1;
            return Ok(AcquireAction::ReusedOpen);
        }

        if self.open_count < self.capacity {
            self.open_count += 1;
            self.pinned_count += 1;
            return Ok(AcquireAction::OpenedFresh);
        }

        // Pool is at capacity: can only evict if at least one open handle is unpinned.
        if self.pinned_count < self.open_count {
            // Evict 1 unpinned handle (-1 open), open new handle (+1 open, +1 pin).
            self.pinned_count += 1;
            return Ok(AcquireAction::EvictedUnpinnedAndOpened);
        }

        Err(FdPoolError::AllHandlesPinned)
    }

    pub fn release_pin(&mut self) -> Result<(), FdPoolError> {
        if self.pinned_count == 0 {
            return Err(FdPoolError::NoPinnedHandle);
        }
        self.pinned_count -= 1;
        Ok(())
    }

    /// Explicitly closes one unpinned idle handle (e.g., on tenant delete or drain).
    pub fn close_unpinned(&mut self) -> Result<(), FdPoolError> {
        if self.open_count <= self.pinned_count {
            return Err(FdPoolError::AllHandlesPinned);
        }
        self.open_count -= 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_rlimit_and_never_evicts_pinned() {
        assert_eq!(
            FdPoolAccounting::new(100, 100, 1),
            Err(FdPoolError::InvalidRlimitBudget)
        );
        assert_eq!(
            FdPoolAccounting::new(100, 80, 21),
            Err(FdPoolError::CapacityExceedsRlimit)
        );

        let mut pool = FdPoolAccounting::new(100, 80, 2).expect("valid pool");
        assert_eq!(pool.acquire_open(false), Ok(AcquireAction::OpenedFresh));
        assert_eq!(pool.acquire_open(false), Ok(AcquireAction::OpenedFresh));
        // Both handles are pinned at capacity => must fail rather than evicting pinned.
        assert_eq!(pool.acquire_open(false), Err(FdPoolError::AllHandlesPinned));

        pool.release_pin().expect("release one pin");
        assert_eq!(
            pool.acquire_open(false),
            Ok(AcquireAction::EvictedUnpinnedAndOpened)
        );
        assert_eq!(pool.open_count, 2);
        assert_eq!(pool.pinned_count, 2);
    }
}
