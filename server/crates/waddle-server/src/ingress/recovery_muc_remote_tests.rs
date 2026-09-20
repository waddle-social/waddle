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

/// How another node relates to the occupant, which decides whether evicting
/// its occupancy here would tear down a seat that is alive elsewhere.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ElsewhereCase {
    /// A fresh `UserActor` claim owned by a different node, whose owner
    /// answers the #1803 cross-node resource-presence probe this way.
    ForeignClaim(OwnerAnswer),
    /// A registered-remote mirror for the exact full JID, installed here by
    /// the claim owner. Decided locally, without any probe.
    RegisteredRemote,
}

/// What the account's claim owner says about the exact ghost RESOURCE.
///
/// A `UserActor` claim is per account, so a fresh foreign claim alone proves
/// only that the USER is online somewhere — which is precisely the production
/// shape that stalled the backlog (#1803): one healthy `web-<uuid>` resource
/// keeping the claim alive while a second, abandoned one pins the room's
/// frozen `route_muc` obligation forever.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OwnerAnswer {
    /// The owner's actor tree lists this exact resource, or it holds a
    /// resumable session for it.
    Present,
    /// The owner holds the fresh claim and knows nothing about this exact
    /// resource: the only authoritative negative.
    Absent,
    /// The ask never produced an answer — a peer that predates
    /// `waddle.clustering.relay.resource_presence.v1` answering
    /// `UnknownMessage` during a rolling update, a timeout, or a transport
    /// failure. Fail closed.
    AskFails,
}

/// One scripted answer from the account's claim owner, recording exactly who
/// was asked about which resource.
struct ScriptedResourcePresence {
    answer: OwnerAnswer,
    asked: Arc<std::sync::Mutex<Vec<(NodeIdentity, jid::FullJid)>>>,
}

#[async_trait::async_trait]
impl crate::clustering::resource_presence::ResourcePresenceAsker for ScriptedResourcePresence {
    async fn resource_presence(
        &self,
        owner: &NodeIdentity,
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
            .push((owner.clone(), target.clone()));
        match self.answer {
            OwnerAnswer::Present => Ok(RelayResourcePresenceReply::Present),
            OwnerAnswer::Absent => Ok(RelayResourcePresenceReply::Absent),
            // Exactly what kameo reports for a peer that does not know the
            // message id: a no-effect codec failure.
            OwnerAnswer::AskFails => Err(RelayAskError::Send {
                failure: RelaySendFailure::Codec,
                effect: RelaySendEffect::NoEffect,
                message: "peer does not know waddle.clustering.relay.resource_presence.v1"
                    .to_string(),
            }),
        }
    }
}

/// XEP-0045 ghost eviction is for occupancies NOTHING can reach. An occupant
/// another node still owns keeps its seat even once its frozen copy has
/// stalled long enough to be classified `no_durable_progress` — but a
/// resource the claim owner authoritatively does not know is a ghost, even
/// while its account stays online on that owner.
async fn stalled_remote_occupant_recovery(f: IngressFixture, case: ElsewhereCase) {
    let evictable = case == ElsewhereCase::ForeignClaim(OwnerAnswer::Absent);
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
    let entity = Entity::new(EntityType::UserActor, occupant.to_bare().to_string());
    // The registered-remote mirror is installed by the claim OWNER, so that
    // case leaves the claim here and isolates the resource-lookup guard.
    let owner = match case {
        ElsewhereCase::ForeignClaim(_) => NodeIdentity::new("socket-node", "foreign-epoch"),
        ElsewhereCase::RegisteredRemote => local.clone(),
    };
    claims.acquire(&entity, &owner).await.expect("claim");
    let bridge = OrderedRelayDeliveryBridge::new(
        tokio_util::sync::CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    // The registered-remote case must be decided locally, so any probe at all
    // there would be a bug: it gets an asker that never answers positively.
    let scripted_answer = match case {
        ElsewhereCase::ForeignClaim(answer) => answer,
        ElsewhereCase::RegisteredRemote => OwnerAnswer::Absent,
    };
    let state = socket_tests::create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(Arc::clone(&claims) as Arc<dyn ClaimStore>),
            node_identity: Some(SharedNodeIdentity::new(local.clone())),
            ordered_relay_delivery_bridge: Some(Arc::clone(&bridge)),
            resource_presence: Some(Arc::new(ScriptedResourcePresence {
                answer: scripted_answer,
                asked: Arc::clone(&asked),
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
        ElsewhereCase::ForeignClaim(_) => assert!(
            !locally_registered,
            "no local entry: the ownership claim must be what decides"
        ),
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
        "only a resource the claim owner authoritatively denies is evicted"
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
        ElsewhereCase::ForeignClaim(_) => {
            assert!(
                !asked.is_empty(),
                "a fresh foreign claim must be resolved by asking its owner"
            );
            assert!(
                asked
                    .iter()
                    .all(|(node, jid)| node == &owner && jid == &occupant),
                "the owner must be asked about the EXACT full JID: {asked:?}"
            );
        }
        ElsewhereCase::RegisteredRemote => assert!(
            asked.is_empty(),
            "a locally mirrored resource needs no cross-node ask: {asked:?}"
        ),
    }
    f.close().await;
}

#[tokio::test]
async fn sqlite_stalled_remote_owned_occupant_keeps_its_seat() {
    stalled_remote_occupant_recovery(
        IngressFixture::sqlite().await,
        ElsewhereCase::ForeignClaim(OwnerAnswer::Present),
    )
    .await;
}

/// Rolling-deploy safety: a claim owner that cannot answer the probe (an
/// older peer answering `UnknownMessage`, a timeout, a transport failure)
/// leaves absence unproven, so the occupant keeps its seat.
#[tokio::test]
async fn sqlite_stalled_remote_owned_occupant_keeps_its_seat_when_the_probe_fails() {
    stalled_remote_occupant_recovery(
        IngressFixture::sqlite().await,
        ElsewhereCase::ForeignClaim(OwnerAnswer::AskFails),
    )
    .await;
}

/// The dominant production shape (#1803): the room is hosted here, the user's
/// `UserActor` claim is fresh on another node — because a healthy sibling
/// resource keeps it there — and the ghost resource pinning this row is one
/// the owner does not know. The per-account claim must not vouch for it.
#[tokio::test]
async fn sqlite_stalled_ghost_resource_of_a_live_remote_user_is_evicted() {
    stalled_remote_occupant_recovery(
        IngressFixture::sqlite().await,
        ElsewhereCase::ForeignClaim(OwnerAnswer::Absent),
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
