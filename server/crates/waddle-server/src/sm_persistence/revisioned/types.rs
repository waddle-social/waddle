use jid::FullJid;
use thiserror::Error;
use uuid::Uuid;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    PersistedDetachedAppend, PersistedIngressAppend, PersistedSession, PersistedUnackedStanza,
    SmAckWindow, SmAcknowledgementMode,
};
use waddle_xmpp::stream_management::SmIngressAppendKey;
use waddle_xmpp::tombstone::TombstoneTarget;
use waddle_xmpp_core::OccupancySessionGeneration;

/// A database capture's opaque generation and revision, bound to its identity.
#[derive(Clone, Debug)]
pub struct Revision {
    pub(super) stream: SmSessionId,
    pub(super) generation: Uuid,
    pub(super) value: i64,
    pub(super) principal: AuthenticatedPrincipalRef,
    pub(super) jid: FullJid,
    pub(super) occupancy: OccupancySessionGeneration,
}

impl Revision {
    pub fn stream(&self) -> &SmSessionId {
        &self.stream
    }

    pub fn generation(&self) -> Uuid {
        self.generation
    }

    pub fn value(&self) -> i64 {
        self.value
    }
}

/// State and revision read together under the database stream gate.
pub struct Capture {
    pub revision: Revision,
    pub principal: AuthenticatedPrincipalRef,
    pub session: PersistedSession,
    pub unacked: Vec<PersistedUnackedStanza>,
}

/// An immutable operation. The caller retains it, including its UUID, until an
/// uncertain commit has been positively reconciled.
pub struct Request<'a> {
    pub id: Uuid,
    pub mutation: Mutation<'a>,
}

/// Supported transaction bodies. New-delivery admission requires a source and
/// recipient fence and is rejected here until that fence can be composed.
pub enum Mutation<'a> {
    Create {
        generation: Uuid,
        principal: &'a AuthenticatedPrincipalRef,
        session: &'a PersistedSession,
        unacked: &'a [PersistedUnackedStanza],
        drained: &'a [PersistedIngressAppend],
    },
    Snapshot {
        revision: &'a Revision,
        session: &'a PersistedSession,
        unacked: &'a [PersistedUnackedStanza],
        drained: &'a [PersistedIngressAppend],
    },
    Append {
        revision: &'a Revision,
        append: &'a PersistedDetachedAppend,
    },
    Acknowledge {
        revision: &'a Revision,
        h: u32,
        mode: SmAcknowledgementMode,
    },
    /// Refused until exact downstream settlement evidence can be supplied.
    ConfirmAcknowledgement {
        revision: &'a Revision,
        window: SmAckWindow,
    },
    Tombstone {
        revision: &'a Revision,
        target: &'a TombstoneTarget,
        through: chrono::DateTime<chrono::Utc>,
        sequences: &'a [u32],
    },
    /// Refused until exact custody settlement proof can be supplied.
    Prune {
        revision: &'a Revision,
        sequences: &'a [u32],
    },
    Retire {
        revision: &'a Revision,
    },
}

impl Mutation<'_> {
    pub(super) fn stream(&self) -> &SmSessionId {
        match self {
            Self::Create { session, .. } => &session.stream_id,
            Self::Snapshot { revision, .. }
            | Self::Append { revision, .. }
            | Self::Acknowledge { revision, .. }
            | Self::ConfirmAcknowledgement { revision, .. }
            | Self::Tombstone { revision, .. }
            | Self::Prune { revision, .. }
            | Self::Retire { revision } => &revision.stream,
        }
    }

    pub(super) fn generation(&self) -> Uuid {
        match self {
            Self::Create { generation, .. } => *generation,
            Self::Snapshot { revision, .. }
            | Self::Append { revision, .. }
            | Self::Acknowledge { revision, .. }
            | Self::ConfirmAcknowledgement { revision, .. }
            | Self::Tombstone { revision, .. }
            | Self::Prune { revision, .. }
            | Self::Retire { revision } => revision.generation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyResult {
    /// The revision after this operation committed.
    pub revision: i64,
    pub effect: Effect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Snapshot { withheld: Vec<SmIngressAppendKey> },
    Appended,
    AlreadyAllocated { accepting_stream: SmSessionId },
    Acknowledged,
    Tombstoned { removed: u64 },
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readback {
    Applied(ApplyResult),
    Unconfirmed,
}

#[derive(Debug, Error)]
pub enum RevisionError {
    #[error("operation ID was nil or reused with different input")]
    OperationIdentity,
    #[error("session identity or generation does not match")]
    GenerationChanged,
    #[error("session generation is terminal")]
    Terminal,
    #[error("session revision is stale")]
    StaleRevision,
    #[error("session revision is exhausted")]
    RevisionExhausted,
    #[error("operation requires an unsupported authority or ledger mode")]
    Unsupported,
    #[error("session still has replay or pending custody")]
    OutstandingCustody,
    #[error("session data is invalid or inconsistent")]
    InvalidInput,
    #[error("revisioned SM storage failed: {0}")]
    Storage(String),
}

pub(super) fn storage(error: impl std::fmt::Display) -> RevisionError {
    RevisionError::Storage(error.to_string())
}
