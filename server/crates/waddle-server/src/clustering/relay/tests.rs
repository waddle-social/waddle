use super::*;
use crate::clustering::claims::{NodeLeaseStore, OrphanedSmSessionClaim};
use crate::clustering::ordered_relay::{
    OrderedRelayChannel, OrderedRelayClaim, OrderedRelayDiversionReason, OrderedRelayOrigin,
    OrderedRelayPayload, OrderedRelayRecipient, OrderedRelaySequence, OriginInboundSequence,
};
use crate::clustering::route_bridge::OrderedRelayDeliveryServices;
use async_trait::async_trait;
use libp2p::PeerId;
use std::collections::HashSet;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, EntityType, NodeIdentity,
    ResumeIdentityProof, SharedNodeIdentity, StalePredicate,
};

#[test]
fn relay_name_is_node_scoped() {
    let (a, b) = (
        NodeId::new("node-1".to_string()),
        NodeId::new("node-2".to_string()),
    );
    assert_eq!(relay_name(&a), "waddle-relay/node-1");
    assert_ne!(relay_name(&a), relay_name(&b));
}

struct HangingClaimStore;

#[async_trait]
impl ClaimStore for HangingClaimStore {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        Ok(())
    }

    async fn acquire(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }

    async fn ensure_claimed(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }

    async fn steal_stale(
        &self,
        _entity: &Entity,
        _observed: ClaimEpoch,
        _staleness: StalePredicate,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }

    async fn steal_for_resume(
        &self,
        _entity: &Entity,
        _observed: ClaimEpoch,
        _witness: ResumeIdentityProof,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }

    async fn current_claim(
        &self,
        _entity: &Entity,
    ) -> Result<Option<ClaimSnapshot>, ClaimError> {
        std::future::pending().await
    }

    async fn fence(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
        _mine: ClaimEpoch,
    ) -> Result<bool, ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }

    async fn release(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
        _mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }

    async fn release_many(
        &self,
        _entities: &[Entity],
        _me: &NodeIdentity,
    ) -> Result<(), ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }
}

struct NoopNodeLease;

#[async_trait]
impl NodeLeaseStore for NoopNodeLease {
    async fn list_orphaned_room_actor_claims_page(
        &self,
        _after: Option<crate::clustering::claims::RoomOrphanScanCursor>,
        _limit: usize,
    ) -> Result<crate::clustering::claims::OrphanedRoomActorClaimPage, ClaimError> {
        Ok(crate::clustering::claims::OrphanedRoomActorClaimPage {
            candidates: Vec::new(),
            next_cursor: None,
            has_more: false,
            quarantined: 0,
        })
    }

    async fn register(
        &self,
        _me: &NodeIdentity,
        _pod_template_hash: Option<String>,
    ) -> Result<(), ClaimError> {
        Ok(())
    }

    async fn heartbeat(
        &self,
        _me: &NodeIdentity,
        _lease_ttl: Duration,
    ) -> Result<bool, ClaimError> {
        Ok(true)
    }

    async fn expire(
        &self,
        _owner: &NodeIdentity,
        _lease_ttl: Duration,
    ) -> Result<bool, ClaimError> {
        Ok(true)
    }

    async fn mark_draining(&self, _me: &NodeIdentity) -> Result<(), ClaimError> {
        Ok(())
    }

    async fn count_other_live_nodes(
        &self,
        _me: &NodeIdentity,
        _lease_ttl: Duration,
    ) -> Result<usize, ClaimError> {
        Ok(0)
    }

    async fn reconcile(
        &self,
        _me: &NodeIdentity,
        _locally_owned: &[Entity],
    ) -> Result<Vec<Entity>, ClaimError> {
        Ok(Vec::new())
    }

    async fn report_steal_intent(
        &self,
        _entity: &Entity,
        _reporter: &NodeIdentity,
    ) -> Result<(), ClaimError> {
        Ok(())
    }

    async fn owner_steal_intents(
        &self,
        _me: &NodeIdentity,
    ) -> Result<Vec<(Entity, ClaimEpoch)>, ClaimError> {
        Ok(Vec::new())
    }

    async fn clear_steal_intent(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
        _mine: ClaimEpoch,
    ) -> Result<u64, ClaimError> {
        Ok(0)
    }

