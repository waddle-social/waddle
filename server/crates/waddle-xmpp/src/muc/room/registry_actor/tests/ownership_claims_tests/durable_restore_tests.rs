use super::*;

fn recover(room_jid: BareJid) -> GetOrRestoreDurableRoom {
    GetOrRestoreDurableRoom { room_jid }
}

fn durable_snapshot() -> DurableRoomState {
    let mut state = restored_room_snapshot("existing-room");
    state.coordinates = Some(crate::muc::RoomCommittedCoordinates {
        lifecycle: crate::muc::RoomLifecycleId::generate(),
        revision: crate::muc::RoomRevision::initial(),
    });
    state
}

#[tokio::test]
async fn durable_recovery_without_a_store_does_not_fabricate_a_room() {
    let registry = spawn_registry().await;
    assert!(registry
        .ask(recover(test_room_jid("no-store")))
        .await
        .expect("lookup")
        .is_none());
    assert_eq!(registry.ask(RoomCount).await.expect("count"), 0);
}

#[tokio::test]
async fn durable_recovery_never_creates_missing_or_destroyed_rooms() {
    for destroyed in [false, true] {
        let registry = spawn_registry().await;
        let store = Arc::new(RecordingDurableStore::default());
        let claims = wire_recording_store(&registry, Arc::clone(&store)).await;
        let room = test_room_jid("absent-recovery");
        if destroyed {
            registry
                .ask(get_or_create(room.clone()))
                .await
                .expect("create");
            assert_eq!(
                registry
                    .ask(DestroyRoom {
                        room_jid: room.clone(),
                        reason: DestroyRoomReason::Destroy,
                    })
                    .await
                    .expect("destroy"),
                DestroyRoomOutcome::Destroyed
            );
        }
        let before = store.recorded_commits().len();
        assert!(registry
            .ask(recover(room.clone()))
            .await
            .expect("recover")
            .is_none());
        assert_eq!(store.recorded_commits().len(), before);
        assert_eq!(registry.ask(RoomCount).await.expect("count"), 0);
        assert!(claims
            .current_claim(&Entity::new(EntityType::RoomActor, room.to_string()))
            .await
            .expect("claim lookup")
            .is_none());
    }
}

#[tokio::test]
async fn durable_recovery_restores_dormant_room_after_registry_restart_without_occupants() {
    let registry = spawn_registry().await;
    let store = Arc::new(RecordingDurableStore::default());
    let claims = wire_recording_store(&registry, Arc::clone(&store)).await;
    let room = test_room_jid("dormant-recovery");
    let actor = registry
        .ask(get_or_create(room.clone()))
        .await
        .expect("create")
        .actor_ref;
    let previous = actor.ask(GetSnapshot).await.expect("snapshot");
    assert!(registry
        .ask(DestroyRoomIfInactive {
            room_jid: room.clone(),
            expected_occupancy_revision: previous.occupancy_revision,
            guard: SealGuard::Dormant,
        })
        .await
        .expect("dormancy")
        .destroyed());
    registry.kill();
    let registry = spawn_registry().await;
    wire_recording_store_with_claims(
        &registry,
        claims,
        SharedNodeIdentity::new(this_identity()),
        Arc::clone(&store),
    )
    .await;
    let before = store.recorded_commits().len();
    let restored = registry
        .ask(recover(room))
        .await
        .expect("recovery")
        .expect("restored");
    let snapshot = restored.ask(GetSnapshot).await.expect("snapshot");
    assert!(snapshot.room.occupants.is_empty());
    assert_eq!(snapshot.room.config, previous.room.config);
    assert_eq!(
        snapshot.durable_coordinates.expect("coordinates").lifecycle,
        previous
            .durable_coordinates
            .expect("previous coordinates")
            .lifecycle
    );
    assert!(!store.recorded_commits()[before..]
        .iter()
        .any(|(intent, _)| matches!(intent, crate::muc::RoomDurableMutation::Create { .. })));
    assert_eq!(
        registry
            .ask(GetRoom {
                room_jid: snapshot.room.room_jid
            })
            .await
            .expect("lookup")
            .expect("registered")
            .id(),
        restored.id()
    );
}

