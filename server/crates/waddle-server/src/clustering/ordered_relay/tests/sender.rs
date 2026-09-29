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
    let signing_bytes = envelope.signing_bytes().expect("signing bytes");
    let signature = keypair.sign(&signing_bytes).expect("sign envelope");
    assert!(
        keypair.public().verify(&signing_bytes, &signature),
        "signature must verify before tampering"
    );

    envelope.payload = OrderedRelayPayload::MucProxy {
        canonical: None,
        principal: None,
        stanza_lang: None,
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
#[test]
fn sender_diversion_expires_after_cooldown_and_keeps_the_channel_sequence() {
    let mut state = OrderedRelaySenderState::default();
    let channel = channel();
    let start = tokio::time::Instant::now();
    let first = state
        .next_envelope_at(
            start,
            origin_node(),
            channel.clone(),
            inbound(1),
            claims(),
            message_payload("one"),
        )
        .expect("first");
    let diversion = OrderedRelayDiversion {
        channel: channel.clone(),
        reason: OrderedRelayDiversionReason::Unreachable,
    };
    state.divert_at(start, diversion.clone());

    let within = state.next_envelope_at(
        start + ORDERED_RELAY_DIVERSION_COOLDOWN - std::time::Duration::from_millis(1),
        origin_node(),
        channel.clone(),
        inbound(2),
        claims(),
        message_payload("within-cooldown"),
    );
    assert_eq!(within.expect_err("still diverted"), diversion);

    let recovered = state
        .next_envelope_at(
            start + ORDERED_RELAY_DIVERSION_COOLDOWN,
            origin_node(),
            channel,
            inbound(3),
            claims(),
            message_payload("after-cooldown"),
        )
        .expect("diversion expired");
    assert_eq!(first.sequence, OrderedRelaySequence(1));
    assert_eq!(recovered.sequence, OrderedRelaySequence(2));
    assert!(state.diversions.is_empty());
}
#[test]
fn sender_gap_resync_relabels_the_next_envelope_to_the_receiver_expectation() {
    let mut state = OrderedRelaySenderState::default();
    let channel = channel();
    for index in 1..=2 {
        state
            .next_envelope(
                origin_node(),
                channel.clone(),
                inbound(index),
                claims(),
                message_payload("lost"),
            )
            .expect("allocate");
    }
    let nacked = state
        .next_envelope(
            origin_node(),
            channel.clone(),
            inbound(3),
            claims(),
            message_payload("three"),
        )
        .expect("third");
    assert_eq!(nacked.sequence, OrderedRelaySequence(3));

    assert!(state.resync_after_gap(&nacked, OrderedRelaySequence(2)));
    let resent = state
        .next_envelope(
            origin_node(),
            channel,
            inbound(3),
            claims(),
            message_payload("three"),
        )
        .expect("resynced");
    assert_eq!(resent.sequence, OrderedRelaySequence(2));
}
#[test]
fn sender_gap_resync_refuses_a_channel_that_moved_on_or_is_diverted() {
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
    state
        .next_envelope(
            origin_node(),
            channel.clone(),
            inbound(2),
            claims(),
            message_payload("two"),
        )
        .expect("second");
    assert!(
        !state.resync_after_gap(&first, OrderedRelaySequence::FIRST),
        "a later envelope already consumed the channel"
    );

    let mut diverted = OrderedRelaySenderState::default();
    let only = diverted
        .next_envelope(
            origin_node(),
            channel.clone(),
            inbound(1),
            claims(),
            message_payload("one"),
        )
        .expect("first");
    diverted.divert(OrderedRelayDiversion {
        channel,
        reason: OrderedRelayDiversionReason::Unreachable,
    });
    assert!(!diverted.resync_after_gap(&only, OrderedRelaySequence(5)));
}
#[test]
fn sender_expired_diversions_are_pruned_before_the_capacity_latch() {
    let mut state = OrderedRelaySenderState::default();
    let start = tokio::time::Instant::now();
    for index in 0..MAX_TRACKED_ORDERED_RELAY_CHANNELS {
        state.divert_at(
            start,
            OrderedRelayDiversion {
                channel: channel_for_bare(&format!("user-{index}@example.test")),
                reason: OrderedRelayDiversionReason::Unreachable,
            },
        );
    }
    assert!(!state.new_channels_diverted);

    state.divert_at(
        start + ORDERED_RELAY_DIVERSION_COOLDOWN,
        OrderedRelayDiversion {
            channel: channel_for_bare("late@example.test"),
            reason: OrderedRelayDiversionReason::Unreachable,
        },
    );

    assert!(!state.new_channels_diverted);
    assert_eq!(state.diversions.len(), 1);
}
