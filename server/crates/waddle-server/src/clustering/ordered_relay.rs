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

/// Typed recipient key for ordered-relay channels.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderedRelayRecipient {
    BareJid(jid::BareJid),
    FullJid(jid::FullJid),
    Room(jid::BareJid),
    /// Side-band lane for room-bound payloads that are not ordinary
    /// MUC stanza traffic (#1597). The recipient participates in the
    /// channel key, so side-band kinds get their own sequence space
    /// and their own diversion: a poisoned side-band lane (e.g. a
    /// peer that cannot decode a newly added payload variant during
    /// version skew) cannot drop the same sender's join/leave/
    /// groupchat traffic for the room.
    RoomSideBand(jid::BareJid),
}

impl OrderedRelayRecipient {
    /// The one lane-selection point for MUC-proxy channels: every
    /// channel carrying an `OrderedRelayPayload::MucProxy` must derive
    /// its recipient here so sender and receiver agree on the lane.
    pub fn for_muc_proxy(room_jid: jid::BareJid, kind: OrderedRelayMucProxyKind) -> Self {
        if kind.is_side_band() {
            Self::RoomSideBand(room_jid)
        } else {
            Self::Room(room_jid)
        }
    }
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
    /// to the channel's room. Rides the `RoomSideBand` lane (#1597),
    /// so a diversion on it cannot stall the sender's ordinary MUC
    /// traffic for the room.
    MujiJingleIq,
}