#[tokio::test]
async fn durable_recovery_does_not_displace_a_fresh_remote_owner() {
    let registry = spawn_registry().await;
    let store = Arc::new(RecordingDurableStore {
        load_result: Some(durable_snapshot()),
        ..RecordingDurableStore::default()
    });
    let claims = wire_recording_store(&registry, Arc::clone(&store)).await;
    let room = test_room_jid("remote-recovery");
    let entity = Entity::new(EntityType::RoomActor, room.to_string());
    claims
        .acquire(&entity, &foreign_identity())
        .await
        .expect("foreign claim");
    assert!(matches!(
        registry.ask(recover(room)).await,
        Err(SendError::HandlerError(
            RoomRegistryError::ClaimHeldByAnotherNode(_)
        ))
    ));
    assert_eq!(store.load_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        claims
            .current_claim(&entity)
            .await
            .expect("claim")
            .expect("owner")
            .owner,
        foreign_identity()
    );
}

#[tokio::test]
async fn durable_recovery_checks_lifecycle_again_before_activation() {
    for destroyed in [false, true] {
        let registry = spawn_registry().await;
        let room = test_room_jid("racing-recovery");
        let started = Arc::new(tokio::sync::Notify::new());
        let allow = Arc::new(tokio::sync::Notify::new());
        let store = Arc::new(RecordingDurableStore {
            block_load_from_call: Some(1),
            load_started: Some(Arc::clone(&started)),
            allow_load: Some(Arc::clone(&allow)),
            ..RecordingDurableStore::default()
        });
        store
            .persisted_room_states
            .lock()
            .expect("states")
            .insert(room.clone(), durable_snapshot());
        wire_recording_store(&registry, Arc::clone(&store)).await;
        let recovery = tokio::spawn({
            let registry = registry.clone();
            let room = room.clone();
            async move { registry.ask(recover(room)).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .expect("discovery load");
        if destroyed {
            store
                .persisted_room_states
                .lock()
                .expect("states")
                .remove(&room);
        } else {
            store
                .persisted_room_states
                .lock()
                .expect("states")
                .insert(room.clone(), durable_snapshot());
        }
        allow.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .expect("restoration load");
        allow.notify_one();
        assert!(recovery.await.expect("worker").expect("recovery").is_none());
        assert!(
            store.recorded_commits().is_empty(),
            "stale discovery must not Create, Activate, or Publish"
        );
        assert!(registry
            .ask(GetRoom { room_jid: room })
            .await
            .expect("lookup")
            .is_none());
    }
}

#[tokio::test]
async fn durable_recovery_releases_its_claim_when_fenced_discovery_fails() {
    let registry = spawn_registry().await;
    let store = Arc::new(RecordingDurableStore {
        lose_restore_ownership_on_call: Some(1),
        load_result: Some(durable_snapshot()),
        ..RecordingDurableStore::default()
    });
    let claims = wire_recording_store(&registry, Arc::clone(&store)).await;
    let room = test_room_jid("lost-recovery");
    assert!(matches!(
        registry.ask(recover(room.clone())).await,
        Err(SendError::HandlerError(
            RoomRegistryError::OwnershipUnavailable(_)
        ))
    ));
    assert!(store.recorded_commits().is_empty());
    assert!(claims
        .current_claim(&Entity::new(EntityType::RoomActor, room.to_string()))
        .await
        .expect("claim lookup")
        .is_none());
}

#[tokio::test]
async fn durable_recovery_bounds_a_stalled_discovery_read() {
    let registry = spawn_registry().await;
    let store = Arc::new(RecordingDurableStore {
        block_all_loads: true,
        allow_load: Some(Arc::new(tokio::sync::Notify::new())),
        ..RecordingDurableStore::default()
    });
    let claims = wire_recording_store(&registry, Arc::clone(&store)).await;
    let room = test_room_jid("stalled-recovery");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        registry.ask(recover(room.clone())),
    )
    .await
    .expect("bounded recovery");
    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            RoomRegistryError::OwnershipUnavailable(_)
        ))
    ));
    assert!(claims
        .current_claim(&Entity::new(EntityType::RoomActor, room.to_string()))
        .await
        .expect("claim lookup")
        .is_none());
    assert_eq!(
        registry.ask(RoomCount).await.expect("responsive registry"),
        0
    );
}
