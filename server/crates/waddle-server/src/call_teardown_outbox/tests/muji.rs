use super::*;

fn active_muji() -> Muji {
    Muji::with_contents(vec![MujiContent::new(
        "audio",
        Creator::Initiator,
        MediaKind::Audio,
    )])
}

#[tokio::test]
async fn unfenced_muji_presence_clear_rejoin_is_skipped_and_preserves_room_muji_state() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-rejoin-skip@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());

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
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: active_muji(),
        })
        .await
        .expect("muji update")
        .expect("occupant update");
    sfu.register_call_participant(&call_id, &identity);

    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        // Backdated for the same N1 reason as the participant test.
        .enqueue_at(
            CallTeardownIntent {
                call_id: call_id.clone(),
                target: TeardownTarget::MujiPresenceClear {
                    room_jid: room_jid.clone(),
                    departed: alice.clone(),
                    participant_sid: None,
                },
                generation: None,
                occupant: None,
                room_sid: None,
                session: None,
            },
            crate::time::now_ms() - 60_000,
        )
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert!(sfu.has_call_participant(&call_id, &identity));
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.muji_for_session("alice", &alice).is_some(),
        "stale skip must not clear the live Muji advertisement"
    );
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
async fn unfenced_muji_presence_clear_without_live_registration_still_clears_room_muji_state() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-live-clear@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");

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
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: active_muji(),
        })
        .await
        .expect("muji update")
        .expect("occupant update");

    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id,
            target: TeardownTarget::MujiPresenceClear {
                room_jid: room_jid.clone(),
                departed: alice.clone(),
                participant_sid: None,
            },
            generation: None,
            occupant: None,
            room_sid: None,
            session: None,
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.muji_for_session("alice", &alice).is_none(),
        "non-stale clear must remove the Muji advertisement"
    );
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

#[tokio::test(flavor = "current_thread")]
async fn sid_fenced_muji_presence_clear_stale_rejoin_preserves_room_muji_state_and_counts_once() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-sid-stale@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    let room_sid = RoomSid::new("RM_shared").expect("room sid");
    let old_sids = ObservedCallSids::new(
        Some(room_sid.clone()),
        Some(ParticipantSid::new("PA_old").expect("participant sid")),
    );
    let current_sids = ObservedCallSids::new(
        Some(room_sid.clone()),
        Some(ParticipantSid::new("PA_current").expect("participant sid")),
    );

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
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: active_muji(),
        })
        .await
        .expect("muji update")
        .expect("occupant update");
    assert_eq!(
        sfu.register_call_participant_observed(&call_id, &identity, &old_sids),
        waddle_sfu::SidObservationDisposition::Applied
    );
    assert!(matches!(
        sfu.note_participant_left(&call_id, &identity, Some(&old_sids)),
        waddle_sfu::TeardownDisposition::Applied(_)
    ));
    assert_eq!(
        sfu.register_call_participant_observed(&call_id, &identity, &current_sids),
        waddle_sfu::SidObservationDisposition::Applied
    );

    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: call_id.clone(),
            target: TeardownTarget::MujiPresenceClear {
                room_jid: room_jid.clone(),
                departed: alice.clone(),
                participant_sid: old_sids.participant_sid.clone(),
            },
            generation: None,
            occupant: None,
            room_sid: old_sids.room_sid.clone(),
            session: None,
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert!(sfu.has_call_participant(&call_id, &identity));
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.muji_for_session("alice", &alice).is_some(),
        "stale SID must not clear the rejoined participant's Muji state"
    );
    assert_eq!(
        metrics.counter_sum("waddle.call.teardown.stale_dropped", &[]),
        Some(1)
    );
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
async fn occupant_fenced_muji_clear_rejects_stale_same_session_binding() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-occupant-stale@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    let displaced = occupant_generation();
    let replacement = occupant_generation();
    let reused_session = waddle_sfu::SessionBinding::new("muji-reused").expect("session binding");

    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        (displaced, &Some(owner_session.clone())),
    )
    .await;
    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        (replacement, &Some(owner_session)),
    )
    .await;
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: active_muji(),
        })
        .await
        .expect("muji update")
        .expect("occupant update");
    sfu.register_call_participant_with_session(&call_id, &identity, &reused_session, replacement);

    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: call_id.clone(),
            target: TeardownTarget::MujiPresenceClear {
                room_jid: room_jid.clone(),
                departed: alice.clone(),
                participant_sid: None,
            },
            generation: None,
            occupant: Some(displaced),
            room_sid: None,
            session: Some(reused_session),
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert!(sfu.has_call_participant(&call_id, &identity));
    assert!(
        snapshot_room(state.as_ref(), &room_jid)
            .await
            .room
            .muji_for_session("alice", &alice)
            .is_some(),
        "the stale durable intent must not clear the replacement advertisement",
    );
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
        CallTeardownStatus::Done,
    );
}

