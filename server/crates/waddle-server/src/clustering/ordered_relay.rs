//! Typed ordered relay substrate for ADR-0017 Phase 4 Slice 2.
//!
//! This module is deliberately only a substrate: it allocates typed per-channel
//! sequence numbers, validates receiver-side ordering, and models internal
//! ACK/NACK/diversion outcomes. It does not deliver to UserActor, RoomActor,
//! ConnectionRegistry, pending delivery, or XEP-0198 state.

use super::codec::RemoteStanza;
use super::NodeId;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp_core::OccupancySessionGeneration;

const RECENT_ACK_CACHE_PER_CHANNEL: usize = 64;
const MAX_TRACKED_ORDERED_RELAY_CHANNELS: usize = 8_192;

/// Monotonic sequence number on one ordered relay channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OrderedRelaySequence(pub u64);

impl OrderedRelaySequence {
    pub const FIRST: Self = Self(1);

    fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// XEP-0198 handled counter observed by the origin stream when it accepted the
/// stanza that produced this relay envelope. This is carried as provenance for
/// later resume-window and durability checks; Slice 2 does not mutate SM state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OriginInboundSequence(pub u32);

/// Ordered lane within one room's relay traffic (#1597).
///
/// The lane is part of the channel key, so each lane has its own
/// sequence space and its own diversion. MUC stanza kinds share one
/// lane because XEP-0045 requires join presence to precede messages;
/// Muji signaling has no ordering relation to MUC stanzas — only to
/// itself (XEP-0166: `session-initiate` before `session-terminate`) —
/// so a poisoned Muji lane cannot stop room join/leave/groupchat
/// traffic, and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderedRelayRoomLane {
    MucStanza,
    MujiSignaling,
}

/// Typed recipient key for ordered-relay channels.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderedRelayRecipient {
    BareJid(jid::BareJid),
    FullJid(jid::FullJid),
    Room {
        room: jid::BareJid,
        lane: OrderedRelayRoomLane,
    },
}

/// Typed origin key for ordered-relay channels.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderedRelayOrigin {
    SmSession(SmSessionId),
    Entity(Entity),
}

/// One origin-stream/recipient lane. Ordering is scoped to this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderedRelayChannel {
    pub origin: OrderedRelayOrigin,
    pub recipient: OrderedRelayRecipient,
    pub target_epoch: ClaimEpoch,
}

impl Hash for OrderedRelayChannel {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.origin.hash(state);
        self.recipient.hash(state);
        self.target_epoch.0.hash(state);
    }
}

/// Claim provenance carried on every ordered relay envelope. Slice 2 only
/// carries this typed proof; later routing slices validate it against
/// Postgres before applying delivery effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderedRelayClaim {
    pub entity: Entity,
    pub epoch: ClaimEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedRelayEnvelopeClaims {
    pub origin: OrderedRelayClaim,
    pub sender: OrderedRelayClaim,
    pub target: OrderedRelayClaim,
}

impl OrderedRelayEnvelopeClaims {
    pub fn new(
        origin: OrderedRelayClaim,
        sender: OrderedRelayClaim,
        target: OrderedRelayClaim,
    ) -> Self {
        Self {
            origin,
            sender,
            target,
        }
    }
}

/// Cryptographic provenance for the sender's asserted origin node.
///
/// The `public_key` is the sender's libp2p public key encoded with
/// `libp2p_identity::PublicKey::encode_protobuf`; the receiver verifies
/// `signature` over the envelope's signing bytes and derives the sender
/// `PeerId` from the decoded public key. This is deliberately separate from
/// `asserted_origin_node`: the node id remains claim provenance, while the
/// public key proves which enrolled swarm identity signed this relay unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderedRelayOriginProof {
    pub public_key: Vec<u8>,
    pub signature: Vec<u8>,
}

/// MUC proxy traffic classes for the later remote-safe MUC message set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderedRelayMucProxyKind {
    JoinPresence,
    OccupantPresence,
    GroupchatMessage,
    PrivateMessage,
    BareRoomIq,
    OccupantIq,
    FanoutChunk,
    /// A Muji (XEP-0272) `session-initiate` or `session-terminate`
    /// relayed to the room-owning node (#1445). Unlike `BareRoomIq`
    /// the stanza is NOT addressed to the room: `to` is the calls
    /// mixer (`calls.<domain>`) and the target room lives in the
    /// `<muji room='…'/>` payload, which the envelope validation binds
    /// to the channel's room.
    MujiJingleIq,
}

impl OrderedRelayMucProxyKind {
    /// The room lane this kind rides (#1597). Every kind maps to
    /// exactly one lane, and envelope validation rejects an envelope
    /// whose channel lane disagrees with its payload kind.
    pub fn room_lane(self) -> OrderedRelayRoomLane {
        match self {
            OrderedRelayMucProxyKind::JoinPresence
            | OrderedRelayMucProxyKind::OccupantPresence
            | OrderedRelayMucProxyKind::GroupchatMessage
            | OrderedRelayMucProxyKind::PrivateMessage
            | OrderedRelayMucProxyKind::BareRoomIq
            | OrderedRelayMucProxyKind::OccupantIq
            | OrderedRelayMucProxyKind::FanoutChunk => OrderedRelayRoomLane::MucStanza,
            OrderedRelayMucProxyKind::MujiJingleIq => OrderedRelayRoomLane::MujiSignaling,
        }
    }

