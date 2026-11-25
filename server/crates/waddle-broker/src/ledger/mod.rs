//! Durable destruction ledger, retired account registry, enrollment nonce
//! idempotency table, egress gate state, and encrypted credential vault storage.
//!
//! Mirrors `server/specs/quint/credential_lifecycle_restore.qnt` 1-to-1.

pub mod postgres;
pub mod turso;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};

use crate::envelope::SealedEnvelope;
use crate::error::LedgerError;
use crate::key_hierarchy::{TenantDekRecord, UserDekRecord};

/// Classification of a permanently destroyed custody target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestructionTargetKind {
    Credential,
    CredentialVersion,
    Account,
    Tenant,
    KmsKey,
}

impl DestructionTargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Credential => "credential",
            Self::CredentialVersion => "credential_version",
            Self::Account => "account",
            Self::Tenant => "tenant",
            Self::KmsKey => "kms_key",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, LedgerError> {
        match raw {
            "credential" => Ok(Self::Credential),
            "credential_version" => Ok(Self::CredentialVersion),
            "account" => Ok(Self::Account),
            "tenant" => Ok(Self::Tenant),
            "kms_key" => Ok(Self::KmsKey),
            other => Err(LedgerError::InvalidState(format!(
                "unknown destruction target_kind: {other}"
            ))),
        }
    }
}

/// Audit reason for a destruction ledger entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestructionReason {
    Revoked,
    Replaced,
    Rotated,
    AccountRetired,
    TenantDeleted,
    RetentionExpired,
}

impl DestructionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::Replaced => "replaced",
            Self::Rotated => "rotated",
            Self::AccountRetired => "account_retired",
            Self::TenantDeleted => "tenant_deleted",
            Self::RetentionExpired => "retention_expired",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, LedgerError> {
        match raw {
            "revoked" => Ok(Self::Revoked),
            "replaced" => Ok(Self::Replaced),
            "rotated" => Ok(Self::Rotated),
            "account_retired" => Ok(Self::AccountRetired),
            "tenant_deleted" => Ok(Self::TenantDeleted),
            "retention_expired" => Ok(Self::RetentionExpired),
            other => Err(LedgerError::InvalidState(format!(
                "unknown destruction reason: {other}"
            ))),
        }
    }
}

/// Append-only entry in `broker_destruction_ledger`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestructionRecord {
    pub entry_id: Uuid,
    pub target_kind: DestructionTargetKind,
    pub tenant_id: TenantId,
    pub target_id: String,
    pub reason: DestructionReason,
    pub destroyed_at_ms: i64,
}

/// Broker-side mirror of `retired_account_jids` (`2008_retired_account_jids`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredAccountRecord {
    pub tenant_id: TenantId,
    pub account_id: AccountId,
    pub jid_key: String,
    pub localpart_key: String,
    pub retired_at_ms: i64,
}

/// Idempotent XEP-0198 enrollment nonce record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentNonceRecord {
    pub nonce_sha256: [u8; 32],
    pub payload_digest_sha256: [u8; 32],
    pub tenant_id: TenantId,
    pub credential_id: Uuid,
    pub credential_version: u32,
    pub expires_at_ms: i64,
    pub created_at_ms: i64,
}

/// Persisted egress gate state (`broker_egress_state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressGateState {
    pub egress_open: bool,
    pub last_restore_applied_ms: Option<i64>,
}

/// Persisted encrypted credential record (zero plaintext).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEnvelopeRecord {
    pub credential_id: Uuid,
    pub tenant_id: TenantId,
    pub account_id: Option<AccountId>,
    pub provider: String,
    pub version: u32,
    pub envelope: SealedEnvelope,
    pub tenant_dek: TenantDekRecord,
    pub user_dek: Option<UserDekRecord>,
    pub created_at_ms: i64,
}

/// Backend-agnostic durable custody + destruction ledger storage trait.
#[async_trait]
pub trait DestructionLedgerStore: Send + Sync {
    async fn append_destruction(&self, record: &DestructionRecord) -> Result<(), LedgerError>;

    async fn list_destructions(&self) -> Result<Vec<DestructionRecord>, LedgerError>;

    async fn is_destroyed(
        &self,
        kind: DestructionTargetKind,
        tenant_id: &TenantId,
        target_id: &str,
    ) -> Result<bool, LedgerError>;

    async fn record_retired_account(
        &self,
        record: &RetiredAccountRecord,
    ) -> Result<(), LedgerError>;

    async fn is_account_retired(
        &self,
        tenant_id: &TenantId,
        account_id: AccountId,
    ) -> Result<bool, LedgerError>;

    async fn is_jid_retired(
        &self,
        tenant_id: &TenantId,
        jid_key: &str,
    ) -> Result<bool, LedgerError>;

    async fn list_retired_accounts(&self) -> Result<Vec<RetiredAccountRecord>, LedgerError>;

    async fn get_enrollment_nonce(
        &self,
        nonce_sha256: &[u8; 32],
    ) -> Result<Option<EnrollmentNonceRecord>, LedgerError>;

    async fn put_enrollment_nonce(&self, record: &EnrollmentNonceRecord)
        -> Result<(), LedgerError>;

    async fn get_egress_state(&self) -> Result<EgressGateState, LedgerError>;

    async fn set_egress_state(&self, state: EgressGateState) -> Result<(), LedgerError>;

    async fn put_envelope(&self, record: &StoredEnvelopeRecord) -> Result<(), LedgerError>;

    async fn get_envelope(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<Option<StoredEnvelopeRecord>, LedgerError>;

    async fn list_envelopes_for_tenant(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Vec<StoredEnvelopeRecord>, LedgerError>;

    async fn list_all_envelopes(&self) -> Result<Vec<StoredEnvelopeRecord>, LedgerError>;

    async fn delete_envelope(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<bool, LedgerError>;

    async fn delete_envelopes_for_account(
        &self,
        tenant_id: &TenantId,
        account_id: AccountId,
    ) -> Result<u64, LedgerError>;

    async fn delete_envelopes_for_tenant(&self, tenant_id: &TenantId) -> Result<u64, LedgerError>;
}
