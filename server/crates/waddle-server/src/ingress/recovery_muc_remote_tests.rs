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
/// Counts its reads so a caller can assert the membership was never consulted.
struct ScriptedMembership {
    peers: Vec<NodeIdentity>,
    readable: bool,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}

impl ScriptedMembership {
    fn new(
        peers: Vec<NodeIdentity>,
        readable: bool,
    ) -> (Arc<Self>, Arc<std::sync::atomic::AtomicUsize>) {
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        (
            Arc::new(Self {
                peers,
                readable,
                reads: Arc::clone(&reads),
            }),
            reads,
        )
    }
}

#[async_trait::async_trait]
impl crate::clustering::resource_presence::ClusterMembership for ScriptedMembership {
    async fn peers(&self) -> Result<Vec<NodeIdentity>, waddle_xmpp::ownership::ClaimError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
            cluster_membership: Some(
                ScriptedMembership::new(peers.clone(), case != ElsewhereCase::MembershipUnreadable)
                    .0,
            ),
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

/// What the cluster says about a frozen occupant the ROOM no longer lists —
/// the departed-copy settlement's side of #1803 (R2-1).
///
/// Settling drops the copy permanently, so it needs the same every-peer proof
/// the eviction does. The shape that makes this load-bearing is a room-host
/// restart: the new incarnation has a valid claim fence and an EMPTY roster,
/// holds no registered-remote mirror for a peer's socket, and reads `Absent`
/// from the resumable-session probe for a live ATTACHED one. Every local
/// signal says "gone" about a user who is simply connected to the other
/// replica.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DepartedElsewhere {
    /// A peer holds the socket. The room host cannot deliver the copy itself
    /// (the mirror is not even installed here), but the peer's own
    /// maintenance pass can and does — see
    /// `muc_recovery_destination_owner_settles_local_copy`. So the copy stays
    /// owed rather than being dropped.
    PeerHoldsTheSocket,
    /// Every unexpired peer denies the resource and this node knows nothing
    /// about it either: nothing anywhere can ever take the copy.
    AbsentOnEveryPeer,
    /// The membership read cannot answer, so the set of nodes that would have
    /// to deny the resource is unknown. Fail closed.
    MembershipUnreadable,
    /// The occupant is still SEATED. The roster decides that alone: nothing
    /// else may be consulted, which is both correct and what keeps the
    /// per-occupant `sm_sessions` read off the `recover_row` hot path.
    StillSeated,
}

/// One scripted peer answer for the settlement fan-out.
struct DepartedPeers {
    case: DepartedElsewhere,
    socket_host: NodeIdentity,
    asked: Arc<std::sync::Mutex<Vec<(NodeIdentity, jid::FullJid)>>>,
}

#[async_trait::async_trait]
impl crate::clustering::resource_presence::ResourcePresenceAsker for DepartedPeers {
    async fn resource_presence(
        &self,
        peer: &NodeIdentity,
        target: &jid::FullJid,
    ) -> Result<
        crate::clustering::relay::RelayResourcePresenceReply,
        crate::clustering::relay::RelayAskError,
    > {
        use crate::clustering::relay::RelayResourcePresenceReply;
        self.asked
            .lock()
            .expect("probe log")
            .push((peer.clone(), target.clone()));
        match self.case {
            DepartedElsewhere::PeerHoldsTheSocket if peer == &self.socket_host => {
                Ok(RelayResourcePresenceReply::Present)
            }
            _ => Ok(RelayResourcePresenceReply::Absent),
        }
    }
}

