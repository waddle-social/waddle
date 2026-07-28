use super::*;
use crate::clustering::claims::NodeLeaseStore;
use crate::clustering::ordered_relay::{OrderedRelayDiversionReason, OrderedRelaySequence};
use crate::clustering::route_bridge::OrderedRelayDeliveryServices;
use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};

#[test]
fn relay_name_is_node_scoped() {
    let (a, b) = (
        NodeId::new("node-1".to_string()),
        NodeId::new("node-2".to_string()),
    );
    assert_eq!(relay_name(&a), "waddle-relay/node-1");
    assert_ne!(relay_name(&a), relay_name(&b));
}
#[tokio::test]
async fn ordered_delivery_timeout_aborts_reserved_effect_before_commit() {
    use crate::config::ClusteringMessagingConfig;
    use kameo::actor::Spawn;
    use waddle_xmpp::registry::{ConnectionRegistry, UserRegistryActor};
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let config = ClusteringMessagingConfig {
        reply_timeout: Duration::from_millis(40),
        mailbox_timeout: Duration::from_millis(40),
        ..ClusteringMessagingConfig::default()
    };
    let bridge = OrderedRelayDeliveryBridge::new(CancellationToken::new(), &config);
    bridge.wire(Arc::new(OrderedRelayDeliveryServices {
        claim_store: Arc::new(HangingClaimStore),
        allowlist_store: Arc::new(NoopAllowlist),
        node_lease: Arc::new(NoopNodeLease),
        node_identity: SharedNodeIdentity::new(NodeIdentity::new("receiver", "epoch")),
        connection_registry: Arc::new(ConnectionRegistry::new()),
        user_registry: UserRegistryActor::spawn(UserRegistryActor::new()),
        sm_session_registry: Arc::new(InMemorySmSessionRegistry::new()),
        blocking_storage: Arc::new(InMemoryBlockingStorage::new()),
        web_socket_state: std::sync::Weak::new(),
    }));

    let receiver = Arc::new(Mutex::new(OrderedRelayReceiverState::default()));
    let envelope = timeout_envelope();
    let reservation = receiver.lock().await.reserve(envelope.clone());
    assert!(matches!(reservation, OrderedRelayReservation::Reserved(_)));

    let started = std::time::Instant::now();
    let reply =
        finish_ordered_reservation(Arc::clone(&receiver), Arc::clone(&bridge), reservation).await;
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "reserved delivery timeout should bound a hung validation effect"
    );
    match reply {
        OrderedRelayReply::Nack(nack) => {
            assert_eq!(nack.sequence, OrderedRelaySequence::FIRST);
            assert_eq!(nack.reason, OrderedRelayNackReason::MaybeCommitted);
        }
        OrderedRelayReply::Ack(_) => panic!("hung validation must not commit an ACK"),
    }

    let retry = {
        let mut receiver = receiver.lock().await;
        receiver.reserve(envelope)
    };
    match retry {
        OrderedRelayReservation::Completed(OrderedRelayReply::Nack(nack)) => match nack.reason {
            OrderedRelayNackReason::Diverted(diversion) => {
                assert_eq!(
                    diversion.reason,
                    OrderedRelayDiversionReason::MaybeCommitted
                );
            }
            other => panic!("expected diverted retry after timeout, got {other:?}"),
        },
        other => panic!("timeout must clear pending reservation and divert channel: {other:?}"),
    }
}
/// Council-adjudicated FIX 2: a slow force-detach wait must not
/// head-of-line-block this node's relay mailbox. Registers one live
/// connection whose force-detach ack is deliberately delayed (standing
/// in for a wedged/slow connection task), asks the relay to
/// `RelayResumeSteal` it, and — WITHOUT awaiting that ask first —
/// concurrently asks the SAME relay actor `RelayPing`. Before the
/// `Context::spawn` delegated-reply fix, kameo's strictly-sequential
/// per-actor mailbox meant the ping could not even be dequeued until
/// the resume-steal handler's own inline await finished; the fix under
/// test frees the mailbox immediately, so the ping must resolve long
/// before the slow ack does.
#[tokio::test]
async fn slow_force_detach_does_not_delay_a_concurrent_relay_ping() {
    use kameo::actor::Spawn;
    use waddle_xmpp::pending_delivery::SmSessionId;
    use waddle_xmpp::registry::{ConnectionRegistry, ForceDetachOutcome};

    let jid: jid::FullJid = "alice@example.com/phone".parse().expect("valid full jid");
    let requester: jid::BareJid = "alice@example.com".parse().expect("valid bare jid");
    let stream_id = SmSessionId::new("stream-slow-detach");

    let registry = Arc::new(ConnectionRegistry::new());
    let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(1);
    registry.register(jid.clone(), outbound_tx);
    registry.set_sm_stream_id(&jid, Some(stream_id.clone()));

    // Simulate a slow/wedged connection: receive the force-detach
    // request, signal that the resume-steal handler has provably
    // reached its blocking wait, then hold the ack until the test
    // releases it. Explicit synchronization instead of sleeps: the
    // `started` gate proves the handler is in-flight before the ping
    // is issued, and the `release` gate keeps the ack pending for
    // exactly as long as the measurement needs.
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entry = registry.get_entry(&jid).expect("entry was just registered");
    let mut force_detach_rx = entry
        .take_force_detach_rx()
        .expect("receiver is available exactly once");
    tokio::spawn(async move {
        if let Some(request) = force_detach_rx.recv().await {
            let _ = started_tx.send(());
            let _ = release_rx.await;
            let _ = request.ack.send(ForceDetachOutcome::Detached);
        }
    });

    let resume_bridge = ResumeStealBridge::new();
    resume_bridge.wire(Arc::clone(&registry));
    let actor_ref: kameo::actor::ActorRef<RelayActor> = RelayActor::spawn(RelayActor::new(
        NodeId::new("node-under-test".to_string()),
        false,
        resume_bridge,
        RoomLocalClaims::new(),
        OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &crate::config::ClusteringMessagingConfig::default(),
        ),
    ));

    // Dispatch the resume-steal ask on its own task so it is genuinely
    // in flight — actually sent into the actor's mailbox and its
    // handler actually invoked — concurrently with the ping ask below,
    // rather than merely constructed-but-unpolled.
    let resume_steal_handle = tokio::spawn({
        let actor_ref = actor_ref.clone();
        async move {
            actor_ref
                .ask(RelayResumeSteal {
                    stream_id,
                    requester_bare_jid: requester,
                    trace: RelayTraceContext::default(),
                })
                .await
        }
    });
    // Wait until the resume-steal handler has provably reached its
    // force-detach wait: `started_rx` fires only after the registry
    // delivered the force-detach request, so the ask is genuinely
    // in-flight and blocked — not merely constructed-but-unpolled.
    tokio::time::timeout(Duration::from_secs(5), started_rx)
        .await
        .expect("resume-steal handler must reach its force-detach wait")
        .expect("force-detach receiver task must signal it started");

    // The ack gate is still closed here, so the resume-steal ask is
    // still pending by construction while the ping is measured.
    let ping_result =
        tokio::time::timeout(Duration::from_millis(500), actor_ref.ask(RelayPing)).await;
    assert!(
        ping_result.is_ok(),
        "RelayPing must resolve well within 500ms while the \
         RelayResumeSteal force-detach ack is provably still pending"
    );

    // Release the ack only now that the ping assertion is done, then
    // let the resume-steal ask complete so the test doesn't leak the
    // background task; confirms the eventual reply is still correct
    // once the ack lands.
    release_tx
        .send(())
        .expect("force-detach task is still waiting on the release gate");
    let resume_steal_reply = resume_steal_handle
        .await
        .expect("resume-steal task did not panic")
        .expect("resume-steal ask succeeds");
    assert_eq!(resume_steal_reply, RelayResumeStealReply::Detached);
}
