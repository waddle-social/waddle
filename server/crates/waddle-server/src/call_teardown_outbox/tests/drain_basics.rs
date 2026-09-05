use super::*;

#[tokio::test]
async fn drain_executes_admin_call_and_marks_intent_done() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let intent = participant_intent();
    let participant_sid = match &intent.target {
        TeardownTarget::Participant {
            participant_sid: Some(participant_sid),
            ..
        } => Some(participant_sid.clone()),
        _ => None,
    };
    let identity = Identity::from_jid(FullJid::from_str("alice@example.test/device").unwrap());
    assert_eq!(
        sfu.register_call_participant_observed(
            &intent.call_id,
            &identity,
            &ObservedCallSids::new(intent.room_sid.clone(), participant_sid),
        ),
        waddle_sfu::SidObservationDisposition::Applied
    );
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(intent)
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 64).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert_eq!(admin.remove_calls.lock().expect("recording lock").len(), 1);
    assert_eq!(
        state
            .deps
            .protocol
            .call_teardown_outbox
            .find(&intent_id)
            .await
            .expect("find")
            .expect("stored intent")
            .status,
        CallTeardownStatus::Done
    );
}

#[tokio::test]
async fn drain_skips_participant_when_intent_occupant_generation_is_stale() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "stale-occupant-generation@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let current_occupant = occupant_generation();
    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        (current_occupant, &Some(owner_session)),
    )
    .await;

    let stale_occupant = occupant_generation();
    let current_session = waddle_sfu::SessionBinding::new("muji-current").expect("binding");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    sfu.register_call_participant_with_session(
        &call_id,
        &identity,
        &current_session,
        current_occupant,
    );

    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: call_id.clone(),
            target: TeardownTarget::Participant {
                identity: alice.clone(),
                participant_sid: None,
            },
            generation: None,
            occupant: Some(stale_occupant),
            unbound_occupant: waddle_sfu::UnboundOccupantPolicy::Keep,
            room_sid: None,
            session: None,
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert!(
        sfu.has_call_participant(&call_id, &identity),
        "a stale occupant-scoped intent must not remove the live participant"
    );
    assert!(
        admin
            .remove_calls
            .lock()
            .expect("recording lock")
            .is_empty(),
        "a stale occupant-scoped intent must not fire RemoveParticipant"
    );
    assert_eq!(
        state
            .deps
            .protocol
            .call_teardown_outbox
            .find(&intent_id)
            .await
            .expect("find")
            .expect("stored intent")
            .status,
        CallTeardownStatus::Done
    );
}

#[tokio::test]
async fn one_to_one_drain_holds_the_producer_fence_through_execution_and_completion() {
    let remove_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let admin = Arc::new(RecordingAdmin {
        remove_gate: Mutex::new(Some(remove_gate.clone())),
        ..RecordingAdmin::default()
    });
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let node_identity = SharedNodeIdentity::new(NodeIdentity::new("node-a", "epoch-a"));
    let mut state = create_test_websocket_state_with_clustering(
        crate::clustering::ClusteringHandles {
            node_identity: Some(node_identity.clone()),
            ..crate::clustering::ClusteringHandles::default()
        },
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
    )
    .await;
    let protocol = &mut Arc::get_mut(&mut state)
        .expect("test state has one strong reference")
        .deps
        .protocol;
    let sfu_service: Arc<dyn SfuService> = sfu.clone();
    protocol.sfu = Some(sfu_service);
    protocol.call_teardown_executor = Some(sfu.teardown_executor());

    let intent = participant_intent();
    let identity = Identity::from_jid(
        FullJid::from_str("alice@example.test/device").expect("participant JID"),
    );
    assert_eq!(
        sfu.register_call_participant_observed(
            &intent.call_id,
            &identity,
            &ObservedCallSids::new(
                intent.room_sid.clone(),
                Some(ParticipantSid::new("PA_test").expect("participant sid")),
            ),
        ),
        waddle_sfu::SidObservationDisposition::Applied
    );
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(intent)
        .await
        .expect("enqueue");
    let drain = tokio::spawn({
        let state = state.clone();
        async move { drain_due(&state, 8).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if admin.remove_calls.lock().expect("recording lock").len() == 1 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("drain reaches blocked admin execution");

    let mut rotate = tokio::spawn({
        let node_identity = node_identity.clone();
        async move {
            node_identity
                .rotate(NodeIdentity::new("node-b", "epoch-b"))
                .await;
        }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut rotate)
            .await
            .is_err(),
        "producer identity must not rotate during an in-flight fenced effect"
    );

    remove_gate.add_permits(1);
    let summary = drain.await.expect("drain task").expect("drain");
    rotate.await.expect("rotation task");
    assert_eq!(summary.drained, 1);
    assert_eq!(
        node_identity.current(),
        NodeIdentity::new("node-b", "epoch-b")
    );
    assert_eq!(
        state
            .deps
            .protocol
            .call_teardown_outbox
            .find(&intent_id)
            .await
            .expect("find")
            .expect("stored intent")
            .status,
        CallTeardownStatus::Done
    );
}

#[tokio::test]
async fn drain_requeues_retryable_admin_failure() {
    let admin = Arc::new(RecordingAdmin {
        fail_remove: true,
        ..RecordingAdmin::default()
    });
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(participant_intent())
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 64).await.expect("drain");
    let stored = state
        .deps
        .protocol
        .call_teardown_outbox
        .find(&intent_id)
        .await
        .expect("find")
        .expect("stored intent");

    assert_eq!(summary.requeued, 1);
    assert_eq!(stored.status, CallTeardownStatus::Queued);
    assert_eq!(stored.attempt_count, 1);
    assert!(stored.next_attempt_at_ms.is_some());
}

#[tokio::test]
async fn drain_marks_higher_generation_stale_without_admin_call() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    // A MUC-style (bare-JID) call id: its generation record survives
    // the empty->re-register cycle, which is the reuse the guard
    // exists for. (1:1-style ids drop their record on final clear
    // because they are never reused -- see call_generations pruning.)
    // The room actor makes this node the room's owner so the drain's
    // ownership gate lets the intent through.
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "stale-room@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        (occupant_generation(), &Some(owner_session.clone())),
    )
    .await;
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    sfu.register_call_participant(&call_id, &identity);
    let _ = sfu.note_participant_left(&call_id, &identity, None);
    sfu.register_call_participant(&call_id, &identity);
    let intent = CallTeardownIntent {
        call_id,
        target: TeardownTarget::Participant {
            identity: identity.as_jid().clone(),
            participant_sid: None,
        },
        generation: Some(CallGeneration::try_from_u64(1).expect("generation")),
        occupant: None,
        unbound_occupant: waddle_sfu::UnboundOccupantPolicy::Keep,
        room_sid: None,
        session: None,
    };
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(intent)
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 64).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert!(admin
        .remove_calls
        .lock()
        .expect("recording lock")
        .is_empty());
    assert_eq!(
        state
            .deps
            .protocol
            .call_teardown_outbox
            .find(&intent_id)
            .await
            .expect("find")
            .expect("stored intent")
            .status,
        CallTeardownStatus::Done
    );
}