async fn departed_occupant_cluster_recovery(f: IngressFixture, case: DepartedElsewhere) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let before = metrics
        .counter_sum("ingress.maintenance.departed_occupant_copies", &[])
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
    let muc = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .cloned()
        .expect("room fanout intent");
    let receipt = receipt_key(&muc).expect("MUC receipt");
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("room commit");
    let key = decision.message_key.expect("key");
    super::departed::settle_non_delivery_effects(&f, &planning_state, &decision).await;

    let local = NodeIdentity::new("recovering-node", "local-epoch");
    let socket_node = NodeIdentity::new("socket-node", "foreign-epoch");
    let bridge = OrderedRelayDeliveryBridge::new(
        tokio_util::sync::CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (membership, membership_reads) = ScriptedMembership::new(
        vec![socket_node.clone()],
        case != DepartedElsewhere::MembershipUnreadable,
    );
    let claims = Arc::new(InProcessClaimStore::new());
    let state = socket_tests::create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(Arc::clone(&claims) as Arc<dyn ClaimStore>),
            node_identity: Some(SharedNodeIdentity::new(local.clone())),
            ordered_relay_delivery_bridge: Some(Arc::clone(&bridge)),
            resource_presence: Some(Arc::new(DepartedPeers {
                case,
                socket_host: socket_node.clone(),
                asked: Arc::clone(&asked),
            })),
            cluster_membership: Some(membership),
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
    // The recovering node authoritatively hosts the room. Only `StillSeated`
    // joins the occupant: every other case models the post-restart incarnation
    // whose roster is empty.
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
    if case == DepartedElsewhere::StillSeated {
        actor
            .ask(Join {
                nick: "foreign".into(),
                real_jid: occupant.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join");
    }
    assert!(
        !state
            .deps
            .protocol
            .connection_registry
            .is_connected(&occupant),
        "the room host holds no socket and no mirror for the occupant"
    );

    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    assert_eq!(
        pass(&f, &env, &MaintenanceCursor::default()).await,
        MaintenanceOutcome::Complete
    );

    let settled = case == DepartedElsewhere::AbsentOnEveryPeer;
    let mut tx = f.uow.begin().await.expect("inspect recovered row");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("delivery progress"),
        if settled {
            vec![occupant.clone()]
        } else {
            vec![]
        },
        "a copy is dropped only when EVERY peer denies the resource"
    );
    assert_eq!(
        CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("terminal"),
        settled
    );
    tx.commit().await.expect("read commit");
    assert_eq!(
        metrics
            .counter_sum("ingress.maintenance.departed_occupant_copies", &[])
            .unwrap_or(0),
        before + u64::from(settled)
    );
    let asked = asked.lock().expect("probe log").clone();
    let reads = membership_reads.load(std::sync::atomic::Ordering::SeqCst);
    match case {
        // The roster is asked FIRST, so a seated occupant short-circuits
        // before the resumable-session read and before the cluster is
        // consulted at all.
        DepartedElsewhere::StillSeated => {
            assert_eq!(reads, 0, "a seated occupant needs no membership read");
            assert!(
                asked.is_empty(),
                "a seated occupant needs no peer ask: {asked:?}"
            );
        }
        DepartedElsewhere::MembershipUnreadable => {
            assert_eq!(reads, 1, "the membership read was attempted");
            assert!(
                asked.is_empty(),
                "no peer may be asked without a membership answer: {asked:?}"
            );
        }
        _ => {
            assert!(
                asked
                    .iter()
                    .all(|(node, jid)| node == &socket_node && jid == &occupant),
                "every peer is asked about the EXACT full JID: {asked:?}"
            );
            assert!(
                !asked.is_empty(),
                "the cluster must be asked before a copy is dropped"
            );
        }
    }
    f.close().await;
}

/// A healthy peer that is simply SLOW: it answers the only thing that permits
/// a settlement — `Absent` — but not before `delay`. That is what turns a
/// serial per-occupant probe loop into a budget overrun while every individual
/// probe stays well inside its own fan-out budget.
struct SlowAbsentPeer {
    delay: Duration,
    asks: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::clustering::resource_presence::ResourcePresenceAsker for SlowAbsentPeer {
    async fn resource_presence(
        &self,
        _peer: &NodeIdentity,
        _target: &jid::FullJid,
    ) -> Result<
        crate::clustering::relay::RelayResourcePresenceReply,
        crate::clustering::relay::RelayAskError,
    > {
        self.asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        Ok(crate::clustering::relay::RelayResourcePresenceReply::Absent)
    }
}

/// A clustered node that authoritatively hosts the room and has exactly one
/// unexpired peer, which `asker` answers for.
async fn one_peer_room(
    sm: Arc<InMemorySmSessionRegistry>,
    asker: Arc<dyn crate::clustering::resource_presence::ResourcePresenceAsker>,
) -> (
    Arc<WebSocketState>,
    kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
) {
    let local = NodeIdentity::new("recovering-node", "local-epoch");
    let peer = NodeIdentity::new("socket-node", "foreign-epoch");
    let bridge = OrderedRelayDeliveryBridge::new(
        tokio_util::sync::CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    let claims = Arc::new(InProcessClaimStore::new());
    let state = socket_tests::create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(Arc::clone(&claims) as Arc<dyn ClaimStore>),
            node_identity: Some(SharedNodeIdentity::new(local.clone())),
            ordered_relay_delivery_bridge: Some(Arc::clone(&bridge)),
            resource_presence: Some(asker),
            cluster_membership: Some(ScriptedMembership::new(vec![peer], true).0),
            ..Default::default()
        },
        sm,
    )
    .await;
    wire_for_test(
        &bridge,
        &state,
        claims as Arc<dyn ClaimStore>,
        SharedNodeIdentity::new(local),
    )
    .await;
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
            room_jid: "recovery@muc.example.com".parse().expect("room"),
            waddle_id: "recovery".into(),
            channel_id: "recovery".into(),
            config: Default::default(),
        })
        .await
        .expect("room");
    (state, actor)
}

