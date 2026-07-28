use super::*;
use crate::clustering::ordered_relay::OrderedRelaySequence;
use kameo::actor::Spawn;
use libp2p::PeerId;
use std::collections::HashSet;
use waddle_xmpp::ownership::{ClaimEpoch, ClaimError, InProcessClaimStore, NodeIdentity};
use xmpp_parsers::message::{Lang, Message};

struct StaticNodeLease {
    origin: NodeIdentity,
    peer_id: String,
}

#[async_trait::async_trait]
impl NodeLeaseStore for StaticNodeLease {
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

    async fn peer_id_for_node(
        &self,
        node: &NodeIdentity,
    ) -> Result<Option<String>, ClaimError> {
        if node == &self.origin {
            Ok(Some(self.peer_id.clone()))
        } else {
            Ok(None)
        }
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
    ) -> Result<Vec<crate::clustering::claims::OrphanedSmSessionClaim>, ClaimError> {
        Ok(Vec::new())
    }

    async fn current_generation(&self) -> Result<Option<String>, ClaimError> {
        Ok(None)
    }
}

struct StaticAllowlist {
    peer_id: PeerId,
}

#[async_trait::async_trait]
impl AllowlistStore for StaticAllowlist {
    async fn ensure_schema(&self) -> Result<(), super::super::allowlist::AllowlistError> {
        Ok(())
    }

    async fn enrolled_peers(
        &self,
    ) -> Result<HashSet<PeerId>, super::super::allowlist::AllowlistError> {
        Ok(HashSet::from([self.peer_id]))
    }
}

fn origin_identity() -> NodeIdentity {
    NodeIdentity::new("origin-node", "origin-epoch")
}

fn receiver_identity() -> NodeIdentity {
    NodeIdentity::new("receiver-node", "receiver-epoch")
}

fn other_identity() -> NodeIdentity {
    NodeIdentity::new("other-node", "other-epoch")
}

fn origin_entity() -> Entity {
    Entity::new(EntityType::SmSession, "stream-1")
}

fn sender_full() -> jid::FullJid {
    "romeo@example.test/home".parse().expect("full jid")
}

fn sender_entity() -> Entity {
    user_entity(&sender_full().to_bare())
}

fn target_bare() -> jid::BareJid {
    "juliet@example.test".parse().expect("bare jid")
}

fn target_full() -> jid::FullJid {
    "juliet@example.test/phone".parse().expect("full jid")
}

fn target_entity() -> Entity {
    user_entity(&target_bare())
}

fn envelope_claims(target_epoch: i64) -> OrderedRelayEnvelopeClaims {
    OrderedRelayEnvelopeClaims::new(
        OrderedRelayClaim {
            entity: origin_entity(),
            epoch: ClaimEpoch(0),
        },
        OrderedRelayClaim {
            entity: sender_entity(),
            epoch: ClaimEpoch(0),
        },
        OrderedRelayClaim {
            entity: target_entity(),
            epoch: ClaimEpoch(target_epoch),
        },
    )
}

fn message_payload() -> OrderedRelayPayload {
    let full = target_full();
    let mut message = Message::new(Some(jid::Jid::from(full.clone())));
    message.from = Some(jid::Jid::from(sender_full()));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Lang::new(), "hello from remote".to_string());
    OrderedRelayPayload::Message {
        recipient: jid::Jid::from(full),
        stanza: RemoteStanza(Stanza::Message(message)),
    }
}

fn iq_payload() -> OrderedRelayPayload {
    let full = target_full();
    let mut iq = xmpp_parsers::iq::Iq::from_get("iq-1", xmpp_parsers::ping::Ping);
    *iq.from_mut() = Some(jid::Jid::from(sender_full()));
    *iq.to_mut() = Some(jid::Jid::from(full.clone()));
    OrderedRelayPayload::Iq {
        recipient: jid::Jid::from(full),
        stanza: RemoteStanza(Stanza::Iq(Box::new(iq))),
    }
}