#[tokio::test]
async fn sid_fenced_muji_presence_clear_matching_sid_still_clears_room_muji_state() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-sid-clear@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    let observed_sids = ObservedCallSids::new(
        Some(RoomSid::new("RM_match").expect("room sid")),
        Some(ParticipantSid::new("PA_match").expect("participant sid")),
    );

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
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: active_muji(),
        })
        .await
        .expect("muji update")
        .expect("occupant update");
    assert_eq!(
        sfu.register_call_participant_observed(&call_id, &identity, &observed_sids),
        waddle_sfu::SidObservationDisposition::Applied
    );

    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: call_id.clone(),
            target: TeardownTarget::MujiPresenceClear {
                room_jid: room_jid.clone(),
                departed: alice.clone(),
                participant_sid: observed_sids.participant_sid.clone(),
            },
            generation: None,
            occupant: None,
            room_sid: observed_sids.room_sid.clone(),
            session: None,
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert!(
        !sfu.has_call_participant(&call_id, &identity),
        "matching SID should allow the queued clear to remove the stale registration"
    );
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.muji_for_session("alice", &alice).is_none(),
        "matching SID should clear the Muji advertisement"
    );
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
async fn muji_room_sweep_clears_owner_local_participants_with_matching_room_sid() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-room-sweep@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    let observed_sids = ObservedCallSids::new(
        Some(RoomSid::new("RM_sweep").expect("room sid")),
        Some(ParticipantSid::new("PA_sweep").expect("participant sid")),
    );

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
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: active_muji(),
        })
        .await
        .expect("muji update")
        .expect("occupant update");
    assert_eq!(
        sfu.register_call_participant_observed(&call_id, &identity, &observed_sids),
        waddle_sfu::SidObservationDisposition::Applied
    );

    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: call_id.clone(),
            target: TeardownTarget::MujiRoomSweep {
                room_jid: room_jid.clone(),
            },
            generation: None,
            occupant: None,
            room_sid: observed_sids.room_sid.clone(),
            session: None,
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert!(!sfu.has_call_participant(&call_id, &identity));
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.muji_for_session("alice", &alice).is_none(),
        "owner-gated room sweep must clear remaining local Muji state"
    );
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
async fn muji_room_sweep_clears_actor_advertisement_absent_from_sfu_registry() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-room-sweep-actor@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");

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
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: active_muji(),
        })
        .await
        .expect("muji update")
        .expect("occupant update");
    assert!(
        sfu.participants_for_call(&call_id).is_empty(),
        "the regression requires actor-only Muji state"
    );

    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id,
            target: TeardownTarget::MujiRoomSweep {
                room_jid: room_jid.clone(),
            },
            generation: None,
            occupant: None,
            room_sid: Some(RoomSid::new("RM_actor_only").expect("room sid")),
            session: None,
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.muji_for_session("alice", &alice).is_none(),
        "room sweep must enumerate and clear actor-held Muji state even when the local SFU registry is empty"
    );
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
async fn muji_room_sweep_retries_when_the_owned_actor_disappears_before_enumeration() {
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::new(RecordingAdmin::default()),
    ));
    let state = state_with_executor(sfu).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-room-sweep-race@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        (occupant_generation(), &Some(owner_session)),
    )
    .await;
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .kill();

    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: CallId::new(room_jid.to_string()).expect("call id"),
            target: TeardownTarget::MujiRoomSweep {
                room_jid: room_jid.clone(),
            },
            generation: None,
            occupant: None,
            room_sid: Some(RoomSid::new("RM_actor_race").expect("room sid")),
            session: None,
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.requeued, 1);
    let stored = state
        .deps
        .protocol
        .call_teardown_outbox
        .find(&intent_id)
        .await
        .expect("find")
        .expect("stored intent");
    assert_eq!(stored.status, CallTeardownStatus::Queued);
    assert_eq!(
        stored.last_error,
        Some(CallTeardownLastError::Retryable(
            CallTeardownRetryReason::MujiPresenceClear
        ))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn muji_room_sweep_stale_room_sid_preserves_room_muji_state_and_counts_once() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-room-sweep-stale@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    let current_sids = ObservedCallSids::new(
        Some(RoomSid::new("RM_current").expect("room sid")),
        Some(ParticipantSid::new("PA_current").expect("participant sid")),
    );

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
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: active_muji(),
        })
        .await
        .expect("muji update")
        .expect("occupant update");
    assert_eq!(
        sfu.register_call_participant_observed(&call_id, &identity, &current_sids),
        waddle_sfu::SidObservationDisposition::Applied
    );

    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: call_id.clone(),
            target: TeardownTarget::MujiRoomSweep {
                room_jid: room_jid.clone(),
            },
            generation: None,
            occupant: None,
            room_sid: Some(RoomSid::new("RM_stale").expect("room sid")),
            session: None,
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert!(sfu.has_call_participant(&call_id, &identity));
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.muji_for_session("alice", &alice).is_some(),
        "stale room SID must not clear the current Muji advertisement"
    );
    assert_eq!(
        metrics.counter_sum("waddle.call.teardown.stale_dropped", &[]),
        Some(1)
    );
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
