//! #1623: a failed ordered send must not silence its channel. Definite
//! no-effect failures leave the channel undiverted, maybe-committed ones
//! divert it only for the cooldown, and a `Gap` NACK resynchronizes the
//! sender onto the receiver's expected sequence.

use super::*;
use crate::clustering::ordered_relay::{
    OrderedRelayReceiverState, OrderedRelayReservation, ORDERED_RELAY_DIVERSION_COOLDOWN,
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
    /// Reserve the envelope, then abort it as its effect would have.
    AbortReserved(OrderedRelayNackReason),
}

/// Fake remote owner: a real receiver state plus scripted failures.
#[derive(Default)]
pub(crate) struct ScriptedOrderedRelay {
    receiver: Mutex<OrderedRelayReceiverState>,
    script: Mutex<VecDeque<ScriptedStep>>,
    sequences: Mutex<Vec<u64>>,
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
        envelope: RemoteStanzaEnvelope,
    ) -> Result<OrderedRelayReply, RelayAskError> {
        self.sequences.lock().await.push(envelope.sequence.0);
        let abort = match self.script.lock().await.pop_front() {
            Some(ScriptedStep::AskError(error)) => return Err(error),
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
    let outcome = Arc::clone(bridge)
        .deliver_seeded_remote(seed(services).await, false)
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