    fn matches_stanza(self, stanza: &waddle_xmpp::Stanza) -> bool {
        matches!(
            (self, stanza),
            (
                OrderedRelayMucProxyKind::JoinPresence | OrderedRelayMucProxyKind::OccupantPresence,
                waddle_xmpp::Stanza::Presence(_)
            ) | (
                OrderedRelayMucProxyKind::GroupchatMessage
                    | OrderedRelayMucProxyKind::PrivateMessage
                    | OrderedRelayMucProxyKind::FanoutChunk,
                waddle_xmpp::Stanza::Message(_)
            ) | (
                OrderedRelayMucProxyKind::BareRoomIq
                    | OrderedRelayMucProxyKind::OccupantIq
                    | OrderedRelayMucProxyKind::MujiJingleIq,
                waddle_xmpp::Stanza::Iq(_)
            )
        )
    }

    fn requires_connection_origin(self) -> bool {
        matches!(
            self,
            Self::JoinPresence | Self::OccupantPresence | Self::MujiJingleIq
        )
    }
}

/// Where a relayed MUC proxy stanza was produced.
///
/// Current production emitters divide cleanly by kind:
/// - `Connection(...)`: `JoinPresence` from the websocket join path,
///   `OccupantPresence` from connection presence updates/leaves and their
///   generation-fenced cleanup replays, and `MujiJingleIq` from relayed Muji
///   Jingle IQs authenticated as one occupant connection.
/// - `Server`: `GroupchatMessage` from server-side room dispatch, plus the
///   server-built MUC routing/fanout kinds `PrivateMessage`, `BareRoomIq`,
///   `OccupantIq`, and `FanoutChunk`.
///
/// Receiver-side envelope validation rejects a kind/origin mismatch before any
/// handler runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MucProxyOrigin {
    Connection(OccupancySessionGeneration),
    Server,
}

/// Typed stanza payloads planned for Phase 4 routing. The stanza remains a
/// typed `RemoteStanza`; XML text exists only inside the existing codec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrderedRelayPayload {
    Message {
        recipient: jid::Jid,
        stanza: RemoteStanza,
    },
    Iq {
        recipient: jid::Jid,
        stanza: RemoteStanza,
    },
    Presence {
        recipient: jid::Jid,
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

impl OrderedRelayPayload {
    fn stanza(&self) -> &waddle_xmpp::Stanza {
        match self {
            OrderedRelayPayload::Message { stanza, .. }
            | OrderedRelayPayload::Iq { stanza, .. }
            | OrderedRelayPayload::Presence { stanza, .. }
            | OrderedRelayPayload::MucProxy { stanza, .. } => &stanza.0,
        }
    }

    fn matches_stanza_kind(&self) -> bool {
        match (self, self.stanza()) {
            (OrderedRelayPayload::Message { .. }, waddle_xmpp::Stanza::Message(message)) => {
                message.type_ != xmpp_parsers::message::MessageType::Groupchat
            }
            (OrderedRelayPayload::Iq { .. }, waddle_xmpp::Stanza::Iq(_)) => true,
            (OrderedRelayPayload::Presence { .. }, waddle_xmpp::Stanza::Presence(_)) => true,
            (OrderedRelayPayload::MucProxy { kind, .. }, stanza) => kind.matches_stanza(stanza),
            _ => false,
        }
    }

    fn matches_channel_recipient(&self, recipient: &OrderedRelayRecipient) -> bool {
        match (self, recipient) {
            (
                OrderedRelayPayload::Message { recipient, .. }
                | OrderedRelayPayload::Iq { recipient, .. }
                | OrderedRelayPayload::Presence { recipient, .. },
                OrderedRelayRecipient::BareJid(bare),
            ) => recipient == &jid::Jid::from(bare.clone()),
            (
                OrderedRelayPayload::Message { recipient, .. }
                | OrderedRelayPayload::Iq { recipient, .. }
                | OrderedRelayPayload::Presence { recipient, .. },
                OrderedRelayRecipient::FullJid(full),
            ) => recipient == &jid::Jid::from(full.clone()),
            (
                OrderedRelayPayload::MucProxy { room_jid, kind, .. },
                OrderedRelayRecipient::Room { room, lane },
            ) => room_jid == room && kind.room_lane() == *lane,
            _ => false,
        }
    }

    fn matches_stanza_addressing(&self) -> bool {
        match self {
            OrderedRelayPayload::Message { recipient, stanza }
            | OrderedRelayPayload::Iq { recipient, stanza }
            | OrderedRelayPayload::Presence { recipient, stanza } => {
                stanza_to(&stanza.0).is_some_and(|to| to == recipient)
            }
            OrderedRelayPayload::MucProxy {
                room_jid,
                kind,
                stanza,
                ..
            } => muc_proxy_stanza_is_addressed_to_room(room_jid, *kind, &stanza.0),
        }
    }

    fn matches_target_claim(&self, claim: &OrderedRelayClaim) -> bool {
        match self {
            OrderedRelayPayload::Message { recipient, .. }
            | OrderedRelayPayload::Iq { recipient, .. }
            | OrderedRelayPayload::Presence { recipient, .. } => {
                claim.entity.entity_type == EntityType::UserActor
                    && claim.entity.id == recipient.to_bare().to_string()
            }
            OrderedRelayPayload::MucProxy { room_jid, .. } => {
                claim.entity.entity_type == EntityType::RoomActor
                    && claim.entity.id == room_jid.to_string()
            }
        }
    }

    fn matches_sender_claim(&self, claim: &OrderedRelayClaim) -> bool {
        let Some(from) = stanza_from(self.stanza()) else {
            return false;
        };
        matches!(
            claim.entity.entity_type,
            EntityType::UserActor | EntityType::RoomActor
        ) && claim.entity.id == from.to_bare().to_string()
    }

    fn fingerprint(&self) -> OrderedRelayPayloadFingerprint {
        match self {
            OrderedRelayPayload::Message { recipient, stanza } => {
                OrderedRelayPayloadFingerprint::Message {
                    recipient: recipient.clone(),
                    stanza: stanza.0.to_element(),
                }
            }
            OrderedRelayPayload::Iq { recipient, stanza } => OrderedRelayPayloadFingerprint::Iq {
                recipient: recipient.clone(),
                stanza: stanza.0.to_element(),
            },
            OrderedRelayPayload::Presence { recipient, stanza } => {
                OrderedRelayPayloadFingerprint::Presence {
                    recipient: recipient.clone(),
                    stanza: stanza.0.to_element(),
                }
            }
            OrderedRelayPayload::MucProxy {
                canonical,
                principal,
                stanza_lang,
                room_jid,
                kind,
                origin,
                stanza,
            } => OrderedRelayPayloadFingerprint::MucProxy {
                canonical: canonical.clone(),
                principal: principal.clone(),
                stanza_lang: stanza_lang.clone(),
                room_jid: room_jid.clone(),
                kind: *kind,
                origin: *origin,
                stanza: stanza.0.to_element(),
            },
        }
    }
}

/// A sequenced relay unit sent over the remote actor boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteStanzaEnvelope {
    /// Sender-asserted node provenance. Production routing must validate this
    /// against the authenticated relay transport/registry origin before any
    /// delivery effect; this field is not proof by itself and is deliberately
    /// not part of the ordering key.
    pub asserted_origin_node: NodeId,
    pub channel: OrderedRelayChannel,
    /// Per-channel sender sequence for the origin-stream/recipient pair.
    pub sequence: OrderedRelaySequence,
    pub origin_inbound_sequence: OriginInboundSequence,
    pub origin_claim: OrderedRelayClaim,
    /// Fresh claim for the XMPP entity named by the carried stanza's `from`.
    ///
    /// SM-origin channels keep the stream id as the ordered lane, but receivers
    /// still validate this sender claim before applying any user/room-visible
    /// effect so a node cannot relay a stanza from an unrelated JID.
    pub sender_claim: OrderedRelayClaim,
    pub target_claim: OrderedRelayClaim,
    pub payload: OrderedRelayPayload,
    /// Optional at the substrate level so ordering tests can construct raw
    /// envelopes, but production delivery rejects unsigned envelopes before
    /// applying any local effect.
    pub origin_proof: Option<OrderedRelayOriginProof>,
}

