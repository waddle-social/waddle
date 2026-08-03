use super::*;

async fn store_with_db(name: &str) -> (Database, CallTeardownOutboxStore) {
    let database = Database::in_memory(name).await.unwrap();
    let store = CallTeardownOutboxStore::new(database.clone())
        .await
        .unwrap();
    (database, store)
}

#[tokio::test]
async fn enqueue_claim_and_complete_round_trip() {
    let store = store("call-teardown-round-trip").await;
    let intent_id = store.enqueue(participant_intent()).await.unwrap();

    let claimed = store.claim_due(64).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].intent, participant_intent());
    assert!(claimed[0].claim_token.is_some());
    assert!(store.mark_done(&claimed[0]).await.unwrap());

    let stored = store.find(&intent_id).await.unwrap().unwrap();
    assert_eq!(stored.status, CallTeardownStatus::Done);
    assert_eq!(stored.attempt_count, 0);
}

#[tokio::test]
async fn raw_one_to_one_intent_round_trips_its_typed_producing_node() {
    let database = Database::in_memory("call-teardown-producing-node")
        .await
        .expect("database");
    let producing_identity = NodeIdentity::new("node-a", "epoch-a");
    let store = CallTeardownOutboxStore::new_with_node_identity(
        database,
        SharedNodeIdentity::new(producing_identity.clone()),
    )
    .await
    .expect("store");

    let intent_id = store.enqueue(participant_intent()).await.expect("enqueue");
    let stored = store
        .find(&intent_id)
        .await
        .expect("find")
        .expect("stored intent");

    assert_eq!(
        stored.producing_node,
        Some(CallTeardownProducingNode::from_node_identity(
            producing_identity
        ))
    );
}

#[tokio::test]
async fn producing_node_guard_blocks_identity_rotation_until_the_durable_boundary_finishes() {
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node-a", "epoch-a"));
    let store = CallTeardownOutboxStore::new_with_node_identity(
        Database::in_memory("call-teardown-producing-node-guard")
            .await
            .expect("database"),
        identity.clone(),
    )
    .await
    .expect("store");
    let guard = store
        .producing_node_guard(true)
        .await
        .expect("guard lookup")
        .expect("producer guard");
    let rotate = tokio::spawn({
        let identity = identity.clone();
        async move {
            identity
                .rotate(NodeIdentity::new("node-b", "epoch-b"))
                .await;
        }
    });
    tokio::task::yield_now().await;
    assert!(
        !rotate.is_finished(),
        "identity rotation must wait for the guarded durable boundary"
    );

    drop(guard);
    rotate.await.expect("rotation task");
    assert_eq!(identity.current(), NodeIdentity::new("node-b", "epoch-b"));
}

#[tokio::test]
async fn identical_one_to_one_intents_from_different_nodes_do_not_dedupe() {
    let database = Database::in_memory("call-teardown-producing-node-dedupe")
        .await
        .expect("database");
    let node_a = CallTeardownOutboxStore::new_with_node_identity(
        database.clone(),
        SharedNodeIdentity::new(NodeIdentity::new("node-a", "epoch-a")),
    )
    .await
    .expect("node A store");
    let node_b = CallTeardownOutboxStore::new_with_node_identity(
        database,
        SharedNodeIdentity::new(NodeIdentity::new("node-b", "epoch-b")),
    )
    .await
    .expect("node B store");

    let first = node_a
        .enqueue(participant_intent())
        .await
        .expect("node A enqueue");
    let second = node_b
        .enqueue(participant_intent())
        .await
        .expect("node B enqueue");

    assert_ne!(first, second);
}

#[tokio::test]
async fn identical_queued_duplicate_returns_the_existing_intent_id() {
    let store = store("call-teardown-dedupe-identical").await;
    let intent = participant_intent();

    let first_id = store.enqueue(intent.clone()).await.unwrap();
    let second_id = store.enqueue(intent).await.unwrap();

    assert_eq!(second_id, first_id);
    let claimed = store.claim_due(8).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].intent_id, first_id);
}

#[tokio::test]
async fn differing_fence_evidence_inserts_a_second_queued_row() {
    let store = store("call-teardown-dedupe-fenced").await;
    let first = participant_intent();
    let second = CallTeardownIntent {
        generation: Some(CallGeneration::try_from(2).unwrap()),
        room_sid: Some(RoomSid::new("RM_other").unwrap()),
        target: TeardownTarget::Participant {
            identity: FullJid::from_str("alice@example.test/device").unwrap(),
            participant_sid: Some(ParticipantSid::new("PA_other").unwrap()),
        },
        ..participant_intent()
    };

    let first_id = store.enqueue(first).await.unwrap();
    let second_id = store.enqueue(second).await.unwrap();

    assert_ne!(second_id, first_id);
    let mut claimed_ids = store
        .claim_due(8)
        .await
        .unwrap()
        .into_iter()
        .map(|job| job.intent_id)
        .collect::<Vec<_>>();
    claimed_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    let mut expected_ids = vec![first_id, second_id];
    expected_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    assert_eq!(claimed_ids, expected_ids);
}

