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
        channel: room_channel_for_lane(OrderedRelayRoomLane::MujiSignaling),
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

/// #1597: the channel lane is part of envelope validation. A Muji
/// IQ smuggled onto the MUC stanza lane (or a groupchat message
/// onto the Muji lane) must be rejected as inconsistent, so lane
/// isolation cannot be bypassed by mislabeling the channel.
#[test]
fn room_lane_must_match_the_muc_proxy_kind() {
    let mut receiver = OrderedRelayReceiverState::default();
    let mut muji_on_muc_lane = muji_envelope(
        OrderedRelayMucProxyKind::MujiJingleIq,
        muji_initiate_stanza(
            xmpp_parsers::jingle::Action::SessionInitiate,
            "room@example.test",
        ),
    );
    muji_on_muc_lane.channel = room_channel_for_lane(OrderedRelayRoomLane::MucStanza);
    assert!(
        matches!(
            receive(&mut receiver, muji_on_muc_lane),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ),
        "a Muji IQ on the MUC stanza lane must be rejected"
    );

    let mut receiver = OrderedRelayReceiverState::default();
    let groupchat_on_muji_lane = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: room_channel_for_lane(OrderedRelayRoomLane::MujiSignaling),
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(1),
        origin_claim: origin_claim(),
        sender_claim: sender_claim(),
        target_claim: room_claim(),
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::GroupchatMessage,
            stanza: groupchat_stanza_to("room@example.test"),
        },
        origin_proof: None,
    };
    assert!(
        matches!(
            receive(&mut receiver, groupchat_on_muji_lane),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ),
        "a groupchat message on the Muji lane must be rejected"
    );
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