impl OrderedRelayMucProxyKind {
    /// Kinds that ride the `RoomSideBand` lane instead of the shared
    /// room lane (#1597). Any future kind whose payload an old binary
    /// may not decode belongs here, so a mixed-version window bounds
    /// the resulting diversion to the side-band lane (all side-band
    /// kinds share it — such a window can stall existing side-band
    /// traffic like Muji call signaling, but never room chat).
    pub fn is_side_band(self) -> bool {
        matches!(self, OrderedRelayMucProxyKind::MujiJingleIq)
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
        room_jid: jid::BareJid,
        kind: OrderedRelayMucProxyKind,
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
            // The lane split (#1597) is enforced here: a MUC-proxy
            // payload is only consistent with the lane its kind rides,
            // so sender and receiver can never disagree about which
            // channel a kind orders on.
            (
                OrderedRelayPayload::MucProxy { room_jid, kind, .. },
                OrderedRelayRecipient::Room(channel_room),
            ) => !kind.is_side_band() && room_jid == channel_room,
            (
                OrderedRelayPayload::MucProxy { room_jid, kind, .. },
                OrderedRelayRecipient::RoomSideBand(channel_room),
            ) => kind.is_side_band() && room_jid == channel_room,
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
                room_jid,
                kind,
                stanza,
            } => OrderedRelayPayloadFingerprint::MucProxy {
                room_jid: room_jid.clone(),
                kind: *kind,
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
    Gap { expected: OrderedRelaySequence },
    InFlight,
    NotOwner { role: OrderedRelayClaimRole },
    Unreachable,
    TargetUnavailable,
    ParseFailure,
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
        room_jid: jid::BareJid,
        kind: OrderedRelayMucProxyKind,
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
mod invariant_tests {
    use super::*;
    use std::str::FromStr;
    use waddle_xmpp::ownership::EntityType;
    use xmpp_parsers::message::{Lang, Message};

    fn channel() -> OrderedRelayChannel {
        channel_for_bare("juliet@example.test")
    }

    fn channel_for_bare(bare: &str) -> OrderedRelayChannel {
        OrderedRelayChannel {
            origin: OrderedRelayOrigin::SmSession(SmSessionId::new("stream-1")),
            recipient: OrderedRelayRecipient::BareJid(
                jid::BareJid::from_str(bare).expect("bare jid"),
            ),
            target_epoch: ClaimEpoch(3),
        }
    }

    fn origin_node() -> NodeId {
        NodeId::new("origin-node".to_string())
    }

    fn inbound(sequence: u32) -> OriginInboundSequence {
        OriginInboundSequence(sequence)
    }

    fn claim(entity_type: EntityType, id: &str, epoch: i64) -> OrderedRelayClaim {
        OrderedRelayClaim {
            entity: Entity::new(entity_type, id),
            epoch: ClaimEpoch(epoch),
        }
    }

    fn origin_claim() -> OrderedRelayClaim {
        claim(EntityType::SmSession, "stream-1", 7)
    }

    fn sender_claim() -> OrderedRelayClaim {
        claim(EntityType::UserActor, "romeo@example.test", 5)
    }

    fn target_claim() -> OrderedRelayClaim {
        claim(EntityType::UserActor, "juliet@example.test", 3)
    }

    fn target_claim_for_bare(bare: &str) -> OrderedRelayClaim {
        claim(EntityType::UserActor, bare, 3)
    }

    fn claims() -> OrderedRelayEnvelopeClaims {
        OrderedRelayEnvelopeClaims::new(origin_claim(), sender_claim(), target_claim())
    }

    fn claims_for_target(target: OrderedRelayClaim) -> OrderedRelayEnvelopeClaims {
        OrderedRelayEnvelopeClaims::new(origin_claim(), sender_claim(), target)
    }

    fn message_payload(id: &str) -> OrderedRelayPayload {
        message_payload_to(id, "juliet@example.test")
    }

    fn message_payload_to(id: &str, recipient: &str) -> OrderedRelayPayload {
        let mut stanza = Message::new(Some(jid::Jid::from_str(recipient).expect("jid")));
        stanza.from = Some(jid::Jid::from_str("romeo@example.test/home").expect("jid"));
        stanza.id = Some(xmpp_parsers::message::Id(id.to_string()));
        stanza.bodies.insert(Lang::new(), format!("payload {id}"));
        OrderedRelayPayload::Message {
            recipient: jid::Jid::from_str(recipient).expect("jid"),
            stanza: RemoteStanza(waddle_xmpp::Stanza::Message(stanza)),
        }
    }

    #[test]
    fn sender_diverts_after_sequence_space_is_exhausted() {
        let mut state = OrderedRelaySenderState::default();
        let channel = channel();
        state
            .next_by_channel
            .insert(channel.clone(), OrderedRelaySequence(u64::MAX));

        let blocked = state.next_envelope(
            origin_node(),
            channel,
            inbound(u32::MAX),
            claims(),
            message_payload("after-max"),
        );
        assert!(matches!(
            blocked,
            Err(OrderedRelayDiversion {
                reason: OrderedRelayDiversionReason::Backpressure,
                ..
            })
        ));
    }

    #[test]
    fn sender_overflow_channels_do_not_grow_diversion_state_unbounded() {
        let mut state = OrderedRelaySenderState::default();
        for index in 0..MAX_TRACKED_ORDERED_RELAY_CHANNELS {
            state.next_by_channel.insert(
                channel_for_bare(&format!("user-{index}@example.test")),
                OrderedRelaySequence::FIRST,
            );
        }

        for index in 0..16 {
            let overflow = format!("overflow-{index}@example.test");
            let result = state.next_envelope(
                origin_node(),
                channel_for_bare(&overflow),
                inbound(index),
                claims_for_target(target_claim_for_bare(&overflow)),
                message_payload_to("overflow", &overflow),
            );
            assert!(matches!(
                result,
                Err(OrderedRelayDiversion {
                    reason: OrderedRelayDiversionReason::Backpressure,
                    ..
                })
            ));
        }

        assert!(state.diversions.is_empty());
        assert!(state.new_channels_diverted);
    }
}

// CI's broad Nix test job compiles the whole waddle-server lib-test crate in
// release+LTO mode. Keep the larger public behavior matrix out of that monolith;
// tests/clustering_ordered_relay.rs covers it through the public API, while the
// small private invariant probes above remain in the release lib-test target.
#[cfg(all(test, debug_assertions))]
mod tests {
    use super::*;
    use std::str::FromStr;
    use waddle_xmpp::ownership::EntityType;
    use xmpp_parsers::message::{Lang, Message};

    fn channel() -> OrderedRelayChannel {
        channel_for_bare("juliet@example.test")
    }

    fn channel_for_bare(bare: &str) -> OrderedRelayChannel {
        OrderedRelayChannel {
            origin: OrderedRelayOrigin::SmSession(SmSessionId::new("stream-1")),
            recipient: OrderedRelayRecipient::BareJid(
                jid::BareJid::from_str(bare).expect("bare jid"),
            ),
            target_epoch: ClaimEpoch(3),
        }
    }

    fn origin_node() -> NodeId {
        NodeId::new("origin-node".to_string())
    }

    fn room_jid() -> jid::BareJid {
        jid::BareJid::from_str("room@example.test").expect("room jid")
    }

    fn room_channel() -> OrderedRelayChannel {
        OrderedRelayChannel {
            origin: OrderedRelayOrigin::SmSession(SmSessionId::new("stream-1")),
            recipient: OrderedRelayRecipient::Room(room_jid()),
            target_epoch: ClaimEpoch(11),
        }
    }

    fn inbound(sequence: u32) -> OriginInboundSequence {
        OriginInboundSequence(sequence)
    }

    fn claim(entity_type: EntityType, id: &str, epoch: i64) -> OrderedRelayClaim {
        OrderedRelayClaim {
            entity: Entity::new(entity_type, id),
            epoch: ClaimEpoch(epoch),
        }
    }

    fn origin_claim() -> OrderedRelayClaim {
        claim(EntityType::SmSession, "stream-1", 7)
    }

    fn sender_claim() -> OrderedRelayClaim {
        claim(EntityType::UserActor, "romeo@example.test", 5)
    }

    fn target_claim() -> OrderedRelayClaim {
        claim(EntityType::UserActor, "juliet@example.test", 3)
    }

    fn target_claim_for_bare(bare: &str) -> OrderedRelayClaim {
        claim(EntityType::UserActor, bare, 3)
    }

    fn room_claim() -> OrderedRelayClaim {
        claim(EntityType::RoomActor, "room@example.test", 11)
    }

    fn claims() -> OrderedRelayEnvelopeClaims {
        OrderedRelayEnvelopeClaims::new(origin_claim(), sender_claim(), target_claim())
    }

    fn claims_for_target(target: OrderedRelayClaim) -> OrderedRelayEnvelopeClaims {
        OrderedRelayEnvelopeClaims::new(origin_claim(), sender_claim(), target)
    }

    fn message_payload(id: &str) -> OrderedRelayPayload {
        message_payload_to(id, "juliet@example.test")
    }

    fn message_payload_to(id: &str, recipient: &str) -> OrderedRelayPayload {
        let mut stanza = Message::new(Some(jid::Jid::from_str(recipient).expect("jid")));
        stanza.from = Some(jid::Jid::from_str("romeo@example.test/home").expect("jid"));
        stanza.id = Some(xmpp_parsers::message::Id(id.to_string()));
        stanza.bodies.insert(Lang::new(), format!("payload {id}"));
        OrderedRelayPayload::Message {
            recipient: jid::Jid::from_str(recipient).expect("jid"),
            stanza: RemoteStanza(waddle_xmpp::Stanza::Message(stanza)),
        }
    }

    fn presence_stanza() -> RemoteStanza {
        presence_stanza_to(
            "room@example.test/romeo",
            xmpp_parsers::presence::Type::None,
        )
    }

    fn presence_stanza_to(to: &str, presence_type: xmpp_parsers::presence::Type) -> RemoteStanza {
        let mut presence = xmpp_parsers::presence::Presence::new(presence_type);
        presence.to = Some(jid::Jid::from_str(to).expect("jid"));
        presence.from = Some(jid::Jid::from_str("romeo@example.test/home").expect("jid"));
        RemoteStanza(waddle_xmpp::Stanza::Presence(presence))
    }

    fn groupchat_stanza_to(to: &str) -> RemoteStanza {
        let mut message = Message::new(Some(jid::Jid::from_str(to).expect("jid")));
        message.from = Some(jid::Jid::from_str("romeo@example.test/home").expect("jid"));
        message.type_ = xmpp_parsers::message::MessageType::Groupchat;
        RemoteStanza(waddle_xmpp::Stanza::Message(message))
    }

    fn receive(
        receiver: &mut OrderedRelayReceiverState,
        envelope: RemoteStanzaEnvelope,
    ) -> OrderedRelayReply {
        match receiver.reserve(envelope) {
            OrderedRelayReservation::Reserved(reserved) => receiver.commit_reserved(*reserved),
            OrderedRelayReservation::Completed(reply) => reply,
        }
    }

    #[test]
    fn sender_allocates_per_channel_sequences() {
        let mut state = OrderedRelaySenderState::default();
        let channel = channel();

        let first = state
            .next_envelope(
                origin_node(),
                channel.clone(),
                inbound(1),
                claims(),
                message_payload("one"),
            )
            .expect("first");
        let second = state
            .next_envelope(
                origin_node(),
                channel,
                inbound(2),
                claims(),
                message_payload("two"),
            )
            .expect("second");

        assert_eq!(first.sequence, OrderedRelaySequence(1));
        assert_eq!(second.sequence, OrderedRelaySequence(2));
    }

    #[test]
    fn sender_sequence_is_stable_across_asserted_origin_node_changes() {
        let mut state = OrderedRelaySenderState::default();
        let channel = channel();

        let first = state
            .next_envelope(
                NodeId::new("old-node".to_string()),
                channel.clone(),
                inbound(1),
                claims(),
                message_payload("one"),
            )
            .expect("first");
        let second = state
            .next_envelope(
                NodeId::new("new-node".to_string()),
                channel,
                inbound(2),
                claims(),
                message_payload("two"),
            )
            .expect("second");

        assert_eq!(first.sequence, OrderedRelaySequence(1));
        assert_eq!(second.sequence, OrderedRelaySequence(2));
    }

    #[test]
    fn sender_diverts_after_sequence_space_is_exhausted() {
        let mut state = OrderedRelaySenderState::default();
        let channel = channel();
        state
            .next_by_channel
            .insert(channel.clone(), OrderedRelaySequence(u64::MAX));

        let blocked = state.next_envelope(
            origin_node(),
            channel,
            inbound(u32::MAX),
            claims(),
            message_payload("after-max"),
        );
        assert!(matches!(
            blocked,
            Err(OrderedRelayDiversion {
                reason: OrderedRelayDiversionReason::Backpressure,
                ..
            })
        ));
    }

    #[test]
    fn sender_backpressure_diversion_stays_sticky_after_capacity_frees() {
        let mut state = OrderedRelaySenderState::default();
        for index in 0..MAX_TRACKED_ORDERED_RELAY_CHANNELS {
            state.next_by_channel.insert(
                channel_for_bare(&format!("user-{index}@example.test")),
                OrderedRelaySequence::FIRST,
            );
        }
        let overflow = channel_for_bare("overflow@example.test");
        let first_blocked = state
            .next_envelope(
                origin_node(),
                overflow.clone(),
                inbound(1),
                claims_for_target(target_claim_for_bare("overflow@example.test")),
                message_payload_to("overflow-one", "overflow@example.test"),
            )
            .expect_err("over-capacity channel diverts");

        state.forget_channel(&channel_for_bare("user-0@example.test"));
        let still_blocked = state
            .next_envelope(
                origin_node(),
                overflow,
                inbound(2),
                claims_for_target(target_claim_for_bare("overflow@example.test")),
                message_payload_to("overflow-two", "overflow@example.test"),
            )
            .expect_err("diversion remains sticky");

        assert_eq!(first_blocked, still_blocked);
    }

    #[test]
    fn sender_overflow_channels_do_not_grow_diversion_state_unbounded() {
        let mut state = OrderedRelaySenderState::default();
        for index in 0..MAX_TRACKED_ORDERED_RELAY_CHANNELS {
            state.next_by_channel.insert(
                channel_for_bare(&format!("user-{index}@example.test")),
                OrderedRelaySequence::FIRST,
            );
        }

        for index in 0..16 {
            let overflow = format!("overflow-{index}@example.test");
            let result = state.next_envelope(
                origin_node(),
                channel_for_bare(&overflow),
                inbound(index),
                claims_for_target(target_claim_for_bare(&overflow)),
                message_payload_to("overflow", &overflow),
            );
            assert!(matches!(
                result,
                Err(OrderedRelayDiversion {
                    reason: OrderedRelayDiversionReason::Backpressure,
                    ..
                })
            ));
        }

        assert!(state.diversions.is_empty());
        assert!(state.new_channels_diverted);
    }

    #[test]
    fn receiver_acks_in_order_and_duplicate_envelopes() {
        let mut receiver = OrderedRelayReceiverState::default();
        let envelope = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel: channel(),
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(1),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: target_claim(),
            payload: message_payload("one"),
            origin_proof: None,
        };

        assert!(matches!(
            receive(&mut receiver, envelope.clone()),
            OrderedRelayReply::Ack(OrderedRelayAck {
                duplicate: false,
                next_expected: OrderedRelaySequence(2),
                ..
            })
        ));
        assert!(matches!(
            receive(&mut receiver, envelope),
            OrderedRelayReply::Ack(OrderedRelayAck {
                duplicate: true,
                next_expected: OrderedRelaySequence(2),
                ..
            })
        ));
    }

    #[test]
    fn receiver_nacks_recent_duplicate_with_different_payload() {
        let mut receiver = OrderedRelayReceiverState::default();
        let envelope = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel: channel(),
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(1),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: target_claim(),
            payload: message_payload("one"),
            origin_proof: None,
        };
        assert!(matches!(
            receive(&mut receiver, envelope.clone()),
            OrderedRelayReply::Ack(OrderedRelayAck {
                duplicate: false,
                ..
            })
        ));

        let tampered = RemoteStanzaEnvelope {
            payload: message_payload("tampered"),
            ..envelope
        };
        assert!(matches!(
            receive(&mut receiver, tampered),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ));
    }

