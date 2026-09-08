//! XEP-0045 §7.4: "the service MUST reflect the message to all occupants".
//! This suite checks ordered-relay occupant copies, including room ownership moves
//! between nodes. The two-process harness lives in clustering_cluster_e2e.rs.

#![cfg(feature = "clustering")]

use waddle_server::clustering::codec::RemoteStanza;
use waddle_server::clustering::ordered_relay::{
    OrderedRelayAck, OrderedRelayChannel, OrderedRelayClaim, OrderedRelayNack,
    OrderedRelayNackReason, OrderedRelayOrigin, OrderedRelayPayload, OrderedRelayReceiverState,
    OrderedRelayRecipient, OrderedRelayReply, OrderedRelayReservation, OrderedRelaySequence,
    OriginInboundSequence, RemoteStanzaEnvelope,
};
use waddle_server::clustering::NodeId;
use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType};
use xmpp_parsers::message::Message;

fn claim(entity_type: EntityType, id: &str, epoch: i64) -> OrderedRelayClaim {
    OrderedRelayClaim {
        entity: Entity::new(entity_type, id),
        epoch: ClaimEpoch(epoch),
    }
}

fn origin_node() -> NodeId {
    NodeId::new("origin-node".to_string())
}

fn inbound(sequence: u32) -> OriginInboundSequence {
    OriginInboundSequence(sequence)
}

fn room_claim() -> OrderedRelayClaim {
    claim(EntityType::RoomActor, "room@example.test", 11)
}

fn target_claim() -> OrderedRelayClaim {
    claim(EntityType::UserActor, "juliet@example.test", 3)
}

fn sender_claim() -> OrderedRelayClaim {
    claim(EntityType::UserActor, "romeo@example.test", 5)
}

fn user_actor_origin_claim() -> OrderedRelayClaim {
    claim(EntityType::UserActor, "romeo@example.test", 5)
}

fn receive(
    receiver: &mut waddle_server::clustering::ordered_relay::OrderedRelayReceiverState,
    envelope: RemoteStanzaEnvelope,
) -> OrderedRelayReply {
    match receiver.reserve(envelope) {
        OrderedRelayReservation::Reserved(reserved) => receiver.commit_reserved(*reserved),
        OrderedRelayReservation::Completed(reply) => reply,
    }
}

#[test]
fn receiver_reserves_full_jid_groupchat_from_room_entity() {
    for (to, accepted) in [
        ("juliet@example.test/phone", true),
        ("juliet@example.test/other", false),
        ("juliet@example.test", false),
    ] {
        let mut receiver =
            waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
        let target: jid::FullJid = "juliet@example.test/phone".parse().expect("target");
        let mut stanza = Message::new(Some(to.parse().expect("stanza to")));
        stanza.from = Some("room@example.test/romeo".parse().expect("occupant"));
        stanza.type_ = xmpp_parsers::message::MessageType::Groupchat;
        stanza.id = Some(xmpp_parsers::message::Id("client-id".into()));
        stanza.payloads.push(room_stanza_id().into());
        let expected_stanza = stanza.clone();
        let envelope = RemoteStanzaEnvelope {
            asserted_origin_node: origin_node(),
            channel: OrderedRelayChannel {
                origin: OrderedRelayOrigin::Entity(room_claim().entity),
                origin_epoch: room_claim().epoch,
                recipient: OrderedRelayRecipient::FullJid(target.clone()),
                target_epoch: target_claim().epoch,
            },
            sequence: OrderedRelaySequence(1),
            origin_inbound_sequence: inbound(0),
            origin_claim: room_claim(),
            sender_claim: room_claim(),
            target_claim: target_claim(),
            payload: OrderedRelayPayload::Message {
                recipient: target.into(),
                stanza: RemoteStanza(waddle_xmpp::Stanza::Message(stanza)),
            },
            origin_proof: None,
        };
        match receiver.reserve(envelope) {
            OrderedRelayReservation::Reserved(reserved) => {
                assert!(
                    accepted,
                    "mismatched groupchat destination must be rejected"
                );
                assert_reflection(reserved.envelope(), &expected_stanza);
                assert!(matches!(
                    receiver.commit_reserved(*reserved),
                    OrderedRelayReply::Ack(_)
                ));
            }
            OrderedRelayReservation::Completed(reply) => {
                assert!(!accepted, "full-JID room copy must reserve: {reply:?}");
                assert!(matches!(
                    reply,
                    OrderedRelayReply::Nack(OrderedRelayNack {
                        reason: OrderedRelayNackReason::ParseFailure,
                        ..
                    })
                ));
            }
        }
    }
}