    async fn list_orphaned_sm_session_claims(
        &self,
    ) -> Result<Vec<OrphanedSmSessionClaim>, ClaimError> {
        Ok(Vec::new())
    }

    async fn current_generation(&self) -> Result<Option<String>, ClaimError> {
        Ok(None)
    }
}

struct NoopAllowlist;

#[async_trait]
impl crate::clustering::allowlist::AllowlistStore for NoopAllowlist {
    async fn ensure_schema(&self) -> Result<(), crate::clustering::allowlist::AllowlistError> {
        Ok(())
    }

    async fn enrolled_peers(
        &self,
    ) -> Result<HashSet<PeerId>, crate::clustering::allowlist::AllowlistError> {
        Ok(HashSet::new())
    }
}

fn timeout_envelope() -> RemoteStanzaEnvelope {
    use waddle_xmpp::pending_delivery::SmSessionId;
    use xmpp_parsers::message::{Lang, Message};

    let target: jid::FullJid = "timeout@example.test/phone"
        .parse()
        .expect("valid full jid");
    let sender: jid::FullJid = "sender@example.test/laptop"
        .parse()
        .expect("valid full jid");
    let origin_stream = SmSessionId::new("stream-timeout");
    let mut message = Message::new(Some(jid::Jid::from(target.clone())));
    message.from = Some(jid::Jid::from(sender.clone()));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Lang::new(), "timeout test".to_string());

    RemoteStanzaEnvelope {
        asserted_origin_node: NodeId::new("origin-node".to_string()),
        channel: OrderedRelayChannel {
            origin: OrderedRelayOrigin::SmSession(origin_stream.clone()),
            recipient: OrderedRelayRecipient::FullJid(target.clone()),
            target_epoch: ClaimEpoch(0),
        },
        sequence: OrderedRelaySequence::FIRST,
        origin_inbound_sequence: OriginInboundSequence(1),
        origin_claim: OrderedRelayClaim {
            entity: Entity::new(EntityType::SmSession, origin_stream.to_string()),
            epoch: ClaimEpoch(0),
        },
        sender_claim: OrderedRelayClaim {
            entity: Entity::new(EntityType::UserActor, sender.to_bare().to_string()),
            epoch: ClaimEpoch(0),
        },
        target_claim: OrderedRelayClaim {
            entity: Entity::new(EntityType::UserActor, target.to_bare().to_string()),
            epoch: ClaimEpoch(0),
        },
        payload: OrderedRelayPayload::Message {
            recipient: jid::Jid::from(target),
            stanza: RemoteStanza(waddle_xmpp::Stanza::Message(message)),
        },
        origin_proof: None,
    }
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
        finish_ordered_reservation(Arc::clone(&receiver), Arc::clone(&bridge), reservation)
            .await;
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
        OrderedRelayReservation::Completed(OrderedRelayReply::Nack(nack)) => {
            match nack.reason {
                OrderedRelayNackReason::Diverted(diversion) => {
                    assert_eq!(
                        diversion.reason,
                        OrderedRelayDiversionReason::MaybeCommitted
                    );
                }
                other => panic!("expected diverted retry after timeout, got {other:?}"),
            }
        }
        other => panic!("timeout must clear pending reservation and divert channel: {other:?}"),
    }
}

#[test]
fn stale_ref_errors_trigger_relookup_and_others_do_not() {
    assert!(is_stale_ref_error::<std::convert::Infallible>(
        &RemoteSendError::ActorNotRunning
    ));
    assert!(is_stale_ref_error::<std::convert::Infallible>(
        &RemoteSendError::ActorStopped
    ));
    assert!(is_stale_ref_error::<std::convert::Infallible>(
        &RemoteSendError::BadActorType
    ));
    assert!(!is_stale_ref_error::<std::convert::Infallible>(
        &RemoteSendError::ReplyTimeout
    ));
    assert!(!is_stale_ref_error::<std::convert::Infallible>(
        &RemoteSendError::MailboxFull
    ));
}