#[test]
fn full_jid_bridge_rejects_groupchat_payloads() {
    let target = target_full();
    let mut message = Message::new(Some(jid::Jid::from(target.clone())));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;

    assert!(payload_for_recipient(jid::Jid::from(target), &Stanza::Message(message)).is_none());
}

fn envelope() -> RemoteStanzaEnvelope {
    RemoteStanzaEnvelope {
        asserted_origin_node: NodeId::new(origin_identity().node_id),
        channel: OrderedRelayChannel {
            origin: OrderedRelayOrigin::SmSession(
                waddle_xmpp::pending_delivery::SmSessionId::new("stream-1"),
            ),
            recipient: OrderedRelayRecipient::FullJid(target_full()),
            target_epoch: waddle_xmpp::ownership::ClaimEpoch(0),
        },
        sequence: OrderedRelaySequence::FIRST,
        origin_inbound_sequence: OriginInboundSequence(1),
        origin_claim: OrderedRelayClaim {
            entity: origin_entity(),
            epoch: waddle_xmpp::ownership::ClaimEpoch(0),
        },
        sender_claim: OrderedRelayClaim {
            entity: sender_entity(),
            epoch: waddle_xmpp::ownership::ClaimEpoch(0),
        },
        target_claim: OrderedRelayClaim {
            entity: target_entity(),
            epoch: waddle_xmpp::ownership::ClaimEpoch(0),
        },
        payload: message_payload(),
        origin_proof: None,
    }
}

fn sign_envelope(
    mut envelope: RemoteStanzaEnvelope,
    keypair: &Keypair,
) -> RemoteStanzaEnvelope {
    let signing_bytes = envelope.signing_bytes().expect("signing bytes");
    envelope.origin_proof = Some(OrderedRelayOriginProof {
        public_key: keypair.public().encode_protobuf(),
        signature: keypair.sign(&signing_bytes).expect("sign envelope"),
    });
    envelope
}

async fn envelope_for_services(
    services: &OrderedRelayDeliveryServices,
) -> RemoteStanzaEnvelope {
    async fn epoch_for(services: &OrderedRelayDeliveryServices, entity: &Entity) -> ClaimEpoch {
        services
            .claim_store
            .current_claim(entity)
            .await
            .expect("claim lookup")
            .expect("seeded claim")
            .claim_epoch
    }

    let mut envelope = envelope();
    envelope.origin_claim.epoch = epoch_for(services, &origin_entity()).await;
    envelope.sender_claim.epoch = epoch_for(services, &sender_entity()).await;
    let target_epoch = epoch_for(services, &target_entity()).await;
    envelope.target_claim.epoch = target_epoch;
    envelope.channel.target_epoch = target_epoch;
    envelope
}

async fn signed_envelope_for_services(
    services: &OrderedRelayDeliveryServices,
    keypair: &Keypair,
) -> RemoteStanzaEnvelope {
    sign_envelope(envelope_for_services(services).await, keypair)
}

fn test_peer_id() -> String {
    Keypair::generate_ed25519()
        .public()
        .to_peer_id()
        .to_string()
}

async fn services_with_claims(
    origin_owner: NodeIdentity,
    target_owner: NodeIdentity,
    receiver: NodeIdentity,
    origin_peer_id: String,
) -> OrderedRelayDeliveryServices {
    services_with_claims_and_blocking(
        origin_owner,
        target_owner,
        receiver,
        origin_peer_id,
        Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
    )
    .await
}

