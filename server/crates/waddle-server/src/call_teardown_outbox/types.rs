use jid::{BareJid, FullJid};
use thiserror::Error;
use waddle_sfu::{CallGeneration, CallId, ParticipantSid, RoomSid, SfuError};

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
pub enum TeardownTarget {
    Participant {
        identity: FullJid,
        participant_sid: Option<ParticipantSid>,
    },
    Room,
    MujiPresenceClear {
        room_jid: BareJid,
        departed: FullJid,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTeardownJob {
    pub intent_id: CallTeardownIntentId,
    pub intent: CallTeardownIntent,
    pub status: CallTeardownStatus,
    pub attempt_count: i64,
    pub last_error: Option<String>,
    pub next_attempt_at_ms: Option<i64>,
    pub claim_token: Option<String>,
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
}