#[test]
fn no_effect_relookup_excludes_maybe_enqueued_actor_stopped() {
    assert!(is_no_effect_stale_ref_relookup_error::<
        std::convert::Infallible,
    >(&RemoteSendError::ActorNotRunning));
    assert!(is_no_effect_stale_ref_relookup_error::<
        std::convert::Infallible,
    >(&RemoteSendError::BadActorType));
    assert!(!is_no_effect_stale_ref_relookup_error::<
        std::convert::Infallible,
    >(&RemoteSendError::ActorStopped));
    assert!(!is_no_effect_stale_ref_relookup_error::<
        std::convert::Infallible,
    >(&RemoteSendError::ReplyTimeout));
}

/// #1597: an old peer that does not know the versioned ordered-relay
/// message id fails with `UnknownMessage` — provably before any
/// handler ran. That must synthesize the typed `UnsupportedEnvelope`
/// NACK (not `ParseFailure`) so the sender rolls back the unconsumed
/// sequence and keeps the channel instead of installing a sticky
/// diversion shared with unrelated traffic.
#[test]
fn unknown_message_synthesizes_unsupported_envelope_nack() {
    let envelope = timeout_envelope();
    let reply = ordered_send_error::<std::convert::Infallible>(
        &envelope,
        RemoteSendError::UnknownMessage {
            actor_remote_id: "actor".into(),
            message_remote_id: "message".into(),
        },
    )
    .expect("UnknownMessage must synthesize a NACK, not an ask error");
    match reply {
        OrderedRelayReply::Nack(nack) => {
            assert_eq!(nack.reason, OrderedRelayNackReason::UnsupportedEnvelope);
            assert_eq!(nack.sequence, envelope.sequence);
            assert_eq!(nack.channel, envelope.channel);
        }
        OrderedRelayReply::Ack(_) => panic!("UnknownMessage must not ACK"),
    }
}

#[test]
fn unsupported_envelope_excludes_ambiguous_codec_errors() {
    assert!(is_ordered_unsupported_envelope_error::<
        std::convert::Infallible,
    >(&RemoteSendError::UnknownMessage {
        actor_remote_id: "actor".into(),
        message_remote_id: "message".into(),
    }));
    assert!(!is_ordered_unsupported_envelope_error::<
        std::convert::Infallible,
    >(&RemoteSendError::DeserializeMessage(
        String::new()
    )));
    assert!(!is_ordered_unsupported_envelope_error::<
        std::convert::Infallible,
    >(&RemoteSendError::SerializeReply(String::new())));
    assert!(!is_ordered_unsupported_envelope_error::<
        std::convert::Infallible,
    >(&RemoteSendError::SerializeMessage(String::new())));
}

#[test]
fn ask_failures_classify_handler_effect_separately_from_failure_kind() {
    use std::convert::Infallible;
    use RelaySendEffect::{MaybeCommitted, NoEffect};

    for (error, expected) in [
        (RemoteSendError::ActorNotRunning, NoEffect),
        (
            RemoteSendError::UnknownActor {
                actor_remote_id: "actor".into(),
            },
            NoEffect,
        ),
        (RemoteSendError::BadActorType, NoEffect),
        (RemoteSendError::MailboxFull, NoEffect),
        (RemoteSendError::SerializeMessage(String::new()), NoEffect),
        (RemoteSendError::SwarmNotBootstrapped, NoEffect),
        (RemoteSendError::DialFailure, NoEffect),
        (RemoteSendError::UnsupportedProtocols, NoEffect),
        (RemoteSendError::ActorStopped, MaybeCommitted),
        (RemoteSendError::ReplyTimeout, MaybeCommitted),
        (
            RemoteSendError::DeserializeMessage(String::new()),
            MaybeCommitted,
        ),
        (
            RemoteSendError::SerializeReply(String::new()),
            MaybeCommitted,
        ),
        (RemoteSendError::NetworkTimeout, MaybeCommitted),
        (RemoteSendError::ConnectionClosed, MaybeCommitted),
    ] {
        assert_eq!(classify_effect::<Infallible>(&error), expected, "{error:?}");
    }
}