async fn services_with_claims_and_blocking(
    origin_owner: NodeIdentity,
    target_owner: NodeIdentity,
    receiver: NodeIdentity,
    origin_peer_id: String,
    blocking_storage: Arc<dyn BlockingStorage>,
) -> OrderedRelayDeliveryServices {
    let store = Arc::new(InProcessClaimStore::new());
    store
        .acquire(&origin_entity(), &origin_owner)
        .await
        .expect("origin claim");
    store
        .acquire(&sender_entity(), &origin_owner)
        .await
        .expect("sender claim");
    store
        .acquire(&target_entity(), &target_owner)
        .await
        .expect("target claim");
    OrderedRelayDeliveryServices {
        claim_store: store,
        allowlist_store: Arc::new(StaticAllowlist {
            peer_id: origin_peer_id.parse().expect("valid test peer id"),
        }),
        node_lease: Arc::new(StaticNodeLease {
            origin: origin_owner,
            peer_id: origin_peer_id,
        }),
        node_identity: SharedNodeIdentity::new(receiver),
        connection_registry: Arc::new(ConnectionRegistry::new()),
        user_registry: UserRegistryActor::spawn(UserRegistryActor::new()),
        sm_session_registry: Arc::new(InMemorySmSessionRegistry::new()),
        blocking_storage,
        web_socket_state: Weak::new(),
    }
}

/// #1594: the owner-side re-assert executor must degrade to
/// `Unavailable` when the bridge is wired but this node's
/// `WebSocketState` has been dropped — the asker maps that to a
/// LiveKit retry, never to an authorization decision.
#[tokio::test]
async fn reassert_media_grants_local_without_live_state_is_unavailable() {
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    ));

    let room: jid::BareJid = "room@muc.example.com".parse().expect("room jid");
    let participant: jid::FullJid = "alice@example.com/web".parse().expect("participant jid");
    let outcome = bridge
        .reassert_media_grants_local(&room, &participant)
        .await;

    assert_eq!(outcome, LocalMediaGrantReassertion::Unavailable);
}

