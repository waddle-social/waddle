// Verus formal proof: Overflow-safe spend & budget ledger arithmetic.
// Paired with `server/crates/waddle-broker-core/src/kernels/budget_ledger.rs`.
//
// Proves:
// 1. Invariant `spent + reserved <= signed_cap` (in mathematical `int`, ruling out u64 overflow)
//    is preserved across `new`, `reserve`, `commit`, `release`, and `quarantine_conservative`.
// 2. Zero arithmetic overflow or underflow.
// 3. `spent` is monotonically non-decreasing across `commit` and `quarantine_conservative`.

use vstd::prelude::*;

verus! {

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct BudgetState {
    pub signed_cap: u64,
    pub spent: u64,
    pub reserved: u64,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum BudgetError {
    CapExceeded,
    Overflow,
    ReservedUnderflow,
    ActualExceedsReserved,
}

impl BudgetState {
    pub open spec fn inv(&self) -> bool {
        (self.spent as int) + (self.reserved as int) <= (self.signed_cap as int)
    }

    pub fn new(signed_cap: u64) -> (s: Self)
        ensures
            s.inv(),
            s.signed_cap == signed_cap,
            s.spent == 0,
            s.reserved == 0,
    {
        BudgetState {
            signed_cap,
            spent: 0,
            reserved: 0,
        }
    }

    pub fn reserve(&mut self, amount: u64) -> (res: Result<(), BudgetError>)
        requires
            old(self).inv(),
        ensures
            final(self).inv(),
            final(self).signed_cap == old(self).signed_cap,
            final(self).spent == old(self).spent,
            res.is_ok() ==> (final(self).reserved as int) == (old(self).reserved as int) + (amount as int),
            res.is_err() ==> *final(self) == *old(self),
    {
        let new_reserved = match self.reserved.checked_add(amount) {
            Some(v) => v,
            None => return Err(BudgetError::Overflow),
        };
        let total = match self.spent.checked_add(new_reserved) {
            Some(v) => v,
            None => return Err(BudgetError::Overflow),
        };
        if total > self.signed_cap {
            return Err(BudgetError::CapExceeded);
        }
        self.reserved = new_reserved;
        Ok(())
    }

    pub fn commit(&mut self, reserved_amount: u64, actual_amount: u64) -> (res: Result<(), BudgetError>)
        requires
            old(self).inv(),
        ensures
            final(self).inv(),
            final(self).signed_cap == old(self).signed_cap,
            final(self).spent >= old(self).spent,
            res.is_ok() ==> (
                actual_amount <= reserved_amount
                && (final(self).reserved as int) == (old(self).reserved as int) - (reserved_amount as int)
                && (final(self).spent as int) == (old(self).spent as int) + (actual_amount as int)
            ),
            res.is_err() ==> *final(self) == *old(self),
    {
        if actual_amount > reserved_amount {
            return Err(BudgetError::ActualExceedsReserved);
        }
        let new_reserved = match self.reserved.checked_sub(reserved_amount) {
            Some(v) => v,
            None => return Err(BudgetError::ReservedUnderflow),
        };
        let new_spent = match self.spent.checked_add(actual_amount) {
            Some(v) => v,
            None => return Err(BudgetError::Overflow),
        };
        let total = match new_spent.checked_add(new_reserved) {
            Some(v) => v,
            None => return Err(BudgetError::Overflow),
        };
        if total > self.signed_cap {
            return Err(BudgetError::CapExceeded);
        }
        self.reserved = new_reserved;
        self.spent = new_spent;
        Ok(())
    }

    pub fn release(&mut self, reserved_amount: u64) -> (res: Result<(), BudgetError>)
        requires
            old(self).inv(),
        ensures
            final(self).inv(),
            final(self).signed_cap == old(self).signed_cap,
            final(self).spent == old(self).spent,
            res.is_ok() ==> (final(self).reserved as int) == (old(self).reserved as int) - (reserved_amount as int),
            res.is_err() ==> *final(self) == *old(self),
    {
        let new_reserved = match self.reserved.checked_sub(reserved_amount) {
            Some(v) => v,
            None => return Err(BudgetError::ReservedUnderflow),
        };
        self.reserved = new_reserved;
        Ok(())
    }

    pub fn quarantine_conservative(&mut self, reserved_amount: u64) -> (res: Result<(), BudgetError>)
        requires
            old(self).inv(),
        ensures
            final(self).inv(),
            final(self).signed_cap == old(self).signed_cap,
            final(self).spent >= old(self).spent,
            res.is_ok() ==> (
                (final(self).reserved as int) == (old(self).reserved as int) - (reserved_amount as int)
                && (final(self).spent as int) == (old(self).spent as int) + (reserved_amount as int)
            ),
            res.is_err() ==> *final(self) == *old(self),
    {
        self.commit(reserved_amount, reserved_amount)
    }
}

fn main() {}

} // verus!
