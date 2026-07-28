#![cfg(feature = "clustering")]

use std::str::FromStr;

use waddle_server::clustering::codec::RemoteStanza;
use waddle_server::clustering::ordered_relay::{
    OrderedRelayAck, OrderedRelayChannel, OrderedRelayClaim, OrderedRelayDiversion,
    OrderedRelayDiversionReason, OrderedRelayEnvelopeClaims, OrderedRelayMucProxyKind,
    OrderedRelayNack, OrderedRelayNackReason, OrderedRelayOrigin, OrderedRelayPayload,
    OrderedRelayRecipient, OrderedRelayReply, OrderedRelayReservation, OrderedRelayRoomLane,
    OrderedRelaySenderState, OrderedRelaySequence, OriginInboundSequence, RemoteStanzaEnvelope,
};
use waddle_server::clustering::NodeId;
use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType};
use waddle_xmpp::pending_delivery::SmSessionId;
use xmpp_parsers::message::{Lang, Message};

const MAX_TRACKED_ORDERED_RELAY_CHANNELS: usize = 8_192;
const RECENT_ACK_CACHE_PLUS_ONE: u64 = 65;

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

fn room_channel() -> OrderedRelayChannel {
    OrderedRelayChannel {
        origin: OrderedRelayOrigin::SmSession(SmSessionId::new("stream-1")),
        recipient: OrderedRelayRecipient::Room {
            room: room_jid(),
            lane: OrderedRelayRoomLane::MucStanza,
        },
        target_epoch: ClaimEpoch(11),
    }
}

