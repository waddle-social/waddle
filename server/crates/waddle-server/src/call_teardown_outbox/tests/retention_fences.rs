use super::*;

#[tokio::test]
async fn prune_failed_also_prunes_done_rows_after_retention() {
    let store = store("call-teardown-prune-terminal").await;
    let now_ms = 1_000_000_i64;
    let done_id = store
        .enqueue_at(participant_intent(), now_ms - FAILED_RETENTION_MS - 10_000)
        .await
        .expect("enqueue done");
    let failed_id = store
        .enqueue_at(
            CallTeardownIntent {
                call_id: CallId::new("alice@example.test:failed-call").expect("call id"),
                target: TeardownTarget::Room,
                generation: None,
                room_sid: None,
            },
            now_ms - FAILED_RETENTION_MS - 10_000,
        )
        .await
        .expect("enqueue failed");
    let fresh_done_id = store
        .enqueue_at(
            CallTeardownIntent {
                call_id: CallId::new("alice@example.test:fresh-done").expect("call id"),
                target: TeardownTarget::Room,
                generation: None,
                room_sid: None,
            },
            now_ms,
        )
        .await
        .expect("enqueue fresh");

    let mut claimed_jobs = store
        .claim_due_at(8, now_ms - FAILED_RETENTION_MS - 5_000)
        .await
        .expect("claim old rows");
    claimed_jobs.sort_by(|left, right| left.intent_id.as_str().cmp(right.intent_id.as_str()));
    let done_job = claimed_jobs
        .iter()
        .find(|job| job.intent_id == done_id)
        .expect("done job")
        .clone();
    let mut failed_job = claimed_jobs
        .iter()
        .find(|job| job.intent_id == failed_id)
        .expect("failed job")
        .clone();
    failed_job.attempt_count = MAX_ATTEMPTS - 1;
    assert!(store
        .mark_done_at(&done_job, now_ms - FAILED_RETENTION_MS - 1)
        .await
        .expect("mark done"));
    assert_eq!(
        store
            .retry_or_fail_at(
                &failed_job,
                CallTeardownRetryReason::MujiPresenceClear,
                now_ms - FAILED_RETENTION_MS - 1,
            )
            .await
            .expect("mark failed"),
        CallTeardownRetryOutcome::Failed {
            attempt_count: MAX_ATTEMPTS
        }
    );
    let fresh_done_job = store
        .claim_due_at(8, now_ms)
        .await
        .expect("claim fresh")
        .into_iter()
        .find(|job| job.intent_id == fresh_done_id)
        .expect("fresh done job");
    assert!(store
        .mark_done_at(&fresh_done_job, now_ms)
        .await
        .expect("mark fresh done"));

    let deleted = store.prune_failed_at(now_ms).await.expect("prune");

    assert_eq!(deleted, 2);
    assert!(store.find(&done_id).await.expect("find done").is_none());
    assert!(store.find(&failed_id).await.expect("find failed").is_none());
    assert_eq!(
        store
            .find(&fresh_done_id)
            .await
            .expect("find fresh")
            .expect("fresh row")
            .status,
        CallTeardownStatus::Done
    );
}

#[tokio::test]
async fn foreign_node_one_to_one_intent_is_released_without_execution() {
    let producing_node = SharedNodeIdentity::new(NodeIdentity::new("node-a", "epoch-a"));
    let state = create_test_websocket_state_with_clustering(
        crate::clustering::ClusteringHandles {
            node_identity: Some(producing_node.clone()),
            ..crate::clustering::ClusteringHandles::default()
        },
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
    )
    .await;
    let now_ms = 2_000_000_i64;
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue_at(participant_intent(), now_ms - 1)
        .await
        .expect("enqueue");
    producing_node
        .rotate(NodeIdentity::new("node-b", "epoch-b"))
        .await;

    let summary = super::drain::drain_due_at(&state, 8, now_ms)
        .await
        .expect("drain");

    assert_eq!(summary.drained, 0);
    assert_eq!(summary.requeued, 0);
    assert_eq!(summary.failed, 0);
    let stored = state
        .deps
        .protocol
        .call_teardown_outbox
        .find(&intent_id)
        .await
        .expect("find")
        .expect("stored intent");
    assert_eq!(stored.status, CallTeardownStatus::Queued);
    assert_eq!(stored.attempt_count, 0);
    assert_eq!(stored.last_error, None);
    assert!(stored.next_attempt_at_ms.is_some_and(|due| due > now_ms));
}

