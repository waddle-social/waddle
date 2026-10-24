//! Durable room observations; room authority and persistence remain host-owned.
use super::*;

/// Positive operator-controlled generation that fences durable observer configurations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct ObservationGeneration(u64);

impl ObservationGeneration {
    /// Create a positive host-owned observation configuration generation.
    ///
    /// # Errors
    ///
    /// Returns `InvalidObservationGeneration` if the generation is zero.
    pub fn try_new(value: u64) -> Result<Self, FrameworkTypeError> {
        if value == 0 {
            return Err(FrameworkTypeError::InvalidObservationGeneration);
        }
        Ok(Self(value))
    }
    /// Return the host-assigned configuration generation.

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Host-assigned revision of a message body; zero is a valid revision.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct MessageRevision(u64);

impl MessageRevision {
    /// Wrap a host-assigned message revision number; zero is accepted.
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }
    /// Return the host-assigned message revision number.

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Explicit set of rooms or all hosted rooms selected for durable observation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "rooms", rename_all = "kebab-case")]
pub enum RoomObservationScope {
    /// Only the explicitly listed rooms; an empty list revokes all room selection.
    Rooms(Vec<BareJid>),
    /// Every authoritative room hosted by this deployment.
    AllHostedRooms,
}

impl RoomObservationScope {
    /// Whether the room is explicitly selected or the scope includes all hosted rooms.
    #[must_use]
    pub fn includes(&self, room: &BareJid) -> bool {
        match self {
            Self::Rooms(rooms) => rooms.contains(room),
            Self::AllHostedRooms => true,
        }
    }
}

/// Frozen plugin configuration identity and scope used to derive durable subscriptions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfiguredRoomObserver {
    /// Extension identifier owning this value.
    pub plugin: PluginId,
    /// Positive operator generation used to fence stale observation configuration.
    pub generation: ObservationGeneration,
    /// Digest of the component bytes, effective configuration, grants, origins,
    /// observation settings, and runtime limits.
    pub identity: Sha256Digest,
    /// Scope in which the route, command, or observation applies.
    pub scope: RoomObservationScope,
    /// Maximum simultaneous calls on this node, not a cluster-wide quota.
    pub max_concurrent: u32,
}

/// Plugin generation and configuration identity bound to one authoritative room.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomObservationSubscription {
    /// Extension identifier owning this value.
    pub plugin: PluginId,
    /// Positive operator generation used to fence stale observation configuration.
    pub generation: ObservationGeneration,
    /// Digest of the component bytes, effective configuration, grants, origins,
    /// observation settings, and runtime limits.
    pub identity: Sha256Digest,
    /// Bare JID of the room in scope.
    pub room: BareJid,
}

/// Canonical room message and revision identity whose body is being observed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomMessageSource {
    /// Bare JID of the room in scope.
    pub room: BareJid,
    /// Stable identity of the canonical room message being observed.
    pub stanza_id: StanzaId,
    /// Stanza identity of the specific original or corrected revision being observed.
    pub revision_stanza_id: StanzaId,
    /// Sender-assigned XEP-0359 origin identifier.
    pub origin_id: Option<OriginId>,
    /// Bare account JID of the canonical source-message author.
    pub sender: BareJid,
    /// Host-assigned revision number of the source message body.
    pub revision: MessageRevision,
    /// SHA-256 digest of the exact UTF-8 body bytes supplied for observation.
    pub body_digest: Sha256Digest,
    /// RFC 3339 time at which the host observed this message revision.
    pub observed_at: Timestamp,
}

/// Validated source metadata and body delivered to a durable room observer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomMessageObserve {
    /// Canonical source metadata identifying the message targeted by this operation.
    pub source: RoomMessageSource,
    /// Text body of the message.
    pub body: DisplayText,
}

/// Provider, model, and micro-USD cost reported for one guest invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvocationUsage {
    /// Identifier of the external provider producing this event or usage record.
    pub provider: ProviderId,
    /// Identifier of the provider model used by the invocation.
    pub model: ModelId,
    /// Reported invocation cost in millionths of a US dollar.
    pub cost_micro_usd: u64,
}

/// Validated durable observation payloads and optional invocation usage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomObservationResult {
    /// Extension payload elements carried alongside the standard protocol fields.
    pub payloads: Vec<ExtensionPayload>,
    /// Optional provider/model cost accounting reported by the invocation.
    pub usage: Option<InvocationUsage>,
}

/// Stable failure categories used by durable observation retry and fencing decisions.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ObservationFailure {
    /// Guest or host reported a potentially transient invocation failure.
    TemporaryFailure,
    /// Invocation input failed the guest or host request contract.
    InvalidRequest,
    /// The requested action lacked the necessary host authority or capability.
    Denied,
    /// Guest does not handle the supplied event category.
    UnsupportedEvent,
    /// Component instantiation or execution trapped or otherwise failed.
    RuntimeFailure,
    /// Guest execution exhausted a configured memory, fuel, or other resource budget.
    ResourceLimit,
    /// Invocation did not finish before its configured deadline.
    DeadlineExceeded,
    /// Guest output violated the response shape, effect, or payload contract.
    InvalidResult,
    /// Source room, body digest, or observation timestamp failed host validation.
    SourceMismatch,
}

/// Reason a durable observation did not apply or produced no result.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ObservationSkip {
    /// The source has no usable sender-assigned origin ID for durable correlation.
    MissingOriginId,
    /// The subscription no longer matches a loaded observer's identity, generation, or room scope.
    SubscriptionUnavailable,
    /// The completed observer returned no publishable room result.
    NoResult,
}

/// Distinguishes safe admission retries, potentially side-effecting failures, and validated
/// completion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RoomObservationOutcome {
    /// Observation returned a validated result suitable for host-owned publication.
    Completed(RoomObservationResult),
    /// Admission rejected before entering the guest; retry is safe.
    NotInvoked,
    /// The guest may have produced effects; wait for lease expiry before a fenced retry.
    UnresolvedFailure(ObservationFailure),
    /// The operation failed in a way that should not be retried.
    PermanentFailure(ObservationFailure),
    /// Observation was skipped or completed without a publishable result for the enclosed reason.
    NotApplicable(ObservationSkip),
}
