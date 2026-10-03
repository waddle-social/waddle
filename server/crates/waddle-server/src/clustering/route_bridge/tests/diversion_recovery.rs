//! #1623: a failed ordered send must not silence its channel. Definite
//! no-effect failures leave the channel undiverted, maybe-committed ones
//! divert it only for the cooldown, and a `Gap` NACK resynchronizes the
//! sender onto the receiver's expected sequence.

use super::*;
use crate::clustering::ordered_relay::{
    OrderedRelayReceiverState, OrderedRelayReservation, OrderedRelayReservedEnvelope,
    ORDERED_RELAY_DIVERSION_COOLDOWN,
};
use crate::clustering::relay::{RelaySendEffect, RelaySendFailure};
use std::collections::VecDeque;

tokio::task_local! {
    pub(crate) static TEST_ORDERED_RELAY: Arc<ScriptedOrderedRelay>;
}

/// One scripted reply that overrides the in-memory receiver.
pub(crate) enum ScriptedStep {
    /// Fail the ask before the receiver sees the envelope.
    AskError(RelayAskError),
    /// Reject before reserving, as ownership validation and gap checks do.
    Nack(OrderedRelayNackReason),
    /// Reserve the envelope, then abort it as its effect would have.
    AbortReserved(OrderedRelayNackReason),
    /// The effect commits, but its acknowledgement is lost in transport.
    CommitThenLoseReply,
    /// The effect is still running when the sender times out.
    HoldReservedThenLoseReply,
}

/// Fake remote owner: a real receiver state plus scripted failures.
#[derive(Default)]
pub(crate) struct ScriptedOrderedRelay {
    receiver: Mutex<OrderedRelayReceiverState>,
    script: Mutex<VecDeque<ScriptedStep>>,
    sequences: Mutex<Vec<u64>>,
    destinations: Mutex<Vec<NodeIdentity>>,
    pending: Mutex<Option<OrderedRelayReservedEnvelope>>,
}

impl ScriptedOrderedRelay {
    fn with_script(steps: impl IntoIterator<Item = ScriptedStep>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(steps.into_iter().collect()),
            ..Self::default()
        })
    }

    pub(crate) async fn deliver(
        &self,
        owner: &NodeIdentity,
        envelope: RemoteStanzaEnvelope,
    ) -> Result<OrderedRelayReply, RelayAskError> {
        let proof = envelope.origin_proof.as_ref().expect("signed envelope");
        let public_key = libp2p::identity::PublicKey::try_decode_protobuf(&proof.public_key)
            .expect("valid origin public key");
        assert!(public_key.verify(
            &envelope.signing_bytes().expect("envelope signing bytes"),
            &proof.signature,
        ));
        self.destinations.lock().await.push(owner.clone());
        self.sequences.lock().await.push(envelope.sequence.0);
        let abort = match self.script.lock().await.pop_front() {
            Some(ScriptedStep::AskError(error)) => return Err(error),
            Some(ScriptedStep::Nack(reason)) => {
                return Ok(OrderedRelayReply::Nack(OrderedRelayNack {
                    channel: envelope.channel,
                    sequence: envelope.sequence,
                    reason,
                }));
            }
            Some(ScriptedStep::CommitThenLoseReply) => {
                let mut receiver = self.receiver.lock().await;
                let OrderedRelayReservation::Reserved(reserved) = receiver.reserve(envelope) else {
                    panic!("the timed-out envelope must reserve successfully");
                };
                assert!(matches!(
                    receiver.commit_reserved(*reserved),
                    OrderedRelayReply::Ack(_)
                ));
                return Err(ask_error(
                    RelaySendFailure::ReplyTimeout,
                    RelaySendEffect::MaybeCommitted,
                ));
            }
            Some(ScriptedStep::HoldReservedThenLoseReply) => {
                let mut receiver = self.receiver.lock().await;
                let OrderedRelayReservation::Reserved(reserved) = receiver.reserve(envelope) else {
                    panic!("the timed-out envelope must reserve successfully");
                };
                *self.pending.lock().await = Some(*reserved);
                return Err(ask_error(
                    RelaySendFailure::ReplyTimeout,
                    RelaySendEffect::MaybeCommitted,
                ));
            }
            Some(ScriptedStep::AbortReserved(reason)) => Some(reason),
            None => None,
        };
        let mut receiver = self.receiver.lock().await;
        Ok(match (abort, receiver.reserve(envelope)) {
            (_, OrderedRelayReservation::Completed(reply)) => reply,
            (Some(reason), OrderedRelayReservation::Reserved(reserved)) => {
                receiver.abort_reserved(*reserved, reason)
            }
            (None, OrderedRelayReservation::Reserved(reserved)) => {
                receiver.commit_reserved(*reserved)
            }
        })
    }
}