#[tokio::test]
async fn foreign_node_one_to_one_intent_older_than_a_day_dead_letters() {
    let producing_node = SharedNodeIdentity::new(NodeIdentity::new("node-a", "epoch-a"));
    let state = create_test_websocket_state_with_clustering(
        crate::clustering::ClusteringHandles {
            node_identity: Some(producing_node.clone()),
            ..crate::clustering::ClusteringHandles::default()
        },
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
    )
    .await;
    let now_ms = 2_000_000_i64;
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue_at(participant_intent(), now_ms - (24 * 60 * 60 * 1_000) - 1)
        .await
        .expect("enqueue");
    producing_node
        .rotate(NodeIdentity::new("node-b", "epoch-b"))
        .await;

    let summary = super::drain::drain_due_at(&state, 8, now_ms)
        .await
        .expect("drain");

    assert_eq!(summary.failed, 1);
    let stored = state
        .deps
        .protocol
        .call_teardown_outbox
        .find(&intent_id)
        .await
        .expect("find")
        .expect("stored intent");
    assert_eq!(stored.status, CallTeardownStatus::Failed);
    assert_eq!(
        stored.last_error,
        Some(CallTeardownLastError::ProducerNeverDrained)
    );
}

#[tokio::test]
async fn unowned_room_scoped_intent_older_than_a_day_dead_letters() {
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let now_ms = 2_000_000_i64;
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue_at(
            CallTeardownIntent {
                call_id: CallId::new("room@muc.example.test").expect("room call id"),
                target: TeardownTarget::Room,
                generation: None,
                room_sid: None,
            },
            now_ms - (24 * 60 * 60 * 1_000) - 1,
        )
        .await
        .expect("enqueue");

    let summary = super::drain::drain_due_at(&state, 8, now_ms)
        .await
        .expect("drain");

    assert_eq!(summary.failed, 1);
    let stored = state
        .deps
        .protocol
        .call_teardown_outbox
        .find(&intent_id)
        .await
        .expect("find")
        .expect("stored intent");
    assert_eq!(stored.status, CallTeardownStatus::Failed);
    assert_eq!(
        stored.last_error,
        Some(CallTeardownLastError::RoomNeverOwned)
    );
}

#[tokio::test]
async fn old_room_scoped_intent_survives_transient_registry_unavailability() {
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let now_ms = 2_000_000_i64;
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue_at(
            CallTeardownIntent {
                call_id: CallId::new("room@muc.example.test").expect("room call id"),
                target: TeardownTarget::Room,
                generation: None,
                room_sid: None,
            },
            now_ms - (24 * 60 * 60 * 1_000) - 1,
        )
        .await
        .expect("enqueue");
    state.deps.protocol.room_registry.kill();

    let summary = super::drain::drain_due_at(&state, 8, now_ms)
        .await
        .expect("drain");

    assert_eq!(summary.failed, 0);
    let stored = state
        .deps
        .protocol
        .call_teardown_outbox
        .find(&intent_id)
        .await
        .expect("find")
        .expect("stored intent");
    assert_eq!(stored.status, CallTeardownStatus::Queued);
    assert_eq!(stored.last_error, None);
}

#[tokio::test]
async fn old_room_scoped_intent_survives_a_live_foreign_owner_claim() {
    let claim_store = Arc::new(InProcessClaimStore::new());
    claim_store.ensure_schema().await.expect("claim schema");
    let room_jid: BareJid = "room@muc.example.test".parse().expect("room jid");
    claim_store
        .acquire(
            &Entity::new(EntityType::RoomActor, room_jid.to_string()),
            &NodeIdentity::new("foreign-node", "foreign-epoch"),
        )
        .await
        .expect("foreign room claim");
    let state = create_test_websocket_state_with_clustering(
        crate::clustering::ClusteringHandles {
            claim_store: Some(claim_store),
            ..crate::clustering::ClusteringHandles::default()
        },
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
    )
    .await;
    let now_ms = 2_000_000_i64;
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue_at(
            CallTeardownIntent {
                call_id: CallId::new(room_jid.to_string()).expect("room call id"),
                target: TeardownTarget::Room,
                generation: None,
                room_sid: None,
            },
            now_ms - (24 * 60 * 60 * 1_000) - 1,
        )
        .await
        .expect("enqueue");

    let summary = super::drain::drain_due_at(&state, 8, now_ms)
        .await
        .expect("drain");

    assert_eq!(summary.failed, 0);
    let stored = state
        .deps
        .protocol
        .call_teardown_outbox
        .find(&intent_id)
        .await
        .expect("find")
        .expect("stored intent");
    assert_eq!(stored.status, CallTeardownStatus::Queued);
    assert_eq!(stored.last_error, None);
}

