use jid::{BareJid, FullJid};
use thiserror::Error;
use waddle_sfu::{CallGeneration, CallId, ParticipantSid, RoomSid, SfuError};
use waddle_xmpp::ownership::NodeIdentity;

use crate::db::DatabaseError;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallTeardownIntentId(pub(crate) String);

impl CallTeardownIntentId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Rehydrate an id we previously wrote (the queued-dedupe read
    /// path); never used for externally-supplied strings.
    pub(crate) fn from_stored(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClaimToken(String);

impl ClaimToken {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub(crate) fn from_stored(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The exact process incarnation that produced a node-local 1:1 teardown.
///
/// The outbox can live in a clustered database, while raw 1:1 call
/// registries are process-local. Keeping this typed instead of passing the
/// stored `TEXT` through the drain prevents a foreign process from treating
/// an opaque database string as local authority.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallTeardownProducingNode(NodeIdentity);

impl CallTeardownProducingNode {
    pub(crate) fn from_node_identity(identity: NodeIdentity) -> Self {
        Self(identity)
    }

    pub(crate) fn node_identity(&self) -> &NodeIdentity {
        &self.0
    }

    pub(crate) fn as_db_value(&self) -> Result<String, CallTeardownOutboxError> {
        serde_json::to_string(&(&self.0.node_id, &self.0.node_epoch))
            .map_err(CallTeardownOutboxError::EncodeProducingNode)
    }

    pub(crate) fn from_db_value(value: String) -> Result<Self, CallTeardownOutboxError> {
        let (node_id, node_epoch): (String, String) = serde_json::from_str(&value)
            .map_err(|source| CallTeardownOutboxError::InvalidProducingNode { value, source })?;
        Ok(Self(NodeIdentity::new(node_id, node_epoch)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TeardownTarget {
    Participant {
        identity: FullJid,
        participant_sid: Option<ParticipantSid>,
    },
    Room,
    MujiPresenceClear {
        room_jid: BareJid,
        departed: FullJid,
        /// `None` is reserved for producers such as the sans-IO Muji
        /// relay fallback, which cannot observe the participant
        /// incarnation SID but still must persist the XEP-0272 leave.
        participant_sid: Option<ParticipantSid>,
    },
    MujiRoomSweep {
        room_jid: BareJid,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallTeardownIntent {
    pub call_id: CallId,
    pub target: TeardownTarget,
    /// `None` is reserved for producers such as cross-node Muji fallback
    /// which cannot observe the room incarnation. Such intents are guarded
    /// only by a SID when one is present.
    pub generation: Option<CallGeneration>,
    pub room_sid: Option<RoomSid>,
}

impl CallTeardownIntent {
    pub(crate) fn room_scope(&self) -> Option<BareJid> {
        match &self.target {
            TeardownTarget::MujiPresenceClear { room_jid, .. }
            | TeardownTarget::MujiRoomSweep { room_jid } => Some(room_jid.clone()),
            TeardownTarget::Participant { .. } | TeardownTarget::Room => {
                // Muji uses the bare room JID verbatim as CallId. Raw 1:1
                // call IDs are opaque values which do not parse as JIDs.
                self.call_id.as_str().parse().ok()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallTeardownStatus {
    Queued,
    InProgress,
    Done,
    Failed,
}

impl CallTeardownStatus {
    pub(crate) fn from_db_value(value: String) -> Result<Self, CallTeardownOutboxError> {
        match value.as_str() {
            "queued" => Ok(Self::Queued),
            "in-progress" => Ok(Self::InProgress),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            _ => Err(CallTeardownOutboxError::InvalidStatus(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallTeardownRetryReason {
    MujiPresenceClear,
    LiveKitExecutorUnavailable,
    LiveKitAdmin,
    LiveKitOccupied,
    Unknown,
}

impl CallTeardownRetryReason {
    pub(crate) const fn as_db_value(self) -> &'static str {
        match self {
            Self::MujiPresenceClear => "muji_presence_clear_retryable",
            Self::LiveKitExecutorUnavailable => "livekit_teardown_executor_unavailable",
            Self::LiveKitAdmin => "livekit_admin_retryable",
            Self::LiveKitOccupied => "livekit_room_occupied",
            Self::Unknown => "unknown",
        }
    }

    fn from_db_value(value: &str) -> Self {
        match value {
            "muji_presence_clear_retryable" => Self::MujiPresenceClear,
            "livekit_teardown_executor_unavailable" => Self::LiveKitExecutorUnavailable,
            "livekit_admin_retryable" => Self::LiveKitAdmin,
            "livekit_room_occupied" => Self::LiveKitOccupied,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CallTeardownLastError {
    Retryable(CallTeardownRetryReason),
    RoomNeverOwned,
    ProducerNeverDrained,
}

impl CallTeardownLastError {
    pub(crate) fn from_db_value(value: String) -> Self {
        match value.as_str() {
            "room_never_owned" => Self::RoomNeverOwned,
            "producer_never_drained" => Self::ProducerNeverDrained,
            _ => Self::Retryable(CallTeardownRetryReason::from_db_value(value.as_str())),
        }
    }

    pub(crate) const fn as_db_value(&self) -> &'static str {
        match self {
            Self::Retryable(reason) => reason.as_db_value(),
            Self::RoomNeverOwned => "room_never_owned",
            Self::ProducerNeverDrained => "producer_never_drained",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTeardownJob {
    pub intent_id: CallTeardownIntentId,
    pub intent: CallTeardownIntent,
    pub producing_node: Option<CallTeardownProducingNode>,
    pub status: CallTeardownStatus,
    pub attempt_count: i64,
    pub last_error: Option<CallTeardownLastError>,
    pub next_attempt_at_ms: Option<i64>,
    pub claim_token: Option<ClaimToken>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallTeardownRetryOutcome {
    Requeued { attempt_count: i64 },
    Failed { attempt_count: i64 },
    ClaimLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CallTeardownQueueStats {
    pub queued_count: u64,
    pub oldest_queued_age_ms: u64,
}

#[derive(Debug, Error)]
pub enum CallTeardownOutboxError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Sfu(#[from] SfuError),
    #[error("invalid full JID in call teardown outbox: {0}")]
    InvalidFullJid(String),
    #[error("invalid bare JID in call teardown outbox: {0}")]
    InvalidBareJid(String),
    #[error("call teardown outbox row has invalid status '{0}'")]
    InvalidStatus(String),
    #[error("call teardown outbox row has invalid action '{0}'")]
    InvalidAction(String),
    #[error("call teardown outbox generation is outside the supported range: {0}")]
    InvalidGeneration(i64),
    #[error("call teardown generation does not fit the database INTEGER type: {0}")]
    GenerationOverflow(u64),
    #[error("call teardown outbox row has an invalid target shape for action '{0}'")]
    InvalidTargetShape(String),
    #[error("failed to encode call teardown producing node: {0}")]
    EncodeProducingNode(serde_json::Error),
    #[error("call teardown outbox row has invalid producing node '{value}': {source}")]
    InvalidProducingNode {
        value: String,
        source: serde_json::Error,
    },
    #[error("call teardown producing node identity changed before the durable boundary")]
    ProducingNodeIdentityChanged,
}
