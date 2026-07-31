use std::str::FromStr;
use std::sync::{Arc, Mutex};

use jid::{BareJid, FullJid};
use waddle_sfu::{
    CallGeneration, CallId, Identity, ListedRoom, LiveKitAdmin, MediaCapabilities, ParticipantSid,
    RoomOccupancy, RoomSid, SfuError, SfuService,
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
    assert!(store.mark_done(&claimed[0]).await.unwrap());

    let stored = store.find(&intent_id).await.unwrap().unwrap();
    assert_eq!(stored.status, CallTeardownStatus::Done);
    assert_eq!(stored.attempt_count, 0);
}

#[tokio::test]
async fn retry_increments_attempt_and_schedules_exponential_backoff() {
    let store = store("call-teardown-retry").await;
    let intent_id = store.enqueue(participant_intent()).await.unwrap();
    let claimed = store.claim_due(1).await.unwrap().pop().unwrap();

    let outcome = store
        .retry_or_fail(&claimed, "livekit unavailable")
        .await
        .unwrap();
    assert_eq!(
        outcome,
        CallTeardownRetryOutcome::Requeued { attempt_count: 1 }
    );
    let stored = store.find(&intent_id).await.unwrap().unwrap();
    assert_eq!(stored.status, CallTeardownStatus::Queued);
    assert_eq!(stored.attempt_count, 1);
    assert_eq!(stored.last_error.as_deref(), Some("livekit unavailable"));
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
        .retry_or_fail(&claimed, "permanent outage")
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
    assert_eq!(stored.last_error.as_deref(), Some("permanent outage"));
    assert_eq!(stored.next_attempt_at_ms, None);
}

#[tokio::test]
async fn terminal_write_requires_the_current_claim_token() {
    let store = store("call-teardown-claim-cas").await;
    let intent_id = store.enqueue(participant_intent()).await.unwrap();
    let claimed = store.claim_due(1).await.unwrap().pop().unwrap();
    let mut second_drainer = claimed.clone();
    second_drainer.claim_token = Some(uuid::Uuid::new_v4().to_string());

    assert!(!store.mark_done(&second_drainer).await.unwrap());
    assert_eq!(
        store
            .retry_or_fail(&second_drainer, "not my claim")
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
async fn muji_intent_round_trips_without_generation_or_sid_sentinels() {
    let store = store("call-teardown-muji").await;
    let intent = CallTeardownIntent {
        call_id: CallId::new("room@conference.example.test").unwrap(),
        target: TeardownTarget::MujiPresenceClear {
            room_jid: BareJid::from_str("room@conference.example.test").unwrap(),
            departed: FullJid::from_str("alice@example.test/device").unwrap(),
        },
        generation: None,
        room_sid: None,
    };

    let intent_id = store.enqueue(intent.clone()).await.unwrap();
    let stored = store.find(&intent_id).await.unwrap().unwrap();
    assert_eq!(stored.intent, intent);
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
    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(participant_intent())
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
    let call_id = CallId::new("alice@example.test:stale-call").expect("call id");
    let identity =
        Identity::from_jid(FullJid::from_str("alice@example.test/device").expect("full JID"));
    sfu.register_call_participant(&call_id, &identity);
    let _ = sfu.note_participant_left(&call_id, &identity, None);
    sfu.register_call_participant(&call_id, &identity);
    let state = state_with_executor(Arc::clone(&sfu)).await;
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
        .enqueue(CallTeardownIntent {
            call_id: call_id.clone(),
            target: TeardownTarget::MujiPresenceClear {
                room_jid: room_jid.clone(),
                departed: alice.clone(),
            },
            generation: None,
            room_sid: None,
        })
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
                "terminal failure".to_string(),
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
    assert_eq!(stored.last_error.as_deref(), Some("room_never_owned"));
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
