//! Frozen wire DTOs. Do not replace these fields with domain target/frame types.
//! Exhaustive adapters deliberately make domain evolution a compile-time review.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RouteV8 {
    pub(super) source_jid: jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    socket_generation: RemoteResourceSocketGeneration,
    target: RouteTargetV8,
    #[serde(default)]
    pub(super) trace: RelayTraceContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum RouteTargetV8 {
    ProcessedDirectMessage {
        target: jid::FullJid,
        stanza: RemoteStanza,
        ingress_append: Option<AppendV8>,
    },
    FullJid {
        target: jid::FullJid,
        stanza: RemoteStanza,
        ingress_append: Option<AppendV8>,
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
        kind: MucKind,
        origin: MucOrigin,
        stanza: RemoteStanza,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppendV8 {
    message_key: waddle_xmpp::ingress::MessageKey,
    sender_bare: jid::BareJid,
    receipt: crate::ingress::EffectReceiptKey,
    received_at: Option<chrono::DateTime<chrono::Utc>>,
    archive_positions: Vec<waddle_xmpp::stream_management::ArchiveDispatchPosition>,
    dispatch_stream: Option<waddle_xmpp::pending_delivery::SmSessionId>,
}

impl From<IngressAppendObligationRef> for AppendV8 {
    fn from(value: IngressAppendObligationRef) -> Self {
        let IngressAppendObligationRef {
            message_key,
            sender_bare,
            receipt,
            received_at,
            archive_positions,
            dispatch_stream,
        } = value;
        Self {
            message_key,
            sender_bare,
            receipt,
            received_at,
            archive_positions,
            dispatch_stream,
        }
    }
}

impl From<AppendV8> for IngressAppendObligationRef {
    fn from(value: AppendV8) -> Self {
        let AppendV8 {
            message_key,
            sender_bare,
            receipt,
            received_at,
            archive_positions,
            dispatch_stream,
        } = value;
        Self {
            message_key,
            sender_bare,
            receipt,
            received_at,
            archive_positions,
            dispatch_stream,
        }
    }
}

impl From<RelayRouteRemoteResourceStanza> for RouteV8 {
    fn from(value: RelayRouteRemoteResourceStanza) -> Self {
        let RelayRouteRemoteResourceStanza {
            source_jid,
            registration_id,
            socket_generation,
            target,
            trace,
        } = value;
        let target = match target {
            RemoteResourceRouteTarget::ProcessedDirectMessage {
                target,
                stanza,
                ingress_append,
            } => RouteTargetV8::ProcessedDirectMessage {
                target,
                stanza,
                ingress_append: ingress_append.map(Into::into),
            },
            RemoteResourceRouteTarget::FullJid {
                target,
                stanza,
                ingress_append,
            } => RouteTargetV8::FullJid {
                target,
                stanza,
                ingress_append: ingress_append.map(Into::into),
            },
            RemoteResourceRouteTarget::BareJid { target, stanza } => {
                RouteTargetV8::BareJid { target, stanza }
            }
            RemoteResourceRouteTarget::MucProxy {
                canonical,
                principal,
                stanza_lang,
                room_jid,
                kind,
                origin,
                stanza,
            } => RouteTargetV8::MucProxy {
                canonical,
                principal,
                stanza_lang,
                room_jid,
                kind: kind.into(),
                origin: origin.into(),
                stanza,
            },
        };
        Self {
            source_jid,
            registration_id,
            socket_generation,
            target,
            trace,
        }
    }
}

impl From<RouteV8> for RelayRouteRemoteResourceStanza {
    fn from(value: RouteV8) -> Self {
        let RouteV8 {
            source_jid,
            registration_id,
            socket_generation,
            target,
            trace,
        } = value;
        let target = match target {
            RouteTargetV8::ProcessedDirectMessage {
                target,
                stanza,
                ingress_append,
            } => RemoteResourceRouteTarget::ProcessedDirectMessage {
                target,
                stanza,
                ingress_append: ingress_append.map(Into::into),
            },
            RouteTargetV8::FullJid {
                target,
                stanza,
                ingress_append,
            } => RemoteResourceRouteTarget::FullJid {
                target,
                stanza,
                ingress_append: ingress_append.map(Into::into),
            },
            RouteTargetV8::BareJid { target, stanza } => {
                RemoteResourceRouteTarget::BareJid { target, stanza }
            }
            RouteTargetV8::MucProxy {
                canonical,
                principal,
                stanza_lang,
                room_jid,
                kind,
                origin,
                stanza,
            } => RemoteResourceRouteTarget::MucProxy {
                canonical,
                principal,
                stanza_lang,
                room_jid,
                kind: kind.into(),
                origin: origin.into(),
                stanza,
            },
        };
        Self {
            source_jid,
            registration_id,
            socket_generation,
            target,
            trace,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FrameV3 {
    pub(super) frame: OutboundFrameV3,
    #[serde(default)]
    pub(super) trace: RelayTraceContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OutboundFrameV3 {
    pub(super) jid: jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    stanza: RemoteStanza,
    kind: FrameKind,
    ingress_append: Option<AppendV8>,
}

impl From<RelayDeliverRemoteResourceFrame> for FrameV3 {
    fn from(value: RelayDeliverRemoteResourceFrame) -> Self {
        let RelayDeliverRemoteResourceFrame { frame, trace } = value;
        let RemoteResourceOutboundFrame {
            jid,
            registration_id,
            stanza,
            kind,
            ingress_append,
        } = frame;
        Self {
            frame: OutboundFrameV3 {
                jid,
                registration_id,
                stanza,
                kind: kind.into(),
                ingress_append: ingress_append.map(Into::into),
            },
            trace,
        }
    }
}

impl From<FrameV3> for RelayDeliverRemoteResourceFrame {
    fn from(value: FrameV3) -> Self {
        let FrameV3 { frame, trace } = value;
        let OutboundFrameV3 {
            jid,
            registration_id,
            stanza,
            kind,
            ingress_append,
        } = frame;
        Self {
            frame: RemoteResourceOutboundFrame {
                jid,
                registration_id,
                stanza,
                kind: kind.into(),
                ingress_append: ingress_append.map(Into::into),
            },
            trace,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LiveRoute {
    pub(super) source_jid: jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    socket_generation: RemoteResourceSocketGeneration,
    target: LiveTarget,
    #[serde(default)]
    pub(super) trace: RelayTraceContext,
}

/// The stable live route never depends on the changing append obligation graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum LiveTarget {
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
        kind: MucKind,
        origin: MucOrigin,
        stanza: RemoteStanza,
    },
}

impl LiveRoute {
    pub(super) fn from_current(value: &RelayRouteRemoteResourceStanza) -> Option<Self> {
        let target = match &value.target {
            RemoteResourceRouteTarget::FullJid {
                target,
                stanza,
                ingress_append: None,
            } => LiveTarget::FullJid {
                target: target.clone(),
                stanza: stanza.clone(),
            },
            RemoteResourceRouteTarget::BareJid { target, stanza } => LiveTarget::BareJid {
                target: target.clone(),
                stanza: stanza.clone(),
            },
            RemoteResourceRouteTarget::MucProxy {
                canonical,
                principal,
                stanza_lang,
                room_jid,
                kind,
                origin,
                stanza,
            } => LiveTarget::MucProxy {
                canonical: canonical.clone(),
                principal: principal.clone(),
                stanza_lang: stanza_lang.clone(),
                room_jid: room_jid.clone(),
                kind: (*kind).into(),
                origin: (*origin).into(),
                stanza: stanza.clone(),
            },
            RemoteResourceRouteTarget::ProcessedDirectMessage { .. }
            | RemoteResourceRouteTarget::FullJid {
                ingress_append: Some(_),
                ..
            } => return None,
        };
        Some(Self {
            source_jid: value.source_jid.clone(),
            registration_id: value.registration_id,
            socket_generation: value.socket_generation,
            target,
            trace: value.trace.clone(),
        })
    }
}

impl From<LiveRoute> for RelayRouteRemoteResourceStanza {
    fn from(value: LiveRoute) -> Self {
        let LiveRoute {
            source_jid,
            registration_id,
            socket_generation,
            target,
            trace,
        } = value;
        let target = match target {
            LiveTarget::FullJid { target, stanza } => RemoteResourceRouteTarget::FullJid {
                target,
                stanza,
                ingress_append: None,
            },
            LiveTarget::BareJid { target, stanza } => {
                RemoteResourceRouteTarget::BareJid { target, stanza }
            }
            LiveTarget::MucProxy {
                canonical,
                principal,
                stanza_lang,
                room_jid,
                kind,
                origin,
                stanza,
            } => RemoteResourceRouteTarget::MucProxy {
                canonical,
                principal,
                stanza_lang,
                room_jid,
                kind: kind.into(),
                origin: origin.into(),
                stanza,
            },
        };
        Self {
            source_jid,
            registration_id,
            socket_generation,
            target,
            trace,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LiveFrame {
    pub(super) jid: jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    stanza: RemoteStanza,
    kind: FrameKind,
    #[serde(default)]
    pub(super) trace: RelayTraceContext,
}

impl LiveFrame {
    pub(super) fn from_current(value: &RelayDeliverRemoteResourceFrame) -> Option<Self> {
        if value.frame.ingress_append.is_some() {
            return None;
        }
        Some(Self {
            jid: value.frame.jid.clone(),
            registration_id: value.frame.registration_id,
            stanza: value.frame.stanza.clone(),
            kind: value.frame.kind.into(),
            trace: value.trace.clone(),
        })
    }
}

impl From<LiveFrame> for RelayDeliverRemoteResourceFrame {
    fn from(value: LiveFrame) -> Self {
        let LiveFrame {
            jid,
            registration_id,
            stanza,
            kind,
            trace,
        } = value;
        Self {
            frame: RemoteResourceOutboundFrame {
                jid,
                registration_id,
                stanza,
                kind: kind.into(),
                ingress_append: None,
            },
            trace,
        }
    }
}

/// Both stable routes retain the baseline receipt and result protocol exactly.
#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub(crate) struct RouteReply {
    reply_receipt: Option<RelayReplyReceiptToken>,
    owner_receipts: Vec<OwnerReceipt>,
    outcome: RouteOutcome,
    replies: Vec<RemoteStanza>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OwnerReceipt {
    message_key: waddle_xmpp::ingress::MessageKey,
    kind: waddle_xmpp::stream_management::SmIngressReceiptKind,
    semantic_identity_hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum RouteOutcome {
    Delivered,
    QueuedDetached,
    Unavailable,
    Dropped,
    StaleRegistration,
    MaybeCommitted,
    JoinMaybeCommitted,
}

impl From<RemoteResourceRouteOutcome> for RouteOutcome {
    fn from(value: RemoteResourceRouteOutcome) -> Self {
        match value {
            RemoteResourceRouteOutcome::Delivered => Self::Delivered,
            RemoteResourceRouteOutcome::QueuedDetached => Self::QueuedDetached,
            RemoteResourceRouteOutcome::Unavailable => Self::Unavailable,
            RemoteResourceRouteOutcome::Dropped => Self::Dropped,
            RemoteResourceRouteOutcome::StaleRegistration => Self::StaleRegistration,
            RemoteResourceRouteOutcome::MaybeCommitted => Self::MaybeCommitted,
            RemoteResourceRouteOutcome::JoinMaybeCommitted => Self::JoinMaybeCommitted,
        }
    }
}

impl From<RouteOutcome> for RemoteResourceRouteOutcome {
    fn from(value: RouteOutcome) -> Self {
        match value {
            RouteOutcome::Delivered => Self::Delivered,
            RouteOutcome::QueuedDetached => Self::QueuedDetached,
            RouteOutcome::Unavailable => Self::Unavailable,
            RouteOutcome::Dropped => Self::Dropped,
            RouteOutcome::StaleRegistration => Self::StaleRegistration,
            RouteOutcome::MaybeCommitted => Self::MaybeCommitted,
            RouteOutcome::JoinMaybeCommitted => Self::JoinMaybeCommitted,
        }
    }
}

impl From<RelayRouteRemoteResourceStanzaReply> for RouteReply {
    fn from(value: RelayRouteRemoteResourceStanzaReply) -> Self {
        let RelayRouteRemoteResourceStanzaReply {
            reply_receipt,
            owner_receipts,
            outcome,
            replies,
        } = value;
        Self {
            reply_receipt,
            owner_receipts: owner_receipts
                .into_iter()
                .map(|receipt| {
                    let waddle_xmpp::stream_management::SmIngressFrameReceipt {
                        message_key,
                        kind,
                        semantic_identity_hash,
                    } = receipt;
                    OwnerReceipt {
                        message_key,
                        kind,
                        semantic_identity_hash,
                    }
                })
                .collect(),
            outcome: outcome.into(),
            replies,
        }
    }
}

impl From<RouteReply> for RelayRouteRemoteResourceStanzaReply {
    fn from(value: RouteReply) -> Self {
        let RouteReply {
            reply_receipt,
            owner_receipts,
            outcome,
            replies,
        } = value;
        Self {
            reply_receipt,
            owner_receipts: owner_receipts
                .into_iter()
                .map(|receipt| {
                    let OwnerReceipt {
                        message_key,
                        kind,
                        semantic_identity_hash,
                    } = receipt;
                    waddle_xmpp::stream_management::SmIngressFrameReceipt {
                        message_key,
                        kind,
                        semantic_identity_hash,
                    }
                })
                .collect(),
            outcome: outcome.into(),
            replies,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub(crate) struct FrameReply {
    status: FrameStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum FrameStatus {
    Delivered,
    Backpressure,
    Unavailable,
}

impl From<RelayRemoteResourceFrameReply> for FrameReply {
    fn from(value: RelayRemoteResourceFrameReply) -> Self {
        Self {
            status: match value.status {
                RelayRemoteResourceFrameStatus::Delivered => FrameStatus::Delivered,
                RelayRemoteResourceFrameStatus::Backpressure => FrameStatus::Backpressure,
                RelayRemoteResourceFrameStatus::Unavailable => FrameStatus::Unavailable,
            },
        }
    }
}

impl From<FrameReply> for RelayRemoteResourceFrameReply {
    fn from(value: FrameReply) -> Self {
        Self {
            status: match value.status {
                FrameStatus::Delivered => RelayRemoteResourceFrameStatus::Delivered,
                FrameStatus::Backpressure => RelayRemoteResourceFrameStatus::Backpressure,
                FrameStatus::Unavailable => RelayRemoteResourceFrameStatus::Unavailable,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum MucKind {
    JoinPresence,
    OccupantPresence,
    GroupchatMessage,
    PrivateMessage,
    BareRoomIq,
    OccupantIq,
    FanoutChunk,
    MujiJingleIq,
}

impl From<OrderedRelayMucProxyKind> for MucKind {
    fn from(value: OrderedRelayMucProxyKind) -> Self {
        match value {
            OrderedRelayMucProxyKind::JoinPresence => Self::JoinPresence,
            OrderedRelayMucProxyKind::OccupantPresence => Self::OccupantPresence,
            OrderedRelayMucProxyKind::GroupchatMessage => Self::GroupchatMessage,
            OrderedRelayMucProxyKind::PrivateMessage => Self::PrivateMessage,
            OrderedRelayMucProxyKind::BareRoomIq => Self::BareRoomIq,
            OrderedRelayMucProxyKind::OccupantIq => Self::OccupantIq,
            OrderedRelayMucProxyKind::FanoutChunk => Self::FanoutChunk,
            OrderedRelayMucProxyKind::MujiJingleIq => Self::MujiJingleIq,
        }
    }
}

impl From<MucKind> for OrderedRelayMucProxyKind {
    fn from(value: MucKind) -> Self {
        match value {
            MucKind::JoinPresence => Self::JoinPresence,
            MucKind::OccupantPresence => Self::OccupantPresence,
            MucKind::GroupchatMessage => Self::GroupchatMessage,
            MucKind::PrivateMessage => Self::PrivateMessage,
            MucKind::BareRoomIq => Self::BareRoomIq,
            MucKind::OccupantIq => Self::OccupantIq,
            MucKind::FanoutChunk => Self::FanoutChunk,
            MucKind::MujiJingleIq => Self::MujiJingleIq,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum FrameKind {
    PeerStanza,
    DirectFrame,
}

impl From<DeliveryKind> for FrameKind {
    fn from(value: DeliveryKind) -> Self {
        match value {
            DeliveryKind::PeerStanza => Self::PeerStanza,
            DeliveryKind::DirectFrame => Self::DirectFrame,
        }
    }
}

impl From<FrameKind> for DeliveryKind {
    fn from(value: FrameKind) -> Self {
        match value {
            FrameKind::PeerStanza => Self::PeerStanza,
            FrameKind::DirectFrame => Self::DirectFrame,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum MucOrigin {
    Connection(waddle_xmpp_core::OccupancySessionGeneration),
    Server,
}

impl From<MucProxyOrigin> for MucOrigin {
    fn from(value: MucProxyOrigin) -> Self {
        match value {
            MucProxyOrigin::Connection(generation) => Self::Connection(generation),
            MucProxyOrigin::Server => Self::Server,
        }
    }
}

impl From<MucOrigin> for MucProxyOrigin {
    fn from(value: MucOrigin) -> Self {
        match value {
            MucOrigin::Connection(generation) => Self::Connection(generation),
            MucOrigin::Server => Self::Server,
        }
    }
}
