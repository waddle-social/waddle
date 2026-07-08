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
}

/// One origin-stream/recipient lane. Ordering is scoped to this value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderedRelayChannel {
    pub origin_stream_id: SmSessionId,
    pub recipient: OrderedRelayRecipient,
}

/// Claim provenance carried on every ordered relay envelope. Slice 2 only
/// carries this typed proof; later routing slices validate it against
/// Postgres before applying delivery effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderedRelayClaim {
    pub entity: Entity,
    pub epoch: ClaimEpoch,
}

/// MUC proxy traffic classes for the later remote-safe MUC message set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderedRelayMucProxyKind {
    JoinPresence,
    OccupantPresence,
    GroupchatMessage,
    PrivateMessage,
    RoomIq,
    FanoutChunk,
}

impl OrderedRelayMucProxyKind {
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
            ) | (OrderedRelayMucProxyKind::RoomIq, waddle_xmpp::Stanza::Iq(_))
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
            (OrderedRelayPayload::Message { .. }, waddle_xmpp::Stanza::Message(_)) => true,
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
            ) => recipient.to_bare() == *bare,
            (
                OrderedRelayPayload::Message { recipient, .. }
                | OrderedRelayPayload::Iq { recipient, .. }
                | OrderedRelayPayload::Presence { recipient, .. },
                OrderedRelayRecipient::FullJid(full),
            ) => recipient == &jid::Jid::from(full.clone()),
            (
                OrderedRelayPayload::MucProxy { room_jid, .. },
                OrderedRelayRecipient::Room(channel_room),
            ) => room_jid == channel_room,
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
    pub target_claim: OrderedRelayClaim,
    pub payload: OrderedRelayPayload,
}

/// Internal ACK for an ordered relay envelope. This is not an XEP-0184 receipt
/// and must never be serialized back to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderedRelayAck {
    pub channel: OrderedRelayChannel,
    pub sequence: OrderedRelaySequence,
    pub duplicate: bool,
    pub next_expected: OrderedRelaySequence,
}

/// Internal NACK reason. These are relay-control outcomes only; they do not
/// synthesize client stanzas or mutate XEP-0198 counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderedRelayNackReason {
    Gap { expected: OrderedRelaySequence },
    NotOwner,
    Unreachable,
    ParseFailure,
    Backpressure,
    Diverted(OrderedRelayDiversion),
}