/// #1594 receiver-side claim gate: even with a live local room
/// actor holding the participant, a node that does NOT own the
/// room's claim must refuse to execute a relayed re-assert — a
/// lingering post-demote actor answering from a superseded
/// occupant set is the #1593 breaker class. Without the gate this
/// setup would answer `Applied` and push a grant.
#[tokio::test]
async fn reassert_media_grants_local_without_owned_claim_refuses_to_execute() {
    use crate::server::routes::websocket::tests::{
        create_test_server_owner_session, create_test_websocket_state_with_sfu, RecordingSfu,
    };

    let recorder = Arc::new(RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room: jid::BareJid = "gate-refused@muc.example.com".parse().expect("room jid");
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("participant jid");
    crate::server::routes::websocket::handlers::presence::handle_muc_join(
        state.as_ref(),
        "example.com",
        &room,
        &alice,
        "alice",
        None,
        &Some(session),
    )
    .await;

    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    // Claim store knows the fixture entities but NOT this room —
    // exactly what a deposed/never-owning receiver observes.
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.web_socket_state = Arc::downgrade(&state);
    bridge.wire(Arc::new(services));

    let outcome = bridge.reassert_media_grants_local(&room, &alice).await;

    assert_eq!(outcome, LocalMediaGrantReassertion::NoLocalRoomActor);
    assert!(
        recorder.update_snapshot().is_empty(),
        "an unowned claim must suppress the grant push"
    );
    assert!(
        recorder.snapshot().is_empty(),
        "an unowned claim must never evict"
    );
}

/// #1594 receiver-side claim gate, freshness half: a claim row
/// still naming this node whose node lease has EXPIRED is not
/// authority — another node may already be stealing the claim, so
/// a lingering local actor must not answer from its (about to be
/// superseded) occupant set. Same fresh-and-mine predicate as
/// every other receiver gate in this module.
#[tokio::test]
async fn reassert_media_grants_local_with_stale_lease_refuses_to_execute() {
    use crate::server::routes::websocket::tests::{
        create_test_server_owner_session, create_test_websocket_state_with_sfu, RecordingSfu,
    };
    use waddle_xmpp::ownership::{ClaimSnapshot, StalePredicate};

    /// `current_claim` answers "owned by `me`, lease expired";
    /// nothing else is reachable from the gate under test.
    struct StaleLeaseClaimStore {
        me: NodeIdentity,
    }

    #[async_trait::async_trait]
    impl waddle_xmpp::ownership::ClaimStore for StaleLeaseClaimStore {
        async fn ensure_schema(&self) -> Result<(), ClaimError> {
            Ok(())
        }
        async fn acquire(
            &self,
            _entity: &Entity,
            _me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            unreachable!("gate test only calls current_claim")
        }
        async fn ensure_claimed(
            &self,
            _entity: &Entity,
            _me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            unreachable!("gate test only calls current_claim")
        }
        async fn steal_stale(
            &self,
            _entity: &Entity,
            _observed: ClaimEpoch,
            _staleness: StalePredicate,
            _me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            unreachable!("gate test only calls current_claim")
        }
        async fn steal_for_resume(
            &self,
            _entity: &Entity,
            _observed: ClaimEpoch,
            _witness: waddle_xmpp::ownership::ResumeIdentityProof,
            _me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            unreachable!("gate test only calls current_claim")
        }
        async fn current_claim(
            &self,
            _entity: &Entity,
        ) -> Result<Option<ClaimSnapshot>, ClaimError> {
            Ok(Some(ClaimSnapshot {
                owner: self.me.clone(),
                claim_epoch: ClaimEpoch(1),
                owner_lease_fresh: false,
            }))
        }
        async fn current_claim_after_pending_writes(
            &self,
            _entity: &Entity,
        ) -> Result<Option<ClaimSnapshot>, ClaimError> {
            unreachable!("gate test only calls current_claim")
        }
        async fn fence(
            &self,
            _entity: &Entity,
            _me: &NodeIdentity,
            _mine: ClaimEpoch,
        ) -> Result<bool, ClaimError> {
            unreachable!("gate test only calls current_claim")
        }
        async fn release(
            &self,
            _entity: &Entity,
            _me: &NodeIdentity,
            _mine: ClaimEpoch,
        ) -> Result<(), ClaimError> {
            unreachable!("gate test only calls current_claim")
        }
        async fn release_many(
            &self,
            _entities: &[Entity],
            _me: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            unreachable!("gate test only calls current_claim")
        }
    }

    let recorder = Arc::new(RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room: jid::BareJid = "gate-stale@muc.example.com".parse().expect("room jid");
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("participant jid");
    crate::server::routes::websocket::handlers::presence::handle_muc_join(
        state.as_ref(),
        "example.com",
        &room,
        &alice,
        "alice",
        None,
        &Some(session),
    )
    .await;

    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.web_socket_state = Arc::downgrade(&state);
    services.claim_store = Arc::new(StaleLeaseClaimStore {
        me: receiver_identity(),
    });
    bridge.wire(Arc::new(services));

    let outcome = bridge.reassert_media_grants_local(&room, &alice).await;

    assert_eq!(outcome, LocalMediaGrantReassertion::NoLocalRoomActor);
    assert!(
        recorder.update_snapshot().is_empty(),
        "an expired lease must suppress the grant push"
    );
    assert!(
        recorder.snapshot().is_empty(),
        "an expired lease must never evict"
    );
}

/// #1594: the receiver-side claim reads are bounded. The executor
/// runs in a delegated relay task that outlives the asker's
/// webhook timeout, so a stalled claim store (pool exhaustion)
/// must resolve to `Unavailable` within the read budget instead
/// of accumulating one pending task per LiveKit retry.
#[tokio::test]
async fn reassert_media_grants_local_bounds_a_stalled_claim_store() {
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state_with_sfu, RecordingSfu,
    };
    use waddle_xmpp::ownership::{ClaimSnapshot, StalePredicate};

    /// `current_claim` never resolves; nothing else is reachable.
    struct StalledClaimStore;

    #[async_trait::async_trait]
    impl waddle_xmpp::ownership::ClaimStore for StalledClaimStore {
        async fn ensure_schema(&self) -> Result<(), ClaimError> {
            Ok(())
        }
        async fn acquire(
            &self,
            _entity: &Entity,
            _me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            unreachable!("stall test only calls current_claim")
        }
        async fn ensure_claimed(
            &self,
            _entity: &Entity,
            _me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            unreachable!("stall test only calls current_claim")
        }
        async fn steal_stale(
            &self,
            _entity: &Entity,
            _observed: ClaimEpoch,
            _staleness: StalePredicate,
            _me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            unreachable!("stall test only calls current_claim")
        }
        async fn steal_for_resume(
            &self,
            _entity: &Entity,
            _observed: ClaimEpoch,
            _witness: waddle_xmpp::ownership::ResumeIdentityProof,
            _me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            unreachable!("stall test only calls current_claim")
        }
        async fn current_claim(
            &self,
            _entity: &Entity,
        ) -> Result<Option<ClaimSnapshot>, ClaimError> {
            std::future::pending().await
        }
        async fn current_claim_after_pending_writes(
            &self,
            _entity: &Entity,
        ) -> Result<Option<ClaimSnapshot>, ClaimError> {
            unreachable!("stall test only calls current_claim")
        }
        async fn fence(
            &self,
            _entity: &Entity,
            _me: &NodeIdentity,
            _mine: ClaimEpoch,
        ) -> Result<bool, ClaimError> {
            unreachable!("stall test only calls current_claim")
        }
        async fn release(
            &self,
            _entity: &Entity,
            _me: &NodeIdentity,
            _mine: ClaimEpoch,
        ) -> Result<(), ClaimError> {
            unreachable!("stall test only calls current_claim")
        }
        async fn release_many(
            &self,
            _entities: &[Entity],
            _me: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            unreachable!("stall test only calls current_claim")
        }
    }

    let recorder = Arc::new(RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.web_socket_state = Arc::downgrade(&state);
    services.claim_store = Arc::new(StalledClaimStore);
    bridge.wire(Arc::new(services));

    let room: jid::BareJid = "gate-stalled@muc.example.com".parse().expect("room jid");
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("participant jid");
    let outcome = tokio::time::timeout(
        REASSERT_CLAIM_READ_TIMEOUT + Duration::from_secs(3),
        bridge.reassert_media_grants_local(&room, &alice),
    )
    .await
    .expect("a stalled claim store must not hang the executor");

    assert_eq!(outcome, LocalMediaGrantReassertion::Unavailable);
    assert!(recorder.update_snapshot().is_empty());
    assert!(recorder.snapshot().is_empty());
}

/// #1594 owner-side executor happy path: claim owned by this node
/// plus a live room actor with the seated occupant → the relayed
/// re-assert pushes the voice-derived grant, observable on the
/// recording SFU.
#[tokio::test]
async fn reassert_media_grants_local_with_owned_claim_pushes_grants() {
    use crate::server::routes::websocket::tests::{
        create_test_server_owner_session, create_test_websocket_state_with_sfu, RecordingSfu,
    };

    let recorder = Arc::new(RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room: jid::BareJid = "gate-owned@muc.example.com".parse().expect("room jid");
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("participant jid");
    crate::server::routes::websocket::handlers::presence::handle_muc_join(
        state.as_ref(),
        "example.com",
        &room,
        &alice,
        "alice",
        None,
        &Some(session),
    )
    .await;

    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.web_socket_state = Arc::downgrade(&state);
    services
        .claim_store
        .acquire(&room_entity(&room), &receiver_identity())
        .await
        .expect("receiver acquires the room claim");
    bridge.wire(Arc::new(services));

    let outcome = bridge.reassert_media_grants_local(&room, &alice).await;

    assert_eq!(outcome, LocalMediaGrantReassertion::Applied);
    let updates = recorder.update_snapshot();
    assert_eq!(updates.len(), 1, "exactly one grant push expected");
    assert_eq!(updates[0].1.as_livekit_identity(), alice.to_string());
}

#[tokio::test]
async fn unwired_bridge_reports_unreachable_without_advancing_effects() {
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );

    let err = bridge
        .deliver_reserved(&envelope())
        .await
        .expect_err("unwired bridge cannot deliver");

    assert_eq!(err, OrderedRelayNackReason::Unreachable);
}

#[tokio::test]
async fn receiver_rejects_target_claim_not_owned_by_this_node() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        other_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;

    let envelope = signed_envelope_for_services(&services, &keypair).await;
    let err = validate_claims(&services, &envelope)
        .await
        .expect_err("foreign target owner is not this relay");

    assert_eq!(
        err,
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target
        }
    );
}