async fn sender_bridge() -> (
    Arc<OrderedRelayDeliveryBridge>,
    Arc<OrderedRelayDeliveryServices>,
) {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            origin_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire_origin_signer(libp2p::identity::Keypair::generate_ed25519());
    bridge.wire(Arc::clone(&services));
    (bridge, services)
}

async fn seed(services: &Arc<OrderedRelayDeliveryServices>) -> RemoteDeliverySeed {
    let envelope = envelope_for_services(services).await;
    let OrderedRelayPayload::Message { stanza, .. } = &envelope.payload else {
        unreachable!("the test envelope carries a message");
    };
    RemoteDeliverySeed {
        ingress_append_context: None,
        services: Arc::clone(services),
        target_entity: target_entity(),
        previous_owner: receiver_identity(),
        channel: envelope.channel.clone(),
        asserted_origin_node: envelope.asserted_origin_node.clone(),
        origin_inbound_sequence: envelope.origin_inbound_sequence,
        origin_claim: envelope.origin_claim.clone(),
        sender_claim: envelope.sender_claim.clone(),
        target_claim: envelope.target_claim.clone(),
        stanza: stanza.0.clone(),
        payload: envelope.payload,
        target: jid::Jid::from(target_full()),
        is_iq: false,
    }
}

async fn send(
    bridge: &Arc<OrderedRelayDeliveryBridge>,
    services: &Arc<OrderedRelayDeliveryServices>,
) -> FullJidDeliveryOutcome {
    send_with_inbound(bridge, services, OriginInboundSequence(1)).await
}

async fn send_with_inbound(
    bridge: &Arc<OrderedRelayDeliveryBridge>,
    services: &Arc<OrderedRelayDeliveryServices>,
    inbound: OriginInboundSequence,
) -> FullJidDeliveryOutcome {
    let mut next = seed(services).await;
    next.origin_inbound_sequence = inbound;
    let outcome = Arc::clone(bridge)
        .deliver_seeded_remote(next, false)
        .await
        .expect("ordered relay attempted");
    caller_delivery_outcome(outcome)
}

fn ask_error(failure: RelaySendFailure, effect: RelaySendEffect) -> RelayAskError {
    RelayAskError::Send {
        failure,
        effect,
        message: "scripted".to_string(),
    }
}

#[tokio::test]
async fn gap_nack_resends_once_at_the_receiver_expectation() {
    let (bridge, services) = sender_bridge().await;
    let lost = seed(&services).await;
    {
        let mut sender = bridge.sender_state.lock().await;
        for _ in 0..2 {
            sender
                .next_envelope(
                    lost.asserted_origin_node.clone(),
                    lost.channel.clone(),
                    lost.origin_inbound_sequence,
                    OrderedRelayEnvelopeClaims::new(
                        lost.origin_claim.clone(),
                        lost.sender_claim.clone(),
                        lost.target_claim.clone(),
                    ),
                    lost.payload.clone(),
                )
                .expect("allocate a sequence the receiver never saw");
        }
    }
    let relay = ScriptedOrderedRelay::with_script([]);

    let outcome = TEST_ORDERED_RELAY
        .scope(Arc::clone(&relay), send(&bridge, &services))
        .await;

    assert_eq!(outcome, FullJidDeliveryOutcome::Delivered);
    assert_eq!(*relay.sequences.lock().await, vec![3, 1]);
}