/// #1803 review, settlement half: several roster-absent occupants on ONE route
/// behind a healthy but slow peer. Probed serially, the route spends
/// `recover_row`'s row deadline before `record_delivery_progress` ever runs —
/// the attempt is cancelled, nothing is persisted, and every later pass
/// repeats it. The occupants of a route are therefore probed concurrently, so
/// the wall-clock cost is one fan-out budget however many of them there are.
#[tokio::test]
async fn sqlite_departed_occupants_of_one_route_settle_within_the_row_deadline() {
    const OCCUPANTS: usize = 5;
    /// Comfortably inside `SETTLEMENT_FANOUT_BUDGET`, so every individual
    /// probe succeeds; five of them in series are not.
    const PEER_DELAY: Duration = Duration::from_millis(400);

    let f = IngressFixture::sqlite().await;
    let sm = persistent_sm(&f).await;
    let planning_state = state_for(&f, sm.clone()).await;
    let occupants: Vec<jid::FullJid> = (0..OCCUPANTS)
        .map(|index| {
            format!("ghost{index}@example.com/phone")
                .parse()
                .expect("occupant")
        })
        .collect();
    let submission = planned_room(&f, &planning_state, Case::Lost, &occupants).await;
    let muc = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .cloned()
        .expect("room fanout intent");
    let receipt = receipt_key(&muc).expect("MUC receipt");
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("room commit");
    let key = decision.message_key.expect("key");
    super::departed::settle_non_delivery_effects(&f, &planning_state, &decision).await;

    let asks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // The recovering incarnation hosts the room with an EMPTY roster, exactly
    // as a room host that just restarted does.
    let (state, _room) = one_peer_room(
        sm.clone(),
        Arc::new(SlowAbsentPeer {
            delay: PEER_DELAY,
            asks: Arc::clone(&asks),
        }),
    )
    .await;
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state));
    assert_eq!(
        pass_with_row_deadline(
            &f,
            &env,
            &MaintenanceCursor::default(),
            Duration::from_secs(1),
        )
        .await,
        MaintenanceOutcome::Complete
    );

    assert_eq!(
        asks.load(std::sync::atomic::Ordering::SeqCst),
        OCCUPANTS,
        "every occupant is asked about exactly once"
    );
    let mut expected = occupants.clone();
    expected.sort();
    let mut tx = f.uow.begin().await.expect("inspect recovered row");
    let mut progress = DeliveryProgressRepository::load(&mut tx, key, &receipt)
        .await
        .expect("delivery progress");
    progress.sort();
    assert_eq!(
        progress, expected,
        "one row deadline settles every occupant of the route"
    );
    assert!(
        EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("aggregate receipt"),
        "the frozen fanout is complete"
    );
    assert!(CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("terminal"));
    tx.commit().await.expect("read commit");
    f.close().await;
}