impl RemoteStanzaEnvelope {
    pub fn signing_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&RemoteStanzaEnvelopeSigningView {
            asserted_origin_node: &self.asserted_origin_node,
            channel: &self.channel,
            sequence: self.sequence,
            origin_inbound_sequence: self.origin_inbound_sequence,
            origin_claim: &self.origin_claim,
            sender_claim: &self.sender_claim,
            target_claim: &self.target_claim,
            payload: &self.payload,
        })
    }
}

#[derive(Serialize)]
struct RemoteStanzaEnvelopeSigningView<'a> {
    asserted_origin_node: &'a NodeId,
    channel: &'a OrderedRelayChannel,
    sequence: OrderedRelaySequence,
    origin_inbound_sequence: OriginInboundSequence,
    origin_claim: &'a OrderedRelayClaim,
    sender_claim: &'a OrderedRelayClaim,
    target_claim: &'a OrderedRelayClaim,
    payload: &'a OrderedRelayPayload,
}

/// Internal ACK for an ordered relay envelope. This is not an XEP-0184 receipt
/// and must never be serialized back to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderedRelayAck {
    pub channel: OrderedRelayChannel,
    pub sequence: OrderedRelaySequence,
    pub duplicate: bool,
    pub next_expected: OrderedRelaySequence,
    /// Typed stanzas the origin node must write back to the originating
    /// client after a proxied server-side operation completes. Empty for
    /// ordinary 1:1 delivery.
    pub client_replies: Vec<RemoteStanza>,
}

/// Which claim in an ordered-relay envelope failed ownership validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderedRelayClaimRole {
    Origin,
    Sender,
    Target,
}

/// Internal NACK reason. These are relay-control outcomes only; they do not
/// synthesize client stanzas or mutate XEP-0198 counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderedRelayNackReason {
    Gap {
        expected: OrderedRelaySequence,
    },
    InFlight,
    NotOwner {
        role: OrderedRelayClaimRole,
    },
    Unreachable,
    TargetUnavailable,
    ParseFailure,
    /// #1597: the peer does not know this envelope's versioned remote
    /// message id (kameo `UnknownMessage`) — provably no handler ran.
    /// Synthesized sender-side; the sender rolls back the unconsumed
    /// sequence and keeps the channel instead of diverting, so a
    /// mixed-version window degrades to per-operation failures rather
    /// than a poisoned channel.
    UnsupportedEnvelope,
    Backpressure,
    MaybeCommitted,
    Diverted(OrderedRelayDiversion),
}

