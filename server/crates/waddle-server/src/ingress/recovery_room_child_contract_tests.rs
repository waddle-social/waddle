//! Native PostgreSQL characterization of accepted sealed room children.
//! Single-node retirement does not authorize retirement after unknown clustered
//! occupancy or lifecycle loss. No clustered manifest activation is used here.
use super::departed::settle_non_delivery_effects;
use super::{
    append_count, commit_submission, pass, persistent_sm, planned_room, state_for, store_detached,
    Case, StateEnvironment,
};
use crate::ingress::append_authority::{same_message_content, AppendAuthority};
use crate::ingress::effects::RoomExecutionPath;
use crate::ingress::maintenance::{MaintenanceCursor, MaintenanceOutcome};
use crate::ingress::room_canonical::occupant_copy_message;
use crate::ingress::room_manifest_replay;
use crate::ingress::test_support::IngressFixture;
use crate::ingress::xep0045_room_manifest_tests::room_submission;
use crate::ingress::{receipt_key, RecoveryEnvironment};
use crate::ingress_substrate::MessageEnvelope;
use crate::ingress_uow::{CanonicalMessageRepository, IngressUowError};
use crate::server::routes::interpret::SmIngressAppendContext;
use crate::server::routes::websocket::WebSocketState;
use jid::FullJid;
use std::sync::Arc;
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey};
use waddle_xmpp::muc::durable::{
    OccupancyLeaveCause, RoomCommittedCoordinates, RoomLifecycleId, RoomRevision,
};
use waddle_xmpp::muc::room_actor::{
    ConfigEffectPlan, GetSnapshot, Join, LeaveAttemptId, LeaveByRealJid, LeaveOrigin,
    LeaveSessionSelector, UpdateConfig,
};
use waddle_xmpp::muc::room_registry_actor::{
    CreateRoom, DestroyRoom, DestroyRoomOutcome, DestroyRoomReason, GetRoom,
};
use waddle_xmpp::stream_management::SmSessionRegistry;
use waddle_xmpp::{Affiliation, Role, Stanza};
use waddle_xmpp_core::OccupancySessionGeneration;

async fn postgres(name: &str) -> IngressFixture {
    IngressFixture::postgres(name).await.expect(
        "room_child_contract requires WADDLE_TEST_POSTGRES_URL pointing to native PostgreSQL",
    )
}

async fn envelope(f: &IngressFixture, key: MessageKey) -> MessageEnvelope {
    let mut tx = f.uow.begin().await.expect("read accepted source");
    let source = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope query")
        .expect("accepted source");
    assert!(source.room_manifest().is_some(), "production seal required");
    tx.commit().await.expect("read commit");
    source
}

async fn join(state: &WebSocketState, resource: &FullJid, nick: &str) {
    state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: "recovery@muc.example.com".parse().expect("room"),
        })
        .await
        .expect("registry")
        .expect("room actor")
        .ask(Join {
            session: OccupancySessionGeneration::mint(),
            nick: nick.into(),
            real_jid: resource.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");
}

async fn leave(state: &WebSocketState, resource: &FullJid) {
    state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: "recovery@muc.example.com".parse().expect("room"),
        })
        .await
        .expect("registry")
        .expect("room actor")
        .ask(LeaveByRealJid {
            sender_jid: resource.clone(),
            cause: OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: LeaveOrigin::Fresh,
        })
        .await
        .expect("leave");
}

async fn recover_twice(f: &IngressFixture, state: Arc<WebSocketState>, key: MessageKey) {
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state));
    for _ in 0..2 {
        assert_eq!(
            pass(f, &env, &MaintenanceCursor::default()).await,
            MaintenanceOutcome::Complete
        );
        let mut tx = f.uow.begin().await.expect("inspect terminal");
        assert!(CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("terminal query"));
        tx.commit().await.expect("inspection commit");
    }
}

