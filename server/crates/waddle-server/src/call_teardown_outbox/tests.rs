use std::str::FromStr;
use std::sync::{Arc, Mutex};

use jid::{BareJid, FullJid};
use waddle_sfu::{
    CallGeneration, CallId, Identity, ListedRoom, LiveKitAdmin, MediaCapabilities,
    ObservedCallSids, ParticipantSid, RoomOccupancy, RoomSid, SfuError, SfuService,
};
use waddle_xmpp::muc::room_actor::UpsertMujiPresence;
use waddle_xmpp::xep::xep0167::MediaKind;
use waddle_xmpp::xep::xep0272::{Creator, Muji, MujiContent};

use super::*;
use crate::db::Database;
use crate::server::routes::websocket::{
    get_room_actor,
    handlers::presence::handle_muc_join,
    tests::{
        create_test_server_owner_session, create_test_websocket_state_with_clustering,
        create_test_websocket_state_with_sfu, snapshot_room,
    },
};
use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity};

async fn store(name: &str) -> CallTeardownOutboxStore {
    CallTeardownOutboxStore::new(Database::in_memory(name).await.unwrap())
        .await
        .unwrap()
}

async fn store_with_db(name: &str) -> (Database, CallTeardownOutboxStore) {
    let database = Database::in_memory(name).await.unwrap();
    let store = CallTeardownOutboxStore::new(database.clone())
        .await
        .unwrap();
    (database, store)
}

fn participant_intent() -> CallTeardownIntent {
    CallTeardownIntent {
        call_id: CallId::new("alice@example.test:call-1").unwrap(),
        target: TeardownTarget::Participant {
            identity: FullJid::from_str("alice@example.test/device").unwrap(),
            participant_sid: Some(ParticipantSid::new("PA_test").unwrap()),
        },
        generation: Some(CallGeneration::try_from(1).unwrap()),
        room_sid: Some(RoomSid::new("RM_test").unwrap()),
    }
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
        .retry_or_fail(&claimed, CallTeardownRetryReason::LiveKitAdmin)
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
            CallTeardownRetryReason::LiveKitAdmin
        ))
    );
    assert_eq!(stored.next_attempt_at_ms, None);
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
    };

    let intent_id = store.enqueue(intent.clone()).await.unwrap();
    let stored = store.find(&intent_id).await.unwrap().unwrap();
    assert_eq!(stored.intent, intent);
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

#[derive(Default)]
struct RecordingAdmin {
    remove_calls: Mutex<Vec<(CallId, Identity)>>,
    fail_remove: bool,
    occupancy: Mutex<RoomOccupancy>,
}

impl LiveKitAdmin for RecordingAdmin {
    fn list_rooms(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<ListedRoom>, SfuError>> + Send + '_>,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn remove_participant<'a>(
        &'a self,
        room: &'a CallId,
        identity: &'a Identity,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SfuError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.remove_calls
                .lock()
                .expect("recording lock")
                .push((room.clone(), identity.clone()));
            if self.fail_remove {
                Err(SfuError::InvalidCallId("simulated admin failure".into()))
            } else {
                Ok(())
            }
        })
    }

    fn delete_room<'a>(
        &'a self,
        _room: &'a CallId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SfuError>> + Send + 'a>>
    {
        Box::pin(async { Ok(()) })
    }

    fn update_participant<'a>(
        &'a self,
        _room: &'a CallId,
        _identity: &'a Identity,
        _capabilities: MediaCapabilities,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SfuError>> + Send + 'a>>
    {
        Box::pin(async { Ok(()) })
    }

    fn room_occupancy<'a>(
        &'a self,
        _room: &'a CallId,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RoomOccupancy, SfuError>> + Send + 'a>,
    > {
        Box::pin(async { Ok(self.occupancy.lock().expect("recording lock").clone()) })
    }
}

fn fixture_config() -> waddle_sfu::SfuConfig {
    waddle_sfu::SfuConfig {
        api_key: waddle_sfu::ApiKey::new("APItestkey"),
        api_secret: waddle_sfu::ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test API secret"),
        webhook_secret: waddle_sfu::ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test webhook secret"),
        ws_url: waddle_sfu::WebsocketUrl::new("wss://livekit.test/".parse().expect("test URL"))
            .expect("test websocket URL"),
        turn_host: waddle_sfu::TurnHost::new("turn.test"),
        turn_tls_port: 443,
        turn_udp_port: 3478,
        turn_shared_secret: waddle_sfu::TurnSharedSecret::from_text("turn-secret"),
        token_ttl: chrono::Duration::seconds(3_600),
        turn_ttl: chrono::Duration::seconds(3_600),
    }
}

async fn state_with_executor(
    sfu: Arc<waddle_sfu::LiveKitSfu>,
) -> Arc<crate::server::routes::websocket::WebSocketState> {
    let sfu_service: Arc<dyn SfuService> = sfu.clone();
    let mut state = create_test_websocket_state_with_sfu(sfu_service).await;
    Arc::get_mut(&mut state)
        .expect("test state has one strong reference")
        .deps
        .protocol
        .call_teardown_executor = Some(sfu.teardown_executor());
    state
}

fn active_muji() -> Muji {
    Muji::with_contents(vec![MujiContent::new(
        "audio",
        Creator::Initiator,
        MediaKind::Audio,
    )])
}

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
    let _ = sfu.note_participant_left(&call_id, &identity, None);
    sfu.register_call_participant(&call_id, &identity);
    let intent = CallTeardownIntent {
        call_id,
        target: TeardownTarget::Participant {
            identity: identity.as_jid().clone(),
            participant_sid: None,
        },
        generation: Some(CallGeneration::try_from_u64(1).expect("generation")),
        room_sid: None,
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
        room_sid: None,
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
async fn ownership_release_defers_old_row_so_later_work_can_be_claimed() {
    let store = store("call-teardown-ownership-deferral").await;
    let foreign_id = store
        .enqueue(CallTeardownIntent {
            call_id: CallId::new("room@muc.example.test").expect("room call id"),
            target: TeardownTarget::Room,
            generation: None,
            room_sid: None,
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

#[tokio::test]
async fn occupied_room_intent_is_requeued_not_consumed() {
    let alice =
        Identity::from_jid(FullJid::from_str("alice@example.test/device").expect("full JID"));
    let admin = Arc::new(RecordingAdmin {
        occupancy: Mutex::new(RoomOccupancy {
            waddle: vec![alice],
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
            room_sid: None,
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
                room_sid: None,
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
            room_sid: None,
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
                room_sid: None,
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
            room_sid: None,
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
            room_sid: old_sids.room_sid.clone(),
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
            room_sid: observed_sids.room_sid.clone(),
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
            room_sid: observed_sids.room_sid.clone(),
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
            room_sid: Some(RoomSid::new("RM_stale").expect("room sid")),
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
                CallTeardownRetryReason::LiveKitAdmin,
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