#[tokio::test]
async fn retry_increments_attempt_and_schedules_exponential_backoff() {
    let store = store("call-teardown-retry").await;
    let intent_id = store.enqueue(participant_intent()).await.unwrap();
    let claimed = store.claim_due(1).await.unwrap().pop().unwrap();

    let outcome = store
        .retry_or_fail(
            &claimed,
            CallTeardownRetryReason::LiveKitExecutorUnavailable,
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        CallTeardownRetryOutcome::Requeued { attempt_count: 1 }
    );
    let stored = store.find(&intent_id).await.unwrap().unwrap();
    assert_eq!(stored.status, CallTeardownStatus::Queued);
    assert_eq!(stored.attempt_count, 1);
    assert_eq!(
        stored.last_error,
        Some(CallTeardownLastError::Retryable(
            CallTeardownRetryReason::LiveKitExecutorUnavailable
        ))
    );
    assert!(stored.next_attempt_at_ms.unwrap() >= stored.created_at_ms + BASE_RETRY_DELAY_MS);
    assert_eq!(retry_delay_ms(1), 5_000);
    assert_eq!(retry_delay_ms(2), 10_000);
    assert_eq!(retry_delay_ms(20), MAX_RETRY_DELAY_MS);
}

#[tokio::test]
async fn twentieth_attempt_becomes_failed_and_retains_error() {
    let store = store("call-teardown-max-attempts").await;
    let intent_id = store.enqueue(participant_intent()).await.unwrap();
    let mut claimed = store.claim_due(1).await.unwrap().pop().unwrap();
    claimed.attempt_count = MAX_ATTEMPTS - 1;

    let outcome = store
        .retry_or_fail(&claimed, CallTeardownRetryReason::MujiPresenceClear)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        CallTeardownRetryOutcome::Failed {
            attempt_count: MAX_ATTEMPTS
        }
    );
    let stored = store.find(&intent_id).await.unwrap().unwrap();
    assert_eq!(stored.status, CallTeardownStatus::Failed);
    assert_eq!(stored.attempt_count, MAX_ATTEMPTS);
    assert_eq!(
        stored.last_error,
        Some(CallTeardownLastError::Retryable(
            CallTeardownRetryReason::MujiPresenceClear
        ))
    );
    assert_eq!(stored.next_attempt_at_ms, None);
}

#[tokio::test]
async fn infrastructure_transient_reason_keeps_requeueing_past_the_attempt_cap() {
    // #1612 review round 8: a pure LiveKit-outage failure must never
    // dead-letter — failed rows are never replayed and no reconcile
    // backstop covers raw 1:1 removals, so the row keeps requeueing at
    // the capped backoff until LiveKit answers.
    let store = store("call-teardown-transient-uncapped").await;
    let intent_id = store.enqueue(participant_intent()).await.unwrap();
    let mut claimed = store.claim_due(1).await.unwrap().pop().unwrap();
    claimed.attempt_count = MAX_ATTEMPTS - 1;

    let outcome = store
        .retry_or_fail(&claimed, CallTeardownRetryReason::LiveKitAdmin)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        CallTeardownRetryOutcome::Requeued {
            attempt_count: MAX_ATTEMPTS
        }
    );
    let stored = store.find(&intent_id).await.unwrap().unwrap();
    assert_eq!(stored.status, CallTeardownStatus::Queued);
    assert_eq!(stored.attempt_count, MAX_ATTEMPTS);
    assert!(
        stored.next_attempt_at_ms.is_some(),
        "an uncapped transient retry still schedules a bounded next attempt"
    );
}

#[tokio::test]
async fn terminal_write_requires_the_current_claim_token() {
    let store = store("call-teardown-claim-cas").await;
    let intent_id = store.enqueue(participant_intent()).await.unwrap();
    let claimed = store.claim_due(1).await.unwrap().pop().unwrap();
    let mut second_drainer = claimed.clone();
    second_drainer.claim_token = Some(ClaimToken::from_stored(uuid::Uuid::new_v4().to_string()));

    assert!(!store.mark_done(&second_drainer).await.unwrap());
    assert_eq!(
        store
            .retry_or_fail(&second_drainer, CallTeardownRetryReason::LiveKitAdmin)
            .await
            .unwrap(),
        CallTeardownRetryOutcome::ClaimLost
    );
    assert_eq!(
        store.find(&intent_id).await.unwrap().unwrap().status,
        CallTeardownStatus::InProgress
    );
    assert!(store.mark_done(&claimed).await.unwrap());
}

