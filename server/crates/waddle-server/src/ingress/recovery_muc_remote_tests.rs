//! Foreign ownership never turns maintenance's detached recovery into a relay.
use super::*;
use crate::clustering::{
    relay::RelayRemoteResourceRegistrationStatus,
    route_bridge::{remote_registration_request, wire_for_test, OrderedRelayDeliveryBridge},
    ClusteringHandles, NodeId,
};
use crate::ingress::{recorded::RouteProgress, recovery_rebuild};
use crate::server::routes::interpret::{effects::EffectOutcome, FullJidDeliveryOutcome};
use waddle_xmpp::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};

async fn owned_recovery(f: IngressFixture, recovering_local: bool) {
    let sm = persistent_sm(&f).await;
    let planning_state = state_for(&f, sm.clone()).await;
    let occupant: jid::FullJid = "foreign@example.com/phone".parse().expect("occupant");
    if recovering_local {
        store_detached(&sm, &occupant).await;
    }
    let submission = planned_room(
        &f,
        &planning_state,
        Case::Lost,
        std::slice::from_ref(&occupant),
    )
    .await;
    let accepted = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("room commit");
    let key = accepted.message_key.expect("key");
    if recovering_local {
        // First acceptance completed room-side effects; only occupant delivery
        // was interrupted before the destination node's maintenance takes over.
        let mut mutations = accepted.clone();
        let indices: Vec<_> = accepted
            .external
            .iter()
            .enumerate()
            .filter_map(|(index, effect)| {
                (!matches!(effect, ExternalEffect::Delivery(_))).then_some(index)
            })
            .collect();
        mutations.external = indices
            .iter()
            .map(|index| accepted.external[*index].clone())
            .collect();
        mutations.external_dependencies = indices
            .iter()
            .map(|index| accepted.external_dependencies[*index].clone())
            .collect();
        mutations.external_receipts = indices
            .iter()
            .map(|index| accepted.external_receipts[*index].clone())
            .collect();
        let deps = build_interpret_deps(&planning_state, None);
        let report = execute_effects(
            &f.uow,
            &f.db,
            &mutations,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        assert!(
            report.receipt_failures.is_empty(),
            "initial room effects persisted: {report:?}"
        );
    }
    let mut tx = f.uow.begin().await.expect("frozen authority");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("load")
        .expect("envelope");
    let recorded = crate::ingress_uow::EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("recorded");
    let receipt_keys = EffectReceiptRepository::keys(&mut tx, key)
        .await
        .expect("receipts");
    let unreceipted: Vec<_> = recorded
        .iter()
        .filter(|intent| !receipt_keys.contains(&receipt_key(intent).expect("receipt key")))
        .cloned()
        .collect();
    tx.commit().await.expect("read commit");
    let muc = recorded
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .expect("MUC");
    if recovering_local {
        assert_eq!(
            unreceipted,
            vec![muc.clone()],
            "only MUC fanout remains for destination recovery"
        );
    }
    let receipt = receipt_key(muc).expect("receipt");
    let progress = RouteProgress::from_intent(muc, None, vec![])
        .expect("progress")
        .expect("MUC progress");
    let rebuilt = recovery_rebuild::rebuild(recovery_rebuild::RecoveryInput {
        key,
        envelope: &envelope,
        created_at: chrono::Utc::now(),
        recorded: &recorded,
        unreceipted: &unreceipted,
        route_progress: vec![progress],
        host_owned_resources: vec![],
        departed_occupants: vec![],
        blocked_recipients: &[],
    })
    .expect("rebuild remote occupant");
    assert!(
        matches!(&rebuilt.decision.external[..], [ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. })] if resources == std::slice::from_ref(&occupant)),
        "recovery emits only the exact detached copy, never a relay"
    );

    let claims = Arc::new(InProcessClaimStore::new());
    let local = NodeIdentity::new("recovering-owner", "local-epoch");
    let entity = Entity::new(EntityType::UserActor, occupant.to_bare().to_string());
    claims
        .acquire(&entity, &local)
        .await
        .expect("foreign claim");
    let owner = claims
        .current_claim(&entity)
        .await
        .expect("claim lookup")
        .expect("foreign owner");
    assert!(owner.owner_lease_fresh);
    assert_eq!(owner.owner, local.clone());
    let bridge = OrderedRelayDeliveryBridge::new(
        tokio_util::sync::CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    let state = socket_tests::create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(Arc::clone(&claims) as Arc<dyn ClaimStore>),
            node_identity: Some(SharedNodeIdentity::new(local)),
            ordered_relay_delivery_bridge: Some(Arc::clone(&bridge)),
            ..Default::default()
        },
        sm.clone(),
    )
    .await;
    wire_for_test(
        &bridge,
        &state,
        claims as Arc<dyn ClaimStore>,
        SharedNodeIdentity::new(NodeIdentity::new("recovering-owner", "local-epoch")),
    )
    .await;
    if !recovering_local {
        let reply = bridge
            .register_remote_user_resource_on_owner(remote_registration_request(
                occupant.clone(),
                NodeId::new("foreign-socket-node".to_string()),
            ))
            .await;
        assert_eq!(
            reply.status,
            RelayRemoteResourceRegistrationStatus::Registered,
            "fixture must install the production remote-hosted mirror"
        );
    }
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state));
    if !recovering_local {
        let mut deps = env.recovery_deps();
        deps.delivery_execution_context =
            crate::server::routes::interpret::DeliveryExecutionContext::MaintenanceRecovery;
        let outcome = crate::ingress::execute_uow::execute_with_uow(
            &f.uow,
            &f.db,
            &rebuilt.decision,
            0,
            &rebuilt.decision.external[0],
            &deps,
            tokio::time::Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect("MUC progress arm");
        let EffectOutcome::Settled(settled) = outcome else {
            panic!("typed MUC completion");
        };
        assert_eq!(
            settled.detached,
            Some(vec![(
                occupant.clone(),
                FullJidDeliveryOutcome::Unavailable
            )])
        );
        assert!(settled.persisted.is_empty(), "no foreign delivery proof");
    }
    let registered_remote_targets = Arc::new(std::sync::Mutex::new(Vec::new()));
    let maintenance = crate::server::routes::interpret::CONTROLLED_REGISTERED_REMOTE_DELIVERY
        .scope(
            (
                FullJidDeliveryOutcome::Delivered,
                Arc::clone(&registered_remote_targets),
            ),
            pass(&f, &env, &MaintenanceCursor::default()),
        )
        .await;
    assert_eq!(maintenance, MaintenanceOutcome::Complete);
    assert!(
        registered_remote_targets
            .lock()
            .expect("registered-remote targets")
            .is_empty(),
        "maintenance must not attempt registered-remote delivery"
    );
    assert!(
        super::super::super::attempt_count(key) > 0,
        "maintenance attempts remote-owned MUC obligation"
    );
    let mut tx = f.uow.begin().await.expect("inspect pending row");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("progress"),
        if recovering_local {
            vec![occupant.clone()]
        } else {
            vec![]
        }
    );
    assert_eq!(
        EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("aggregate"),
        recovering_local
    );
    assert_eq!(
        CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("terminal"),
        recovering_local
    );
    tx.commit().await.expect("read commit");
    assert_eq!(
        f.count("sm_ingress_appends").await,
        i64::from(recovering_local)
    );
    if recovering_local {
        assert_eq!(
            append_count(&sm, &occupant).await,
            1,
            "destination maintenance delivered the remaining copy"
        );
    }
    assert_eq!(f.count("mam_messages").await, 0);
    f.close().await;
}