/// #1803 review, ghost half: `abandoned_occupancy` may spend the whole 1.5 s
/// ghost fan-out budget per owed occupant, and the whole repair runs inside
/// maintenance's 5 s `GHOST_REPAIR_BUDGET`. Probed serially, a handful of
/// seated ghosts behind one slow peer discard the proven prefix and the row is
/// parked and repeated forever; probed concurrently the repair costs one
/// fan-out budget.
#[tokio::test]
async fn sqlite_stalled_ghosts_of_one_room_are_repaired_within_the_repair_budget() {
    const GHOSTS: usize = 6;
    /// Inside `GHOST_FANOUT_BUDGET`, so each probe proves absence; six of them
    /// in series outrun the 5 s repair budget.
    const PEER_DELAY: Duration = Duration::from_millis(1_100);

    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let before = metrics
        .counter_sum("muc.ghost_occupants.evicted", &[])
        .unwrap_or(0);
    let f = IngressFixture::sqlite().await;
    let sm = persistent_sm(&f).await;
    let planning_state = state_for(&f, sm.clone()).await;
    let ghosts: Vec<jid::FullJid> = (0..GHOSTS)
        .map(|index| {
            format!("ghost{index}@example.com/phone")
                .parse()
                .expect("ghost")
        })
        .collect();
    let submission = planned_room(&f, &planning_state, Case::Lost, &ghosts).await;
    let muc = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .cloned()
        .expect("room fanout intent");
    let receipt = receipt_key(&muc).expect("MUC receipt");
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("room commit");
    let key = decision.message_key.expect("key");
    super::departed::settle_non_delivery_effects(&f, &planning_state, &decision).await;

    let asks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (state, actor) = one_peer_room(
        sm.clone(),
        Arc::new(SlowAbsentPeer {
            delay: PEER_DELAY,
            asks: Arc::clone(&asks),
        }),
    )
    .await;
    // Every ghost is SEATED here, so only the ghost repair — never the
    // departed-copy settlement — can discharge its copy.
    for ghost in &ghosts {
        actor
            .ask(Join {
                nick: ghost.node().expect("node").to_string(),
                real_jid: ghost.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join");
    }

    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    let cursor = MaintenanceCursor::default();
    for _ in 0..3 {
        assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
        cursor.wait_for_recovery_accounting().await;
    }

    for ghost in &ghosts {
        assert!(
            actor
                .ask(waddle_xmpp::muc::room_actor::GetOccupantByJid { jid: ghost.clone() })
                .await
                .expect("occupancy probe")
                .is_none(),
            "every proven ghost is unseated: {ghost}"
        );
    }
    assert_eq!(
        metrics
            .counter_sum("muc.ghost_occupants.evicted", &[])
            .unwrap_or(0),
        before + u64::try_from(GHOSTS).expect("ghost count"),
    );
    let mut expected = ghosts.clone();
    expected.sort();
    let mut tx = f.uow.begin().await.expect("inspect repaired row");
    let mut progress = DeliveryProgressRepository::load(&mut tx, key, &receipt)
        .await
        .expect("delivery progress");
    progress.sort();
    assert_eq!(progress, expected, "one repair settles every ghost's copy");
    assert!(CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("terminal"));
    tx.commit().await.expect("read commit");
    f.close().await;
}

/// THE R2-1 REGRESSION: the room host restarted, so its roster is empty and it
/// knows nothing about the occupant — but the occupant has a LIVE socket on the
/// other replica, which delivers the copy in its own maintenance pass. Dropping
/// it here would lose a message for a connected user.
#[tokio::test]
async fn sqlite_departed_occupant_with_a_peer_socket_keeps_its_copy_owed() {
    departed_occupant_cluster_recovery(
        IngressFixture::sqlite().await,
        DepartedElsewhere::PeerHoldsTheSocket,
    )
    .await;
}

#[tokio::test]
async fn sqlite_departed_occupant_absent_on_every_peer_settles() {
    departed_occupant_cluster_recovery(
        IngressFixture::sqlite().await,
        DepartedElsewhere::AbsentOnEveryPeer,
    )
    .await;
}

/// An unknown cluster is not an empty one.
#[tokio::test]
async fn sqlite_departed_occupant_keeps_its_copy_when_the_membership_read_fails() {
    departed_occupant_cluster_recovery(
        IngressFixture::sqlite().await,
        DepartedElsewhere::MembershipUnreadable,
    )
    .await;
}

/// Ordering: the roster answer alone decides a SEATED occupant, so neither the
/// per-occupant `sm_sessions` probe nor the cross-node fan-out runs for it.
#[tokio::test]
async fn sqlite_seated_occupant_is_decided_by_the_roster_alone() {
    departed_occupant_cluster_recovery(
        IngressFixture::sqlite().await,
        DepartedElsewhere::StillSeated,
    )
    .await;
}

/// #1803: what makes "no node hosts this room" PROVABLE in a cluster, and what
/// still keeps the copy owed once it is.
///
/// `GetRoom -> Ok(None)` alone only means "not here". With clustering the
/// durable room claim is the cluster-wide record of who owns the room, and it
/// is exactly what the empty-room destroy releases, so its absence is the
/// proof. Every other answer leaves the copy owed, and the every-peer
/// reachability proof is unchanged by any of them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum UnhostedCase {
    /// No `room_actor` claim row anywhere and every peer denies the resource.
    /// The only case that settles.
    NoClaimAnywhere,
    /// Hosted nowhere, but a peer still holds the occupant's socket: the
    /// hosting answer replaces the ROSTER evidence, never the reachability
    /// evidence.
    NoClaimButPeerHoldsTheSocket,
    /// A fresh claim on ANOTHER node: that node hosts the room (or is about to)
    /// and settles the row against its own roster.
    ForeignRoomClaim,
    /// A fresh claim held by THIS node with no local actor — a handoff or a
    /// restart in flight, and the actor is about to exist.
    OwnRoomClaim,
    /// The claim read cannot answer, so the hosting state is unknown.
    ClaimReadFails,
}

/// A claim store whose ROOM claim reads fail. Everything else delegates to the
/// real in-process store the rest of the fixture needs, so only the hosting
/// proof is unreadable.
struct UnreadableRoomClaims(Arc<InProcessClaimStore>);

#[async_trait::async_trait]
impl ClaimStore for UnreadableRoomClaims {
    async fn ensure_schema(&self) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.0.ensure_schema().await
    }
    async fn acquire(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.0.acquire(entity, me).await
    }
    async fn ensure_claimed(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.0.ensure_claimed(entity, me).await
    }
    async fn steal_stale(
        &self,
        entity: &Entity,
        observed: waddle_xmpp::ownership::ClaimEpoch,
        staleness: waddle_xmpp::ownership::StalePredicate,
        me: &NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.0.steal_stale(entity, observed, staleness, me).await
    }
    async fn steal_for_resume(
        &self,
        entity: &Entity,
        observed: waddle_xmpp::ownership::ClaimEpoch,
        witness: waddle_xmpp::ownership::ResumeIdentityProof,
        me: &NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.0.steal_for_resume(entity, observed, witness, me).await
    }
    async fn current_claim(
        &self,
        entity: &Entity,
    ) -> Result<Option<waddle_xmpp::ownership::ClaimSnapshot>, waddle_xmpp::ownership::ClaimError>
    {
        if entity.entity_type == EntityType::RoomActor {
            return Err(waddle_xmpp::ownership::ClaimError::Backend(
                "clustering_claims unreadable".to_string(),
            ));
        }
        self.0.current_claim(entity).await
    }
    async fn fence(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: waddle_xmpp::ownership::ClaimEpoch,
    ) -> Result<bool, waddle_xmpp::ownership::ClaimError> {
        self.0.fence(entity, me, mine).await
    }
    async fn release(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: waddle_xmpp::ownership::ClaimEpoch,
    ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.0.release(entity, me, mine).await
    }
    async fn release_many(
        &self,
        entities: &[Entity],
        me: &NodeIdentity,
    ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.0.release_many(entities, me).await
    }
}

