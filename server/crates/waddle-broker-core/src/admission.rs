use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::identity::{AccountId, HolderInstance, TenantId};
use crate::placement::FenceGeneration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttemptId(Uuid);

impl AttemptId {
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(Uuid);

impl RunId {
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StepId(u32);

impl StepId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CredentialScope {
    Tenant,
    Member { account_id: AccountId },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CredentialKey {
    pub credential_id: Uuid,
    pub scope: CredentialScope,
}

/// Canonical room or space scope key (never raw untyped JID string at call sites).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RoomScopeKey {
    pub scope_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FenceKind {
    Credential(CredentialKey),
    RoomOrSpace(RoomScopeKey),
    Placement,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FenceKey {
    pub tenant_id: TenantId,
    pub kind: FenceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FenceState {
    Open,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FenceSnapshot {
    pub generation: FenceGeneration,
    pub state: FenceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreeFenceSnapshot {
    pub credential: FenceSnapshot,
    pub room_or_space: FenceSnapshot,
    pub placement: FenceSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetLimits {
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
    pub max_spend_microcents: u64,
    pub max_steps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmitAttemptRequest {
    pub attempt_id: AttemptId,
    pub run_id: RunId,
    pub step_id: StepId,
    pub tenant_id: TenantId,
    pub credential: CredentialKey,
    pub room_or_space: RoomScopeKey,
    pub holder: HolderInstance,
    pub expected_fences: ThreeFenceSnapshot,
    pub invocation_nonce: [u8; 32],
    pub expires_at_ms: i64,
    pub reserved_spend_microcents: u64,
    pub budget: BudgetLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmitReceipt {
    pub attempt_id: AttemptId,
    pub run_id: RunId,
    pub step_id: StepId,
    pub admitted_at_ms: i64,
    pub reserved_spend_microcents: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttemptOutcome {
    Completed {
        input_tokens: u64,
        output_tokens: u64,
        actual_spend_microcents: u64,
    },
    FailedBeforeSend,
    Canceled {
        actual_spend_microcents: u64,
    },
    /// Ambiguous provider acceptance: MUST quarantine and NEVER re-dispatch.
    UnknownAfterSend,
}

#[derive(Debug, Error)]
pub enum AdmissionError {
    #[error("fence closed or generation advanced: {0:?}")]
    FenceStaleOrClosed(FenceKind),
    #[error("duplicate attempt id")]
    DuplicateAttempt,
    #[error("replay nonce already consumed")]
    NonceReplayed,
    #[error("budget or spend limit exceeded")]
    BudgetExceeded,
    #[error("invocation expired")]
    InvocationExpired,
    #[error("unauthorized holder or stale placement")]
    HolderUnauthorized,
    #[error("attempt not found")]
    AttemptNotFound,
    #[error("attempt is quarantined due to unknown_after_send")]
    Quarantined,
    #[error("admission ledger backend error: {0}")]
    Backend(String),
}

#[async_trait]
pub trait AdmissionLedgerStore: Send + Sync {
    async fn snapshot_fences(
        &self,
        tenant: &TenantId,
        credential: &CredentialKey,
        room_or_space: &RoomScopeKey,
    ) -> Result<ThreeFenceSnapshot, AdmissionError>;

    /// Atomically close the fence and advance its generation by 1; returns the new generation.
    async fn close_and_advance_fence(
        &self,
        key: &FenceKey,
    ) -> Result<FenceGeneration, AdmissionError>;

    /// Reopen a closed fence only if its current generation equals `expected_generation`.
    async fn reopen_fence(
        &self,
        key: &FenceKey,
        expected_generation: FenceGeneration,
    ) -> Result<(), AdmissionError>;

    /// Single atomic transaction verifying all 3 fences remain Open at `expected_fences` generations,
    /// holder/expiry/budgets/nonce are valid, reserving spend, and claiming `attempt_id`.
    async fn admit_attempt(
        &self,
        req: &AdmitAttemptRequest,
    ) -> Result<AdmitReceipt, AdmissionError>;

    /// Reconcile a previously admitted attempt; `UnknownAfterSend` transitions to `Quarantined`.
    async fn reconcile_attempt(
        &self,
        attempt_id: AttemptId,
        outcome: AttemptOutcome,
    ) -> Result<(), AdmissionError>;
}