    #[test]
    fn receiver_replays_duplicate_ack_across_mutable_provenance_changes() {
        let mut receiver = OrderedRelayReceiverState::default();
        let envelope = RemoteStanzaEnvelope {
            asserted_origin_node: NodeId::new("old-node".to_string()),
            channel: channel(),
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(1),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: target_claim(),
            payload: message_payload("one"),
            origin_proof: None,
        };
        assert!(matches!(
            receive(&mut receiver, envelope.clone()),
            OrderedRelayReply::Ack(OrderedRelayAck {
                duplicate: false,
                ..
            })
        ));

        let retry_after_move = RemoteStanzaEnvelope {
            asserted_origin_node: NodeId::new("new-node".to_string()),
            origin_claim: OrderedRelayClaim {
                epoch: ClaimEpoch(99),
                ..origin_claim()
            },
            ..envelope
        };
        assert!(matches!(
            receive(&mut receiver, retry_after_move),
            OrderedRelayReply::Ack(OrderedRelayAck {
                duplicate: true,
                next_expected: OrderedRelaySequence(2),
                ..
            })
        ));
    }

    #[test]
    fn receiver_nacks_channel_payload_and_claim_mismatch() {
        let mut receiver = OrderedRelayReceiverState::default();
        let envelope = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel: channel(),
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(1),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: target_claim(),
            payload: message_payload_to("wrong-recipient", "romeo@example.test"),
            origin_proof: None,
        };