async fn unhosted_room_recovery(f: IngressFixture, case: UnhostedCase) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let before = metrics
        .counter_sum("ingress.maintenance.departed_occupant_copies", &[])
        .unwrap_or(0);
    let sm = persistent_sm(&f).await;
    let planning_state = state_for(&f, sm.clone()).await;
    let occupant: jid::FullJid = "foreign@example.com/phone".parse().expect("occupant");
    let room: jid::BareJid = "recovery@muc.example.com".parse().expect("room");
    let submission = planned_room(
        &f,
        &planning_state,
        Case::Lost,
        std::slice::from_ref(&occupant),
    )
    .await;
    let muc = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .cloned()
        .expect("room fanout intent");
    let receipt = receipt_key(&muc).expect("MUC receipt");
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("room commit");
    let key = decision.message_key.expect("key");
    super::departed::settle_non_delivery_effects(&f, &planning_state, &decision).await;

    let local = NodeIdentity::new("recovering-node", "local-epoch");
    let elsewhere = NodeIdentity::new("other-node", "foreign-epoch");
    let inner = Arc::new(InProcessClaimStore::new());
    let room_entity = Entity::new(EntityType::RoomActor, room.to_string());
    match case {
        UnhostedCase::ForeignRoomClaim => {
            inner
                .acquire(&room_entity, &elsewhere)
                .await
                .expect("a peer owns the room");
        }
        UnhostedCase::OwnRoomClaim => {
            inner
                .acquire(&room_entity, &local)
                .await
                .expect("this node owns the room but has not published the actor");
        }
        _ => {}
    }
    let claims: Arc<dyn ClaimStore> = if case == UnhostedCase::ClaimReadFails {
        Arc::new(UnreadableRoomClaims(Arc::clone(&inner)))
    } else {
        Arc::clone(&inner) as Arc<dyn ClaimStore>
    };
    let bridge = OrderedRelayDeliveryBridge::new(
        tokio_util::sync::CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    let (membership, _reads) = ScriptedMembership::new(vec![elsewhere.clone()], true);
    let state = socket_tests::create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(Arc::clone(&claims)),
            node_identity: Some(SharedNodeIdentity::new(local.clone())),
            ordered_relay_delivery_bridge: Some(Arc::clone(&bridge)),
            resource_presence: Some(Arc::new(DepartedPeers {
                case: if case == UnhostedCase::NoClaimButPeerHoldsTheSocket {
                    DepartedElsewhere::PeerHoldsTheSocket
                } else {
                    DepartedElsewhere::AbsentOnEveryPeer
                },
                socket_host: elsewhere.clone(),
                asked: Arc::new(std::sync::Mutex::new(Vec::new())),
            })),
            cluster_membership: Some(membership),
            ..Default::default()
        },
        sm.clone(),
    )
    .await;
    wire_for_test(
        &bridge,
        &state,
        Arc::clone(&claims),
        SharedNodeIdentity::new(local),
    )
    .await;
    // The recovering node hosts NO room actor: `GetRoom` answers `Ok(None)`
    // and only the claim read can say whether anybody else does.
    assert!(
        state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
                room_jid: room.clone(),
            })
            .await
            .expect("registry lookup")
            .is_none(),
        "the fixture must host no local incarnation of the room"
    );

    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    assert_eq!(
        pass(&f, &env, &MaintenanceCursor::default()).await,
        MaintenanceOutcome::Complete
    );

    let settled = case == UnhostedCase::NoClaimAnywhere;
    let mut tx = f.uow.begin().await.expect("inspect recovered row");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("delivery progress"),
        if settled {
            vec![occupant.clone()]
        } else {
            vec![]
        },
        "only a room provably hosted by nobody settles without a roster"
    );
    assert_eq!(
        CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("terminal"),
        settled
    );
    tx.commit().await.expect("read commit");
    assert_eq!(
        metrics
            .counter_sum("ingress.maintenance.departed_occupant_copies", &[])
            .unwrap_or(0),
        before + u64::from(settled)
    );
    f.close().await;
}