fn muji_room_channel() -> OrderedRelayChannel {
    OrderedRelayChannel {
        origin: OrderedRelayOrigin::SmSession(SmSessionId::new("stream-1")),
        recipient: OrderedRelayRecipient::Room {
            room: room_jid(),
            lane: OrderedRelayRoomLane::MujiSignaling,
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

fn user_actor_origin_claim() -> OrderedRelayClaim {
    claim(EntityType::UserActor, "romeo@example.test", 5)
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
    presence.from = Some(jid::Jid::from_str("romeo@example.test/home").expect("jid"));
    presence.to = Some(jid::Jid::from_str(to).expect("jid"));
    RemoteStanza(waddle_xmpp::Stanza::Presence(presence))
}

fn groupchat_stanza_to(to: &str) -> RemoteStanza {
    let mut message = Message::new(Some(jid::Jid::from_str(to).expect("jid")));
    message.from = Some(jid::Jid::from_str("romeo@example.test/home").expect("jid"));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    RemoteStanza(waddle_xmpp::Stanza::Message(message))
}

fn iq_stanza_to(to: &str) -> RemoteStanza {
    let mut iq = xmpp_parsers::iq::Iq::from_get("room-iq", xmpp_parsers::ping::Ping);
    *iq.from_mut() = Some(jid::Jid::from_str("romeo@example.test/home").expect("jid"));
    *iq.to_mut() = Some(jid::Jid::from_str(to).expect("jid"));
    RemoteStanza(waddle_xmpp::Stanza::Iq(Box::new(iq)))
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
fn sender_backpressure_diversion_stays_sticky_after_capacity_frees() {
    let mut state = OrderedRelaySenderState::default();
    for index in 0..MAX_TRACKED_ORDERED_RELAY_CHANNELS {
        state
            .next_envelope(
                origin_node(),
                channel_for_bare(&format!("user-{index}@example.test")),
                inbound(index as u32),
                claims_for_target(target_claim_for_bare(&format!("user-{index}@example.test"))),
                message_payload_to("fill", &format!("user-{index}@example.test")),
            )
            .expect("tracked channel");
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
fn sender_overflow_channels_remain_backpressured_without_per_channel_stickiness() {
    let mut state = OrderedRelaySenderState::default();
    for index in 0..MAX_TRACKED_ORDERED_RELAY_CHANNELS {
        let bare = format!("user-{index}@example.test");
        state
            .next_envelope(
                origin_node(),
                channel_for_bare(&bare),
                inbound(index as u32),
                claims_for_target(target_claim_for_bare(&bare)),
                message_payload_to("fill", &bare),
            )
            .expect("tracked channel");
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

    state.forget_channel(&channel_for_bare("user-0@example.test"));
    let fresh = "fresh-after-overflow@example.test";
    assert!(matches!(
        state.next_envelope(
            origin_node(),
            channel_for_bare(fresh),
            inbound(99),
            claims_for_target(target_claim_for_bare(fresh)),
            message_payload_to("fresh", fresh),
        ),
        Err(OrderedRelayDiversion {
            reason: OrderedRelayDiversionReason::Backpressure,
            ..
        })
    ));
}

#[test]
fn receiver_acks_in_order_and_duplicate_envelopes() {
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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
fn receiver_duplicate_ack_replays_client_reply_stanzas() {
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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
    let client_replies = vec![presence_stanza_to(
        "romeo@example.test/garden",
        xmpp_parsers::presence::Type::None,
    )];

    let reserved = match receiver.reserve(envelope.clone()) {
        OrderedRelayReservation::Reserved(reserved) => reserved,
        other => panic!("expected reservation, got {other:?}"),
    };
    let first_ack = receiver.commit_reserved_with_replies(*reserved, client_replies.clone());
    match first_ack {
        OrderedRelayReply::Ack(OrderedRelayAck {
            duplicate: false,
            client_replies: replies,
            ..
        }) => assert_eq!(replies, client_replies),
        other => panic!("expected first ack with replies, got {other:?}"),
    }

    match receiver.reserve(envelope) {
        OrderedRelayReservation::Completed(OrderedRelayReply::Ack(OrderedRelayAck {
            duplicate: true,
            client_replies: replies,
            ..
        })) => assert_eq!(replies, client_replies),
        other => panic!("expected duplicate ack with replies, got {other:?}"),
    }
}

#[test]
fn receiver_nacks_channel_payload_and_claim_mismatch() {
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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
fn receiver_nacks_full_jid_payload_on_bare_channel() {
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
    let envelope = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: channel(),
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(1),
        origin_claim: origin_claim(),
        sender_claim: sender_claim(),
        target_claim: target_claim(),
        payload: message_payload_to("full-as-bare", "juliet@example.test/phone"),
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
fn receiver_nacks_groupchat_on_user_message_payload() {
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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
            stanza: groupchat_stanza_to("juliet@example.test"),
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
fn receiver_reservation_does_not_advance_expected_until_commit() {
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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
fn receiver_duplicate_pending_envelope_does_not_reserve_second_effect() {
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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

    let reserved = match receiver.reserve(envelope.clone()) {
        OrderedRelayReservation::Reserved(reserved) => reserved,
        OrderedRelayReservation::Completed(reply) => {
            panic!("expected first reservation before side effect, got {reply:?}");
        }
    };

    assert!(matches!(
        receiver.reserve(envelope),
        OrderedRelayReservation::Completed(OrderedRelayReply::Nack(OrderedRelayNack {
            reason: OrderedRelayNackReason::InFlight,
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
fn receiver_accepts_entity_origin_when_claim_matches_channel_origin() {
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
    let envelope = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: OrderedRelayChannel {
            origin: OrderedRelayOrigin::Entity(user_actor_origin_claim().entity.clone()),
            recipient: OrderedRelayRecipient::BareJid(
                jid::BareJid::from_str("juliet@example.test").expect("bare jid"),
            ),
            target_epoch: ClaimEpoch(3),
        },
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(0),
        origin_claim: user_actor_origin_claim(),
        sender_claim: sender_claim(),
        target_claim: target_claim(),
        payload: message_payload("entity-origin"),
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
}

#[test]
fn receiver_rejects_entity_origin_when_claim_differs_from_channel_origin() {
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
    let envelope = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: OrderedRelayChannel {
            origin: OrderedRelayOrigin::Entity(Entity::new(
                EntityType::UserActor,
                "romeo@example.test",
            )),
            recipient: OrderedRelayRecipient::BareJid(
                jid::BareJid::from_str("juliet@example.test").expect("bare jid"),
            ),
            target_epoch: ClaimEpoch(3),
        },
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(0),
        origin_claim: claim(EntityType::UserActor, "mallory@example.test", 5),
        sender_claim: sender_claim(),
        target_claim: target_claim(),
        payload: message_payload("wrong-entity-origin"),
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
fn receiver_rejects_sender_claim_that_does_not_match_stanza_from() {
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
    let envelope = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: channel(),
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(1),
        origin_claim: origin_claim(),
        sender_claim: claim(EntityType::UserActor, "mallory@example.test", 5),
        target_claim: target_claim(),
        payload: message_payload("forged-from"),
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
fn receiver_nacks_gaps_without_advancing_expected_sequence() {
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
    let channel = channel();
    for sequence in 1..=RECENT_ACK_CACHE_PLUS_ONE {
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
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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

/// #1597: Muji signaling rides its own room lane. A sticky diversion on
/// one lane must not stop the same room's other lane — a poisoned Muji
/// lane cannot take ordinary MUC join/leave/groupchat traffic with it,
/// and vice versa.
#[test]
fn diverted_room_lane_leaves_the_other_lane_flowing() {
    let mut state = OrderedRelaySenderState::default();
    state.divert(OrderedRelayDiversion {
        channel: muji_room_channel(),
        reason: OrderedRelayDiversionReason::Unreachable,
    });

    state
        .next_envelope(
            origin_node(),
            muji_room_channel(),
            inbound(1),
            claims_for_target(room_claim()),
            OrderedRelayPayload::MucProxy {
                room_jid: room_jid(),
                kind: OrderedRelayMucProxyKind::MujiJingleIq,
                stanza: iq_stanza_to("calls.example.test"),
            },
        )
        .expect_err("the diverted Muji lane must stay diverted");

    let muc_envelope = state
        .next_envelope(
            origin_node(),
            room_channel(),
            inbound(2),
            claims_for_target(room_claim()),
            OrderedRelayPayload::MucProxy {
                room_jid: room_jid(),
                kind: OrderedRelayMucProxyKind::GroupchatMessage,
                stanza: groupchat_stanza_to("room@example.test"),
            },
        )
        .expect("the MUC stanza lane must keep flowing");
    assert_eq!(muc_envelope.sequence, OrderedRelaySequence::FIRST);

    let mut state = OrderedRelaySenderState::default();
    state.divert(OrderedRelayDiversion {
        channel: room_channel(),
        reason: OrderedRelayDiversionReason::Unreachable,
    });
    let muji_envelope = state
        .next_envelope(
            origin_node(),
            muji_room_channel(),
            inbound(3),
            claims_for_target(room_claim()),
            OrderedRelayPayload::MucProxy {
                room_jid: room_jid(),
                kind: OrderedRelayMucProxyKind::MujiJingleIq,
                stanza: iq_stanza_to("calls.example.test"),
            },
        )
        .expect("a diverted MUC lane must not stop Muji signaling");
    assert_eq!(muji_envelope.sequence, OrderedRelaySequence::FIRST);
}

#[test]
fn muc_proxy_kind_validates_the_carried_stanza_kind() {
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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

    let mut fresh_receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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

    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
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
    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
    assert!(matches!(
        receive(&mut receiver, full_groupchat),
        OrderedRelayReply::Nack(OrderedRelayNack {
            reason: OrderedRelayNackReason::ParseFailure,
            ..
        })
    ));
}

#[test]
fn muc_proxy_room_iq_kinds_validate_bare_room_vs_occupant_addressing() {
    let bare_room_iq = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: room_channel(),
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(1),
        origin_claim: origin_claim(),
        sender_claim: sender_claim(),
        target_claim: room_claim(),
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::BareRoomIq,
            stanza: iq_stanza_to("room@example.test"),
        },
        origin_proof: None,
    };
    let full_jid_as_bare_room_iq = RemoteStanzaEnvelope {
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::BareRoomIq,
            stanza: iq_stanza_to("room@example.test/romeo"),
        },
        ..bare_room_iq.clone()
    };
    let occupant_iq = RemoteStanzaEnvelope {
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::OccupantIq,
            stanza: iq_stanza_to("room@example.test/romeo"),
        },
        ..bare_room_iq.clone()
    };
    let bare_jid_as_occupant_iq = RemoteStanzaEnvelope {
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::OccupantIq,
            stanza: iq_stanza_to("room@example.test"),
        },
        ..bare_room_iq.clone()
    };

    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
    assert!(matches!(
        receive(&mut receiver, bare_room_iq),
        OrderedRelayReply::Ack(OrderedRelayAck { .. })
    ));

    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
    assert!(matches!(
        receive(&mut receiver, full_jid_as_bare_room_iq),
        OrderedRelayReply::Nack(OrderedRelayNack {
            reason: OrderedRelayNackReason::ParseFailure,
            ..
        })
    ));

    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
    assert!(matches!(
        receive(&mut receiver, occupant_iq),
        OrderedRelayReply::Ack(OrderedRelayAck { .. })
    ));

    let mut receiver =
        waddle_server::clustering::ordered_relay::OrderedRelayReceiverState::default();
    assert!(matches!(
        receive(&mut receiver, bare_jid_as_occupant_iq),
        OrderedRelayReply::Nack(OrderedRelayNack {
            reason: OrderedRelayNackReason::ParseFailure,
            ..
        })
    ));
}
