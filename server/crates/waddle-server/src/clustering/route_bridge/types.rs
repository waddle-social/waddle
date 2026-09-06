use super::*;

pub(super) fn stanza_message_id(stanza: &Stanza) -> &str {
    match stanza {
        Stanza::Message(message) => message.id.as_ref().map_or("", |id| id.0.as_str()),
        Stanza::Iq(_) | Stanza::Presence(_) => "",
    }
}

pub(super) type RemoteDeliveryFuture<'a> =
    Pin<Box<dyn Future<Output = Option<FullJidDeliveryOutcome>> + Send + 'a>>;
pub(super) type CapturedRemoteDeliveryFuture<'a> =
    Pin<Box<dyn Future<Output = Option<CapturedRemoteDeliveryOutcome>> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapturedRemoteDeliveryOutcome {
    pub(crate) outcome: FullJidDeliveryOutcome,
    pub(crate) recipient_sm_append_streams: Vec<waddle_xmpp::pending_delivery::SmSessionId>,
}

impl CapturedRemoteDeliveryOutcome {
    pub(crate) fn from_outcome(outcome: FullJidDeliveryOutcome) -> Self {
        Self {
            outcome,
            recipient_sm_append_streams: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RemoteResourceRegistrationId(uuid::Uuid);

impl RemoteResourceRegistrationId {
    pub(super) fn fresh() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct RemoteResourceSocketGeneration(u64);

impl RemoteResourceSocketGeneration {
    pub(super) fn next(current: Option<Self>) -> Self {
        Self(
            current
                .map(|generation| generation.0)
                .unwrap_or(0)
                .saturating_add(1),
        )
    }
}

/// Typed RFC 6121 §4.7.2.1 presence `<show/>` for cross-node state
/// transfer; the default "available" state is the absent `Option` on
/// [`RemotePresenceStateSnapshot::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RemotePresenceShow {
    Away,
    Chat,
    Dnd,
    Xa,
}

impl RemotePresenceShow {
    /// The wire value used by the registry's `PresenceState` boundary.
    fn as_wire(self) -> &'static str {
        match self {
            Self::Away => "away",
            Self::Chat => "chat",
            Self::Dnd => "dnd",
            Self::Xa => "xa",
        }
    }

    /// Parse the registry's stored show string. `PresenceState.show` is
    /// only ever populated from parsed RFC 6121 presence, so anything
    /// out of contract degrades to `None` (plain "available") instead
    /// of relaying junk cross-node.
    fn from_wire(show: &str) -> Option<Self> {
        match show {
            "away" => Some(Self::Away),
            "chat" => Some(Self::Chat),
            "dnd" => Some(Self::Dnd),
            "xa" => Some(Self::Xa),
            _ => None,
        }
    }
}

impl From<xmpp_parsers::presence::Show> for RemotePresenceShow {
    fn from(show: xmpp_parsers::presence::Show) -> Self {
        match show {
            xmpp_parsers::presence::Show::Away => Self::Away,
            xmpp_parsers::presence::Show::Chat => Self::Chat,
            xmpp_parsers::presence::Show::Dnd => Self::Dnd,
            xmpp_parsers::presence::Show::Xa => Self::Xa,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemotePresenceStateSnapshot {
    pub show: Option<RemotePresenceShow>,
    pub status: Option<String>,
    pub priority: i8,
    pub payloads: Vec<RemoteElement>,
}

impl From<PresenceState> for RemotePresenceStateSnapshot {
    fn from(state: PresenceState) -> Self {
        Self {
            show: state
                .show
                .as_deref()
                .and_then(RemotePresenceShow::from_wire),
            status: state.status,
            priority: state.priority,
            payloads: state.payloads.into_iter().map(RemoteElement).collect(),
        }
    }
}

impl From<RemotePresenceStateSnapshot> for PresenceState {
    fn from(state: RemotePresenceStateSnapshot) -> Self {
        Self {
            show: state.show.map(|show| show.as_wire().to_string()),
            status: state.status,
            priority: state.priority,
            payloads: state
                .payloads
                .into_iter()
                .map(|element| element.0)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteResourceStateSnapshot {
    pub carbons_enabled: bool,
    pub roster_interested: bool,
    pub blocklist_interested: bool,
    pub presence_available: bool,
    pub presence_priority: i8,
    pub presence_state: Option<RemotePresenceStateSnapshot>,
}

impl RemoteResourceStateSnapshot {
    pub(super) fn from_entry(
        entry: &ConnectionEntry,
        presence_state: Option<PresenceState>,
    ) -> Self {
        Self {
            carbons_enabled: entry.carbons_enabled.load(Ordering::Relaxed),
            roster_interested: entry.roster_interested.load(Ordering::Relaxed),
            blocklist_interested: entry.blocklist_interested.load(Ordering::Relaxed),
            presence_available: entry.presence_available.load(Ordering::Relaxed),
            presence_priority: entry.presence_priority.load(Ordering::Relaxed),
            presence_state: presence_state.map(RemotePresenceStateSnapshot::from),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RemoteResourceStateUpdate {
    Presence {
        available: bool,
        priority: i8,
        state: Option<RemotePresenceStateSnapshot>,
    },
    Carbons {
        enabled: bool,
    },
    RosterInterested,
    BlocklistInterested,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum RemoteCarbonKind {
    Sent,
    Received,
}

impl From<CarbonKind> for RemoteCarbonKind {
    fn from(kind: CarbonKind) -> Self {
        match kind {
            CarbonKind::Sent => Self::Sent,
            CarbonKind::Received => Self::Received,
        }
    }
}

impl From<RemoteCarbonKind> for CarbonKind {
    fn from(kind: RemoteCarbonKind) -> Self {
        match kind {
            RemoteCarbonKind::Sent => Self::Sent,
            RemoteCarbonKind::Received => Self::Received,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RemoteUserSideEffect {
    Carbons {
        owner: jid::BareJid,
        message: RemoteStanza,
        kind: RemoteCarbonKind,
        exclude: Vec<jid::FullJid>,
    },
    RosterPush {
        user_jid: jid::BareJid,
        source_jid: jid::FullJid,
        item: RosterItem,
        version: RosterVersion,
    },
    BlocklistPush {
        user_bare: jid::BareJid,
        blocked: bool,
        jids: Vec<jid::Jid>,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteResourceOutboundFrame {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub stanza: RemoteStanza,
    pub kind: DeliveryKind,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteResourceWriteAcceptedOutboundFrame {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub socket_generation: RemoteResourceSocketGeneration,
    pub stanza: RemoteStanza,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RemoteResourceRouteTarget {
    FullJid {
        target: jid::FullJid,
        stanza: RemoteStanza,
    },
    BareJid {
        target: jid::BareJid,
        stanza: RemoteStanza,
    },
    MucProxy {
        canonical: Option<crate::ingress::IngressCanonicalRef>,
        principal: Option<waddle_xmpp::auth::AuthenticatedPrincipalRef>,
        #[serde(with = "crate::ingress::identity::stanza_lang_serde")]
        stanza_lang: Option<xmpp_parsers::message::Lang>,
        room_jid: jid::BareJid,
        kind: OrderedRelayMucProxyKind,
        origin: MucProxyOrigin,
        stanza: RemoteStanza,
    },
}

pub(super) fn route_target_stanza_is_iq(target: &RemoteResourceRouteTarget) -> bool {
    match target {
        RemoteResourceRouteTarget::FullJid { stanza, .. }
        | RemoteResourceRouteTarget::BareJid { stanza, .. }
        | RemoteResourceRouteTarget::MucProxy { stanza, .. } => matches!(stanza.0, Stanza::Iq(_)),
    }
}

/// The few small fields the delivery-outcome log needs, extracted
/// before the route target (and its full stanza) is moved into the
/// routing future — logging must never force a stanza deep-clone.
pub(super) struct RouteOutcomeLog {
    pub(super) kind: &'static str,
    pub(super) entity: String,
    pub(super) message_id: String,
}

pub(super) fn route_outcome_log(target: &RemoteResourceRouteTarget) -> RouteOutcomeLog {
    match target {
        RemoteResourceRouteTarget::FullJid { target, stanza } => RouteOutcomeLog {
            kind: "full-JID",
            entity: target.to_string(),
            message_id: stanza_message_id(&stanza.0).to_owned(),
        },
        RemoteResourceRouteTarget::BareJid { target, stanza } => RouteOutcomeLog {
            kind: "bare-JID",
            entity: target.to_string(),
            message_id: stanza_message_id(&stanza.0).to_owned(),
        },
        RemoteResourceRouteTarget::MucProxy {
            room_jid, stanza, ..
        } => RouteOutcomeLog {
            kind: "MUC proxy",
            entity: room_jid.to_string(),
            message_id: stanza_message_id(&stanza.0).to_owned(),
        },
    }
}

pub(super) fn log_remote_resource_route_outcome(
    context: &RouteOutcomeLog,
    outcome: FullJidDeliveryOutcome,
) {
    tracing::debug!(
        kind = context.kind,
        entity = %context.entity,
        message_id = %context.message_id,
        ?outcome,
        "ordered-relay delivery outcome"
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RemoteResourceRouteOutcome {
    Delivered,
    QueuedDetached,
    Unavailable,
    Dropped,
    StaleRegistration,
    MaybeCommitted,
    JoinMaybeCommitted,
}

impl From<FullJidDeliveryOutcome> for RemoteResourceRouteOutcome {
    fn from(outcome: FullJidDeliveryOutcome) -> Self {
        match outcome {
            FullJidDeliveryOutcome::Delivered => Self::Delivered,
            FullJidDeliveryOutcome::QueuedDetached => Self::QueuedDetached,
            FullJidDeliveryOutcome::Unavailable => Self::Unavailable,
            FullJidDeliveryOutcome::Dropped => Self::Dropped,
            FullJidDeliveryOutcome::MaybeCommitted => Self::MaybeCommitted,
        }
    }
}

impl From<RemoteResourceRouteOutcome> for FullJidDeliveryOutcome {
    fn from(outcome: RemoteResourceRouteOutcome) -> Self {
        match outcome {
            RemoteResourceRouteOutcome::Delivered => Self::Delivered,
            RemoteResourceRouteOutcome::QueuedDetached => Self::QueuedDetached,
            RemoteResourceRouteOutcome::Unavailable
            | RemoteResourceRouteOutcome::StaleRegistration => Self::Unavailable,
            RemoteResourceRouteOutcome::Dropped => Self::Dropped,
            RemoteResourceRouteOutcome::MaybeCommitted
            | RemoteResourceRouteOutcome::JoinMaybeCommitted => Self::MaybeCommitted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteResourceRegisterOutcome {
    Registered,
    NotRemote,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteResourceUnregisterOutcome {
    NotRegistered,
    Unregistered,
    RecordedRetry,
    Failed,
}

impl RemoteResourceUnregisterOutcome {
    pub(crate) fn permits_detached_force_ack(self) -> bool {
        matches!(
            self,
            Self::NotRegistered | Self::Unregistered | Self::RecordedRetry
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RemoteResourceOriginRefresh {
    Remote(RemoteResourceOriginSnapshot),
    LocalOwner,
    Failed,
}

#[derive(Debug, Clone)]
pub(super) struct RemoteSocketRegistration {
    pub(super) registration_id: RemoteResourceRegistrationId,
    pub(super) socket_generation: RemoteResourceSocketGeneration,
    pub(super) owner: Arc<AtomicBool>,
    pub(super) user_owner: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct PendingRemoteSocketUnregisterKey {
    pub(super) jid: jid::FullJid,
    pub(super) registration_id: RemoteResourceRegistrationId,
    pub(super) socket_generation: RemoteResourceSocketGeneration,
}

#[derive(Debug, Clone)]
pub(super) struct PendingRemoteSocketUnregister {
    pub(super) key: PendingRemoteSocketUnregisterKey,
    pub(super) user_owner: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteResourceOriginSnapshot {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub socket_generation: RemoteResourceSocketGeneration,
    pub user_owner: NodeId,
}

#[derive(Debug, Clone)]
pub(super) struct RemoteOwnerRegistration {
    pub(super) registration_id: RemoteResourceRegistrationId,
    pub(super) socket_node: NodeId,
    pub(super) socket_generation: RemoteResourceSocketGeneration,
    pub(super) owner: Arc<AtomicBool>,
}

pub(super) struct RelayOriginSigner {
    pub(super) keypair: Keypair,
    pub(super) public_key: Vec<u8>,
}

impl RelayOriginSigner {
    pub(super) fn new(keypair: Keypair) -> Self {
        let public_key = keypair.public().encode_protobuf();
        Self {
            keypair,
            public_key,
        }
    }
}

pub(super) struct PreparedRemoteDelivery {
    pub(super) services: Arc<OrderedRelayDeliveryServices>,
    pub(super) target_entity: Entity,
    pub(super) previous_owner: NodeIdentity,
    pub(super) channel: OrderedRelayChannel,
    pub(super) envelope: RemoteStanzaEnvelope,
    pub(super) target: jid::Jid,
    pub(super) stanza: Stanza,
    pub(super) is_iq: bool,
}

pub(super) struct RemoteDeliveryOutcome {
    pub(super) delivery: FullJidDeliveryOutcome,
    pub(super) client_replies: Vec<Stanza>,
    pub(super) maybe_committed: bool,
    pub(super) join_repair_allowed: bool,
    /// The owner that actually accepted this delivery.  A target-refresh can
    /// change it from the owner used to construct the initial envelope.
    pub(super) relay_target: Option<NodeIdentity>,
    /// Exact target claim carried by the envelope accepted by a remote owner.
    /// This is retained separately from the mutable room-claim cache so
    /// callers can freeze the claim that authorized a delivered MUC stanza.
    pub(super) target_claim: Option<OrderedRelayClaim>,
}

pub(super) fn caller_delivery_outcome(outcome: RemoteDeliveryOutcome) -> FullJidDeliveryOutcome {
    if outcome.maybe_committed {
        FullJidDeliveryOutcome::MaybeCommitted
    } else {
        outcome.delivery
    }
}

pub(super) struct RemoteDeliverySeed {
    pub(super) services: Arc<OrderedRelayDeliveryServices>,
    pub(super) target_entity: Entity,
    pub(super) previous_owner: NodeIdentity,
    pub(super) channel: OrderedRelayChannel,
    pub(super) asserted_origin_node: NodeId,
    pub(super) origin_inbound_sequence: OriginInboundSequence,
    pub(super) origin_claim: OrderedRelayClaim,
    pub(super) sender_claim: OrderedRelayClaim,
    pub(super) target_claim: OrderedRelayClaim,
    pub(super) payload: OrderedRelayPayload,
    pub(super) target: jid::Jid,
    pub(super) stanza: Stanza,
    pub(super) is_iq: bool,
}