impl OrderedRelayNackReason {
    pub(crate) fn metric_label(&self) -> &'static str {
        match self {
            OrderedRelayNackReason::Gap { .. } => "gap",
            OrderedRelayNackReason::NotOwner => "not_owner",
            OrderedRelayNackReason::Unreachable => "unreachable",
            OrderedRelayNackReason::ParseFailure => "parse_failure",
            OrderedRelayNackReason::Backpressure => "backpressure",
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OrderedRelayRecentAck {
    sequence: OrderedRelaySequence,
    fingerprint: OrderedRelayEnvelopeFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OrderedRelayEnvelopeFingerprint {
    origin_inbound_sequence: OriginInboundSequence,
    payload: OrderedRelayPayloadFingerprint,
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
        origin_claim: OrderedRelayClaim,
        target_claim: OrderedRelayClaim,
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
            origin_claim,
            target_claim,
            payload,
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
}

/// Receiver-side expected-sequence tracker.
#[derive(Debug, Default)]
pub struct OrderedRelayReceiverState {
    next_expected_by_channel: HashMap<OrderedRelayChannel, OrderedRelaySequence>,
    recent_acked_by_channel: HashMap<OrderedRelayChannel, VecDeque<OrderedRelayRecentAck>>,
    diversions: HashMap<OrderedRelayChannel, OrderedRelayDiversion>,
    new_channels_diverted: bool,
}

impl OrderedRelayReceiverState {
    pub fn reserve(&self, envelope: RemoteStanzaEnvelope) -> OrderedRelayReservation {
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
            && self.next_expected_by_channel.len() >= MAX_TRACKED_ORDERED_RELAY_CHANNELS
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

        OrderedRelayReservation::Reserved(Box::new(OrderedRelayReservedEnvelope {
            envelope,
            next_expected,
        }))
    }

    pub fn commit_reserved(&mut self, reserved: OrderedRelayReservedEnvelope) -> OrderedRelayReply {
        let envelope = reserved.envelope;
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
            && self.next_expected_by_channel.len() >= MAX_TRACKED_ORDERED_RELAY_CHANNELS
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
        );
        OrderedRelayReply::Ack(OrderedRelayAck {
            channel: envelope.channel,
            sequence: envelope.sequence,
            duplicate: false,
            next_expected: reserved.next_expected,
        })
    }

    pub fn divert(&mut self, diversion: OrderedRelayDiversion) {
        if !self.diversions.contains_key(&diversion.channel)
            && self.diversions.len() >= MAX_TRACKED_ORDERED_RELAY_CHANNELS
        {
            self.next_expected_by_channel.remove(&diversion.channel);
            self.recent_acked_by_channel.remove(&diversion.channel);
            self.new_channels_diverted = true;
            return;
        }
        self.diversions.insert(diversion.channel.clone(), diversion);
    }

    pub fn forget_channel(&mut self, channel: &OrderedRelayChannel) {
        self.next_expected_by_channel.remove(channel);
        self.recent_acked_by_channel.remove(channel);
        self.diversions.remove(channel);
    }
}

fn record_ack(
    recent_acked_by_channel: &mut HashMap<OrderedRelayChannel, VecDeque<OrderedRelayRecentAck>>,
    channel: &OrderedRelayChannel,
    envelope: &RemoteStanzaEnvelope,
) {
    let recent = recent_acked_by_channel.entry(channel.clone()).or_default();
    recent.push_back(OrderedRelayRecentAck {
        sequence: envelope.sequence,
        fingerprint: OrderedRelayEnvelopeFingerprint::from(envelope),
    });
    while recent.len() > RECENT_ACK_CACHE_PER_CHANNEL {
        recent.pop_front();
    }
}

fn envelope_is_consistent(envelope: &RemoteStanzaEnvelope) -> bool {
    envelope.payload.matches_stanza_kind()
        && envelope.origin_claim.entity.entity_type == EntityType::SmSession
        && envelope.origin_claim.entity.id == envelope.channel.origin_stream_id.as_str()
        && envelope
            .payload
            .matches_channel_recipient(&envelope.channel.recipient)
        && envelope.payload.matches_stanza_addressing()
        && envelope
            .payload
            .matches_target_claim(&envelope.target_claim)
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

fn muc_proxy_stanza_is_addressed_to_room(
    room_jid: &jid::BareJid,
    kind: OrderedRelayMucProxyKind,
    stanza: &waddle_xmpp::Stanza,
) -> bool {
    let Some(to) = stanza_to(stanza) else {
        return false;
    };
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
        (OrderedRelayMucProxyKind::RoomIq, waddle_xmpp::Stanza::Iq(_)) => true,
        _ => false,
    }
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
            origin_stream_id: SmSessionId::new("stream-1"),
            recipient: OrderedRelayRecipient::BareJid(
                jid::BareJid::from_str(bare).expect("bare jid"),
            ),
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

    fn target_claim() -> OrderedRelayClaim {
        claim(EntityType::UserActor, "juliet@example.test", 3)
    }

    fn target_claim_for_bare(bare: &str) -> OrderedRelayClaim {
        claim(EntityType::UserActor, bare, 3)
    }

    fn message_payload(id: &str) -> OrderedRelayPayload {
        message_payload_to(id, "juliet@example.test")
    }

    fn message_payload_to(id: &str, recipient: &str) -> OrderedRelayPayload {
        let mut stanza = Message::new(Some(jid::Jid::from_str(recipient).expect("jid")));
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
            origin_claim(),
            target_claim(),
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
                origin_claim(),
                target_claim_for_bare(&overflow),
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
            origin_stream_id: SmSessionId::new("stream-1"),
            recipient: OrderedRelayRecipient::BareJid(
                jid::BareJid::from_str(bare).expect("bare jid"),
            ),
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
            origin_stream_id: SmSessionId::new("stream-1"),
            recipient: OrderedRelayRecipient::Room(room_jid()),
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

    fn target_claim() -> OrderedRelayClaim {
        claim(EntityType::UserActor, "juliet@example.test", 3)
    }

    fn target_claim_for_bare(bare: &str) -> OrderedRelayClaim {
        claim(EntityType::UserActor, bare, 3)
    }

    fn room_claim() -> OrderedRelayClaim {
        claim(EntityType::RoomActor, "room@example.test", 11)
    }

    fn message_payload(id: &str) -> OrderedRelayPayload {
        message_payload_to(id, "juliet@example.test")
    }

    fn message_payload_to(id: &str, recipient: &str) -> OrderedRelayPayload {
        let mut stanza = Message::new(Some(jid::Jid::from_str(recipient).expect("jid")));
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
        RemoteStanza(waddle_xmpp::Stanza::Presence(presence))
    }

    fn groupchat_stanza_to(to: &str) -> RemoteStanza {
        let mut message = Message::new(Some(jid::Jid::from_str(to).expect("jid")));
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
                origin_claim(),
                target_claim(),
                message_payload("one"),
            )
            .expect("first");
        let second = state
            .next_envelope(
                origin_node(),
                channel,
                inbound(2),
                origin_claim(),
                target_claim(),
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
                origin_claim(),
                target_claim(),
                message_payload("one"),
            )
            .expect("first");
        let second = state
            .next_envelope(
                NodeId::new("new-node".to_string()),
                channel,
                inbound(2),
                origin_claim(),
                target_claim(),
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
            origin_claim(),
            target_claim(),
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
                origin_claim(),
                target_claim_for_bare("overflow@example.test"),
                message_payload_to("overflow-one", "overflow@example.test"),
            )
            .expect_err("over-capacity channel diverts");

        state.forget_channel(&channel_for_bare("user-0@example.test"));
        let still_blocked = state
            .next_envelope(
                origin_node(),
                overflow,
                inbound(2),
                origin_claim(),
                target_claim_for_bare("overflow@example.test"),
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
                origin_claim(),
                target_claim_for_bare(&overflow),
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
            target_claim: target_claim(),
            payload: message_payload("one"),
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
            target_claim: target_claim(),
            payload: message_payload("one"),
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
            target_claim: target_claim(),
            payload: message_payload("one"),
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
            target_claim: OrderedRelayClaim {
                epoch: ClaimEpoch(100),
                ..target_claim()
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
            target_claim: target_claim(),
            payload: message_payload_to("wrong-recipient", "romeo@example.test"),
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
            target_claim: target_claim(),
            payload: message_payload("one"),
        };
        let second = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel,
            sequence: OrderedRelaySequence(2),
            origin_inbound_sequence: inbound(2),
            origin_claim: origin_claim(),
            target_claim: target_claim(),
            payload: message_payload("two"),
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
            target_claim: target_claim(),
            payload: message_payload("two"),
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
            target_claim: target_claim(),
            payload: message_payload("one"),
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
                    target_claim: target_claim(),
                    payload: message_payload(&format!("m{sequence}")),
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
                target_claim: target_claim(),
                payload: message_payload("too-old"),
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
            target_claim: target_claim(),
            payload: OrderedRelayPayload::Message {
                recipient: jid::Jid::from_str("juliet@example.test").expect("jid"),
                stanza: presence_stanza(),
            },
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
            origin_claim(),
            target_claim(),
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
            target_claim: room_claim(),
            payload: OrderedRelayPayload::MucProxy {
                room_jid: room_jid(),
                kind: OrderedRelayMucProxyKind::JoinPresence,
                stanza: presence_stanza(),
            },
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
            target_claim: room_claim(),
            payload: OrderedRelayPayload::MucProxy {
                room_jid: room_jid(),
                kind: OrderedRelayMucProxyKind::GroupchatMessage,
                stanza: presence_stanza(),
            },
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

    #[test]
    fn muc_proxy_validation_rejects_malformed_xep0045_shapes() {
        let unavailable_join = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel: room_channel(),
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(1),
            origin_claim: origin_claim(),
            target_claim: room_claim(),
            payload: OrderedRelayPayload::MucProxy {
                room_jid: room_jid(),
                kind: OrderedRelayMucProxyKind::JoinPresence,
                stanza: presence_stanza_to(
                    "room@example.test/romeo",
                    xmpp_parsers::presence::Type::Unavailable,
                ),
            },
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
