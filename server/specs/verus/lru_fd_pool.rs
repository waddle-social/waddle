// Verus formal proof: Bounded LRU FD pool accounting for Turso per-tenant DBs.
// Paired with `server/crates/waddle-broker-core/src/kernels/lru_fd_pool.rs`.
//
// Proves:
// 1. Invariant `reserved_fds < rlimit_nofile && capacity >= 1 && capacity <= rlimit_nofile - reserved_fds
//    && pinned_count <= open_count && open_count <= capacity` holds across all transitions.
// 2. Eviction NEVER closes a pinned handle (`EvictedUnpinnedAndOpened` only fires when
//    `pinned_count < open_count`; returns `Err(AllHandlesPinned)` when `pinned_count == open_count`).

use vstd::prelude::*;

verus! {

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum AcquireAction {
    ReusedOpen,
    OpenedFresh,
    EvictedUnpinnedAndOpened,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum FdPoolError {
    InvalidRlimitBudget,
    CapacityExceedsRlimit,
    ZeroCapacity,
    AllHandlesPinned,
    PinExceedsOpen,
    NoPinnedHandle,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct FdPoolAccounting {
    pub rlimit_nofile: usize,
    pub reserved_fds: usize,
    pub capacity: usize,
    pub open_count: usize,
    pub pinned_count: usize,
}

impl FdPoolAccounting {
    pub open spec fn inv(&self) -> bool {
        self.reserved_fds < self.rlimit_nofile
            && self.capacity >= 1
            && self.capacity <= self.rlimit_nofile - self.reserved_fds
            && self.pinned_count <= self.open_count
            && self.open_count <= self.capacity
    }

    pub fn new(rlimit_nofile: usize, reserved_fds: usize, capacity: usize) -> (res: Result<Self, FdPoolError>)
        ensures
            match res {
                Ok(p) => p.inv() && p.open_count == 0 && p.pinned_count == 0,
                Err(_) => true,
            },
    {
        if reserved_fds >= rlimit_nofile {
            return Err(FdPoolError::InvalidRlimitBudget);
        }
        if capacity == 0 {
            return Err(FdPoolError::ZeroCapacity);
        }
        if capacity > rlimit_nofile - reserved_fds {
            return Err(FdPoolError::CapacityExceedsRlimit);
        }
        Ok(FdPoolAccounting {
            rlimit_nofile,
            reserved_fds,
            capacity,
            open_count: 0,
            pinned_count: 0,
        })
    }

    pub fn acquire_open(&mut self, already_open: bool) -> (res: Result<AcquireAction, FdPoolError>)
        requires
            old(self).inv(),
        ensures
            final(self).inv(),
            final(self).open_count <= final(self).capacity,
            final(self).capacity <= final(self).rlimit_nofile - final(self).reserved_fds,
            res == Ok::<AcquireAction, FdPoolError>(AcquireAction::EvictedUnpinnedAndOpened)
                ==> old(self).pinned_count < old(self).open_count,
            (!already_open && old(self).open_count == old(self).capacity && old(self).pinned_count == old(self).open_count)
                ==> res == Err::<AcquireAction, FdPoolError>(FdPoolError::AllHandlesPinned),
            res.is_err() ==> *final(self) == *old(self),
    {
        if already_open {
            if self.pinned_count >= self.open_count {
                return Err(FdPoolError::PinExceedsOpen);
            }
            self.pinned_count = self.pinned_count + 1;
            return Ok(AcquireAction::ReusedOpen);
        }

        if self.open_count < self.capacity {
            self.open_count = self.open_count + 1;
            self.pinned_count = self.pinned_count + 1;
            return Ok(AcquireAction::OpenedFresh);
        }

        if self.pinned_count < self.open_count {
            self.pinned_count = self.pinned_count + 1;
            return Ok(AcquireAction::EvictedUnpinnedAndOpened);
        }

        Err(FdPoolError::AllHandlesPinned)
    }

    pub fn release_pin(&mut self) -> (res: Result<(), FdPoolError>)
        requires
            old(self).inv(),
        ensures
            final(self).inv(),
            res.is_ok() ==> final(self).pinned_count == old(self).pinned_count - 1,
            res.is_err() ==> *final(self) == *old(self),
    {
        if self.pinned_count == 0 {
            return Err(FdPoolError::NoPinnedHandle);
        }
        self.pinned_count = self.pinned_count - 1;
        Ok(())
    }

    pub fn close_unpinned(&mut self) -> (res: Result<(), FdPoolError>)
        requires
            old(self).inv(),
        ensures
            final(self).inv(),
            res.is_ok() ==> old(self).open_count > old(self).pinned_count && final(self).open_count == old(self).open_count - 1,
            res.is_err() ==> *final(self) == *old(self),
    {
        if self.open_count <= self.pinned_count {
            return Err(FdPoolError::AllHandlesPinned);
        }
        self.open_count = self.open_count - 1;
        Ok(())
    }
}

fn main() {}

} // verus!