#[tokio::test]
async fn receiver_accepts_fresh_origin_and_local_target_claims() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;

    let envelope = signed_envelope_for_services(&services, &keypair).await;
    validate_claims(&services, &envelope)
        .await
        .expect("claims match origin and receiver");
}

#[tokio::test]
async fn receiver_rejects_stale_sender_claim_before_delivery_effects() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let mut envelope = envelope_for_services(&services).await;
    envelope.sender_claim.epoch = ClaimEpoch(99);
    let envelope = sign_envelope(envelope, &keypair);

    let err = validate_claims(&services, &envelope)
        .await
        .expect_err("stale sender claim must be rejected");

    assert_eq!(
        err,
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Sender
        }
    );
}

#[tokio::test]
async fn bare_presence_direct_drops_blocked_sender_before_detached_replay() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

    let blocking = Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new());
    blocking.set_blocklist(target_bare(), vec![sender_full().to_bare()]);
    let services = services_with_claims_and_blocking(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
        blocking,
    )
    .await;

    let detached_stream = "stream-detached-presence";
    services
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: detached_stream.to_string(),
            user_id: target_bare().to_string(),
            jid: target_full(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached target resource");

    let mut presence =
        xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.from = Some(jid::Jid::from(sender_full()));
    presence.to = Some(jid::Jid::from(target_bare()));

    deliver_reserved_bare_presence_direct(
        &services,
        &target_bare(),
        &Stanza::Presence(presence),
    )
    .await
    .expect("blocked presence is silently dropped");

    let detached = services
        .sm_session_registry
        .peek_session(detached_stream)
        .await
        .expect("peek detached target resource")
        .expect("detached target resource remains");
    assert!(
        detached.unacked_stanzas.is_empty(),
        "blocked remote presence must not be queued for SM replay"
    );
}