        assert!(matches!(
            receive(&mut receiver, envelope),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ));
    }

    #[test]
    fn receiver_reservation_does_not_advance_expected_until_commit() {
        let mut receiver = OrderedRelayReceiverState::default();
        let channel = channel();
        let first = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel: channel.clone(),
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(1),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: target_claim(),
            payload: message_payload("one"),
            origin_proof: None,
        };
        let second = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel,
            sequence: OrderedRelaySequence(2),
            origin_inbound_sequence: inbound(2),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: target_claim(),
            payload: message_payload("two"),
            origin_proof: None,
        };

        let reserved = match receiver.reserve(first) {
            OrderedRelayReservation::Reserved(reserved) => reserved,
            OrderedRelayReservation::Completed(reply) => {
                panic!("expected reservation before side effect, got {reply:?}");
            }
        };

        assert!(matches!(
            receiver.reserve(second),
            OrderedRelayReservation::Completed(OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::Gap {
                    expected: OrderedRelaySequence(1)
                },
                ..
            }))
        ));
        assert!(matches!(
            receiver.commit_reserved(*reserved),
            OrderedRelayReply::Ack(OrderedRelayAck {
                duplicate: false,
                next_expected: OrderedRelaySequence(2),
                ..
            })
        ));
    }

    #[test]
    fn receiver_nacks_gaps_without_advancing_expected_sequence() {
        let mut receiver = OrderedRelayReceiverState::default();
        let channel = channel();
        let gap = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel: channel.clone(),
            sequence: OrderedRelaySequence(2),
            origin_inbound_sequence: inbound(2),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: target_claim(),
            payload: message_payload("two"),
            origin_proof: None,
        };

        assert!(matches!(
            receive(&mut receiver, gap),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::Gap {
                    expected: OrderedRelaySequence(1)
                },
                ..
            })
        ));

        let first = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel,
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(1),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: target_claim(),
            payload: message_payload("one"),
            origin_proof: None,
        };
        assert!(matches!(
            receive(&mut receiver, first),
            OrderedRelayReply::Ack(OrderedRelayAck {
                duplicate: false,
                next_expected: OrderedRelaySequence(2),
                ..
            })
        ));
    }

    #[test]
    fn receiver_replays_only_recent_duplicate_acks() {
        let mut receiver = OrderedRelayReceiverState::default();
        let channel = channel();
        for sequence in 1..=RECENT_ACK_CACHE_PER_CHANNEL as u64 + 1 {
            let reply = receive(
                &mut receiver,
                RemoteStanzaEnvelope {
                    asserted_origin_node: origin_node(),
                    channel: channel.clone(),
                    sequence: OrderedRelaySequence(sequence),
                    origin_inbound_sequence: inbound(sequence as u32),
                    origin_claim: origin_claim(),
                    sender_claim: sender_claim(),
                    target_claim: target_claim(),
                    payload: message_payload(&format!("m{sequence}")),
                    origin_proof: None,
                },
            );
            assert!(matches!(reply, OrderedRelayReply::Ack(_)));
        }

        let too_old_duplicate = receive(
            &mut receiver,
            RemoteStanzaEnvelope {
                asserted_origin_node: origin_node(),
                channel,
                sequence: OrderedRelaySequence(1),
                origin_inbound_sequence: inbound(1),
                origin_claim: origin_claim(),
                sender_claim: sender_claim(),
                target_claim: target_claim(),
                payload: message_payload("too-old"),
                origin_proof: None,
            },
        );

        assert!(matches!(
            too_old_duplicate,
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::Gap {
                    expected: OrderedRelaySequence(66)
                },
                ..
            })
        ));
    }

    #[test]
    fn receiver_nacks_stanza_kind_mismatch_as_parse_failure() {
        let mut receiver = OrderedRelayReceiverState::default();
        let envelope = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel: channel(),
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(1),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: target_claim(),
            payload: OrderedRelayPayload::Message {
                recipient: jid::Jid::from_str("juliet@example.test").expect("jid"),
                stanza: presence_stanza(),
            },
            origin_proof: None,
        };

        assert!(matches!(
            receive(&mut receiver, envelope),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ));
    }

    #[test]
    fn sender_sticky_diversion_short_circuits_later_envelopes_for_channel() {
        let mut state = OrderedRelaySenderState::default();
        let channel = channel();
        let diversion = OrderedRelayDiversion {
            channel: channel.clone(),
            reason: OrderedRelayDiversionReason::Unreachable,
        };
        state.divert(diversion.clone());

        let result = state.next_envelope(
            origin_node(),
            channel,
            inbound(1),
            claims(),
            message_payload("after-diversion"),
        );

        assert_eq!(result.expect_err("diverted"), diversion);
    }

    #[test]
    fn muc_proxy_kind_validates_the_carried_stanza_kind() {
        let mut receiver = OrderedRelayReceiverState::default();
        let envelope = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel: room_channel(),
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(1),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: room_claim(),
            payload: OrderedRelayPayload::MucProxy {
                room_jid: room_jid(),
                kind: OrderedRelayMucProxyKind::JoinPresence,
                stanza: presence_stanza(),
            },
            origin_proof: None,
        };

        assert!(matches!(
            receive(&mut receiver, envelope),
            OrderedRelayReply::Ack(OrderedRelayAck {
                duplicate: false,
                next_expected: OrderedRelaySequence(2),
                ..
            })
        ));

        let wrong_kind = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel: room_channel(),
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(1),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: room_claim(),
            payload: OrderedRelayPayload::MucProxy {
                room_jid: room_jid(),
                kind: OrderedRelayMucProxyKind::GroupchatMessage,
                stanza: presence_stanza(),
            },
            origin_proof: None,
        };

        let mut fresh_receiver = OrderedRelayReceiverState::default();
        assert!(matches!(
            receive(&mut fresh_receiver, wrong_kind),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ));
    }

    /// #1445: a Muji `session-initiate` is not addressed to the room —
    /// `to` is the calls mixer and the room lives in the `<muji/>`
    /// payload — so its envelope validation must bind the payload's
    /// room to the channel's room instead of the stanza's `to`.
    fn muji_envelope(kind: OrderedRelayMucProxyKind, stanza: RemoteStanza) -> RemoteStanzaEnvelope {
        RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel: OrderedRelayChannel {
                recipient: OrderedRelayRecipient::for_muc_proxy(room_jid(), kind),
                ..room_channel()
            },
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(1),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: room_claim(),
            payload: OrderedRelayPayload::MucProxy {
                room_jid: room_jid(),
                kind,
                stanza,
            },
            origin_proof: None,
        }
    }

    fn muji_initiate_stanza(action: xmpp_parsers::jingle::Action, room: &str) -> RemoteStanza {
        use waddle_xmpp::xep::xep0167::MediaKind;
        use waddle_xmpp::xep::xep0272::{Creator, Muji, MujiContent};
        let muji = Muji {
            room: Some(room.parse().expect("valid room jid")),
            preparing: false,
            contents: vec![MujiContent::new(
                "audio",
                Creator::Initiator,
                MediaKind::Audio,
            )],
        };
        let jingle = xmpp_parsers::jingle::Jingle::new(
            action,
            xmpp_parsers::jingle::SessionId("muji-sid-1".into()),
        );
        let mut payload: xmpp_parsers::minidom::Element = jingle.into();
        payload.append_child(muji.to_element());
        RemoteStanza(waddle_xmpp::Stanza::Iq(Box::new(
            xmpp_parsers::iq::Iq::Set {
                from: Some(jid::Jid::from_str("romeo@example.test/phone").expect("full jid")),
                to: Some(jid::Jid::from_str("calls.example.test").expect("mixer jid")),
                id: "muji-iq-1".into(),
                payload,
            },
        )))
    }

    #[test]
    fn muji_jingle_iq_validation_binds_payload_room_to_channel_room() {
        let mut receiver = OrderedRelayReceiverState::default();
        assert!(
            matches!(
                receive(
                    &mut receiver,
                    muji_envelope(
                        OrderedRelayMucProxyKind::MujiJingleIq,
                        muji_initiate_stanza(
                            xmpp_parsers::jingle::Action::SessionInitiate,
                            "room@example.test"
                        ),
                    ),
                ),
                OrderedRelayReply::Ack(OrderedRelayAck { .. })
            ),
            "a session-initiate whose <muji room> matches the channel room must validate"
        );

        let mut receiver = OrderedRelayReceiverState::default();
        assert!(
            matches!(
                receive(
                    &mut receiver,
                    muji_envelope(
                        OrderedRelayMucProxyKind::MujiJingleIq,
                        muji_initiate_stanza(
                            xmpp_parsers::jingle::Action::SessionInitiate,
                            "other-room@example.test"
                        ),
                    ),
                ),
                OrderedRelayReply::Nack(OrderedRelayNack {
                    reason: OrderedRelayNackReason::ParseFailure,
                    ..
                })
            ),
            "a <muji room> naming a different room than the channel must be rejected"
        );
    }

    /// `session-terminate` rides the same kind as `session-initiate`
    /// (#1445): the initiate registers the participant on the room
    /// owner, so the terminate must reach that same node to unregister
    /// them — a locally-executed terminate would leave a phantom
    /// in-call participant there.
    #[test]
    fn muji_jingle_iq_validation_accepts_terminate_for_the_channel_room() {
        let mut receiver = OrderedRelayReceiverState::default();
        assert!(
            matches!(
                receive(
                    &mut receiver,
                    muji_envelope(
                        OrderedRelayMucProxyKind::MujiJingleIq,
                        muji_initiate_stanza(
                            xmpp_parsers::jingle::Action::SessionTerminate,
                            "room@example.test"
                        ),
                    ),
                ),
                OrderedRelayReply::Ack(OrderedRelayAck { .. })
            ),
            "a terminate for the channel's room must relay to the owner"
        );

        let mut receiver = OrderedRelayReceiverState::default();
        assert!(
            matches!(
                receive(
                    &mut receiver,
                    muji_envelope(
                        OrderedRelayMucProxyKind::MujiJingleIq,
                        muji_initiate_stanza(
                            xmpp_parsers::jingle::Action::SessionTerminate,
                            "other-room@example.test"
                        ),
                    ),
                ),
                OrderedRelayReply::Nack(OrderedRelayNack {
                    reason: OrderedRelayNackReason::ParseFailure,
                    ..
                })
            ),
            "the payload room must stay bound to the channel room on terminate"
        );
    }

    #[test]
    fn muji_jingle_iq_validation_rejects_other_actions_and_non_muji_iqs() {
        let mut receiver = OrderedRelayReceiverState::default();
        assert!(
            matches!(
                receive(
                    &mut receiver,
                    muji_envelope(
                        OrderedRelayMucProxyKind::MujiJingleIq,
                        muji_initiate_stanza(
                            xmpp_parsers::jingle::Action::SessionInfo,
                            "room@example.test"
                        ),
                    ),
                ),
                OrderedRelayReply::Nack(OrderedRelayNack {
                    reason: OrderedRelayNackReason::ParseFailure,
                    ..
                })
            ),
            "only initiate and terminate have owner-node side effects"
        );

        let plain_jingle = {
            let payload: xmpp_parsers::minidom::Element = xmpp_parsers::jingle::Jingle::new(
                xmpp_parsers::jingle::Action::SessionInitiate,
                xmpp_parsers::jingle::SessionId("p2p-sid".into()),
            )
            .into();
            RemoteStanza(waddle_xmpp::Stanza::Iq(Box::new(
                xmpp_parsers::iq::Iq::Set {
                    from: Some(jid::Jid::from_str("romeo@example.test/phone").expect("full jid")),
                    to: Some(jid::Jid::from_str("calls.example.test").expect("mixer jid")),
                    id: "p2p-iq".into(),
                    payload,
                },
            )))
        };
        let mut receiver = OrderedRelayReceiverState::default();
        assert!(
            matches!(
                receive(
                    &mut receiver,
                    muji_envelope(OrderedRelayMucProxyKind::MujiJingleIq, plain_jingle),
                ),
                OrderedRelayReply::Nack(OrderedRelayNack {
                    reason: OrderedRelayNackReason::ParseFailure,
                    ..
                })
            ),
            "a Jingle IQ without a <muji/> child (1:1 call) must never ride this kind"
        );
    }

    #[test]
    fn muc_proxy_validation_rejects_malformed_xep0045_shapes() {
        let unavailable_join = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel: room_channel(),
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(1),
            origin_claim: origin_claim(),
            sender_claim: sender_claim(),
            target_claim: room_claim(),
            payload: OrderedRelayPayload::MucProxy {
                room_jid: room_jid(),
                kind: OrderedRelayMucProxyKind::JoinPresence,
                stanza: presence_stanza_to(
                    "room@example.test/romeo",
                    xmpp_parsers::presence::Type::Unavailable,
                ),
            },
            origin_proof: None,
        };
        let bare_groupchat = RemoteStanzaEnvelope {
            payload: OrderedRelayPayload::MucProxy {
                room_jid: room_jid(),
                kind: OrderedRelayMucProxyKind::GroupchatMessage,
                stanza: groupchat_stanza_to("room@example.test"),
            },
            ..unavailable_join.clone()
        };
        let full_groupchat = RemoteStanzaEnvelope {
            payload: OrderedRelayPayload::MucProxy {
                room_jid: room_jid(),
                kind: OrderedRelayMucProxyKind::GroupchatMessage,
                stanza: groupchat_stanza_to("room@example.test/romeo"),
            },
            ..unavailable_join.clone()
        };

        let mut receiver = OrderedRelayReceiverState::default();
        assert!(matches!(
            receive(&mut receiver, unavailable_join),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ));
        assert!(matches!(
            receive(&mut receiver, bare_groupchat),
            OrderedRelayReply::Ack(OrderedRelayAck { .. })
        ));
        let mut receiver = OrderedRelayReceiverState::default();
        assert!(matches!(
            receive(&mut receiver, full_groupchat),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ));
    }
}