async fn policy_reasons(f: &IngressFixture, key: MessageKey) -> Vec<(String, String)> {
    let connection = f.db.guard().await.expect("policy inspection");
    let mut rows = connection.query(
        "SELECT resource, policy_discard_reason FROM ingress_delivery_receipts WHERE message_key = ?::uuid AND policy_discard_reason IS NOT NULL ORDER BY resource",
        crate::db_params![key.to_storage().to_string()],
    ).await.expect("child resolutions");
    let mut reasons = Vec::new();
    while let Some(row) = rows.next().await.expect("resolution row") {
        reasons.push((row.get(0).expect("resource"), row.get(1).expect("reason")));
    }
    reasons
}

#[tokio::test]
#[ignore = "requires native PostgreSQL; dedicated room_child_contract lane"]
async fn postgres_sealed_child_delayed_release_preserves_payload_and_target() {
    let f = postgres("sealed_delayed").await;
    let sm = persistent_sm(&f).await;
    let state = state_for(&f, sm.clone()).await;
    let original: FullJid = "alice@example.com/phone".parse().expect("original");
    let newcomer: FullJid = "claire@example.com/phone".parse().expect("newcomer");
    let submission = planned_room(&f, &state, Case::Partial, std::slice::from_ref(&original)).await;
    for resource in [&original, &newcomer, &submission.sender] {
        store_detached(&sm, resource).await;
    }
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("accept");
    let key = decision.message_key.expect("key");
    settle_non_delivery_effects(&f, &state, &decision).await;
    let frozen = envelope(&f, key).await;
    assert!(
        !frozen.message().bodies.is_empty(),
        "exercise actual content"
    );
    assert_eq!(f.count("sm_ingress_appends").await, 0, "release is delayed");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: frozen.room_manifest().expect("manifest").room.clone(),
        })
        .await
        .expect("registry")
        .expect("room");
    let before = actor.ask(GetSnapshot).await.expect("snapshot");
    join(&state, &newcomer, "claire").await;
    // Joining advances occupancy, not admission policy. A real config mutation
    // advances admission without replacing the accepted child identities.
    let mut config = before.room.config.clone();
    config.name = "Renamed after acceptance".into();
    actor
        .ask(UpdateConfig {
            config,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await
        .expect("config revision advance");
    let after = actor.ask(GetSnapshot).await.expect("snapshot");
    assert!(after.admission_revision > before.admission_revision);
    assert!(after.occupancy_revision > before.occupancy_revision);
    assert_eq!(after.occupancy_incarnation, before.occupancy_incarnation);
    recover_twice(&f, state, key).await;
    assert_eq!(
        envelope(&f, key).await.room_manifest(),
        frozen.room_manifest()
    );
    assert_eq!(
        append_count(&sm, &newcomer).await,
        0,
        "audience cannot widen"
    );
    for resource in [&original, &submission.sender] {
        let session = sm
            .peek_session(&resource.to_string())
            .await
            .expect("SM")
            .expect("session");
        assert_eq!(
            session.unacked_stanzas.len(),
            1,
            "retry cannot duplicate custody"
        );
        let delivered =
            waddle_xmpp::parser::message_from_string(&session.unacked_stanzas[0].stanza_xml)
                .expect("persisted copy");
        assert_eq!(delivered.to, Some(resource.clone().into()));
        assert!(same_message_content(
            &occupant_copy_message(frozen.message(), resource),
            &delivered
        ));
    }
    assert_eq!(
        f.count("sm_ingress_appends").await,
        2,
        "original recipient and reflection each own one durable append"
    );
    assert!(policy_reasons(&f, key).await.is_empty());
    f.close().await;
}

#[tokio::test]
#[ignore = "requires native PostgreSQL; dedicated room_child_contract lane"]
async fn postgres_sealed_child_leave_rejoin_retires_original_without_delivery() {
    let f = postgres("sealed_rejoin").await;
    let sm = persistent_sm(&f).await;
    let state = state_for(&f, sm.clone()).await;
    let original: FullJid = "alice@example.com/phone".parse().expect("original");
    let submission = planned_room(&f, &state, Case::Partial, std::slice::from_ref(&original)).await;
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("accept");
    let key = decision.message_key.expect("key");
    settle_non_delivery_effects(&f, &state, &decision).await;
    let frozen = envelope(&f, key).await;
    leave(&state, &original).await;
    join(&state, &original, "replacement-alice").await;
    store_detached(&sm, &original).await;
    store_detached(&sm, &submission.sender).await;
    recover_twice(&f, state, key).await;
    assert_eq!(
        envelope(&f, key).await.room_manifest(),
        frozen.room_manifest()
    );
    assert_eq!(
        append_count(&sm, &original).await,
        0,
        "replacement occupancy cannot inherit old child"
    );
    assert_eq!(append_count(&sm, &submission.sender).await, 1);
    assert_eq!(
        f.count("sm_ingress_appends").await,
        1,
        "only the unchanged sender occupancy acquires durable custody"
    );
    assert_eq!(
        policy_reasons(&f, key).await,
        vec![(original.to_string(), "occupancy_superseded".into())]
    );
    f.close().await;
}

#[tokio::test]
#[ignore = "requires native PostgreSQL; dedicated room_child_contract lane"]
async fn postgres_sealed_children_destroy_recreate_retire_without_transport() {
    for recreate in [false, true] {
        let f = postgres("sealed_destroy").await;
        let sm = persistent_sm(&f).await;
        let state = state_for(&f, sm.clone()).await;
        let original: FullJid = "alice@example.com/phone".parse().expect("original");
        let submission =
            planned_room(&f, &state, Case::Partial, std::slice::from_ref(&original)).await;
        let decision = commit_submission(&f.uow, &submission, 1)
            .await
            .expect("accept");
        let key = decision.message_key.expect("key");
        settle_non_delivery_effects(&f, &state, &decision).await;
        let frozen = envelope(&f, key).await;
        let room = frozen.room_manifest().expect("manifest").room.clone();
        assert_eq!(
            state
                .deps
                .protocol
                .room_registry
                .ask(DestroyRoom {
                    room_jid: room.clone(),
                    reason: DestroyRoomReason::Destroy,
                })
                .await
                .expect("destroy"),
            DestroyRoomOutcome::Destroyed
        );
        if recreate {
            state
                .deps
                .protocol
                .room_registry
                .ask(CreateRoom {
                    room_jid: room,
                    waddle_id: "replacement".into(),
                    channel_id: "replacement".into(),
                    config: Default::default(),
                })
                .await
                .expect("recreate");
            join(&state, &original, "alice").await;
            join(&state, &submission.sender, "romeo").await;
        }
        for resource in [&original, &submission.sender] {
            store_detached(&sm, resource).await;
        }
        recover_twice(&f, state, key).await;
        assert_eq!(
            envelope(&f, key).await.room_manifest(),
            frozen.room_manifest()
        );
        assert_eq!(f.count("sm_ingress_appends").await, 0);
        for resource in [&original, &submission.sender] {
            assert_eq!(append_count(&sm, resource).await, 0);
        }
        let reason = if recreate {
            "occupancy_incarnation_lost"
        } else {
            "room_retired"
        };
        assert_eq!(
            policy_reasons(&f, key).await,
            vec![
                (original.to_string(), reason.into()),
                (submission.sender.to_string(), reason.into())
            ]
        );
        f.close().await;
    }
}

#[tokio::test]
#[ignore = "requires native PostgreSQL; dedicated room_child_contract lane"]
async fn postgres_sealed_child_rejects_changed_payload_or_target_before_sink() {
    let f = postgres("sealed_payload").await;
    let sm = persistent_sm(&f).await;
    let state = state_for(&f, sm).await;
    let original: FullJid = "alice@example.com/phone".parse().expect("original");
    let sibling: FullJid = "alice@example.com/new".parse().expect("sibling");
    let submission = planned_room(&f, &state, Case::Partial, std::slice::from_ref(&original)).await;
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("accept");
    let key = decision.message_key.expect("key");
    let frozen = envelope(&f, key).await;
    let route = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .expect("route");
    let reflection =
        crate::ingress::reflection_dispatch::original_intent(route).expect("reflection");
    for (intent, target) in [(route, &original), (&reflection, &submission.sender)] {
        let context = SmIngressAppendContext {
            message_key: key,
            receipt: receipt_key(intent).expect("receipt"),
            received_at: None,
            archive_positions: Vec::new(),
            dispatch_stream: None,
            authority: AppendAuthority::Verified,
        };
        let copy = occupant_copy_message(frozen.message(), target);
        assert_eq!(
            room_manifest_replay::load(&f.uow, &context, target, &Stanza::Message(copy.clone()))
                .await
                .expect("original child"),
            frozen.room_manifest().cloned()
        );
        let mut changed = copy;
        changed
            .bodies
            .insert(Default::default(), "replacement content".into());
        assert!(matches!(
            room_manifest_replay::load(&f.uow, &context, target, &Stanza::Message(changed)).await,
            Err(IngressUowError::EffectIntentConflict)
        ));
        let changed_target = occupant_copy_message(frozen.message(), &sibling);
        assert!(matches!(
            room_manifest_replay::load(
                &f.uow,
                &context,
                &sibling,
                &Stanza::Message(changed_target)
            )
            .await,
            Err(IngressUowError::EffectIntentConflict)
        ));
    }
    assert_eq!(f.count("sm_ingress_appends").await, 0);
    assert_eq!(f.count("ingress_delivery_receipts").await, 0);
    assert_eq!(
        envelope(&f, key).await.room_manifest(),
        frozen.room_manifest()
    );
    f.close().await;
}

/// The minimal coordinate table isolates durable admission, not a clustered
/// room-store model. Live actor release is exercised by the other four tests.
#[tokio::test]
#[ignore = "requires native PostgreSQL; dedicated room_child_contract lane"]
async fn postgres_sealed_manifest_preserves_children_after_durable_revision_advance() {
    let f = postgres("sealed_revision").await;
    let mut submission = room_submission(&f);
    let coordinates = RoomCommittedCoordinates {
        lifecycle: RoomLifecycleId::generate(),
        revision: RoomRevision::initial(),
    };
    let RoomExecutionPath::Local {
        manifest_snapshot: Some(snapshot),
        ..
    } = &mut submission.plan.room_execution
    else {
        panic!("snapshot")
    };
    snapshot.coordinates = Some(coordinates);
    f.execute("CREATE TABLE clustering_muc_rooms (room_jid TEXT PRIMARY KEY, lifecycle_id TEXT, revision BIGINT)", ()).await;
    f.execute(
        "INSERT INTO clustering_muc_rooms (room_jid, lifecycle_id, revision) VALUES (?, ?, ?)",
        crate::db_params![
            "room@muc.example.com",
            coordinates.lifecycle.to_string(),
            2_i64
        ],
    )
    .await;
    assert!(commit_submission(&f.uow, &submission, 1).await.is_err());
    assert_eq!(f.count("ingress_messages").await, 0);
    f.execute("UPDATE clustering_muc_rooms SET revision = 1", ())
        .await;
    let first = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("current revision");
    let key = first.message_key.expect("key");
    let frozen = envelope(&f, key).await;
    assert_eq!(
        frozen.room_manifest().expect("manifest").coordinates,
        Some(coordinates)
    );
    f.execute("UPDATE clustering_muc_rooms SET revision = 2", ())
        .await;
    let replay = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("accepted replay");
    assert_eq!(replay.message_key, Some(key));
    assert_eq!(
        envelope(&f, key).await.room_manifest(),
        frozen.room_manifest()
    );
    f.close().await;
}