/// How the rest of the cluster relates to the occupant, which decides whether
/// evicting its occupancy here would tear down a seat that is alive elsewhere.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ElsewhereCase {
    /// A peer answers `Present` for the exact resource: it is holding that
    /// socket. Parameterised by where the account's `UserActor` claim sits,
    /// because the claim must decide NOTHING — see [`ClaimState`].
    PeerHoldsTheSocket(ClaimState),
    /// Every peer denies the exact resource while still answering `Present`
    /// for a healthy sibling resource of the same account. The production
    /// ghost shape (#1803).
    AbsentOnEveryPeer,
    /// A peer cannot answer at all — an old replica answering
    /// `UnknownMessage` during a rolling update, a timeout, or a transport
    /// failure. Fail closed.
    PeerAskFails,
    /// The membership read itself cannot answer, so the set of nodes that
    /// would have to deny the resource is unknown. Fail closed.
    MembershipUnreadable,
    /// A registered-remote mirror for the exact full JID, installed here by
    /// the socket's host. Decided locally, without any probe.
    RegisteredRemote,
}

/// Where the account's `UserActor` claim sits while a peer holds the socket.
///
/// A claim is ROUTING AUTHORITY, not socket liveness: a live idle socket on a
/// peer is known only to that peer's own connection registry, and nothing
/// re-registers it when the claim owner dies or moves. Every one of these
/// states must therefore keep the occupant seated, and the first two are the
/// exact shapes the superseded claim-gated guard evicted a LIVE user in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClaimState {
    /// No claim row at all — the owner died and its claim was reaped.
    Unclaimed,
    /// The room's own host holds the account claim, while the socket is on a
    /// peer (the claim moved here after the socket was bound elsewhere).
    OwnedByTheAsker,
    /// A fresh claim on a third node that is not the socket's host.
    FreshOnAnotherNode,
}