#[tokio::test]
async fn no_effect_send_failure_rolls_back_without_diverting() {
    let (bridge, services) = sender_bridge().await;
    let relay = ScriptedOrderedRelay::with_script([ScriptedStep::AskError(ask_error(
        RelaySendFailure::Transport,
        RelaySendEffect::NoEffect,
    ))]);

    let (failed, retried) = TEST_ORDERED_RELAY
        .scope(Arc::clone(&relay), async {
            (
                send(&bridge, &services).await,
                send(&bridge, &services).await,
            )
        })
        .await;

    assert_eq!(failed, FullJidDeliveryOutcome::Dropped);
    assert_eq!(retried, FullJidDeliveryOutcome::Delivered);
    assert_eq!(*relay.sequences.lock().await, vec![1, 1]);
}

#[tokio::test]
async fn target_unavailable_leaves_both_ends_undiverted_for_the_rebound_resource() {
    let (bridge, services) = sender_bridge().await;
    let relay = ScriptedOrderedRelay::with_script([ScriptedStep::AbortReserved(
        OrderedRelayNackReason::TargetUnavailable,
    )]);

    let (offline, rebound) = TEST_ORDERED_RELAY
        .scope(Arc::clone(&relay), async {
            (
                send(&bridge, &services).await,
                send(&bridge, &services).await,
            )
        })
        .await;

    assert_eq!(offline, FullJidDeliveryOutcome::Unavailable);
    assert_eq!(rebound, FullJidDeliveryOutcome::Delivered);
    assert_eq!(*relay.sequences.lock().await, vec![1, 1]);
}

#[tokio::test]
async fn maybe_committed_failure_diverts_only_until_the_cooldown_ends() {
    let (bridge, services) = sender_bridge().await;
    tokio::time::pause();
    let relay = ScriptedOrderedRelay::with_script([ScriptedStep::AskError(ask_error(
        RelaySendFailure::ReplyTimeout,
        RelaySendEffect::MaybeCommitted,
    ))]);

    let (uncertain, suppressed, recovered) = TEST_ORDERED_RELAY
        .scope(Arc::clone(&relay), async {
            let uncertain = send(&bridge, &services).await;
            let suppressed = send(&bridge, &services).await;
            tokio::time::advance(ORDERED_RELAY_DIVERSION_COOLDOWN).await;
            (uncertain, suppressed, send(&bridge, &services).await)
        })
        .await;

    assert_eq!(uncertain, FullJidDeliveryOutcome::MaybeCommitted);
    assert_eq!(suppressed, FullJidDeliveryOutcome::Dropped);
    assert_eq!(recovered, FullJidDeliveryOutcome::Delivered);
    // The uncertain envelope never reached the receiver, so the recovered
    // send learns `Gap { expected: 1 }` and is resent there.
    assert_eq!(*relay.sequences.lock().await, vec![1, 2, 1]);
}

async fn move_target_owner(services: &OrderedRelayDeliveryServices) -> ClaimEpoch {
    let claim = services
        .claim_store
        .current_claim(&target_entity())
        .await
        .expect("claim lookup")
        .expect("target claim");
    services
        .claim_store
        .release(&target_entity(), &claim.owner, claim.claim_epoch)
        .await
        .expect("release old owner");
    services
        .claim_store
        .acquire(&target_entity(), &other_identity())
        .await
        .expect("acquire new owner")
}