#[tokio::test]
async fn room_not_owned_by_this_node_remains_queued() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let intent = CallTeardownIntent {
        call_id: CallId::new("room@muc.example.test").expect("room call id"),
        target: TeardownTarget::Participant {
            identity: FullJid::from_str("alice@example.test/device").expect("full JID"),
            participant_sid: None,
        },
        generation: None,
        occupant: None,
        unbound_occupant: waddle_sfu::UnboundOccupantPolicy::Keep,
        room_sid: None,
        session: None,
    };
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(intent)
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 64).await.expect("drain");

    assert_eq!(summary.drained, 0);
    assert_eq!(summary.requeued, 0);
    assert_eq!(summary.failed, 0);
    assert!(admin
        .remove_calls
        .lock()
        .expect("recording lock")
        .is_empty());
    assert_eq!(
        state
            .deps
            .protocol
            .call_teardown_outbox
            .find(&intent_id)
            .await
            .expect("find")
            .expect("stored intent")
            .status,
        CallTeardownStatus::Queued
    );
}

#[tokio::test]
async fn occupied_room_intent_is_requeued_not_consumed() {
    let alice =
        Identity::from_jid(FullJid::from_str("alice@example.test/device").expect("full JID"));
    let admin = Arc::new(RecordingAdmin {
        occupancy: Mutex::new(RoomOccupancy {
            waddle: vec![(alice, None)],
            foreign: 0,
        }),
        ..RecordingAdmin::default()
    });
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(fixture_config(), admin));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: CallId::new("alice@example.test:occupied-call").expect("call id"),
            target: TeardownTarget::Room,
            generation: None,
            occupant: None,
            unbound_occupant: waddle_sfu::UnboundOccupantPolicy::Keep,
            room_sid: None,
            session: None,
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.requeued, 1);
    assert_eq!(
        state
            .deps
            .protocol
            .call_teardown_outbox
            .find(&intent_id)
            .await
            .expect("find")
            .expect("stored intent")
            .status,
        CallTeardownStatus::Queued
    );
}

#[tokio::test]
async fn unfenced_participant_rejoin_is_skipped_without_admin_call() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let call_id = CallId::new("alice@example.test:rejoin-call").expect("call id");
    let identity =
        Identity::from_jid(FullJid::from_str("alice@example.test/device").expect("full JID"));
    sfu.register_call_participant(&call_id, &identity);
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        // Backdate the intent: only a registration that POSTDATES the
        // intent is a rejoin (N1) — the fence must not fire for the
        // owner-node case where the registration merely predates the
        // never-applied departure.
        .enqueue_at(
            CallTeardownIntent {
                call_id: call_id.clone(),
                target: TeardownTarget::Participant {
                    identity: identity.as_jid().clone(),
                    participant_sid: None,
                },
                generation: None,
                occupant: None,
                unbound_occupant: waddle_sfu::UnboundOccupantPolicy::Keep,
                room_sid: None,
                session: None,
            },
            crate::time::now_ms() - 60_000,
        )
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert!(admin
        .remove_calls
        .lock()
        .expect("recording lock")
        .is_empty());
    assert!(sfu.has_call_participant(&call_id, &identity));
    assert_eq!(
        state
            .deps
            .protocol
            .call_teardown_outbox
            .find(&intent_id)
            .await
            .expect("find")
            .expect("stored intent")
            .status,
        CallTeardownStatus::Done
    );
}

#[tokio::test]
async fn unfenced_participant_without_live_registration_still_removes_participant() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: CallId::new("alice@example.test:no-live-registration").expect("call id"),
            target: TeardownTarget::Participant {
                identity: FullJid::from_str("alice@example.test/device").expect("full JID"),
                participant_sid: None,
            },
            generation: None,
            occupant: None,
            unbound_occupant: waddle_sfu::UnboundOccupantPolicy::Keep,
            room_sid: None,
            session: None,
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert_eq!(admin.remove_calls.lock().expect("recording lock").len(), 1);
    assert_eq!(
        state
            .deps
            .protocol
            .call_teardown_outbox
            .find(&intent_id)
            .await
            .expect("find")
            .expect("stored intent")
            .status,
        CallTeardownStatus::Done
    );
}
