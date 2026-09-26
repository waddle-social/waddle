//! Durable room observations; room authority and persistence remain host-owned.
use super::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct ObservationGeneration(u64);

impl ObservationGeneration {
    pub fn new(value: u64) -> Result<Self, FrameworkTypeError> {
        if value == 0 {
            return Err(FrameworkTypeError::InvalidObservationGeneration);
        }
        Ok(Self(value))
    }
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct MessageRevision(u64);

impl MessageRevision {
    pub fn new(value: u64) -> Self {
        Self(value)
    }
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "rooms", rename_all = "kebab-case")]
pub enum RoomObservationScope {
    Rooms(Vec<BareJid>),
    AllHostedRooms,
}

impl RoomObservationScope {
    pub fn includes(&self, room: &BareJid) -> bool {
        match self {
            Self::Rooms(rooms) => rooms.contains(room),
            Self::AllHostedRooms => true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfiguredRoomObserver {
    pub plugin: PluginId,
    pub generation: ObservationGeneration,
    pub identity: Sha256Digest,
    pub scope: RoomObservationScope,
    /// Maximum simultaneous calls on this node, not a cluster-wide quota.
    pub max_concurrent: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomObservationSubscription {
    pub plugin: PluginId,
    pub generation: ObservationGeneration,
    pub identity: Sha256Digest,
    pub room: BareJid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomMessageSource {
    pub room: BareJid,
    pub stanza_id: StanzaId,
    pub revision_stanza_id: StanzaId,
    pub origin_id: Option<OriginId>,
    pub sender: BareJid,
    pub revision: MessageRevision,
    pub body_digest: Sha256Digest,
    pub observed_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomMessageObserve {
    pub source: RoomMessageSource,
    pub body: DisplayText,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvocationUsage {
    pub provider: ProviderId,
    pub model: ModelId,
    pub cost_micro_usd: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomObservationResult {
    pub payloads: Vec<ExtensionPayload>,
    pub usage: Option<InvocationUsage>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ObservationFailure {
    TemporaryFailure,
    InvalidRequest,
    Denied,
    UnsupportedEvent,
    RuntimeFailure,
    ResourceLimit,
    DeadlineExceeded,
    InvalidResult,
    SourceMismatch,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ObservationSkip {
    MissingOriginId,
    SubscriptionUnavailable,
    NoResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RoomObservationOutcome {
    Completed(RoomObservationResult),
    RetryableFailure(ObservationFailure),
    PermanentFailure(ObservationFailure),
    NotApplicable(ObservationSkip),
}