#[tokio::test]
async fn gap_resend_target_not_owner_refreshes_once() {
    let (bridge, services) = sender_bridge().await;
    let stale = seed(&services).await;
    let refreshed_epoch = move_target_owner(&services).await;
    let relay = ScriptedOrderedRelay::with_script([
        ScriptedStep::Nack(OrderedRelayNackReason::Gap {
            expected: OrderedRelaySequence(3),
        }),
        ScriptedStep::Nack(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target,
        }),
    ]);

    let outcome = TEST_ORDERED_RELAY
        .scope(
            Arc::clone(&relay),
            Arc::clone(&bridge).deliver_seeded_remote(stale, true),
        )
        .await
        .expect("ordered relay attempted");

    assert_eq!(outcome.delivery, FullJidDeliveryOutcome::Delivered);
    assert_eq!(outcome.relay_target, Some(other_identity()));
    assert_eq!(
        outcome.target_claim.expect("target claim").epoch,
        refreshed_epoch
    );
    assert_eq!(*relay.sequences.lock().await, vec![1, 3, 1]);
    assert_eq!(
        *relay.destinations.lock().await,
        vec![receiver_identity(), receiver_identity(), other_identity()]
    );
}

#[tokio::test]
async fn gap_resend_no_effect_error_refreshes_once() {
    let (bridge, services) = sender_bridge().await;
    let stale = seed(&services).await;
    let refreshed_epoch = move_target_owner(&services).await;
    let relay = ScriptedOrderedRelay::with_script([
        ScriptedStep::Nack(OrderedRelayNackReason::Gap {
            expected: OrderedRelaySequence(3),
        }),
        ScriptedStep::AskError(ask_error(
            RelaySendFailure::Transport,
            RelaySendEffect::NoEffect,
        )),
    ]);

    let outcome = TEST_ORDERED_RELAY
        .scope(
            Arc::clone(&relay),
            Arc::clone(&bridge).deliver_seeded_remote(stale, true),
        )
        .await
        .expect("ordered relay attempted");

    assert_eq!(outcome.delivery, FullJidDeliveryOutcome::Delivered);
    assert_eq!(outcome.relay_target, Some(other_identity()));
    assert_eq!(
        outcome.target_claim.expect("target claim").epoch,
        refreshed_epoch
    );
    assert_eq!(*relay.sequences.lock().await, vec![1, 3, 1]);
    assert_eq!(
        *relay.destinations.lock().await,
        vec![receiver_identity(), receiver_identity(), other_identity()]
    );
}

#[tokio::test]
async fn repeated_gap_is_bounded_to_one_resend() {
    let (bridge, services) = sender_bridge().await;
    let relay = ScriptedOrderedRelay::with_script([
        ScriptedStep::Nack(OrderedRelayNackReason::Gap {
            expected: OrderedRelaySequence(3),
        }),
        ScriptedStep::Nack(OrderedRelayNackReason::Gap {
            expected: OrderedRelaySequence(5),
        }),
    ]);

    let outcome = TEST_ORDERED_RELAY
        .scope(
            Arc::clone(&relay),
            Arc::clone(&bridge).deliver_seeded_remote(seed(&services).await, true),
        )
        .await
        .expect("ordered relay attempted");

    assert_eq!(outcome.delivery, FullJidDeliveryOutcome::Dropped);
    assert_eq!(*relay.sequences.lock().await, vec![1, 3]);
}

#[tokio::test]
async fn refreshed_owner_gap_is_resynchronized_once() {
    let (bridge, services) = sender_bridge().await;
    let stale = seed(&services).await;
    let refreshed_epoch = move_target_owner(&services).await;
    let lost = seed(&services).await;
    bridge
        .sender_state
        .lock()
        .await
        .next_envelope(
            lost.asserted_origin_node,
            lost.channel,
            lost.origin_inbound_sequence,
            OrderedRelayEnvelopeClaims::new(
                lost.origin_claim,
                lost.sender_claim,
                lost.target_claim,
            ),
            lost.payload,
        )
        .expect("allocate a sequence the refreshed owner never saw");
    let relay = ScriptedOrderedRelay::with_script([
        ScriptedStep::Nack(OrderedRelayNackReason::Gap {
            expected: OrderedRelaySequence(3),
        }),
        ScriptedStep::Nack(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target,
        }),
    ]);

    let outcome = TEST_ORDERED_RELAY
        .scope(
            Arc::clone(&relay),
            Arc::clone(&bridge).deliver_seeded_remote(stale, true),
        )
        .await
        .expect("ordered relay attempted");

    assert_eq!(outcome.delivery, FullJidDeliveryOutcome::Delivered);
    assert_eq!(outcome.relay_target, Some(other_identity()));
    assert_eq!(
        outcome.target_claim.expect("target claim").epoch,
        refreshed_epoch
    );
    assert_eq!(*relay.sequences.lock().await, vec![1, 3, 2, 1]);
    assert_eq!(
        *relay.destinations.lock().await,
        vec![
            receiver_identity(),
            receiver_identity(),
            other_identity(),
            other_identity()
        ]
    );
}

