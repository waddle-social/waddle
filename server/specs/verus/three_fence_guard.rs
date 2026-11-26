// Verus formal proof: 3-fence snapshot & generation guard.
// Paired with `server/crates/waddle-broker-core/src/kernels/three_fence_guard.rs`.
//
// Proves:
// 1. `close_and_advance` strictly increments generation with zero u64 wraparound.
// 2. `reopen_fence` succeeds iff closed and exact generation matches.
// 3. `evaluate_admission` returns true iff perm_allowed, all 3 fences are open,
//    all 3 generations match snapshot, holder matches, and now_ms < lease_expiry_ms.

use vstd::prelude::*;

verus! {

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct FenceState {
    pub open: bool,
    pub generation: u64,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct ThreeFenceState {
    pub cred: FenceState,
    pub room: FenceState,
    pub placement: FenceState,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct ThreeFenceSnapshot {
    pub cred_gen: u64,
    pub room_gen: u64,
    pub placement_gen: u64,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct AdmissionCheckInput {
    pub snapshot: ThreeFenceSnapshot,
    pub current: ThreeFenceState,
    pub expected_holder_id: u64,
    pub actual_holder_id: u64,
    pub now_ms: i64,
    pub lease_expiry_ms: i64,
    pub perm_allowed: bool,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum FenceGuardError {
    GenerationOverflow,
    GenerationMismatch,
    AlreadyOpen,
}

pub open spec fn admission_allowed_spec(input: AdmissionCheckInput) -> bool {
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

pub fn close_and_advance(fence: FenceState) -> (res: Result<FenceState, FenceGuardError>)
    ensures
        fence.generation == u64::MAX ==> res == Err::<FenceState, FenceGuardError>(FenceGuardError::GenerationOverflow),
        fence.generation < u64::MAX ==> match res {
            Ok(next) => !next.open && next.generation == fence.generation + 1 && next.generation > fence.generation,
            Err(_) => false,
        },
{
    if fence.generation == u64::MAX {
        Err(FenceGuardError::GenerationOverflow)
    } else {
        Ok(FenceState {
            open: false,
            generation: fence.generation + 1,
        })
    }
}

pub fn reopen_fence(fence: FenceState, expected_gen: u64) -> (res: Result<FenceState, FenceGuardError>)
    ensures
        fence.open ==> res == Err::<FenceState, FenceGuardError>(FenceGuardError::AlreadyOpen),
        (!fence.open && fence.generation != expected_gen) ==> res == Err::<FenceState, FenceGuardError>(FenceGuardError::GenerationMismatch),
        (!fence.open && fence.generation == expected_gen) ==> res == Ok::<FenceState, FenceGuardError>(FenceState {
            open: true,
            generation: fence.generation,
        }),
{
    if fence.open {
        Err(FenceGuardError::AlreadyOpen)
    } else if fence.generation != expected_gen {
        Err(FenceGuardError::GenerationMismatch)
    } else {
        Ok(FenceState {
            open: true,
            generation: fence.generation,
        })
    }
}

pub fn evaluate_admission(input: &AdmissionCheckInput) -> (admitted: bool)
    ensures
        admitted <==> admission_allowed_spec(*input),
{
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

pub proof fn lemma_advanced_cred_fence_denies_stale_snapshot(input: AdmissionCheckInput)
    requires
        input.current.cred.generation > input.snapshot.cred_gen,
    ensures
        !admission_allowed_spec(input),
{
}

fn main() {}

} // verus!
