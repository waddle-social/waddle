use super::*;
use std::str::FromStr;
use waddle_xmpp::ownership::EntityType;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::message::{Lang, Message};

fn channel() -> OrderedRelayChannel {
    channel_for_bare("juliet@example.test")
}

fn channel_for_bare(bare: &str) -> OrderedRelayChannel {
    OrderedRelayChannel {
        origin: OrderedRelayOrigin::SmSession(SmSessionId::new("stream-1")),
        recipient: OrderedRelayRecipient::BareJid(jid::BareJid::from_str(bare).expect("bare jid")),
        target_epoch: ClaimEpoch(3),
    }
}

fn origin_node() -> NodeId {
    NodeId::new("origin-node".to_string())
}

fn room_jid() -> jid::BareJid {
    jid::BareJid::from_str("room@example.test").expect("room jid")
}

fn occupancy_session(value: u128) -> OccupancySessionGeneration {
    OccupancySessionGeneration::from_uuid(uuid::Uuid::from_u128(value))
}

fn connection_origin(value: u128) -> MucProxyOrigin {
    MucProxyOrigin::Connection(occupancy_session(value))
}

fn room_channel() -> OrderedRelayChannel {
    room_channel_for_lane(OrderedRelayRoomLane::MucStanza)
}

fn room_channel_for_lane(lane: OrderedRelayRoomLane) -> OrderedRelayChannel {
    OrderedRelayChannel {
        origin: OrderedRelayOrigin::SmSession(SmSessionId::new("stream-1")),
        recipient: OrderedRelayRecipient::Room {
            room: room_jid(),
            lane,
        },
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

/// #1445: a Muji `session-initiate` is not addressed to the room —
/// `to` is the calls mixer and the room lives in the `<muji/>`
/// payload — so its envelope validation must bind the payload's
/// room to the channel's room instead of the stanza's `to`.
fn muji_envelope(kind: OrderedRelayMucProxyKind, stanza: RemoteStanza) -> RemoteStanzaEnvelope {
    RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: room_channel_for_lane(OrderedRelayRoomLane::MujiSignaling),
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(1),
        origin_claim: origin_claim(),
        sender_claim: sender_claim(),
        target_claim: room_claim(),
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind,
            origin: connection_origin(1),
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

mod muc_validation;
mod receiver;
mod sender;
