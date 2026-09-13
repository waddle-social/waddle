//! Foreign ownership never turns maintenance's detached recovery into a relay.
use super::*;
use crate::clustering::{route_bridge::OrderedRelayDeliveryBridge, ClusteringHandles};
use crate::ingress::{recorded::RouteProgress, recovery_rebuild};
use crate::server::routes::interpret::{effects::EffectOutcome, FullJidDeliveryOutcome};
use waddle_xmpp::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};

async fn remote_owned_recovery(f: IngressFixture) {
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
    let accepted = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("room commit");
    let key = accepted.message_key.expect("key");
    let mut tx = f.uow.begin().await.expect("frozen authority");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("load")
        .expect("envelope");
    let recorded = crate::ingress_uow::EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("recorded");
    tx.commit().await.expect("read commit");
    let muc = recorded
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .expect("MUC");
    let receipt = receipt_key(muc).expect("receipt");
    let progress = RouteProgress::from_intent(muc, None, vec![])
        .expect("progress")
        .expect("MUC progress");
    let rebuilt = recovery_rebuild::rebuild(recovery_rebuild::RecoveryInput {
        key,
        envelope: &envelope,
        created_at: chrono::Utc::now(),
        recorded: &recorded,
        unreceipted: &recorded,
        route_progress: vec![progress],
        blocked_recipients: &[],
    })
    .expect("rebuild remote occupant");
    assert!(
        matches!(&rebuilt.decision.external[..], [ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. })] if resources == std::slice::from_ref(&occupant)),
        "recovery emits only the exact detached copy, never a relay"
    );

    let claims = Arc::new(InProcessClaimStore::new());
    let remote = NodeIdentity::new("foreign-owner", "foreign-epoch");
    let local = NodeIdentity::new("recovering-owner", "local-epoch");
    let entity = Entity::new(EntityType::UserActor, occupant.to_bare().to_string());
    claims
        .acquire(&entity, &remote)
        .await
        .expect("foreign claim");
    let owner = claims
        .current_claim(&entity)
        .await
        .expect("claim lookup")
        .expect("foreign owner");
    assert!(owner.owner_lease_fresh);
    assert_eq!(owner.owner, remote);
    assert_ne!(owner.owner, local);
    let state = socket_tests::create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(claims),
            node_identity: Some(SharedNodeIdentity::new(local)),
            ordered_relay_delivery_bridge: Some(OrderedRelayDeliveryBridge::new(
                tokio_util::sync::CancellationToken::new(),
                &crate::config::ClusteringMessagingConfig::default(),
            )),
            ..Default::default()
        },
        sm,
    )
    .await;
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state));
    let deps = env.recovery_deps();
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
        Some(vec![(occupant, FullJidDeliveryOutcome::Unavailable)])
    );
    assert!(settled.persisted.is_empty(), "no foreign delivery proof");
    assert_eq!(
        pass(&f, &env, &MaintenanceCursor::default()).await,
        MaintenanceOutcome::Complete
    );
    assert!(
        super::super::super::attempt_count(key) > 0,
        "maintenance attempts remote-owned MUC obligation"
    );
    let mut tx = f.uow.begin().await.expect("inspect pending row");
    assert!(DeliveryProgressRepository::load(&mut tx, key, &receipt)
        .await
        .expect("progress")
        .is_empty());
    assert!(!EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash
    )
    .await
    .expect("aggregate"));
    assert!(!CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("terminal"));
    tx.commit().await.expect("read commit");
    assert_eq!(f.count("sm_ingress_appends").await, 0);
    assert_eq!(f.count("mam_messages").await, 0);
    f.close().await;
}

#[tokio::test]
async fn sqlite_muc_recovery_remote_owner_stays_pending_without_relay() {
    remote_owned_recovery(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_muc_recovery_remote_owner_stays_pending_without_relay() {
    if let Some(f) = IngressFixture::postgres("muc_recovery_remote").await {
        remote_owned_recovery(f).await;
    }
}
