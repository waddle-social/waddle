//! Pure-Rust overflow-safe spend & budget ledger arithmetic kernel.
//!
//! Formally verified for zero overflow/underflow, `spent + reserved <= signed_cap`,
//! and monotonic non-decreasing `spent` in `server/specs/verus/budget_ledger.rs`.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetState {
    pub signed_cap: u64,
    pub spent: u64,
    pub reserved: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BudgetError {
    #[error("reservation exceeds signed cap")]
    CapExceeded,
    #[error("arithmetic overflow")]
    Overflow,
    #[error("reserved balance underflow")]
    ReservedUnderflow,
    #[error("actual spend exceeds reserved amount")]
    ActualExceedsReserved,
}

impl BudgetState {
    pub const fn new(signed_cap: u64) -> Self {
        Self {
            signed_cap,
            spent: 0,
            reserved: 0,
        }
    }

    pub fn reserve(&mut self, amount: u64) -> Result<(), BudgetError> {
        let new_reserved = self
            .reserved
            .checked_add(amount)
            .ok_or(BudgetError::Overflow)?;
        let total = self
            .spent
            .checked_add(new_reserved)
            .ok_or(BudgetError::Overflow)?;
        if total > self.signed_cap {
            return Err(BudgetError::CapExceeded);
        }
        self.reserved = new_reserved;
        Ok(())
    }

    pub fn commit(&mut self, reserved_amount: u64, actual_amount: u64) -> Result<(), BudgetError> {
        if actual_amount > reserved_amount {
            return Err(BudgetError::ActualExceedsReserved);
        }
        let new_reserved = self
            .reserved
            .checked_sub(reserved_amount)
            .ok_or(BudgetError::ReservedUnderflow)?;
        let new_spent = self
            .spent
            .checked_add(actual_amount)
            .ok_or(BudgetError::Overflow)?;
        let total = new_spent
            .checked_add(new_reserved)
            .ok_or(BudgetError::Overflow)?;
        if total > self.signed_cap {
            return Err(BudgetError::CapExceeded);
        }
        self.reserved = new_reserved;
        self.spent = new_spent;
        Ok(())
    }

    pub fn release(&mut self, reserved_amount: u64) -> Result<(), BudgetError> {
        let new_reserved = self
            .reserved
            .checked_sub(reserved_amount)
            .ok_or(BudgetError::ReservedUnderflow)?;
        self.reserved = new_reserved;
        Ok(())
    }

    /// Conservatively accounts for an `unknown_after_send` quarantined attempt by
    /// charging the full `reserved_amount` to `spent` so the budget is never over-released.
    pub fn quarantine_conservative(&mut self, reserved_amount: u64) -> Result<(), BudgetError> {
        self.commit(reserved_amount, reserved_amount)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::{prop_assert, proptest};

    use super::*;

    #[test]
    fn reserve_commit_release_and_quarantine() {
        let mut ledger = BudgetState::new(100);
        ledger.reserve(60).expect("reserve 60");
        assert_eq!(ledger.reserved, 60);
        assert_eq!(ledger.reserve(41), Err(BudgetError::CapExceeded));

        ledger.commit(60, 45).expect("commit 45");
        assert_eq!(ledger.spent, 45);
        assert_eq!(ledger.reserved, 0);

        ledger.reserve(50).expect("reserve 50");
        ledger
            .quarantine_conservative(50)
            .expect("quarantine conservative");
        assert_eq!(ledger.spent, 95);
        assert_eq!(ledger.reserved, 0);
        assert_eq!(ledger.reserve(6), Err(BudgetError::CapExceeded));
        ledger.reserve(5).expect("reserve remaining 5");
        ledger.release(5).expect("release 5");
        assert_eq!(ledger.reserved, 0);
    }

    #[test]
    fn overflow_near_u64_max_is_rejected() {
        let mut ledger = BudgetState::new(u64::MAX);
        ledger.reserve(u64::MAX - 10).expect("reserve near max");
        assert_eq!(ledger.reserve(11), Err(BudgetError::Overflow));
    }

    proptest! {
        #[test]
        fn prop_budget_invariant_always_holds(
            cap in 0u64..10_000,
            ops in proptest::collection::vec((0u8..4, 0u64..2_000, 0u64..2_000), 1..64)
        ) {
            let mut ledger = BudgetState::new(cap);
            let mut prev_spent = 0u64;
            for (op, a, b) in ops {
                match op {
                    0 => { let _ = ledger.reserve(a); }
                    1 => { let _ = ledger.commit(a, b); }
                    2 => { let _ = ledger.release(a); }
                    _ => { let _ = ledger.quarantine_conservative(a); }
                }
                prop_assert!(ledger.spent >= prev_spent);
                prop_assert!(ledger.spent.checked_add(ledger.reserved).is_some_and(|t| t <= ledger.signed_cap));
                prev_spent = ledger.spent;
            }
        }
    }
}