#[tokio::test]
async fn committed_envelope_with_lost_reply_does_not_replay_after_cooldown() {
    let (bridge, services) = sender_bridge().await;
    tokio::time::pause();
    let relay = ScriptedOrderedRelay::with_script([ScriptedStep::CommitThenLoseReply]);

    let (uncertain, recovered) = TEST_ORDERED_RELAY
        .scope(Arc::clone(&relay), async {
            let uncertain = send(&bridge, &services).await;
            tokio::time::advance(ORDERED_RELAY_DIVERSION_COOLDOWN).await;
            (
                uncertain,
                send_with_inbound(&bridge, &services, OriginInboundSequence(2)).await,
            )
        })
        .await;

    assert_eq!(uncertain, FullJidDeliveryOutcome::MaybeCommitted);
    assert_eq!(recovered, FullJidDeliveryOutcome::Delivered);
    assert_eq!(*relay.sequences.lock().await, vec![1, 2]);
}

#[tokio::test]
async fn recovery_cannot_overtake_a_still_reserved_envelope() {
    let (bridge, services) = sender_bridge().await;
    tokio::time::pause();
    let relay = ScriptedOrderedRelay::with_script([ScriptedStep::HoldReservedThenLoseReply]);

    let (uncertain, blocked, recovered) = TEST_ORDERED_RELAY
        .scope(Arc::clone(&relay), async {
            let uncertain = send(&bridge, &services).await;
            tokio::time::advance(ORDERED_RELAY_DIVERSION_COOLDOWN).await;
            let blocked = send_with_inbound(&bridge, &services, OriginInboundSequence(2)).await;
            let pending = relay.pending.lock().await.take().expect("pending effect");
            assert!(matches!(
                relay.receiver.lock().await.commit_reserved(pending),
                OrderedRelayReply::Ack(_)
            ));
            tokio::time::advance(ORDERED_RELAY_DIVERSION_COOLDOWN).await;
            (
                uncertain,
                blocked,
                send_with_inbound(&bridge, &services, OriginInboundSequence(3)).await,
            )
        })
        .await;

    assert_eq!(uncertain, FullJidDeliveryOutcome::MaybeCommitted);
    assert_eq!(blocked, FullJidDeliveryOutcome::Dropped);
    assert_eq!(recovered, FullJidDeliveryOutcome::Delivered);
    assert_eq!(*relay.sequences.lock().await, vec![1, 2, 1, 2]);
}

#[tokio::test]
async fn gap_resend_no_effect_error_rolls_back_the_resent_sequence() {
    let (bridge, services) = sender_bridge().await;
    let relay = ScriptedOrderedRelay::with_script([
        ScriptedStep::Nack(OrderedRelayNackReason::Gap {
            expected: OrderedRelaySequence(3),
        }),
        ScriptedStep::AskError(ask_error(
            RelaySendFailure::Transport,
            RelaySendEffect::NoEffect,
        )),
    ]);

    let (failed, recovered) = TEST_ORDERED_RELAY
        .scope(Arc::clone(&relay), async {
            let failed = Arc::clone(&bridge)
                .deliver_seeded_remote(seed(&services).await, true)
                .await
                .expect("ordered relay attempted");
            (
                caller_delivery_outcome(failed),
                send(&bridge, &services).await,
            )
        })
        .await;

    assert_eq!(failed, FullJidDeliveryOutcome::Dropped);
    assert_eq!(recovered, FullJidDeliveryOutcome::Delivered);
    assert_eq!(*relay.sequences.lock().await, vec![1, 3, 3, 1]);
}
