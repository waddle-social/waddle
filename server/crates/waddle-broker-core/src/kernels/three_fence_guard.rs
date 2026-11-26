//! Pure-Rust 3-fence snapshot and generation guard kernel.
//!
//! Formally verified for strict monotonicity, zero generation wraparound,
//! and exact fail-closed admission equivalence in
//! `server/specs/verus/three_fence_guard.rs`.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelFenceState {
    pub open: bool,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelThreeFenceState {
    pub cred: KernelFenceState,
    pub room: KernelFenceState,
    pub placement: KernelFenceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelThreeFenceSnapshot {
    pub cred_gen: u64,
    pub room_gen: u64,
    pub placement_gen: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionCheckInput {
    pub snapshot: KernelThreeFenceSnapshot,
    pub current: KernelThreeFenceState,
    pub expected_holder_id: u64,
    pub actual_holder_id: u64,
    pub now_ms: i64,
    pub lease_expiry_ms: i64,
    pub perm_allowed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FenceGuardError {
    #[error("fence generation overflow at u64::MAX")]
    GenerationOverflow,
    #[error("fence generation mismatch on reopen")]
    GenerationMismatch,
    #[error("fence is already open")]
    AlreadyOpen,
}

/// Closes a fence and strictly increments its generation without wraparound.
pub fn close_and_advance(fence: KernelFenceState) -> Result<KernelFenceState, FenceGuardError> {
    let next_gen = fence
        .generation
        .checked_add(1)
        .ok_or(FenceGuardError::GenerationOverflow)?;
    Ok(KernelFenceState {
        open: false,
        generation: next_gen,
    })
}

/// Reopens a closed fence only if its current generation matches `expected_gen`.
pub fn reopen_fence(
    fence: KernelFenceState,
    expected_gen: u64,
) -> Result<KernelFenceState, FenceGuardError> {
    if fence.open {
        return Err(FenceGuardError::AlreadyOpen);
    }
    if fence.generation != expected_gen {
        return Err(FenceGuardError::GenerationMismatch);
    }
    Ok(KernelFenceState {
        open: true,
        generation: fence.generation,
    })
}

/// Evaluates whether an inference admission attempt may commit.
///
/// Admits if and only if:
/// - authorization check succeeded (`perm_allowed`),
/// - all 3 fences (`cred`, `room`, `placement`) are currently open,
/// - all 3 fence generations exactly match the pre-check snapshot,
/// - the leaseholder identity matches, and
/// - the authoritative clock `now_ms` is strictly before `lease_expiry_ms`.
pub const fn evaluate_admission(input: &AdmissionCheckInput) -> bool {
    input.perm_allowed
        && input.current.cred.open
        && input.current.room.open
        && input.current.placement.open
        && input.current.cred.generation == input.snapshot.cred_gen
        && input.current.room.generation == input.snapshot.room_gen
        && input.current.placement.generation == input.snapshot.placement_gen
        && input.expected_holder_id == input.actual_holder_id
        && input.now_ms < input.lease_expiry_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_input() -> AdmissionCheckInput {
        AdmissionCheckInput {
            snapshot: KernelThreeFenceSnapshot {
                cred_gen: 1,
                room_gen: 2,
                placement_gen: 3,
            },
            current: KernelThreeFenceState {
                cred: KernelFenceState {
                    open: true,
                    generation: 1,
                },
                room: KernelFenceState {
                    open: true,
                    generation: 2,
                },
                placement: KernelFenceState {
                    open: true,
                    generation: 3,
                },
            },
            expected_holder_id: 42,
            actual_holder_id: 42,
            now_ms: 1_000,
            lease_expiry_ms: 5_000,
            perm_allowed: true,
        }
    }

    #[test]
    fn admits_when_all_conditions_hold() {
        assert!(evaluate_admission(&valid_input()));
    }

    #[test]
    fn fails_closed_on_any_fence_or_holder_or_expiry_violation() {
        let mut stale_cred = valid_input();
        stale_cred.current.cred = close_and_advance(stale_cred.current.cred).expect("advance");
        assert!(!evaluate_admission(&stale_cred));

        let mut closed_room = valid_input();
        closed_room.current.room.open = false;
        assert!(!evaluate_admission(&closed_room));

        let mut wrong_holder = valid_input();
        wrong_holder.actual_holder_id = 99;
        assert!(!evaluate_admission(&wrong_holder));

        let mut expired = valid_input();
        expired.now_ms = 5_000;
        assert!(!evaluate_admission(&expired));

        let mut denied_perm = valid_input();
        denied_perm.perm_allowed = false;
        assert!(!evaluate_admission(&denied_perm));
    }

    #[test]
    fn generation_overflow_is_rejected() {
        let max_fence = KernelFenceState {
            open: true,
            generation: u64::MAX,
        };
        assert_eq!(
            close_and_advance(max_fence),
            Err(FenceGuardError::GenerationOverflow)
        );
    }

    #[test]
    fn reopen_requires_closed_and_exact_generation() {
        let closed = close_and_advance(KernelFenceState {
            open: true,
            generation: 5,
        })
        .expect("advance");
        assert_eq!(
            reopen_fence(closed, 5),
            Err(FenceGuardError::GenerationMismatch)
        );
        let reopened = reopen_fence(closed, 6).expect("reopen");
        assert!(reopened.open);
        assert_eq!(reopened.generation, 6);
        assert_eq!(reopen_fence(reopened, 6), Err(FenceGuardError::AlreadyOpen));
    }
}
