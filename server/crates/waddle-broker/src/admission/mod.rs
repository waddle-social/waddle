//! 3-fence `AdmissionLedgerStore` (Turso WAL & PostgreSQL / SqliteCompat),
//! device-signed inference invocation validation, short-tick `BrokerReplicaFence`,
//! and `BrokerAdmissionService`.

pub mod invocation;
pub mod postgres;
pub mod replica_fence;
pub mod service;
pub mod turso;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_broker_core::admission::{
    AdmissionError, AttemptId, CredentialKey, CredentialScope, FenceKey, FenceKind, RoomScopeKey,
    RunId, StepId,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::placement::FenceGeneration;

/// Durable lifecycle state of an admitted inference attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttemptStatus {
    Admitted,
    Completed,
    FailedBeforeSend,
    Canceled,
    Quarantined,
}

impl AttemptStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Completed => "completed",
            Self::FailedBeforeSend => "failed_before_send",
            Self::Canceled => "canceled",
            Self::Quarantined => "quarantined",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, AdmissionError> {
        match raw {
            "admitted" => Ok(Self::Admitted),
            "completed" => Ok(Self::Completed),
            "failed_before_send" => Ok(Self::FailedBeforeSend),
            "canceled" => Ok(Self::Canceled),
            "quarantined" => Ok(Self::Quarantined),
            other => Err(AdmissionError::Backend(format!(
                "unknown attempt status in ledger: {other}"
            ))),
        }
    }
}

/// Persisted ledger record for a single inference attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub attempt_id: AttemptId,
    pub run_id: RunId,
    pub step_id: StepId,
    pub tenant_id: TenantId,
    pub status: AttemptStatus,
    pub reserved_spend_microcents: u64,
    pub actual_spend_microcents: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub admitted_at_ms: i64,
}

/// Persisted budget accounting snapshot for a multi-turn inference run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunBudgetSnapshot {
    pub run_id: RunId,
    pub tenant_id: TenantId,
    pub signed_cap_microcents: u64,
    pub spent_microcents: u64,
    pub reserved_microcents: u64,
    pub max_steps: u32,
    pub steps_used: u32,
    pub max_input_tokens: u64,
    pub input_tokens_used: u64,
    pub max_output_tokens: u64,
    pub output_tokens_used: u64,
}

/// Result of checking whether all active peer broker replicas have acknowledged
/// a fence advancement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceAcknowledgement {
    Acknowledged,
    Pending { unacked_replicas: usize },
}

/// Durable replica registration and fence-acknowledgement store backing [`replica_fence::BrokerReplicaFence`].
#[async_trait]
pub trait ReplicaFenceStore: Send + Sync {
    async fn register_replica(
        &self,
        replica_id: Uuid,
        ttl_ms: i64,
    ) -> Result<Vec<(FenceKey, FenceGeneration)>, AdmissionError>;

    async fn renew_replica(
        &self,
        replica_id: Uuid,
        ttl_ms: i64,
    ) -> Result<Option<Vec<(FenceKey, FenceGeneration)>>, AdmissionError>;

    async fn ack_replica_fences(
        &self,
        replica_id: Uuid,
        acks: &[(FenceKey, FenceGeneration)],
    ) -> Result<(), AdmissionError>;

    async fn release_replica(&self, replica_id: Uuid) -> Result<(), AdmissionError>;

    async fn check_revocation_ack(
        &self,
        key: &FenceKey,
        target_gen: FenceGeneration,
        expiry_grace_ms: i64,
    ) -> Result<FenceAcknowledgement, AdmissionError>;
}

/// Query helpers over the admission ledger for idempotent command replay and
/// budget auditing.
#[async_trait]
pub trait AdmissionQueryStore: Send + Sync {
    async fn get_attempt(
        &self,
        attempt_id: AttemptId,
    ) -> Result<Option<AttemptRecord>, AdmissionError>;

    async fn get_attempt_by_nonce(
        &self,
        tenant_id: &TenantId,
        nonce: &[u8; 32],
    ) -> Result<Option<AttemptRecord>, AdmissionError>;

    async fn get_run_budget(
        &self,
        run_id: RunId,
    ) -> Result<Option<RunBudgetSnapshot>, AdmissionError>;
}

pub(crate) fn fence_kind_and_scope(kind: &FenceKind) -> (&'static str, String) {
    match kind {
        FenceKind::Credential(CredentialKey {
            credential_id,
            scope: CredentialScope::Tenant,
        }) => ("credential", format!("tenant:{credential_id}")),
        FenceKind::Credential(CredentialKey {
            credential_id,
            scope: CredentialScope::Member { account_id },
        }) => ("credential", format!("member:{account_id}:{credential_id}")),
        FenceKind::RoomOrSpace(RoomScopeKey { scope_id }) => ("room_or_space", scope_id.clone()),
        FenceKind::Placement => ("placement", "placement".to_owned()),
    }
}

pub(crate) fn parse_fence_kind_and_scope(
    kind: &str,
    scope: &str,
) -> Result<FenceKind, AdmissionError> {
    match kind {
        "credential" => {
            if let Some(rest) = scope.strip_prefix("tenant:") {
                let credential_id = Uuid::parse_str(rest).map_err(|err| {
                    AdmissionError::Backend(format!("invalid tenant credential_id in fence: {err}"))
                })?;
                Ok(FenceKind::Credential(CredentialKey {
                    credential_id,
                    scope: CredentialScope::Tenant,
                }))
            } else if let Some(rest) = scope.strip_prefix("member:") {
                let (acct_str, cred_str) = rest.split_once(':').ok_or_else(|| {
                    AdmissionError::Backend(format!(
                        "invalid member credential fence scope: {scope}"
                    ))
                })?;
                let acct_uuid = Uuid::parse_str(acct_str).map_err(|err| {
                    AdmissionError::Backend(format!("invalid account_id in fence: {err}"))
                })?;
                let credential_id = Uuid::parse_str(cred_str).map_err(|err| {
                    AdmissionError::Backend(format!("invalid member credential_id in fence: {err}"))
                })?;
                Ok(FenceKind::Credential(CredentialKey {
                    credential_id,
                    scope: CredentialScope::Member {
                        account_id: AccountId::from_uuid(acct_uuid),
                    },
                }))
            } else {
                Err(AdmissionError::Backend(format!(
                    "unrecognized credential fence scope: {scope}"
                )))
            }
        }
        "room_or_space" => Ok(FenceKind::RoomOrSpace(RoomScopeKey {
            scope_id: scope.to_owned(),
        })),
        "placement" => Ok(FenceKind::Placement),
        other => Err(AdmissionError::Backend(format!(
            "unknown fence kind in ledger: {other}"
        ))),
    }
}

pub(crate) fn encode_u64(value: u64) -> String {
    value.to_string()
}

pub(crate) fn decode_u64(raw: &str) -> Result<u64, AdmissionError> {
    raw.parse::<u64>()
        .map_err(|err| AdmissionError::Backend(format!("corrupt u64 decimal value '{raw}': {err}")))
}
