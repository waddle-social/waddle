use super::*;
use waddle_xmpp::ownership::{ClaimEpoch, ClaimError, NodeIdentity};

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
    let alice_occupancy_session = waddle_xmpp_core::OccupancySessionGeneration::mint();
    crate::server::routes::websocket::handlers::presence::handle_muc_join(
        state.as_ref(),
        "example.com",
        &room,
        &alice,
        "alice",
        None,
        crate::server::routes::websocket::handlers::presence::MucJoinConnectionContext {
            registry_owner: None,
            occupancy_session: alice_occupancy_session,
            authenticated_session: &Some(session),
        },
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
    let alice_occupancy_session = waddle_xmpp_core::OccupancySessionGeneration::mint();
    crate::server::routes::websocket::handlers::presence::handle_muc_join(
        state.as_ref(),
        "example.com",
        &room,
        &alice,
        "alice",
        None,
        crate::server::routes::websocket::handlers::presence::MucJoinConnectionContext {
            registry_owner: None,
            occupancy_session: alice_occupancy_session,
            authenticated_session: &Some(session),
        },
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
    let alice_occupancy_session = waddle_xmpp_core::OccupancySessionGeneration::mint();
    crate::server::routes::websocket::handlers::presence::handle_muc_join(
        state.as_ref(),
        "example.com",
        &room,
        &alice,
        "alice",
        None,
        crate::server::routes::websocket::handlers::presence::MucJoinConnectionContext {
            registry_owner: None,
            occupancy_session: alice_occupancy_session,
            authenticated_session: &Some(session),
        },
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