#[tokio::test]
async fn receiver_nacks_iq_without_live_resource_instead_of_detached_queue() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let mut envelope = envelope_for_services(&services).await;
    envelope.payload = iq_payload();
    let envelope = sign_envelope(envelope, &keypair);
    bridge.wire(Arc::new(services));

    let err = bridge
        .deliver_reserved(&envelope)
        .await
        .expect_err("remote full-JID IQ requires a live local resource");

    assert_eq!(err, OrderedRelayNackReason::TargetUnavailable);
}

#[tokio::test]
async fn receiver_delivers_full_jid_iq_as_peer_stanza() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target_full(),
            entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
        })
        .await
        .expect("register resource");
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let mut envelope = envelope_for_services(&services).await;
    envelope.payload = iq_payload();
    let envelope = sign_envelope(envelope, &keypair);
    bridge.wire(Arc::new(services));

    bridge
        .deliver_reserved(&envelope)
        .await
        .expect("remote full-JID IQ delivers to live resource");

    let outbound = rx.try_recv().expect("queued outbound stanza");
    assert_eq!(
        outbound.kind,
        waddle_xmpp::registry::DeliveryKind::PeerStanza
    );
}

#[tokio::test]
async fn stale_registered_remote_resource_cleans_mirror_and_allows_local_fallback() {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));

    let target = target_full();
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target.clone(),
            entry,
        })
        .await
        .expect("register remote mirror in user actor");

    let registration_id = RemoteResourceRegistrationId::fresh();
    bridge.remote_owner_resources.lock().await.insert(
        target.clone(),
        RemoteOwnerRegistration {
            registration_id,
            socket_node: NodeId::new("missing-socket-node".to_string()),
            socket_generation: RemoteResourceSocketGeneration::next(None),
            owner: Arc::clone(&owner),
        },
    );

    let outcome = bridge
        .try_deliver_registered_remote_resource(
            &target,
            &Stanza::Message(Message::new(Some(jid::Jid::from(target.clone())))),
            DeliveryKind::PeerStanza,
        )
        .await;

    assert_eq!(outcome, None);
    assert!(
        !bridge
            .remote_owner_resources
            .lock()
            .await
            .contains_key(&target),
        "stale remote owner map entry must be removed"
    );
    assert!(
        services
            .connection_registry
            .entry_if_owner(&target, &owner)
            .is_none(),
        "stale remote connection mirror must be removed so local fallback can run"
    );
}