/// N1's primary scenario: on the room-claim owner the registration
/// legitimately SURVIVES the failed cross-node departure — that is why
/// the intent exists. A live registration that merely PREDATES the
/// intent must not read as a rejoin: the intent must execute.
#[tokio::test]
async fn live_registration_predating_the_intent_still_executes_removal() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "owner-room@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    sfu.register_call_participant(&call_id, &identity);
    let _intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: call_id.clone(),
            target: TeardownTarget::Participant {
                identity: identity.as_jid().clone(),
                participant_sid: None,
            },
            generation: None,
            room_sid: None,
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert_eq!(
        admin.remove_calls.lock().expect("recording lock").len(),
        1,
        "a registration predating the intent is the never-applied departure, not a rejoin"
    );
}

/// NN1: a second observation of an ALREADY-present participant refreshes
/// `registered_at` for reconcile grace accounting, but the teardown fence
/// must still consult the absent->present incarnation timestamp so this
/// mid-window intent executes instead of being falsely swallowed.
#[tokio::test]
async fn existing_participant_reobservation_does_not_reopen_the_swallow_window() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let call_id = CallId::new("alice@example.test:restamped-registration").expect("call id");
    let identity =
        Identity::from_jid(FullJid::from_str("alice@example.test/device").expect("full JID"));

    sfu.register_call_participant(&call_id, &identity);
    let first_registered_at_ms = sfu
        .participant_registered_at(&call_id, &identity)
        .expect("first registration time")
        .timestamp_millis();

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(
        sfu.register_call_participant_observed(&call_id, &identity, &ObservedCallSids::none()),
        waddle_sfu::SidObservationDisposition::Applied
    );

    let intent_created_at_ms = first_registered_at_ms - 1_990;
    let _intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue_at(
            CallTeardownIntent {
                call_id: call_id.clone(),
                target: TeardownTarget::Participant {
                    identity: identity.as_jid().clone(),
                    participant_sid: None,
                },
                generation: None,
                room_sid: None,
            },
            intent_created_at_ms,
        )
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert_eq!(
        admin.remove_calls.lock().expect("recording lock").len(),
        1,
        "re-observing an existing participant must not make a mid-window intent look newer than the original incarnation"
    );
}

#[tokio::test]
async fn later_local_token_mint_skips_an_older_queued_participant_eject() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let call_id = CallId::new("alice@example.test:later-mint-skip").expect("call id");
    let identity =
        Identity::from_jid(FullJid::from_str("alice@example.test/device").expect("full JID"));

    sfu.register_call_participant(&call_id, &identity);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let intent_created_at_ms = crate::time::now_ms();
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue_at(
            CallTeardownIntent {
                call_id: call_id.clone(),
                target: TeardownTarget::Participant {
                    identity: identity.as_jid().clone(),
                    participant_sid: None,
                },
                generation: None,
                room_sid: None,
            },
            intent_created_at_ms,
        )
        .await
        .expect("enqueue");

    tokio::time::sleep(std::time::Duration::from_millis(2_100)).await;
    let _token = sfu
        .issue_join_token(&call_id, &identity, MediaCapabilities::direct_call_peer())
        .expect("join token");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert!(
        sfu.has_call_participant(&call_id, &identity),
        "a later local mint proves the participant is current and the queued eject must be swallowed"
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
async fn no_later_local_token_mint_executes_the_queued_participant_eject() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let call_id = CallId::new("alice@example.test:no-later-mint").expect("call id");
    let identity =
        Identity::from_jid(FullJid::from_str("alice@example.test/device").expect("full JID"));

    sfu.register_call_participant(&call_id, &identity);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let intent_created_at_ms = crate::time::now_ms();
    let _intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue_at(
            CallTeardownIntent {
                call_id: call_id.clone(),
                target: TeardownTarget::Participant {
                    identity: identity.as_jid().clone(),
                    participant_sid: None,
                },
                generation: None,
                room_sid: None,
            },
            intent_created_at_ms,
        )
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert_eq!(
        admin.remove_calls.lock().expect("recording lock").len(),
        1,
        "without a later mint, the queued eject must execute"
    );
}
