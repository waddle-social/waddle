use super::*;
use std::str::FromStr;

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
fn receiver_nacks_same_sequence_muc_proxy_with_different_generation() {
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
            canonical: None,
            principal: None,
            stanza_lang: None,
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::JoinPresence,
            origin: connection_origin(1),
            stanza: presence_stanza(),
        },
        origin_proof: None,
    };
    assert!(matches!(
        receive(&mut receiver, envelope.clone()),
        OrderedRelayReply::Ack(OrderedRelayAck {
            duplicate: false,
            ..
        })
    ));

    let replacement_generation = RemoteStanzaEnvelope {
        payload: OrderedRelayPayload::MucProxy {
            canonical: None,
            principal: None,
            stanza_lang: None,
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::JoinPresence,
            origin: connection_origin(2),
            stanza: presence_stanza(),
        },
        ..envelope
    };
    assert!(matches!(
        receive(&mut receiver, replacement_generation),
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