#[tokio::test]
async fn row_decode_maps_unknown_last_error_to_the_typed_unknown_variant() {
    let (database, store) = store_with_db("call-teardown-unknown-last-error").await;
    let intent_id = store.enqueue(participant_intent()).await.unwrap();
    let claimed = store.claim_due(1).await.unwrap().pop().unwrap();
    let now_ms = crate::time::now_ms();
    let connection = database.guard().await.unwrap();
    connection
        .execute(
            "UPDATE call_teardown_outbox \
             SET last_error = ?, updated_at_ms = ? \
             WHERE intent_id = ?",
            crate::db_params!["future_retry_reason", now_ms, intent_id.as_str()],
        )
        .await
        .unwrap();

    let stored = store.find(&intent_id).await.unwrap().unwrap();
    assert_eq!(
        stored.last_error,
        Some(CallTeardownLastError::Retryable(
            CallTeardownRetryReason::Unknown
        ))
    );
    assert_eq!(
        stored.claim_token.as_ref().map(ClaimToken::as_str),
        claimed.claim_token.as_ref().map(ClaimToken::as_str)
    );
}

#[tokio::test]
async fn a_new_store_over_the_same_pool_drains_enqueued_work() {
    let database = Database::in_memory("call-teardown-restart").await.unwrap();
    let first = CallTeardownOutboxStore::new(database.clone())
        .await
        .unwrap();
    let intent_id = first.enqueue(participant_intent()).await.unwrap();
    drop(first);

    let restarted = CallTeardownOutboxStore::new(database).await.unwrap();
    let claimed = restarted.claim_due(1).await.unwrap().pop().unwrap();
    assert_eq!(claimed.intent_id, intent_id);
    assert!(restarted.mark_done(&claimed).await.unwrap());
}

#[tokio::test]
async fn muji_presence_clear_round_trips_with_typed_participant_sid() {
    let store = store("call-teardown-muji").await;
    let intent = CallTeardownIntent {
        call_id: CallId::new("room@conference.example.test").unwrap(),
        target: TeardownTarget::MujiPresenceClear {
            room_jid: BareJid::from_str("room@conference.example.test").unwrap(),
            departed: FullJid::from_str("alice@example.test/device").unwrap(),
            participant_sid: Some(ParticipantSid::new("PA_muji").unwrap()),
        },
        generation: None,
        room_sid: Some(RoomSid::new("RM_muji").unwrap()),
        session: None,
    };

    let intent_id = store.enqueue(intent.clone()).await.unwrap();
    let stored = store.find(&intent_id).await.unwrap().unwrap();
    assert_eq!(stored.intent, intent);
    assert_eq!(stored.producing_node, None);
}

#[tokio::test]
async fn muji_room_sweep_round_trips_with_webhook_room_sid() {
    let store = store("call-teardown-muji-room-sweep").await;
    let intent = CallTeardownIntent {
        call_id: CallId::new("room@conference.example.test").unwrap(),
        target: TeardownTarget::MujiRoomSweep {
            room_jid: BareJid::from_str("room@conference.example.test").unwrap(),
        },
        generation: None,
        room_sid: Some(RoomSid::new("RM_sweep").unwrap()),
        session: None,
    };

    let intent_id = store.enqueue(intent.clone()).await.unwrap();
    let stored = store.find(&intent_id).await.unwrap().unwrap();
    assert_eq!(stored.intent, intent);
}

#[tokio::test]
async fn muji_presence_clear_dedupe_is_exact_match_on_participant_sid() {
    let store = store("call-teardown-muji-dedupe").await;
    let first = CallTeardownIntent {
        call_id: CallId::new("room@conference.example.test").unwrap(),
        target: TeardownTarget::MujiPresenceClear {
            room_jid: BareJid::from_str("room@conference.example.test").unwrap(),
            departed: FullJid::from_str("alice@example.test/device").unwrap(),
            participant_sid: Some(ParticipantSid::new("PA_same").unwrap()),
        },
        generation: None,
        room_sid: Some(RoomSid::new("RM_same").unwrap()),
        session: None,
    };
    let second = first.clone();
    let third = CallTeardownIntent {
        target: TeardownTarget::MujiPresenceClear {
            room_jid: BareJid::from_str("room@conference.example.test").unwrap(),
            departed: FullJid::from_str("alice@example.test/device").unwrap(),
            participant_sid: Some(ParticipantSid::new("PA_other").unwrap()),
        },
        ..first.clone()
    };

    let first_id = store.enqueue(first).await.unwrap();
    let second_id = store.enqueue(second).await.unwrap();
    let third_id = store.enqueue(third).await.unwrap();

    assert_eq!(second_id, first_id);
    assert_ne!(third_id, first_id);
}

