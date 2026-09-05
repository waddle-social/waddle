use super::*;
use libp2p::identity::Keypair;

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
fn changing_muc_proxy_origin_invalidates_existing_signature() {
    let keypair = Keypair::generate_ed25519();
    let mut envelope = RemoteStanzaEnvelope {
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
            origin: connection_origin(1),
            stanza: presence_stanza(),
        },
        origin_proof: None,
    };
    let signing_bytes = envelope.signing_bytes().expect("signing bytes");
    let signature = keypair.sign(&signing_bytes).expect("sign envelope");
    assert!(
        keypair.public().verify(&signing_bytes, &signature),
        "signature must verify before tampering"
    );

    envelope.payload = OrderedRelayPayload::MucProxy {
        room_jid: room_jid(),
        kind: OrderedRelayMucProxyKind::JoinPresence,
        origin: connection_origin(2),
        stanza: presence_stanza(),
    };
    let tampered_bytes = envelope.signing_bytes().expect("tampered signing bytes");
    assert_ne!(signing_bytes, tampered_bytes);
    assert!(
        !keypair.public().verify(&tampered_bytes, &signature),
        "signature over the original origin must fail after origin tampering"
    );
}
