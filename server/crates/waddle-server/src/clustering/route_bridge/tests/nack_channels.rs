use super::*;
use crate::clustering::ordered_relay::OrderedRelaySequence;
use waddle_xmpp::ownership::ClaimEpoch;
use xmpp_parsers::message::Message;

#[tokio::test]
async fn origin_not_owner_nack_is_terminal_provenance_failure() {
    let services = services_with_claims(
        origin_identity(),
        other_identity(),
        origin_identity(),
        test_peer_id(),
    )
    .await;
    let nack = OrderedRelayNack {
        channel: envelope().channel,
        sequence: OrderedRelaySequence::FIRST,
        reason: OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        },
    };

    let (iq_outcome, iq_action, iq_maybe_committed) = outcome_for_nack(
        &services,
        &target_entity(),
        &receiver_identity(),
        &nack,
        true,
    )
    .await;
    let (message_outcome, message_action, message_maybe_committed) = outcome_for_nack(
        &services,
        &target_entity(),
        &receiver_identity(),
        &nack,
        false,
    )
    .await;

    assert_eq!(iq_outcome, Some(FullJidDeliveryOutcome::Unavailable));
    assert_eq!(message_outcome, Some(FullJidDeliveryOutcome::Dropped));
    assert!(!iq_maybe_committed);
    assert!(!message_maybe_committed);
    assert_eq!(
        iq_action,
        NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner)
    );
    assert_eq!(
        message_action,
        NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner)
    );
}
#[tokio::test]
async fn maybe_committed_diversion_suppresses_iq_fallback_on_replay() {
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    let channel = envelope().channel;
    let nack = OrderedRelayNack {
        channel: channel.clone(),
        sequence: OrderedRelaySequence::FIRST,
        reason: OrderedRelayNackReason::Diverted(OrderedRelayDiversion {
            channel,
            reason: OrderedRelayDiversionReason::MaybeCommitted,
        }),
    };

    let (outcome, action, maybe_committed) = outcome_for_nack(
        &services,
        &target_entity(),
        &receiver_identity(),
        &nack,
        true,
    )
    .await;

    assert_eq!(outcome, Some(FullJidDeliveryOutcome::Dropped));
    assert!(maybe_committed);
    assert_eq!(
        action,
        NackChannelAction::Divert(OrderedRelayDiversionReason::MaybeCommitted)
    );
}
#[test]
fn maybe_committed_remote_delivery_maps_to_fallback_suppressing_outcome() {
    let outcome = no_client_reply_outcome_with_commit_state(FullJidDeliveryOutcome::Dropped, true);

    assert_eq!(
        caller_delivery_outcome(outcome),
        FullJidDeliveryOutcome::MaybeCommitted
    );
}
#[tokio::test]
async fn failed_ordered_delivery_sticky_diverts_channel_instead_of_rewinding_sequence() {
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let channel = envelope().channel;
    {
        let mut sender = bridge.sender_state.lock().await;
        let first = sender
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                channel.clone(),
                OriginInboundSequence(1),
                envelope_claims(0),
                message_payload(),
            )
            .expect("first envelope allocates");
        assert_eq!(first.sequence, OrderedRelaySequence::FIRST);
    }

    let nack = OrderedRelayNack {
        channel: channel.clone(),
        sequence: OrderedRelaySequence::FIRST,
        reason: OrderedRelayNackReason::TargetUnavailable,
    };
    bridge
        .divert_channel(channel.clone(), diversion_reason_for_nack(&nack))
        .await;

    let diverted = bridge
        .sender_state
        .lock()
        .await
        .next_envelope(
            NodeId::new(origin_identity().node_id),
            channel,
            OriginInboundSequence(2),
            envelope_claims(0),
            message_payload(),
        )
        .expect_err("later sends must not restart at sequence one");
    assert_eq!(diverted.reason, OrderedRelayDiversionReason::Unreachable);
}
#[tokio::test]
async fn not_owner_nack_clears_sender_channel_for_refreshed_owner_retry() {
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let channel = envelope().channel;
    {
        let mut sender = bridge.sender_state.lock().await;
        let first = sender
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                channel.clone(),
                OriginInboundSequence(1),
                envelope_claims(0),
                message_payload(),
            )
            .expect("first envelope allocates");
        assert_eq!(first.sequence, OrderedRelaySequence::FIRST);
    }

    bridge
        .apply_nack_channel_action(&envelope(), NackChannelAction::Forget)
        .await;

    let refreshed_channel = OrderedRelayChannel {
        target_epoch: ClaimEpoch(1),
        ..channel
    };
    let retried = bridge
        .sender_state
        .lock()
        .await
        .next_envelope(
            NodeId::new(origin_identity().node_id),
            refreshed_channel,
            OriginInboundSequence(2),
            envelope_claims(1),
            message_payload(),
        )
        .expect("not-owner no-effect path must allow refreshed-owner retry");
    assert_eq!(retried.sequence, OrderedRelaySequence::FIRST);
}
#[tokio::test]
async fn relay_lookup_miss_rolls_back_unseen_sender_sequence() {
    let bridge = Arc::new(OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    ));
    let channel = envelope().channel;
    {
        let mut sender = bridge.sender_state.lock().await;
        let first = sender
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                channel.clone(),
                OriginInboundSequence(1),
                envelope_claims(0),
                message_payload(),
            )
            .expect("first envelope allocates");
        assert_eq!(first.sequence, OrderedRelaySequence::FIRST);
    }

    let outcome = OrderedRelayDeliveryBridge::finish_prepared_delivery_result(
        Arc::clone(&bridge),
        PreparedRemoteDelivery {
            services: Arc::new(
                services_with_claims(
                    origin_identity(),
                    receiver_identity(),
                    receiver_identity(),
                    test_peer_id(),
                )
                .await,
            ),
            target_entity: target_entity(),
            previous_owner: receiver_identity(),
            channel: channel.clone(),
            envelope: envelope(),
            target: jid::Jid::from(target_full()),
            stanza: Stanza::Message(Message::new(Some(jid::Jid::from(target_full())))),
            is_iq: false,
        },
        Err(RelayAskError::NotFound {
            node_id: NodeId::new(receiver_identity().node_id),
        }),
    )
    .await;
    assert!(
        outcome.is_none(),
        "relay lookup miss must let the caller continue normal fallback"
    );

    let next = bridge
        .sender_state
        .lock()
        .await
        .next_envelope(
            NodeId::new(origin_identity().node_id),
            channel,
            OriginInboundSequence(2),
            envelope_claims(0),
            message_payload(),
        )
        .expect("lookup miss must leave the ordered channel usable");
    assert_eq!(next.sequence, OrderedRelaySequence::FIRST);
}
#[tokio::test]
async fn relay_lookup_miss_retries_established_channel_at_missed_sequence() {
    let bridge = Arc::new(OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    ));
    let channel = envelope().channel;
    let mut receiver = crate::clustering::ordered_relay::OrderedRelayReceiverState::default();
    let first = bridge
        .sender_state
        .lock()
        .await
        .next_envelope(
            NodeId::new(origin_identity().node_id),
            channel.clone(),
            OriginInboundSequence(1),
            envelope_claims(0),
            message_payload(),
        )
        .expect("first envelope allocates");
    let reserved = match receiver.reserve(first) {
        crate::clustering::ordered_relay::OrderedRelayReservation::Reserved(reserved) => reserved,
        other => panic!("first envelope should reserve, got {other:?}"),
    };
    assert!(matches!(
        receiver.commit_reserved(*reserved),
        OrderedRelayReply::Ack(_)
    ));

    let missed = bridge
        .sender_state
        .lock()
        .await
        .next_envelope(
            NodeId::new(origin_identity().node_id),
            channel.clone(),
            OriginInboundSequence(2),
            envelope_claims(0),
            message_payload(),
        )
        .expect("second envelope allocates");
    assert_eq!(missed.sequence, OrderedRelaySequence(2));

    let outcome = OrderedRelayDeliveryBridge::finish_prepared_delivery_result(
        Arc::clone(&bridge),
        PreparedRemoteDelivery {
            services: Arc::new(
                services_with_claims(
                    origin_identity(),
                    receiver_identity(),
                    receiver_identity(),
                    test_peer_id(),
                )
                .await,
            ),
            target_entity: target_entity(),
            previous_owner: receiver_identity(),
            channel: channel.clone(),
            envelope: missed,
            target: jid::Jid::from(target_full()),
            stanza: Stanza::Message(Message::new(Some(jid::Jid::from(target_full())))),
            is_iq: false,
        },
        Err(RelayAskError::NotFound {
            node_id: NodeId::new(receiver_identity().node_id),
        }),
    )
    .await;
    assert!(outcome.is_none());

    let retry = bridge
        .sender_state
        .lock()
        .await
        .next_envelope(
            NodeId::new(origin_identity().node_id),
            channel,
            OriginInboundSequence(3),
            envelope_claims(0),
            message_payload(),
        )
        .expect("lookup miss must retry at the missed sequence");
    assert_eq!(retry.sequence, OrderedRelaySequence(2));
    assert!(matches!(
        receiver.reserve(retry),
        crate::clustering::ordered_relay::OrderedRelayReservation::Reserved(_)
    ));
}
#[tokio::test]
async fn in_flight_nack_suppresses_fallback_without_join_repair() {
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let channel = envelope().channel;
    let outcome = OrderedRelayDeliveryBridge::finish_prepared_delivery_result(
        Arc::clone(&bridge),
        PreparedRemoteDelivery {
            services: Arc::new(
                services_with_claims(
                    origin_identity(),
                    receiver_identity(),
                    receiver_identity(),
                    test_peer_id(),
                )
                .await,
            ),
            target_entity: target_entity(),
            previous_owner: receiver_identity(),
            channel: channel.clone(),
            envelope: envelope(),
            target: jid::Jid::from(target_full()),
            stanza: Stanza::Message(Message::new(Some(jid::Jid::from(target_full())))),
            is_iq: false,
        },
        Ok(OrderedRelayReply::Nack(OrderedRelayNack {
            channel,
            sequence: OrderedRelaySequence::FIRST,
            reason: OrderedRelayNackReason::InFlight,
        })),
    )
    .await
    .expect("InFlight is an attempted delivery outcome");

    assert_eq!(outcome.delivery, FullJidDeliveryOutcome::Dropped);
    assert!(outcome.maybe_committed);
    assert!(
        !outcome.join_repair_allowed,
        "duplicate pending receiver effect must not race MUC join repair"
    );
}
/// #1597: an `UnsupportedEnvelope` NACK (an old peer that does not
/// know the versioned ordered-relay message id — provably no
/// handler ran) must roll back the unconsumed sequence and keep
/// the channel. No sticky diversion, and the next envelope on the
/// channel reuses the rolled-back sequence, so a mixed-version
/// window degrades to per-operation failures instead of silently
/// dropping the channel's later traffic.
#[tokio::test]
async fn unsupported_envelope_nack_rolls_back_and_keeps_the_channel() {
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let channel = envelope().channel;
    let allocated = bridge
        .sender_state
        .lock()
        .await
        .next_envelope(
            NodeId::new(origin_identity().node_id),
            channel.clone(),
            OriginInboundSequence(1),
            envelope_claims(1),
            message_payload(),
        )
        .expect("fresh channel allocates");
    assert_eq!(allocated.sequence, OrderedRelaySequence::FIRST);

    let outcome = OrderedRelayDeliveryBridge::finish_prepared_delivery_result(
        Arc::clone(&bridge),
        PreparedRemoteDelivery {
            services: Arc::new(
                services_with_claims(
                    origin_identity(),
                    receiver_identity(),
                    receiver_identity(),
                    test_peer_id(),
                )
                .await,
            ),
            target_entity: target_entity(),
            previous_owner: receiver_identity(),
            channel: channel.clone(),
            envelope: allocated.clone(),
            target: jid::Jid::from(target_full()),
            stanza: Stanza::Message(Message::new(Some(jid::Jid::from(target_full())))),
            is_iq: false,
        },
        Ok(OrderedRelayReply::Nack(OrderedRelayNack {
            channel: channel.clone(),
            sequence: allocated.sequence,
            reason: OrderedRelayNackReason::UnsupportedEnvelope,
        })),
    )
    .await
    .expect("UnsupportedEnvelope is an attempted delivery outcome");

    assert_eq!(outcome.delivery, FullJidDeliveryOutcome::Dropped);
    assert!(
        !outcome.maybe_committed,
        "UnknownMessage proves no handler ran"
    );

    let retry = bridge
        .sender_state
        .lock()
        .await
        .next_envelope(
            NodeId::new(origin_identity().node_id),
            channel,
            OriginInboundSequence(2),
            envelope_claims(1),
            message_payload(),
        )
        .expect("the channel must stay undiverted");
    assert_eq!(
        retry.sequence,
        OrderedRelaySequence::FIRST,
        "the unconsumed sequence must be rolled back and reused"
    );
}
#[tokio::test]
async fn same_owner_target_not_owner_nack_diverts_rejected_channel() {
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        origin_identity(),
        test_peer_id(),
    )
    .await;
    let nack = OrderedRelayNack {
        channel: envelope().channel,
        sequence: OrderedRelaySequence(5),
        reason: OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target,
        },
    };
    let (outcome, action, maybe_committed) = outcome_for_nack(
        &services,
        &target_entity(),
        &receiver_identity(),
        &nack,
        true,
    )
    .await;
    assert_eq!(outcome, Some(FullJidDeliveryOutcome::Unavailable));
    assert!(!maybe_committed);
    assert_eq!(
        action,
        NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner)
    );

    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let channel = envelope().channel;
    bridge.apply_nack_channel_action(&envelope(), action).await;

    let diverted = bridge
        .sender_state
        .lock()
        .await
        .next_envelope(
            NodeId::new(origin_identity().node_id),
            channel,
            OriginInboundSequence(6),
            envelope_claims(1),
            message_payload(),
        )
        .expect_err("same-owner claim churn must divert the rejected channel");
    assert_eq!(diverted.reason, OrderedRelayDiversionReason::NotOwner);
}
#[test]
fn handoff_completion_synthesizes_iq_fallback_only_for_unavailable() {
    let mut iq = xmpp_parsers::iq::Iq::from_get("iq-1", xmpp_parsers::ping::Ping);
    *iq.to_mut() = Some(jid::Jid::from(target_full()));
    let stanza = Stanza::Iq(Box::new(iq));

    assert_eq!(
        replies_for_origin_handoff(&stanza, FullJidDeliveryOutcome::Delivered).len(),
        0
    );
    assert_eq!(
        replies_for_origin_handoff(&stanza, FullJidDeliveryOutcome::Dropped).len(),
        0
    );
    assert_eq!(
        replies_for_origin_handoff(&stanza, FullJidDeliveryOutcome::Unavailable).len(),
        1
    );
}
#[test]
fn muc_join_maybe_committed_keeps_join_specific_outcome() {
    assert!(matches!(
        muc_proxy_result_to_ordered_outcome(
            OrderedRelayMucProxyKind::JoinPresence,
            Err(OrderedRelayNackReason::MaybeCommitted)
        ),
        OrderedRelayMucProxyOutcome::JoinMaybeCommitted
    ));
    assert!(matches!(
        muc_proxy_result_to_ordered_outcome(
            OrderedRelayMucProxyKind::OccupantPresence,
            Err(OrderedRelayNackReason::MaybeCommitted)
        ),
        OrderedRelayMucProxyOutcome::MaybeCommitted
    ));

    let room_jid: jid::BareJid = "room@muc.example.test".parse().expect("room jid");
    let target = RemoteResourceRouteTarget::MucProxy {
        room_jid,
        kind: OrderedRelayMucProxyKind::JoinPresence,
        stanza: RemoteStanza(Stanza::Presence(xmpp_parsers::presence::Presence::new(
            xmpp_parsers::presence::Type::None,
        ))),
    };
    let maybe_committed = RelayAskError::Send {
        failure: RelaySendFailure::ReplyTimeout,
        effect: RelaySendEffect::MaybeCommitted,
        message: "reply timeout after enqueue".to_string(),
    };
    assert!(matches!(
        remote_resource_muc_ask_error_outcome(&target, &maybe_committed),
        OrderedRelayMucProxyOutcome::JoinMaybeCommitted
    ));
}
#[test]
fn iq_ask_error_classifier_falls_back_only_for_definite_no_effect_failures() {
    let not_found = RelayAskError::NotFound {
        node_id: NodeId::new("missing-node".to_string()),
    };
    assert!(ask_error_allows_target_refresh(&not_found));
    assert_eq!(outcome_for_ask_error(&not_found, true), None);
    let mailbox_full = RelayAskError::Send {
        failure: RelaySendFailure::MailboxFull,
        effect: RelaySendEffect::NoEffect,
        message: "mailbox full".to_string(),
    };
    assert!(ask_error_allows_target_refresh(&mailbox_full));
    assert_eq!(
        outcome_for_ask_error(&mailbox_full, true),
        Some(FullJidDeliveryOutcome::Unavailable)
    );
    assert_eq!(channel_diversion_for_ask_error(&not_found), None);
    assert_eq!(
        channel_diversion_for_ask_error(&mailbox_full),
        Some(OrderedRelayDiversionReason::Backpressure)
    );
    let stale_ref = RelayAskError::Send {
        failure: RelaySendFailure::StaleRef,
        effect: RelaySendEffect::NoEffect,
        message: "actor not running before enqueue".to_string(),
    };
    assert!(ask_error_allows_target_refresh(&stale_ref));
    assert_eq!(
        outcome_for_ask_error(&stale_ref, true),
        Some(FullJidDeliveryOutcome::Unavailable)
    );
    let reply_timeout = RelayAskError::Send {
        failure: RelaySendFailure::ReplyTimeout,
        effect: RelaySendEffect::MaybeCommitted,
        message: "reply timeout".to_string(),
    };
    assert!(!ask_error_allows_target_refresh(&reply_timeout));
    assert_eq!(
        outcome_for_ask_error(&reply_timeout, true),
        Some(FullJidDeliveryOutcome::MaybeCommitted)
    );
    let codec_after_handler = RelayAskError::Send {
        failure: RelaySendFailure::Codec,
        effect: RelaySendEffect::MaybeCommitted,
        message: "reply codec failed after handler".to_string(),
    };
    assert!(!ask_error_allows_target_refresh(&codec_after_handler));
    assert_eq!(
        outcome_for_ask_error(&codec_after_handler, true),
        Some(FullJidDeliveryOutcome::MaybeCommitted)
    );
    assert!(!ask_error_allows_target_refresh(&RelayAskError::Cancelled));
    assert_eq!(
        channel_diversion_for_ask_error(&RelayAskError::Cancelled),
        Some(OrderedRelayDiversionReason::Unreachable)
    );
}