#[test]
fn receiver_nacks_groupchat_vouched_by_a_user_claim() {
    // XEP-0045 §7.4: an occupant copy is authored by the room, so only the
    // room's claim may vouch for it even when the user claim matches `from`.
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
    let target: jid::FullJid = "juliet@example.test/phone".parse().expect("target");
    let mut stanza = Message::new(Some(target.clone().into()));
    stanza.from = Some("romeo@example.test/phone".parse().expect("user sender"));
    stanza.type_ = xmpp_parsers::message::MessageType::Groupchat;
    let envelope = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: OrderedRelayChannel {
            origin: OrderedRelayOrigin::Entity(user_actor_origin_claim().entity),
            origin_epoch: user_actor_origin_claim().epoch,
            recipient: OrderedRelayRecipient::FullJid(target.clone()),
            target_epoch: target_claim().epoch,
        },
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(0),
        origin_claim: user_actor_origin_claim(),
        sender_claim: sender_claim(),
        target_claim: target_claim(),
        payload: OrderedRelayPayload::Message {
            recipient: target.into(),
            stanza: RemoteStanza(waddle_xmpp::Stanza::Message(stanza)),
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

fn room_stanza_id() -> xmpp_parsers::stanza_id::StanzaId {
    xmpp_parsers::stanza_id::StanzaId {
        id: "room-stanza-id".into(),
        by: "room@example.test".parse().expect("room jid"),
    }
}

fn assert_reflection(envelope: &RemoteStanzaEnvelope, expected: &Message) {
    let OrderedRelayPayload::Message { recipient, stanza } = &envelope.payload else {
        panic!("expected message payload");
    };
    let waddle_xmpp::Stanza::Message(message) = &stanza.0 else {
        panic!("expected message stanza");
    };
    assert_eq!(
        recipient,
        &"juliet@example.test/phone"
            .parse::<jid::Jid>()
            .expect("occupant jid")
    );
    assert_eq!(message, expected, "reservation preserves the whole stanza");
    assert_eq!(
        message.from,
        Some("room@example.test/romeo".parse().expect("room/nick"))
    );
    assert_eq!(message.to, Some(recipient.clone()));
    assert_eq!(message.type_, xmpp_parsers::message::MessageType::Groupchat);
    assert_eq!(
        message.id,
        Some(xmpp_parsers::message::Id("client-id".into()))
    );
    assert_eq!(
        message.payloads,
        vec![minidom::Element::from(room_stanza_id())]
    );
}

#[test]
fn receiver_reflects_when_room_ownership_moves_between_nodes() {
    let mut receiver = OrderedRelayReceiverState::default();
    let target: jid::FullJid = "juliet@example.test/phone".parse().expect("occupant");
    let mut stanza = Message::new(Some(target.clone().into()));
    stanza.from = Some("room@example.test/romeo".parse().expect("room/nick"));
    stanza.type_ = xmpp_parsers::message::MessageType::Groupchat;
    stanza.id = Some(xmpp_parsers::message::Id("client-id".into()));
    stanza.payloads.push(room_stanza_id().into());
    // The target claim stays fixed while a new owner restarts the room sequence.
    // Returning to the old epoch also proves its counter was retained separately.
    for (epoch, sequence, node) in [
        (11, 1, "old"),
        (11, 2, "old"),
        (12, 1, "new"),
        (11, 3, "old"),
    ] {
        let origin_claim = claim(EntityType::RoomActor, "room@example.test", epoch);
        let envelope = RemoteStanzaEnvelope {
            asserted_origin_node: NodeId::new(node.into()),
            channel: OrderedRelayChannel {
                origin: OrderedRelayOrigin::Entity(origin_claim.entity.clone()),
                origin_epoch: origin_claim.epoch,
                recipient: OrderedRelayRecipient::FullJid(target.clone()),
                target_epoch: target_claim().epoch,
            },
            sequence: OrderedRelaySequence(sequence),
            origin_inbound_sequence: inbound(0),
            origin_claim: origin_claim.clone(),
            sender_claim: origin_claim,
            target_claim: target_claim(),
            payload: OrderedRelayPayload::Message {
                recipient: target.clone().into(),
                stanza: RemoteStanza(waddle_xmpp::Stanza::Message(stanza.clone())),
            },
            origin_proof: None,
        };
        let expected_channel = envelope.channel.clone();
        let OrderedRelayReservation::Reserved(reserved) = receiver.reserve(envelope) else {
            panic!("room epoch {epoch} sequence {sequence} must reserve without Gap");
        };
        assert_reflection(reserved.envelope(), &stanza);
        assert!(
            matches!(receiver.commit_reserved(*reserved), OrderedRelayReply::Ack(OrderedRelayAck {
            channel, duplicate: false, next_expected, ..
        }) if channel == expected_channel && next_expected == OrderedRelaySequence(sequence + 1))
        );
    }
}