/// One scripted peer answer, recording exactly who was asked about which
/// resource.
struct ScriptedResourcePresence {
    case: ElsewhereCase,
    /// The one peer actually holding the socket, when any does.
    socket_host: NodeIdentity,
    ghost: jid::FullJid,
    asked: Arc<std::sync::Mutex<Vec<(NodeIdentity, jid::FullJid)>>>,
}

#[async_trait::async_trait]
impl crate::clustering::resource_presence::ResourcePresenceAsker for ScriptedResourcePresence {
    async fn resource_presence(
        &self,
        peer: &NodeIdentity,
        target: &jid::FullJid,
    ) -> Result<
        crate::clustering::relay::RelayResourcePresenceReply,
        crate::clustering::relay::RelayAskError,
    > {
        use crate::clustering::relay::{
            RelayAskError, RelayResourcePresenceReply, RelaySendEffect, RelaySendFailure,
        };
        self.asked
            .lock()
            .expect("probe log")
            .push((peer.clone(), target.clone()));
        assert_eq!(
            target, &self.ghost,
            "only the ghost resource is asked about"
        );
        match self.case {
            // ONLY the node actually holding the socket knows about it. Every
            // other peer — including whichever one happens to hold the
            // account's claim — answers a truthful `Absent`.
            ElsewhereCase::PeerHoldsTheSocket(_) if peer == &self.socket_host => {
                Ok(RelayResourcePresenceReply::Present)
            }
            // Exactly what kameo reports for a peer that does not know the
            // message id: a no-effect codec failure.
            ElsewhereCase::PeerAskFails => Err(RelayAskError::Send {
                failure: RelaySendFailure::Codec,
                effect: RelaySendEffect::NoEffect,
                message: "peer does not know waddle.clustering.relay.resource_presence.v1"
                    .to_string(),
            }),
            _ => Ok(RelayResourcePresenceReply::Absent),
        }
    }
}

/// The cluster's unexpired peers, or a membership read that cannot answer.
struct ScriptedMembership {
    peers: Vec<NodeIdentity>,
    readable: bool,
}

#[async_trait::async_trait]
impl crate::clustering::resource_presence::ClusterMembership for ScriptedMembership {
    async fn peers(&self) -> Result<Vec<NodeIdentity>, waddle_xmpp::ownership::ClaimError> {
        if self.readable {
            Ok(self.peers.clone())
        } else {
            Err(waddle_xmpp::ownership::ClaimError::Backend(
                "clustering_nodes unreadable".to_string(),
            ))
        }
    }
}