#[tokio::test]
async fn stale_force_detach_error_cleans_old_socket_mirror() {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));

    let target = target_full();
    let (old_tx, _old_rx) = mpsc::channel(1);
    let old_entry = ConnectionEntry::new(old_tx);
    let old_owner = old_entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), old_entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old remote mirror in user actor");

    let old_generation = RemoteResourceSocketGeneration::next(None);
    let registration = RemoteOwnerRegistration {
        registration_id: RemoteResourceRegistrationId::fresh(),
        socket_node: NodeId::new("missing-old-socket-node".to_string()),
        socket_generation: old_generation,
        owner: Arc::clone(&old_owner),
    };

    assert!(
        bridge
            .finish_remote_owner_registration_retire(
                &services,
                &target,
                &registration,
                Err(RelayAskError::NotFound {
                    node_id: registration.socket_node.clone(),
                }),
            )
            .await,
        "a missing old socket node proves no detach was committed and should permit cleanup"
    );
    assert!(
        services
            .connection_registry
            .entry_if_owner(&target, &old_owner)
            .is_none(),
        "stale old owner mirror must be removed before replacement"
    );
    if let Some(actor) = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .await
        .expect("get user actor")
    {
        assert!(
            actor
                .ask(waddle_xmpp::registry::GetConnectionEntry {
                    jid: target.clone(),
                })
                .await
                .expect("get old actor mirror")
                .is_none(),
            "stale old user-actor mirror must be removed before replacement"
        );
    }
}

#[tokio::test]
async fn receiver_nacks_dropped_peer_delivery_instead_of_acknowledging_loss() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    tx.try_send(waddle_xmpp::registry::OutboundStanza::peer_stanza(
        Stanza::Message(Message::new(Some(jid::Jid::from(target_full())))),
    ))
    .expect("seed outbound channel to capacity");
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target_full(),
            entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
        })
        .await
        .expect("register resource");
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let envelope = sign_envelope(envelope_for_services(&services).await, &keypair);
    bridge.wire(Arc::new(services));

    let err = bridge
        .deliver_reserved(&envelope)
        .await
        .expect_err("full outbound channel must not be ACKed as delivered");

    assert_eq!(err, OrderedRelayNackReason::Backpressure);
}

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
    let outcome =
        no_client_reply_outcome_with_commit_state(FullJidDeliveryOutcome::Dropped, true);

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
    let mut receiver = super::super::ordered_relay::OrderedRelayReceiverState::default();
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
        super::super::ordered_relay::OrderedRelayReservation::Reserved(reserved) => reserved,
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
        super::super::ordered_relay::OrderedRelayReservation::Reserved(_)
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