impl OrderedRelayNackReason {
    pub(crate) fn metric_label(&self) -> &'static str {
        match self {
            OrderedRelayNackReason::Gap { .. } => "gap",
            OrderedRelayNackReason::InFlight => "in_flight",
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Origin,
            } => "not_owner_origin",
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Sender,
            } => "not_owner_sender",
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Target,
            } => "not_owner_target",
            OrderedRelayNackReason::Unreachable => "unreachable",
            OrderedRelayNackReason::TargetUnavailable => "target_unavailable",
            OrderedRelayNackReason::ParseFailure => "parse_failure",
            OrderedRelayNackReason::UnsupportedEnvelope => "unsupported_envelope",
            OrderedRelayNackReason::Backpressure => "backpressure",
            OrderedRelayNackReason::MaybeCommitted => "maybe_committed",
            OrderedRelayNackReason::Diverted(_) => "diverted",
        }
    }
}

/// Internal NACK for an ordered relay envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderedRelayNack {
    pub channel: OrderedRelayChannel,
    pub sequence: OrderedRelaySequence,
    pub reason: OrderedRelayNackReason,
}

/// Reply to one ordered relay ask.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, kameo::Reply)]
pub enum OrderedRelayReply {
    Ack(OrderedRelayAck),
    Nack(OrderedRelayNack),
}

/// Receiver-side reservation for an envelope whose ordering and payload shape
/// are valid, but whose ACK has not yet been committed. Later routing slices
/// place the local delivery/durable effect between reservation and commit.
#[derive(Debug, Clone)]
pub struct OrderedRelayReservedEnvelope {
    envelope: RemoteStanzaEnvelope,
    next_expected: OrderedRelaySequence,
}

impl OrderedRelayReservedEnvelope {
    pub fn envelope(&self) -> &RemoteStanzaEnvelope {
        &self.envelope
    }
}

/// Result of receiver-side reservation. `Completed` covers duplicate ACK
/// replay and immediate NACKs where no local delivery side effect may run.
#[derive(Debug, Clone)]
pub enum OrderedRelayReservation {
    Reserved(Box<OrderedRelayReservedEnvelope>),
    Completed(OrderedRelayReply),
}