/// The sibling-row shape after an empty-room destroy released the claim: no
/// node hosts the room, so it lists nobody and the copy is owed to nobody.
#[tokio::test]
async fn sqlite_unhosted_room_without_a_claim_settles_the_departed_copy() {
    unhosted_room_recovery(
        IngressFixture::sqlite().await,
        UnhostedCase::NoClaimAnywhere,
    )
    .await;
}

/// A hosting answer may only replace the roster evidence. An occupant a peer
/// still holds a socket for keeps its copy however the room is hosted.
#[tokio::test]
async fn sqlite_unhosted_room_keeps_a_copy_a_peer_can_still_take() {
    unhosted_room_recovery(
        IngressFixture::sqlite().await,
        UnhostedCase::NoClaimButPeerHoldsTheSocket,
    )
    .await;
}

#[tokio::test]
async fn sqlite_room_claimed_by_a_peer_settles_nothing_here() {
    unhosted_room_recovery(
        IngressFixture::sqlite().await,
        UnhostedCase::ForeignRoomClaim,
    )
    .await;
}

/// This node owns the claim but has not published the actor yet — a handoff or
/// restart in flight. The roster is about to exist, so nothing may be dropped.
#[tokio::test]
async fn sqlite_room_claimed_here_without_an_actor_settles_nothing() {
    unhosted_room_recovery(IngressFixture::sqlite().await, UnhostedCase::OwnRoomClaim).await;
}

#[tokio::test]
async fn sqlite_unreadable_room_claim_settles_nothing() {
    unhosted_room_recovery(IngressFixture::sqlite().await, UnhostedCase::ClaimReadFails).await;
}