#[tokio::test]
async fn muji_room_sweep_schema_requires_webhook_room_sid() {
    let (database, _store) = store_with_db("call-teardown-muji-room-sweep-check").await;
    let connection = database.guard().await.unwrap();

    let error = connection
        .execute(
            "INSERT INTO call_teardown_outbox (\
                intent_id, call_id, identity, room_jid, action, generation, room_sid, \
                participant_sid, status, attempt_count, last_error, next_attempt_at_ms, \
                claimed_at_ms, claim_token, created_at_ms, updated_at_ms\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, ?, NULL, NULL, ?, ?)",
            crate::db_params![
                "invalid-muji-room-sweep",
                "room@conference.example.test",
                Option::<&str>::None,
                Some("room@conference.example.test"),
                "muji_room_sweep",
                Option::<i64>::None,
                Option::<&str>::None,
                Option::<&str>::None,
                "queued",
                1_i64,
                1_i64,
                1_i64,
            ],
        )
        .await
        .expect_err("missing room_sid must violate the action CHECK");

    let error_text = error.to_string();
    assert!(
        error_text.contains("CHECK") || error_text.contains("constraint"),
        "unexpected error: {error_text}"
    );
}

#[tokio::test]
async fn queue_stats_report_depth_and_oldest_age() {
    let store = store("call-teardown-stats").await;
    store.enqueue(participant_intent()).await.unwrap();
    let stats = store.queue_stats().await.unwrap();
    assert_eq!(stats.queued_count, 1);
    assert!(stats.oldest_queued_age_ms < 5_000);
}

#[tokio::test(flavor = "current_thread")]
async fn producer_retry_supervisor_coalesces_duplicate_batches() {
    let store = Arc::new(store("call-teardown-producer-supervisor").await);
    let supervisor = CallTeardownPersistenceSupervisor::new(
        Arc::clone(&store),
        tokio::runtime::Handle::current(),
    );
    let batch = vec![participant_intent()];

    supervisor.retry_batch(batch.clone());
    supervisor.retry_batch(batch);
    assert_eq!(supervisor.state_snapshot(), (true, 1));

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let persisted = store.queue_stats().await.expect("queue stats").queued_count == 1;
            if persisted && supervisor.state_snapshot() == (false, 0) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("supervised producer retry must persist and quiesce");
}

#[tokio::test]
async fn ownership_release_defers_old_row_so_later_work_can_be_claimed() {
    let store = store("call-teardown-ownership-deferral").await;
    let foreign_id = store
        .enqueue(CallTeardownIntent {
            call_id: CallId::new("room@muc.example.test").expect("room call id"),
            target: TeardownTarget::Room,
            generation: None,
            room_sid: None,
            session: None,
        })
        .await
        .expect("enqueue foreign");
    let foreign = store.claim_due(1).await.expect("claim foreign").remove(0);
    assert!(store
        .release_claim(&foreign)
        .await
        .expect("release foreign"));
    let local_id = store
        .enqueue(participant_intent())
        .await
        .expect("enqueue local");

    let next = store.claim_due(1).await.expect("claim local").remove(0);

    assert_eq!(next.intent_id, local_id);
    let foreign = store
        .find(&foreign_id)
        .await
        .expect("find foreign")
        .expect("foreign row");
    assert_eq!(foreign.status, CallTeardownStatus::Queued);
    assert!(foreign.next_attempt_at_ms.is_some());
}

#[tokio::test]
async fn participant_waits_for_pending_muji_presence_clear() {
    let store = store("call-teardown-muji-dependency").await;
    let room = BareJid::from_str("room@conference.example.test").expect("room");
    let departed = FullJid::from_str("alice@example.test/device").expect("full JID");
    let call_id = CallId::new(room.to_string()).expect("call id");
    store
        .enqueue(CallTeardownIntent {
            call_id: call_id.clone(),
            target: TeardownTarget::Participant {
                identity: departed.clone(),
                participant_sid: None,
            },
            generation: None,
            room_sid: None,
            session: None,
        })
        .await
        .expect("enqueue participant");
    let clear_id = store
        .enqueue(CallTeardownIntent {
            call_id: call_id.clone(),
            target: TeardownTarget::MujiPresenceClear {
                room_jid: room,
                departed: departed.clone(),
                participant_sid: None,
            },
            generation: None,
            room_sid: None,
            session: None,
        })
        .await
        .expect("enqueue presence clear");

    assert!(store
        .has_pending_muji_presence_clear(&call_id, &departed)
        .await
        .expect("dependency query"));
    let jobs = store.claim_due(8).await.expect("claim pair");
    let clear = jobs
        .iter()
        .find(|job| job.intent_id == clear_id)
        .expect("claimed presence clear");
    assert!(store.mark_done(clear).await.expect("complete clear"));
    assert!(!store
        .has_pending_muji_presence_clear(&call_id, &departed)
        .await
        .expect("dependency query"));
}
