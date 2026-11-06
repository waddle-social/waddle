use chrono::{DateTime, Utc};
use jid::FullJid;
use thiserror::Error;
use uuid::Uuid;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ownership::{BindingRef, ClaimEpoch, NodeIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::SmIngressAppendKey;
use xmpp_parsers::message::Message;

use crate::ingress_uow::{IngressUowError, SendLease};

/// Durable session generation, deliberately distinct from physical transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionGeneration(i64);

impl SessionGeneration {
    pub fn new(value: u64) -> Result<Self, CustodyError> {
        i64::try_from(value)
            .ok()
            .filter(|value| *value > 0)
            .map(Self)
            .ok_or(CustodyError::Rejected)
    }

    pub fn get(self) -> u64 {
        self.0 as u64
    }
}

/// Supplied endpoint identity. Installation does not publish a live endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyCapture {
    pub gate_id: Uuid,
    pub token: Uuid,
    pub resource: FullJid,
    pub binding: BindingRef,
    pub session: SmSessionId,
    pub generation: SessionGeneration,
    pub principal: AuthenticatedPrincipalRef,
    pub owner: NodeIdentity,
    pub claim: ClaimEpoch,
}

/// Caller preparation is rechecked against the canonical envelope and intent.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedDirectFrame {
    pub operation_id: Uuid,
    pub key: SmIngressAppendKey,
    pub payload: Message,
    pub received_at: DateTime<Utc>,
    pub(super) attempt: Uuid,
}

impl PreparedDirectFrame {
    pub fn for_lease(
        operation_id: Uuid,
        key: SmIngressAppendKey,
        payload: Message,
        received_at: DateTime<Utc>,
        lease: &SendLease,
    ) -> Result<Self, CustodyError> {
        let obligation = lease.obligation();
        if operation_id.is_nil()
            || key.message_key != obligation.message
            || key.kind.to_storage() != obligation.receipt.kind.to_storage()
            || key.semantic_identity_hash != obligation.receipt.semantic_identity_hash
            || key.resource != obligation.recipient
            || !lease.never_started()
        {
            return Err(CustodyError::Rejected);
        }
        Ok(Self {
            operation_id,
            key,
            payload,
            received_at,
            attempt: lease.token(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CustodyLimits {
    pub(super) rows: i64,
    pub(super) bytes: i64,
}

impl CustodyLimits {
    pub fn new(rows: u32, bytes: u64) -> Result<Self, CustodyError> {
        if rows == 0 || rows > 1024 || bytes == 0 || bytes > 16 * 1024 * 1024 {
            return Err(CustodyError::Capacity);
        }
        Ok(Self {
            rows: i64::from(rows),
            bytes: bytes as i64,
        })
    }
}

/// Known committed admission. This is not yet permission to count or enqueue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedAdmission {
    pub(super) gate: Uuid,
    pub(super) operation: Uuid,
}

impl CommittedAdmission {
    pub fn operation_id(&self) -> Uuid {
        self.operation
    }
}

/// Known committed immutable slot. No socket or memory enqueue is performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedSlot {
    pub(super) admission: CommittedAdmission,
    pub(super) ordinal: i64,
}

impl CommittedSlot {
    pub fn ordinal(&self) -> u64 {
        self.ordinal as u64
    }
    pub fn h(&self) -> u32 {
        self.ordinal as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustodyTerminal {
    Ack,
    Transfer,
}

impl CustodyTerminal {
    pub(super) fn code(self) -> i32 {
        match self {
            Self::Ack => 1,
            Self::Transfer => 2,
        }
    }
    pub(super) fn decode(code: i32) -> Result<Self, CustodyError> {
        match code {
            1 => Ok(Self::Ack),
            2 => Ok(Self::Transfer),
            _ => Err(CustodyError::Conflict),
        }
    }
}

/// Autonomous inventory, decoded once at the storage boundary.
#[derive(Debug, Clone)]
pub struct CustodyOperation {
    pub(super) gate: Uuid,
    pub(super) token: Uuid,
    pub(super) frame: PreparedDirectFrame,
    pub(super) slot: Option<CommittedSlot>,
    pub(super) terminal: Option<CustodyTerminal>,
}

impl CustodyOperation {
    pub fn frame(&self) -> &PreparedDirectFrame {
        &self.frame
    }
    pub fn slot(&self) -> Option<&CommittedSlot> {
        self.slot.as_ref()
    }
    pub fn terminal(&self) -> Option<CustodyTerminal> {
        self.terminal
    }
    pub fn admission(&self, sealed: &SealedSession) -> Result<CommittedAdmission, CustodyError> {
        if self.gate != sealed.gate || self.token != sealed.token {
            return Err(CustodyError::Rejected);
        }
        Ok(CommittedAdmission {
            gate: sealed.gate,
            operation: self.frame.operation_id,
        })
    }
}

/// A fresh recovery owner's exact current claim; it does not revive an old
/// principal, endpoint or generation. Used only for sealed reads/fencing.
#[derive(Debug, Clone)]
pub struct CustodyRecoveryAuthority {
    pub owner: NodeIdentity,
    pub claim: ClaimEpoch,
}

#[derive(Debug, Clone)]
pub struct CustodyInventoryPage {
    pub operations: Vec<CustodyOperation>,
    pub next: Option<Uuid>,
}

/// Retained, pending slots in ordinal order. This is evidence, not a send permit.
#[derive(Debug, Clone)]
pub struct CustodyReplayPage {
    pub operations: Vec<CustodyOperation>,
    pub next: Option<u64>,
}

/// Minted only after a known committed, irreversible gate seal.
#[derive(Debug, Clone)]
pub struct SealedSession {
    pub(super) gate: Uuid,
    pub(super) token: Uuid,
}

#[derive(Debug, Clone)]
pub enum CustodyResolution {
    Committed(Box<CustodyOperation>),
    ProvenAbsent,
}

#[derive(Debug, Error)]
pub enum CustodyError {
    #[error("the principal, claim, captured endpoint, session or operation was rejected")]
    Rejected,
    #[error("immutable custody evidence conflicts")]
    Conflict,
    #[error("custody count or byte capacity is exhausted")]
    Capacity,
    #[error("the nonwrapping output ordinal is exhausted")]
    Overflow,
    #[error(
        "the outcome remains unknown; no count, enqueue, or generation rollback is authorized"
    )]
    CommitUnknown,
    #[error("the canonical source is absent; recovery remains unresolved")]
    SourceMissing,
    #[error(transparent)]
    Uow(#[from] IngressUowError),
    #[error("PostgreSQL rejected the custody operation")]
    Database(#[source] sqlx::Error),
    #[error("stored custody data is invalid")]
    InvalidStored,
}

pub(super) fn db(error: sqlx::Error) -> CustodyError {
    CustodyError::Database(error)
}