/// Reason a channel was diverted away from immediate relay delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderedRelayDiversionReason {
    OrderingGap,
    NotOwner,
    Unreachable,
    Backpressure,
    MaybeCommitted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OrderedRelayRecentAck {
    sequence: OrderedRelaySequence,
    fingerprint: OrderedRelayEnvelopeFingerprint,
    client_replies: Vec<RemoteStanza>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OrderedRelayEnvelopeFingerprint {
    origin_inbound_sequence: OriginInboundSequence,
    payload: OrderedRelayPayloadFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OrderedRelayPendingReservation {
    sequence: OrderedRelaySequence,
    fingerprint: OrderedRelayEnvelopeFingerprint,
}

impl From<&RemoteStanzaEnvelope> for OrderedRelayEnvelopeFingerprint {
    fn from(envelope: &RemoteStanzaEnvelope) -> Self {
        Self {
            origin_inbound_sequence: envelope.origin_inbound_sequence,
            payload: envelope.payload.fingerprint(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OrderedRelayPayloadFingerprint {
    Message {
        recipient: jid::Jid,
        stanza: minidom::Element,
    },
    Iq {
        recipient: jid::Jid,
        stanza: minidom::Element,
    },
    Presence {
        recipient: jid::Jid,
        stanza: minidom::Element,
    },
    MucProxy {
        canonical: Option<crate::ingress::IngressCanonicalRef>,
        principal: Option<waddle_xmpp::auth::AuthenticatedPrincipalRef>,
        stanza_lang: Option<xmpp_parsers::message::Lang>,
        room_jid: jid::BareJid,
        kind: OrderedRelayMucProxyKind,
        origin: MucProxyOrigin,
        stanza: minidom::Element,
    },
}

/// Sticky durable-fallback marker, scoped to the same origin-stream/recipient
/// channel as ordered delivery. The actual durable write lands in later slices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderedRelayDiversion {
    pub channel: OrderedRelayChannel,
    pub reason: OrderedRelayDiversionReason,
}

/// Sender-side sequence allocator and sticky-diversion tracker.
#[derive(Debug, Default)]
pub struct OrderedRelaySenderState {
    next_by_channel: HashMap<OrderedRelayChannel, OrderedRelaySequence>,
    diversions: HashMap<OrderedRelayChannel, OrderedRelayDiversion>,
    new_channels_diverted: bool,
}

impl OrderedRelaySenderState {
    pub fn next_envelope(
        &mut self,
        asserted_origin_node: NodeId,
        channel: OrderedRelayChannel,
        origin_inbound_sequence: OriginInboundSequence,
        claims: OrderedRelayEnvelopeClaims,
        payload: OrderedRelayPayload,
    ) -> Result<RemoteStanzaEnvelope, OrderedRelayDiversion> {
        if let Some(diversion) = self.diversions.get(&channel) {
            return Err(diversion.clone());
        }
        if self.new_channels_diverted && !self.next_by_channel.contains_key(&channel) {
            return Err(OrderedRelayDiversion {
                channel,
                reason: OrderedRelayDiversionReason::Backpressure,
            });
        }
        if !self.next_by_channel.contains_key(&channel)
            && self.next_by_channel.len() >= MAX_TRACKED_ORDERED_RELAY_CHANNELS
        {
            self.new_channels_diverted = true;
            let diversion = OrderedRelayDiversion {
                channel,
                reason: OrderedRelayDiversionReason::Backpressure,
            };
            return Err(diversion);
        }

        let sequence = self
            .next_by_channel
            .get(&channel)
            .copied()
            .unwrap_or(OrderedRelaySequence::FIRST);
        let Some(next_sequence) = sequence.checked_next() else {
            let diversion = OrderedRelayDiversion {
                channel: channel.clone(),
                reason: OrderedRelayDiversionReason::Backpressure,
            };
            self.divert(diversion.clone());
            return Err(diversion);
        };
        let envelope = RemoteStanzaEnvelope {
            asserted_origin_node,
            channel: channel.clone(),
            sequence,
            origin_inbound_sequence,
            origin_claim: claims.origin,
            sender_claim: claims.sender,
            target_claim: claims.target,
            payload,
            origin_proof: None,
        };
        self.next_by_channel.insert(channel, next_sequence);
        Ok(envelope)
    }

    pub fn divert(&mut self, diversion: OrderedRelayDiversion) {
        if !self.diversions.contains_key(&diversion.channel)
            && self.diversions.len() >= MAX_TRACKED_ORDERED_RELAY_CHANNELS
        {
            self.next_by_channel.remove(&diversion.channel);
            self.new_channels_diverted = true;
            return;
        }
        self.diversions.insert(diversion.channel.clone(), diversion);
    }

    pub fn forget_channel(&mut self, channel: &OrderedRelayChannel) {
        self.next_by_channel.remove(channel);
        self.diversions.remove(channel);
    }

    pub fn rollback_unseen_envelope(&mut self, envelope: &RemoteStanzaEnvelope) {
        if self.diversions.contains_key(&envelope.channel) {
            return;
        }
        let Some(expected_after_envelope) = envelope.sequence.checked_next() else {
            return;
        };
        match self.next_by_channel.get_mut(&envelope.channel) {
            Some(next) if *next == expected_after_envelope => {
                *next = envelope.sequence;
            }
            None if envelope.sequence == OrderedRelaySequence::FIRST => {
                self.next_by_channel.remove(&envelope.channel);
            }
            _ => {}
        }
    }
}

/// Receiver-side expected-sequence tracker.
#[derive(Debug, Default)]
pub struct OrderedRelayReceiverState {
    next_expected_by_channel: HashMap<OrderedRelayChannel, OrderedRelaySequence>,
    pending_by_channel: HashMap<OrderedRelayChannel, OrderedRelayPendingReservation>,
    recent_acked_by_channel: HashMap<OrderedRelayChannel, VecDeque<OrderedRelayRecentAck>>,
    diversions: HashMap<OrderedRelayChannel, OrderedRelayDiversion>,
    new_channels_diverted: bool,
}

impl OrderedRelayReceiverState {
    pub fn reserve(&mut self, envelope: RemoteStanzaEnvelope) -> OrderedRelayReservation {
        if let Some(diversion) = self.diversions.get(&envelope.channel) {
            return OrderedRelayReservation::Completed(OrderedRelayReply::Nack(OrderedRelayNack {
                channel: envelope.channel,
                sequence: envelope.sequence,
                reason: OrderedRelayNackReason::Diverted(diversion.clone()),
            }));
        }
        if self.new_channels_diverted
            && !self
                .next_expected_by_channel
                .contains_key(&envelope.channel)
            && !self.pending_by_channel.contains_key(&envelope.channel)
        {
            return OrderedRelayReservation::Completed(OrderedRelayReply::Nack(OrderedRelayNack {
                channel: envelope.channel,
                sequence: envelope.sequence,
                reason: OrderedRelayNackReason::Backpressure,
            }));
        }

        if !self
            .next_expected_by_channel
            .contains_key(&envelope.channel)
            && !self.pending_by_channel.contains_key(&envelope.channel)
            && self.tracked_channel_count() >= MAX_TRACKED_ORDERED_RELAY_CHANNELS
        {
            return OrderedRelayReservation::Completed(OrderedRelayReply::Nack(OrderedRelayNack {
                channel: envelope.channel,
                sequence: envelope.sequence,
                reason: OrderedRelayNackReason::Backpressure,
            }));
        }

        if !envelope_is_consistent(&envelope) {
            return OrderedRelayReservation::Completed(OrderedRelayReply::Nack(OrderedRelayNack {
                channel: envelope.channel,
                sequence: envelope.sequence,
                reason: OrderedRelayNackReason::ParseFailure,
            }));
        }

        if let Some(pending) = self.pending_by_channel.get(&envelope.channel) {
            if envelope.sequence == pending.sequence {
                let reason =
                    if OrderedRelayEnvelopeFingerprint::from(&envelope) == pending.fingerprint {
                        OrderedRelayNackReason::InFlight
                    } else {
                        OrderedRelayNackReason::ParseFailure
                    };
                return OrderedRelayReservation::Completed(OrderedRelayReply::Nack(
                    OrderedRelayNack {
                        channel: envelope.channel,
                        sequence: envelope.sequence,
                        reason,
                    },
                ));
            }
            return OrderedRelayReservation::Completed(OrderedRelayReply::Nack(OrderedRelayNack {
                channel: envelope.channel,
                sequence: envelope.sequence,
                reason: OrderedRelayNackReason::Gap {
                    expected: pending.sequence,
                },
            }));
        }

        let expected = self
            .next_expected_by_channel
            .get(&envelope.channel)
            .copied()
            .unwrap_or(OrderedRelaySequence::FIRST);

        if envelope.sequence.0 < expected.0 {
            let recently_acked = self
                .recent_acked_by_channel
                .get(&envelope.channel)
                .and_then(|recent| recent.iter().find(|ack| ack.sequence == envelope.sequence));
            if let Some(recently_acked) = recently_acked {
                if recently_acked.fingerprint == OrderedRelayEnvelopeFingerprint::from(&envelope) {
                    return OrderedRelayReservation::Completed(OrderedRelayReply::Ack(
                        OrderedRelayAck {
                            channel: envelope.channel,
                            sequence: envelope.sequence,
                            duplicate: true,
                            next_expected: expected,
                            client_replies: recently_acked.client_replies.clone(),
                        },
                    ));
                }
                return OrderedRelayReservation::Completed(OrderedRelayReply::Nack(
                    OrderedRelayNack {
                        channel: envelope.channel,
                        sequence: envelope.sequence,
                        reason: OrderedRelayNackReason::ParseFailure,
                    },
                ));
            }
            return OrderedRelayReservation::Completed(OrderedRelayReply::Nack(OrderedRelayNack {
                channel: envelope.channel,
                sequence: envelope.sequence,
                reason: OrderedRelayNackReason::Gap { expected },
            }));
        }

        if envelope.sequence != expected {
            return OrderedRelayReservation::Completed(OrderedRelayReply::Nack(OrderedRelayNack {
                channel: envelope.channel,
                sequence: envelope.sequence,
                reason: OrderedRelayNackReason::Gap { expected },
            }));
        }

        let Some(next_expected) = expected.checked_next() else {
            return OrderedRelayReservation::Completed(OrderedRelayReply::Nack(OrderedRelayNack {
                channel: envelope.channel,
                sequence: envelope.sequence,
                reason: OrderedRelayNackReason::Backpressure,
            }));
        };

        self.pending_by_channel.insert(
            envelope.channel.clone(),
            OrderedRelayPendingReservation {
                sequence: envelope.sequence,
                fingerprint: OrderedRelayEnvelopeFingerprint::from(&envelope),
            },
        );
        OrderedRelayReservation::Reserved(Box::new(OrderedRelayReservedEnvelope {
            envelope,
            next_expected,
        }))
    }

    pub fn commit_reserved(&mut self, reserved: OrderedRelayReservedEnvelope) -> OrderedRelayReply {
        self.commit_reserved_with_replies(reserved, Vec::new())
    }

    pub fn commit_reserved_with_replies(
        &mut self,
        reserved: OrderedRelayReservedEnvelope,
        client_replies: Vec<RemoteStanza>,
    ) -> OrderedRelayReply {
        let envelope = reserved.envelope;
        self.pending_by_channel.remove(&envelope.channel);
        if let Some(diversion) = self.diversions.get(&envelope.channel) {
            return OrderedRelayReply::Nack(OrderedRelayNack {
                channel: envelope.channel,
                sequence: envelope.sequence,
                reason: OrderedRelayNackReason::Diverted(diversion.clone()),
            });
        }

        if !self
            .next_expected_by_channel
            .contains_key(&envelope.channel)
            && self.tracked_channel_count() >= MAX_TRACKED_ORDERED_RELAY_CHANNELS
        {
            return OrderedRelayReply::Nack(OrderedRelayNack {
                channel: envelope.channel,
                sequence: envelope.sequence,
                reason: OrderedRelayNackReason::Backpressure,
            });
        }

        let expected = self
            .next_expected_by_channel
            .get(&envelope.channel)
            .copied()
            .unwrap_or(OrderedRelaySequence::FIRST);
        if envelope.sequence != expected {
            return OrderedRelayReply::Nack(OrderedRelayNack {
                channel: envelope.channel,
                sequence: envelope.sequence,
                reason: OrderedRelayNackReason::Gap { expected },
            });
        }

        self.next_expected_by_channel
            .insert(envelope.channel.clone(), reserved.next_expected);
        record_ack(
            &mut self.recent_acked_by_channel,
            &envelope.channel,
            &envelope,
            &client_replies,
        );
        OrderedRelayReply::Ack(OrderedRelayAck {
            channel: envelope.channel,
            sequence: envelope.sequence,
            duplicate: false,
            next_expected: reserved.next_expected,
            client_replies,
        })
    }

    pub fn abort_reserved(
        &mut self,
        reserved: OrderedRelayReservedEnvelope,
        reason: OrderedRelayNackReason,
    ) -> OrderedRelayReply {
        let envelope = reserved.envelope;
        self.pending_by_channel.remove(&envelope.channel);
        self.divert(OrderedRelayDiversion {
            channel: envelope.channel.clone(),
            reason: receiver_diversion_reason_for_nack(&reason),
        });
        OrderedRelayReply::Nack(OrderedRelayNack {
            channel: envelope.channel,
            sequence: envelope.sequence,
            reason,
        })
    }

    pub fn abort_reserved_without_diversion(
        &mut self,
        reserved: OrderedRelayReservedEnvelope,
        reason: OrderedRelayNackReason,
    ) -> OrderedRelayReply {
        let envelope = reserved.envelope;
        self.pending_by_channel.remove(&envelope.channel);
        OrderedRelayReply::Nack(OrderedRelayNack {
            channel: envelope.channel,
            sequence: envelope.sequence,
            reason,
        })
    }

    pub fn divert(&mut self, diversion: OrderedRelayDiversion) {
        if !self.diversions.contains_key(&diversion.channel)
            && self.diversions.len() >= MAX_TRACKED_ORDERED_RELAY_CHANNELS
        {
            self.next_expected_by_channel.remove(&diversion.channel);
            self.pending_by_channel.remove(&diversion.channel);
            self.recent_acked_by_channel.remove(&diversion.channel);
            self.new_channels_diverted = true;
            return;
        }
        self.diversions.insert(diversion.channel.clone(), diversion);
    }

    pub fn forget_channel(&mut self, channel: &OrderedRelayChannel) {
        self.next_expected_by_channel.remove(channel);
        self.pending_by_channel.remove(channel);
        self.recent_acked_by_channel.remove(channel);
        self.diversions.remove(channel);
    }

    fn tracked_channel_count(&self) -> usize {
        self.next_expected_by_channel.len() + self.pending_by_channel.len()
    }
}

fn receiver_diversion_reason_for_nack(
    reason: &OrderedRelayNackReason,
) -> OrderedRelayDiversionReason {
    match reason {
        OrderedRelayNackReason::Gap { .. }
        | OrderedRelayNackReason::ParseFailure
        // Sender-synthesized only; if it ever reaches the receiver
        // abort path, treat it like any other parse-shaped failure.
        | OrderedRelayNackReason::UnsupportedEnvelope
        | OrderedRelayNackReason::Diverted(_) => OrderedRelayDiversionReason::OrderingGap,
        OrderedRelayNackReason::InFlight | OrderedRelayNackReason::Backpressure => {
            OrderedRelayDiversionReason::Backpressure
        }
        OrderedRelayNackReason::MaybeCommitted => OrderedRelayDiversionReason::MaybeCommitted,
        OrderedRelayNackReason::NotOwner { .. } => OrderedRelayDiversionReason::NotOwner,
        OrderedRelayNackReason::Unreachable | OrderedRelayNackReason::TargetUnavailable => {
            OrderedRelayDiversionReason::Unreachable
        }
    }
}

fn record_ack(
    recent_acked_by_channel: &mut HashMap<OrderedRelayChannel, VecDeque<OrderedRelayRecentAck>>,
    channel: &OrderedRelayChannel,
    envelope: &RemoteStanzaEnvelope,
    client_replies: &[RemoteStanza],
) {
    let recent = recent_acked_by_channel.entry(channel.clone()).or_default();
    recent.push_back(OrderedRelayRecentAck {
        sequence: envelope.sequence,
        fingerprint: OrderedRelayEnvelopeFingerprint::from(envelope),
        client_replies: client_replies.to_vec(),
    });
    while recent.len() > RECENT_ACK_CACHE_PER_CHANNEL {
        recent.pop_front();
    }
}

fn envelope_is_consistent(envelope: &RemoteStanzaEnvelope) -> bool {
    envelope.payload.matches_stanza_kind()
        && envelope.payload.matches_muc_proxy_origin()
        && origin_claim_matches_channel(&envelope.origin_claim, &envelope.channel.origin)
        && sender_claim_matches_channel(&envelope.sender_claim, &envelope.channel.origin)
        && envelope.target_claim.epoch == envelope.channel.target_epoch
        && envelope
            .payload
            .matches_channel_recipient(&envelope.channel.recipient)
        && envelope.payload.matches_stanza_addressing()
        && envelope
            .payload
            .matches_sender_claim(&envelope.sender_claim)
        && envelope
            .payload
            .matches_target_claim(&envelope.target_claim)
}

impl OrderedRelayPayload {
    fn matches_muc_proxy_origin(&self) -> bool {
        match self {
            OrderedRelayPayload::MucProxy { kind, origin, .. } => {
                kind.requires_connection_origin() == matches!(origin, MucProxyOrigin::Connection(_))
            }
            OrderedRelayPayload::Message { .. }
            | OrderedRelayPayload::Iq { .. }
            | OrderedRelayPayload::Presence { .. } => true,
        }
    }
}

fn origin_claim_matches_channel(claim: &OrderedRelayClaim, origin: &OrderedRelayOrigin) -> bool {
    match origin {
        OrderedRelayOrigin::SmSession(stream_id) => {
            claim.entity.entity_type == EntityType::SmSession
                && claim.entity.id == stream_id.as_str()
        }
        OrderedRelayOrigin::Entity(entity) => {
            entity.entity_type != EntityType::SmSession && claim.entity == *entity
        }
    }
}

fn sender_claim_matches_channel(claim: &OrderedRelayClaim, origin: &OrderedRelayOrigin) -> bool {
    match origin {
        OrderedRelayOrigin::SmSession(_) => claim.entity.entity_type == EntityType::UserActor,
        OrderedRelayOrigin::Entity(entity) => claim.entity == *entity,
    }
}

fn stanza_to(stanza: &waddle_xmpp::Stanza) -> Option<&jid::Jid> {
    match stanza {
        waddle_xmpp::Stanza::Message(message) => message.to.as_ref(),
        waddle_xmpp::Stanza::Presence(presence) => presence.to.as_ref(),
        waddle_xmpp::Stanza::Iq(iq) => match iq.as_ref() {
            xmpp_parsers::iq::Iq::Get { to, .. }
            | xmpp_parsers::iq::Iq::Set { to, .. }
            | xmpp_parsers::iq::Iq::Result { to, .. }
            | xmpp_parsers::iq::Iq::Error { to, .. } => to.as_ref(),
        },
    }
}

fn stanza_from(stanza: &waddle_xmpp::Stanza) -> Option<&jid::Jid> {
    match stanza {
        waddle_xmpp::Stanza::Message(message) => message.from.as_ref(),
        waddle_xmpp::Stanza::Presence(presence) => presence.from.as_ref(),
        waddle_xmpp::Stanza::Iq(iq) => match iq.as_ref() {
            xmpp_parsers::iq::Iq::Get { from, .. }
            | xmpp_parsers::iq::Iq::Set { from, .. }
            | xmpp_parsers::iq::Iq::Result { from, .. }
            | xmpp_parsers::iq::Iq::Error { from, .. } => from.as_ref(),
        },
    }
}

fn muc_proxy_stanza_is_addressed_to_room(
    room_jid: &jid::BareJid,
    kind: OrderedRelayMucProxyKind,
    stanza: &waddle_xmpp::Stanza,
) -> bool {
    let Some(to) = stanza_to(stanza) else {
        return false;
    };
    // The Muji Jingle IQ (#1445) is the one MUC-proxy shape not
    // addressed to the room: `to` is the calls mixer and the room
    // binding lives in the `<muji room='…'/>` payload instead.
    if let (OrderedRelayMucProxyKind::MujiJingleIq, waddle_xmpp::Stanza::Iq(iq)) = (kind, stanza) {
        return jid_is_bare(to) && muji_iq_targets_room(room_jid, iq);
    }
    if to.to_bare() != *room_jid {
        return false;
    }
    match (kind, stanza) {
        (OrderedRelayMucProxyKind::JoinPresence, waddle_xmpp::Stanza::Presence(presence)) => {
            jid_is_full(to) && presence.type_ == xmpp_parsers::presence::Type::None
        }
        (OrderedRelayMucProxyKind::OccupantPresence, waddle_xmpp::Stanza::Presence(presence)) => {
            jid_is_full(to)
                && matches!(
                    presence.type_,
                    xmpp_parsers::presence::Type::None | xmpp_parsers::presence::Type::Unavailable
                )
        }
        (
            OrderedRelayMucProxyKind::GroupchatMessage | OrderedRelayMucProxyKind::FanoutChunk,
            waddle_xmpp::Stanza::Message(message),
        ) => jid_is_bare(to) && message.type_ == xmpp_parsers::message::MessageType::Groupchat,
        (OrderedRelayMucProxyKind::PrivateMessage, waddle_xmpp::Stanza::Message(message)) => {
            jid_is_full(to)
                && (message.type_ == xmpp_parsers::message::MessageType::Chat
                    || message.type_ == xmpp_parsers::message::MessageType::Normal)
        }
        (OrderedRelayMucProxyKind::BareRoomIq, waddle_xmpp::Stanza::Iq(_)) => jid_is_bare(to),
        (OrderedRelayMucProxyKind::OccupantIq, waddle_xmpp::Stanza::Iq(_)) => jid_is_full(to),
        _ => false,
    }
}

/// #1445: a relayed Muji `session-initiate` is the one MUC-proxy shape
/// not addressed to the room — `to` is the calls mixer and the room
/// JID lives in the `<muji room='…'/>` payload. Bind that payload room
/// to the channel's room so an envelope on room A's ordered channel
/// (fenced by room A's claim epoch) cannot smuggle a token mint for
/// room B past the fence. Both `session-initiate` and
/// `session-terminate` ride this kind — the initiate registers the
/// participant on the owner, so the terminate must reach that same
/// node to unregister them.
fn muji_iq_targets_room(room_jid: &jid::BareJid, iq: &xmpp_parsers::iq::Iq) -> bool {
    let xmpp_parsers::iq::Iq::Set { payload, .. } = iq else {
        return false;
    };
    if payload.ns() != waddle_xmpp::xep::xep0166::NS_JINGLE || payload.name() != "jingle" {
        return false;
    }
    // Only the two actions with node-local side effects ride this
    // kind: `session-initiate` mints and registers on the owner, and
    // `session-terminate` must unregister on that same owner.
    if !matches!(
        payload.attr("action"),
        Some("session-initiate") | Some("session-terminate")
    ) {
        return false;
    }
    let Some(muji_elem) = waddle_xmpp::xep::xep0272::find_muji(payload) else {
        return false;
    };
    let Ok(muji) = waddle_xmpp::xep::xep0272::Muji::try_from(muji_elem) else {
        return false;
    };
    muji.room.as_ref() == Some(room_jid)
}

fn jid_is_full(jid: &jid::Jid) -> bool {
    jid.resource().is_some()
}

fn jid_is_bare(jid: &jid::Jid) -> bool {
    jid.resource().is_none()
}

#[cfg(test)]
mod invariant_tests;

// CI's broad Nix test job compiles the whole waddle-server lib-test crate in
// release+LTO mode. Keep the larger public behavior matrix out of that monolith;
// tests/clustering_ordered_relay.rs covers it through the public API, while the
// small private invariant probes above remain in the release lib-test target.
#[cfg(all(test, debug_assertions))]
mod tests;