/// XEP-0045 ghost eviction is for occupancies NOTHING can reach. An occupant
/// some peer still holds a socket for keeps its seat even once its frozen copy
/// has stalled long enough to be classified `no_durable_progress` — but a
/// resource EVERY peer authoritatively denies is a ghost, even while its
/// account stays online on one of them.
async fn stalled_remote_occupant_recovery(f: IngressFixture, case: ElsewhereCase) {
    let evictable = case == ElsewhereCase::AbsentOnEveryPeer;
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let before = metrics
        .counter_sum("muc.ghost_occupants.evicted", &[])
        .unwrap_or(0);
    let sm = persistent_sm(&f).await;
    let planning_state = state_for(&f, sm.clone()).await;
    let occupant: jid::FullJid = "foreign@example.com/phone".parse().expect("occupant");
    let submission = planned_room(
        &f,
        &planning_state,
        Case::Lost,
        std::slice::from_ref(&occupant),
    )
    .await;
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("room commit");
    let key = decision.message_key.expect("key");
    super::departed::settle_non_delivery_effects(&f, &planning_state, &decision).await;

    let claims = Arc::new(InProcessClaimStore::new());
    let local = NodeIdentity::new("recovering-node", "local-epoch");
    let socket_node = NodeIdentity::new("socket-node", "foreign-epoch");
    let entity = Entity::new(EntityType::UserActor, occupant.to_bare().to_string());
    // The claim must decide nothing, so every case pins it somewhere
    // DIFFERENT and asserts the same seat outcome.
    let third_node = NodeIdentity::new("third-node", "third-epoch");
    let claim_owner = match case {
        ElsewhereCase::PeerHoldsTheSocket(ClaimState::Unclaimed) => None,
        ElsewhereCase::PeerHoldsTheSocket(ClaimState::OwnedByTheAsker) => Some(local.clone()),
        ElsewhereCase::PeerHoldsTheSocket(ClaimState::FreshOnAnotherNode) => {
            Some(third_node.clone())
        }
        // The registered-remote mirror is installed by the socket's host, so
        // that case leaves the claim here and isolates the resource lookup.
        ElsewhereCase::RegisteredRemote => Some(local.clone()),
        _ => Some(socket_node.clone()),
    };
    // Every unexpired peer must be asked, not just whichever one the claim
    // names: the third node holds the claim but never held the socket.
    let peers = match case {
        ElsewhereCase::PeerHoldsTheSocket(ClaimState::FreshOnAnotherNode) => {
            vec![socket_node.clone(), third_node.clone()]
        }
        _ => vec![socket_node.clone()],
    };
    if let Some(owner) = claim_owner.as_ref() {
        claims.acquire(&entity, owner).await.expect("claim");
    }
    let bridge = OrderedRelayDeliveryBridge::new(
        tokio_util::sync::CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let state = socket_tests::create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(Arc::clone(&claims) as Arc<dyn ClaimStore>),
            node_identity: Some(SharedNodeIdentity::new(local.clone())),
            ordered_relay_delivery_bridge: Some(Arc::clone(&bridge)),
            resource_presence: Some(Arc::new(ScriptedResourcePresence {
                case,
                socket_host: socket_node.clone(),
                ghost: occupant.clone(),
                asked: Arc::clone(&asked),
            })),
            cluster_membership: Some(Arc::new(ScriptedMembership {
                peers: peers.clone(),
                readable: case != ElsewhereCase::MembershipUnreadable,
            })),
            ..Default::default()
        },
        sm.clone(),
    )
    .await;
    wire_for_test(
        &bridge,
        &state,
        claims as Arc<dyn ClaimStore>,
        SharedNodeIdentity::new(local),
    )
    .await;
    if case == ElsewhereCase::RegisteredRemote {
        assert_eq!(
            bridge
                .register_remote_user_resource_on_owner(remote_registration_request(
                    occupant.clone(),
                    NodeId::new("socket-node".to_string()),
                ))
                .await
                .status,
            RelayRemoteResourceRegistrationStatus::Registered,
        );
    }
    // The recovering node authoritatively hosts the room and still lists the
    // occupant, so only the cross-node guards can keep the seat.
    let room: jid::BareJid = "recovery@muc.example.com".parse().expect("room");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
            room_jid: room.clone(),
            waddle_id: "recovery".into(),
            channel_id: "recovery".into(),
            config: Default::default(),
        })
        .await
        .expect("room");
    actor
        .ask(Join {
            nick: "foreign".into(),
            real_jid: occupant.clone(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("join");
    let locally_registered = state
        .deps
        .protocol
        .connection_registry
        .is_connected(&occupant);
    match case {
        ElsewhereCase::RegisteredRemote => {
            // The production mirror installs both a non-locally-hosted
            // connection entry and an actor-tree resource; either one alone
            // is enough to keep the seat.
            assert!(locally_registered, "the remote mirror is registered here");
            assert!(
                waddle_xmpp::registry::try_get_resources_for_user(
                    &state.deps.protocol.user_registry,
                    &occupant.to_bare()
                )
                .await
                .expect("resource lookup")
                .contains(&occupant),
                "the actor tree lists the mirrored resource"
            );
        }
        _ => assert!(
            !locally_registered,
            "no local entry: only the cross-node fan-out may decide"
        ),
    }

    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    let cursor = MaintenanceCursor::default();
    for _ in 0..3 {
        assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
        cursor.wait_for_recovery_accounting().await;
    }
    assert_eq!(
        actor
            .ask(waddle_xmpp::muc::room_actor::GetOccupantByJid {
                jid: occupant.clone()
            })
            .await
            .expect("occupancy probe")
            .is_some(),
        !evictable,
        "only a resource EVERY peer authoritatively denies is evicted"
    );
    assert_eq!(
        metrics
            .counter_sum("muc.ghost_occupants.evicted", &[])
            .unwrap_or(0),
        before + u64::from(evictable)
    );
    let mut tx = f.uow.begin().await.expect("inspect pending row");
    assert_eq!(
        CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("terminal"),
        evictable,
        "an evicted ghost releases the frozen copy and the row terminalizes"
    );
    tx.commit().await.expect("read commit");
    let asked = asked.lock().expect("probe log").clone();
    match case {
        ElsewhereCase::RegisteredRemote => assert!(
            asked.is_empty(),
            "a locally mirrored resource needs no cross-node ask: {asked:?}"
        ),
        ElsewhereCase::MembershipUnreadable => assert!(
            asked.is_empty(),
            "no peer may be asked without a membership answer: {asked:?}"
        ),
        _ => {
            assert!(
                !asked.is_empty(),
                "every unexpired peer must be asked before an eviction"
            );
            assert!(
                asked
                    .iter()
                    .all(|(node, jid)| peers.contains(node) && jid == &occupant),
                "each peer must be asked about the EXACT full JID: {asked:?}"
            );
            if case == ElsewhereCase::AbsentOnEveryPeer {
                assert!(
                    peers
                        .iter()
                        .all(|peer| asked.iter().any(|(node, _)| node == peer)),
                    "an eviction needs an Absent from EVERY peer: {asked:?}"
                );
            }
        }
    }
    f.close().await;
}

/// THE #1803 REGRESSION, three ways: a peer is holding this socket, and the
/// account claim is unclaimed, owned by the room's own host, or fresh on a
/// third node — every arrangement the superseded claim-gated guard read as
/// "nothing keeps this resource alive elsewhere" before evicting a LIVE user
/// from the room.
#[tokio::test]
async fn sqlite_stalled_occupant_with_a_peer_socket_keeps_its_seat_when_unclaimed() {
    stalled_remote_occupant_recovery(
        IngressFixture::sqlite().await,
        ElsewhereCase::PeerHoldsTheSocket(ClaimState::Unclaimed),
    )
    .await;
}

#[tokio::test]
async fn sqlite_stalled_occupant_with_a_peer_socket_keeps_its_seat_when_the_asker_holds_the_claim()
{
    stalled_remote_occupant_recovery(
        IngressFixture::sqlite().await,
        ElsewhereCase::PeerHoldsTheSocket(ClaimState::OwnedByTheAsker),
    )
    .await;
}

#[tokio::test]
async fn sqlite_stalled_occupant_with_a_peer_socket_keeps_its_seat_when_the_claim_moved() {
    stalled_remote_occupant_recovery(
        IngressFixture::sqlite().await,
        ElsewhereCase::PeerHoldsTheSocket(ClaimState::FreshOnAnotherNode),
    )
    .await;
}

/// Rolling-deploy safety: a peer that cannot answer the probe (an older
/// replica answering `UnknownMessage`, a timeout, a transport failure) leaves
/// absence unproven, so the occupant keeps its seat.
#[tokio::test]
async fn sqlite_stalled_remote_occupant_keeps_its_seat_when_a_peer_ask_fails() {
    stalled_remote_occupant_recovery(IngressFixture::sqlite().await, ElsewhereCase::PeerAskFails)
        .await;
}

/// An unknown cluster is not an empty one: a membership read that fails must
/// not be mistaken for "there is nobody else to ask".
#[tokio::test]
async fn sqlite_stalled_remote_occupant_keeps_its_seat_when_the_membership_read_fails() {
    stalled_remote_occupant_recovery(
        IngressFixture::sqlite().await,
        ElsewhereCase::MembershipUnreadable,
    )
    .await;
}

/// The dominant production shape (#1803): the room is hosted here, a healthy
/// sibling resource of the same account is live on the peer, and the ghost
/// resource pinning this row is one every peer denies.
#[tokio::test]
async fn sqlite_stalled_ghost_resource_of_a_live_remote_user_is_evicted() {
    stalled_remote_occupant_recovery(
        IngressFixture::sqlite().await,
        ElsewhereCase::AbsentOnEveryPeer,
    )
    .await;
}

#[tokio::test]
async fn sqlite_stalled_registered_remote_occupant_keeps_its_seat() {
    stalled_remote_occupant_recovery(
        IngressFixture::sqlite().await,
        ElsewhereCase::RegisteredRemote,
    )
    .await;
}

#[tokio::test]
async fn sqlite_muc_recovery_remote_owner_stays_pending_without_relay() {
    owned_recovery(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn postgres_muc_recovery_remote_owner_stays_pending_without_relay() {
    if let Some(f) = IngressFixture::postgres("muc_recovery_remote").await {
        owned_recovery(f, false).await;
    }
}

// The foreign-owner case above proves the complementary pending/Unavailable path.
#[tokio::test]
async fn sqlite_muc_recovery_destination_owner_settles_local_copy() {
    owned_recovery(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn postgres_muc_recovery_destination_owner_settles_local_copy() {
    if let Some(f) = IngressFixture::postgres("muc_recovery_destination").await {
        owned_recovery(f, true).await;
    }
}