#[test]
fn ask_failures_classify_into_typed_kinds() {
    use std::convert::Infallible;
    for (error, expected) in [
        (RemoteSendError::ActorStopped, RelaySendFailure::StaleRef),
        (RemoteSendError::MailboxFull, RelaySendFailure::MailboxFull),
        (
            RemoteSendError::ReplyTimeout,
            RelaySendFailure::ReplyTimeout,
        ),
        (
            RemoteSendError::SerializeMessage(String::new()),
            RelaySendFailure::Codec,
        ),
        (RemoteSendError::DialFailure, RelaySendFailure::Transport),
        (RemoteSendError::NetworkTimeout, RelaySendFailure::Transport),
    ] {
        assert_eq!(classify::<Infallible>(&error), expected, "{error:?}");
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
    // request but wait well past when the concurrent ping below must
    // already have resolved before acking.
    const ACK_DELAY: Duration = Duration::from_secs(3);
    let entry = registry.get_entry(&jid).expect("entry was just registered");
    let mut force_detach_rx = entry
        .take_force_detach_rx()
        .expect("receiver is available exactly once");
    tokio::spawn(async move {
        if let Some(request) = force_detach_rx.recv().await {
            tokio::time::sleep(ACK_DELAY).await;
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
    // Give the spawned ask a moment to actually reach the actor and
    // start executing: before the fix under test, the handler would
    // still be blocked inline on the (3s) force-detach ack at this
    // point; after the fix, `ctx.spawn` has already returned and the
    // mailbox is free again, well within this margin.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let ping_started = std::time::Instant::now();
    let ping_result =
        tokio::time::timeout(Duration::from_millis(500), actor_ref.ask(RelayPing)).await;
    let ping_elapsed = ping_started.elapsed();

    assert!(
        ping_result.is_ok(),
        "RelayPing must resolve well within 500ms even while a slow \
         RelayResumeSteal ack is still pending"
    );
    assert!(
        ping_elapsed < ACK_DELAY,
        "ping took {ping_elapsed:?}, which is not plausibly faster than the \
         {ACK_DELAY:?} force-detach ack delay — the mailbox was likely blocked"
    );

    // Let the still-pending resume-steal ask complete so the test
    // doesn't leak the background task; confirms the eventual reply is
    // still correct once the slow ack lands.
    let resume_steal_reply = resume_steal_handle
        .await
        .expect("resume-steal task did not panic")
        .expect("resume-steal ask succeeds");
    assert_eq!(resume_steal_reply, RelayResumeStealReply::Detached);
}

fn spawn_test_relay_actor() -> kameo::actor::ActorRef<RelayActor> {
    use kameo::actor::Spawn;
    let resume_bridge = ResumeStealBridge::new();
    resume_bridge.wire(Arc::new(waddle_xmpp::registry::ConnectionRegistry::new()));
    RelayActor::spawn(RelayActor::new(
        NodeId::new("span-test-node".to_string()),
        false,
        resume_bridge,
        RoomLocalClaims::new(),
        OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &crate::config::ClusteringMessagingConfig::default(),
        ),
    ))
}

/// #1594: a re-assert ask against a relay whose delivery bridge has
/// no wired services (this node's `WebSocketState` is unreachable)
/// must answer `Unavailable` — never a fabricated occupancy answer,
/// and never a hang.
#[tokio::test(flavor = "current_thread")]
async fn reassert_media_grants_without_wired_services_answers_unavailable() {
    // Thread-scoped subscriber, not asserted on: a relay ask on a
    // subscriber-less thread destabilizes the interest cache the
    // *_records_the_dispatch_span tests depend on when the tests
    // overlap (pre-existing test-support limitation, observed
    // deterministically pairwise).
    let _spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let actor_ref = spawn_test_relay_actor();

    let reply = actor_ref
        .ask(RelayReassertMediaGrants {
            room: "room@muc.example.com".parse().expect("room jid"),
            participant: "alice@example.com/web".parse().expect("participant jid"),
            trace: RelayTraceContext::default(),
        })
        .await
        .expect("reassert ask succeeds");

    assert_eq!(reply, RelayReassertMediaGrantsReply::Unavailable);
}

/// #1594: the re-assert handler delegates its reply (the owner-side
/// room-actor ask is bounded but slow-able), and delegated work must
/// still run under the named relay dispatch root span (#1483).
#[tokio::test(flavor = "current_thread")]
async fn reassert_media_grants_ask_records_the_dispatch_span() {
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let actor_ref = spawn_test_relay_actor();

    let _reply = actor_ref
        .ask(RelayReassertMediaGrants {
            room: "room@muc.example.com".parse().expect("room jid"),
            participant: "alice@example.com/web".parse().expect("participant jid"),
            trace: RelayTraceContext::default(),
        })
        .await
        .expect("reassert ask succeeds");

    assert_eq!(
        spans
            .recorded_field("clustering.relay.dispatch", "relay.message")
            .as_deref(),
        Some("reassert_media_grants"),
        "reassert handling must run under the named relay dispatch root span"
    );
}

/// #1483: an inbound relay ask handled inline (no delegated reply) must
/// open the named `clustering.relay.dispatch` root span, so the actor
/// work it triggers is parented and survives the #1438 span-noise
/// sampler.
#[tokio::test(flavor = "current_thread")]
async fn inline_relay_ask_records_the_dispatch_span() {
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let actor_ref = spawn_test_relay_actor();

    let reply = actor_ref
        .ask(Demote {
            entity: Entity::new(EntityType::RoomActor, "room@muc.example.com".to_string()),
            new_epoch: ClaimEpoch(7),
            trace: RelayTraceContext::default(),
        })
        .await
        .expect("demote ask succeeds");
    assert_eq!(reply, DemoteReply::Acked);

    assert_eq!(
        spans
            .recorded_field("clustering.relay.dispatch", "relay.message")
            .as_deref(),
        Some("demote"),
        "demote handling must run under the named relay dispatch root span"
    );
}

/// #1483: a delegated-reply relay ask must carry the named dispatch span
/// onto the spawned reply task, so the whole delivery — not just the
/// mailbox slice — is covered by the root span.
#[tokio::test(flavor = "current_thread")]
async fn delegated_relay_ask_records_the_dispatch_span() {
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let actor_ref = spawn_test_relay_actor();

    // No live local connection for the stream: the delegated task
    // resolves quickly with NotLiveLocally.
    let reply = actor_ref
        .ask(RelayResumeSteal {
            stream_id: waddle_xmpp::pending_delivery::SmSessionId::new("span-test-stream"),
            requester_bare_jid: "alice@example.com".parse().expect("valid bare jid"),
            trace: RelayTraceContext::default(),
        })
        .await
        .expect("resume-steal ask succeeds");
    assert_eq!(reply, RelayResumeStealReply::NotLiveLocally);

    assert_eq!(
        spans
            .recorded_field("clustering.relay.dispatch", "relay.message")
            .as_deref(),
        Some("resume_steal"),
        "resume-steal handling must run under the named relay dispatch root span"
    );
    assert_eq!(
        spans
            .recorded_field("clustering.relay.dispatch", "stream_id")
            .as_deref(),
        Some("span-test-stream"),
        "the dispatch span must carry the stream id"
    );
}

/// #1483: `parent: None` is the load-bearing property — the handlers
/// run inside kameo's own suppressed root `actor.handle_message` span,
/// and a child of a locally-unsampled parent is dropped by the #1438
/// sampler too. Pin that the production constructor starts a fresh
/// root even when a span is active.
#[tokio::test(flavor = "current_thread")]
async fn relay_dispatch_span_is_a_root_even_inside_an_active_span() {
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let outer = tracing::info_span!("actor.handle_message");
    let dispatch =
        outer.in_scope(|| relay_dispatch_span("root_check", &RelayTraceContext::default()));
    drop(dispatch);
    drop(outer);

    let exported = spans.exported();
    let dispatch = exported
        .iter()
        .find(|span| span.name == "clustering.relay.dispatch")
        .expect("dispatch span must export");
    assert_eq!(
        dispatch.parent_span_id,
        opentelemetry::trace::SpanId::INVALID,
        "the dispatch span must root a fresh trace, not inherit the \
         active (suppressed) actor span as its parent"
    );
}

/// #1485: when the sending node propagated its W3C trace context, the
/// receiving node's dispatch root must join that trace instead of
/// starting its own — the whole point of the propagation.
#[test]
fn relay_dispatch_span_joins_a_propagated_sender_trace() {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();

    // Sending node: an active span whose context is stamped onto the
    // relay message at the send seam.
    let sender = tracing::info_span!("clustering.relay.send-under-test");
    let (trace, sender_trace_id, sender_span_id) = sender.in_scope(|| {
        let span_context = tracing::Span::current()
            .context()
            .span()
            .span_context()
            .clone();
        (
            RelayTraceContext::capture(),
            span_context.trace_id(),
            span_context.span_id(),
        )
    });
    assert_ne!(
        trace,
        RelayTraceContext::default(),
        "an active, valid sender span must yield a propagatable context"
    );

    // Receiving node: the same context after a real codec round-trip.
    let encoded = rmp_serde::to_vec_named(&trace).expect("trace context encodes");
    let decoded: RelayTraceContext =
        rmp_serde::from_slice(&encoded).expect("trace context decodes");
    let dispatch = relay_dispatch_span("cross_node_check", &decoded);
    drop(dispatch);
    drop(sender);

    let exported = spans.exported();
    let dispatch = exported
        .iter()
        .find(|span| span.name == "clustering.relay.dispatch")
        .expect("dispatch span must export");
    assert_eq!(
        dispatch.span_context.trace_id(),
        sender_trace_id,
        "the receiving node's dispatch span must join the sender's trace"
    );
    assert_eq!(
        dispatch.parent_span_id, sender_span_id,
        "the dispatch span must be parented on the sending span"
    );
}

/// #1485 mixed-version rolling deploy, old sender → new receiver: a
/// relay message encoded WITHOUT the additive trace field must still
/// decode, with an empty context that falls back to a root dispatch
/// span.
#[test]
fn a_relay_message_without_the_trace_field_still_decodes() {
    /// The pre-#1485 wire shape of [`Demote`].
    #[derive(Serialize, Deserialize)]
    struct LegacyDemote {
        entity: Entity,
        new_epoch: ClaimEpoch,
    }

    let entity = Entity::new(EntityType::RoomActor, "room@muc.example.com".to_string());
    let encoded = rmp_serde::to_vec_named(&LegacyDemote {
        entity: entity.clone(),
        new_epoch: ClaimEpoch(11),
    })
    .expect("legacy demote encodes");

    let decoded: Demote = rmp_serde::from_slice(&encoded).expect("legacy demote decodes");
    assert_eq!(decoded.entity, entity);
    assert_eq!(decoded.new_epoch, ClaimEpoch(11));
    assert_eq!(
        decoded.trace,
        RelayTraceContext::default(),
        "an absent trace field must default to no context, not fail the decode"
    );
}

/// #1485 mixed-version rolling deploy, new sender → old receiver: the
/// pre-#1485 decoder must ignore the extra field rather than reject the
/// message (serde's derived `Deserialize` skips unknown map keys, and
/// kameo encodes remote messages as named maps).
#[test]
fn an_older_decoder_ignores_the_added_trace_field() {
    #[derive(Serialize, Deserialize)]
    struct LegacyDemote {
        entity: Entity,
        new_epoch: ClaimEpoch,
    }

    let entity = Entity::new(EntityType::RoomActor, "room@muc.example.com".to_string());
    let encoded = rmp_serde::to_vec_named(&Demote {
        entity: entity.clone(),
        new_epoch: ClaimEpoch(13),
        trace: RelayTraceContext::default(),
    })
    .expect("demote encodes");

    let legacy: LegacyDemote =
        rmp_serde::from_slice(&encoded).expect("pre-#1485 decoder tolerates the new field");
    assert_eq!(legacy.entity, entity);
    assert_eq!(legacy.new_epoch, ClaimEpoch(13));
}

/// #1483 guard: every delegated relay reply must be spawned through
/// `spawn_in_dispatch_span`, the one seam that binds the reply task
/// to its dispatch span. A direct `ctx.spawn` in a handler would run
/// the delivery — where the actor messages happen — outside the root
/// span, silently restoring the #1438 trace loss, and the
/// field-recording tests above cannot catch that (the span still
/// records its fields at creation). Comment lines are skipped; no
/// parsing beyond that is needed, so string/paren contents cannot
/// cause false failures.
#[test]
fn delegated_relay_replies_go_through_the_dispatch_span_helper() {
    let source = include_str!("../relay.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("split always yields a first segment");
    let direct_spawns = production
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter(|line| line.contains("ctx.spawn("))
        .count();
    assert_eq!(
        direct_spawns, 1,
        "ctx.spawn must appear exactly once — inside spawn_in_dispatch_span; \
         route new delegated replies through that helper"
    );
}
