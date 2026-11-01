use std::sync::Arc;

use jid::{BareJid, FullJid};
use waddle_sfu::{CallId, Identity, MediaCapabilities, SessionBinding, SfuService};

use super::{
    create_test_server_owner_session, drain_due, fixture_config, get_room_actor,
    handle_muc_join_with_occupancy_session, occupant_generation, snapshot_room,
    state_with_executor, store, CallTeardownIntent, CallTeardownOutboxStore,
    CallTeardownRetryOutcome, CallTeardownRetryReason, CallTeardownStatus, Creator, Database,
    MediaKind, Muji, MujiContent, RecordingAdmin, TeardownTarget, UpsertMujiPresence, MAX_ATTEMPTS,
    MAX_RETRY_DELAY_MS,
};
use crate::call_teardown_outbox::CallTeardownIntentId;

fn actor_only_intent() -> CallTeardownIntent {
    CallTeardownIntent {
        call_id: CallId::try_new("ghost-clear@muc.example.com").expect("call ID"),
        target: TeardownTarget::MujiPresenceClearOnly {
            room_jid: "ghost-clear@muc.example.com".parse().expect("room JID"),
            departed: "alice@example.com/web".parse().expect("participant JID"),
        },
        generation: None,
        occupant: None,
        unbound_occupant: waddle_sfu::UnboundOccupantPolicy::Keep,
        room_sid: None,
        session: None,
    }
}

#[tokio::test]
async fn actor_only_retry_round_trips_and_never_deduplicates_with_a_departure() {
    let store = store("actor-only-round-trip").await;
    let intent = actor_only_intent();
    let id = store.enqueue(intent.clone()).await.expect("enqueue");
    assert_eq!(store.enqueue(intent.clone()).await.expect("dedupe"), id);
    let mut departure = intent.clone();
    departure.target = TeardownTarget::MujiPresenceClear {
        room_jid: "ghost-clear@muc.example.com".parse().expect("room"),
        departed: "alice@example.com/web".parse().expect("JID"),
        participant_sid: None,
    };
    assert_ne!(
        store.enqueue(departure).await.expect("ordinary departure"),
        id
    );
    assert_eq!(
        store.find(&id).await.expect("find").expect("stored").intent,
        intent
    );
}

#[tokio::test]
async fn actor_only_action_upgrades_all_previous_outbox_check_shapes() {
    for (index, fixture) in [
        include_str!("fixtures/pre_displaced_retirement.sql"),
        include_str!("fixtures/mandatory_sid_retirement.sql"),
        include_str!("fixtures/generation_retirement.sql"),
    ]
    .into_iter()
    .enumerate()
    {
        let db = Database::in_memory(&format!("actor-only-upgrade-{index}"))
            .await
            .expect("database");
        let connection = db.guard().await.expect("connection");
        connection
            .execute_batch(fixture)
            .await
            .expect("previous schema");
        connection.execute(
            "INSERT INTO call_teardown_outbox \
             (intent_id, call_id, identity, room_jid, action, status, created_at_ms, updated_at_ms) \
             VALUES ('old-clear', 'ghost-clear@muc.example.com', 'alice@example.com/web', \
             'ghost-clear@muc.example.com', 'muji_presence_clear', 'queued', 100, 200)", (),
        ).await.expect("old work");
        drop(connection);
        let store = CallTeardownOutboxStore::try_new(db.clone())
            .await
            .expect("upgrade");
        let old = store
            .find(&CallTeardownIntentId::from_stored("old-clear".into()))
            .await
            .expect("find")
            .expect("old work");
        assert!(matches!(
            old.intent.target,
            TeardownTarget::MujiPresenceClear { .. }
        ));
        let intent = actor_only_intent();
        let id = store
            .enqueue(intent.clone())
            .await
            .expect("new actor-only action");
        let reopened = CallTeardownOutboxStore::try_new(db)
            .await
            .expect("idempotent upgrade");
        assert_eq!(
            reopened
                .find(&id)
                .await
                .expect("find")
                .expect("stored")
                .intent,
            intent
        );
    }
}

#[tokio::test]
async fn actor_only_infrastructure_retry_survives_the_normal_attempt_cap() {
    let store = store("actor-only-infinite-retry").await;
    let id = store
        .enqueue_at(actor_only_intent(), 0)
        .await
        .expect("enqueue");
    let mut now = 0;
    for _ in 0..=MAX_ATTEMPTS {
        let job = store
            .claim_due_at(1, now)
            .await
            .expect("claim")
            .pop()
            .expect("due job");
        assert!(matches!(
            store
                .retry_or_fail_at(&job, CallTeardownRetryReason::MujiPresenceClearOnly, now)
                .await
                .expect("retry"),
            CallTeardownRetryOutcome::Requeued { .. }
        ));
        now += MAX_RETRY_DELAY_MS + 1;
    }
    assert_eq!(
        store
            .find(&id)
            .await
            .expect("find")
            .expect("pending")
            .status,
        CallTeardownStatus::Queued
    );
}

async fn assert_owner_drains_only_the_actor_advertisement(new_registration: bool) {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        admin.clone() as Arc<_>,
    ));
    let state = state_with_executor(sfu.clone()).await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "ghost-clear@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/web".parse().expect("JID");
    let generation = occupant_generation();
    handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        (generation, Some(&session)),
    )
    .await;
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            occupant: None,
            muji: Muji::with_contents(vec![MujiContent::new(
                "audio",
                Creator::Initiator,
                MediaKind::Audio,
            )]),
        })
        .await
        .expect("Muji update")
        .expect("occupant update");
    let identity = Identity::from_jid(alice.clone());
    let call = CallId::try_new(room_jid.to_string()).expect("call");
    let token = new_registration.then(|| {
        sfu.issue_join_token_with_session(
            &call,
            &identity,
            MediaCapabilities::direct_call_peer(),
            &SessionBinding::try_new("new-session").expect("session"),
            generation,
            sfu.participant_registration_expectation(&call, &identity)
                .expect("expectation"),
        )
        .expect("mint")
        .expect("publish")
    });
    let store = &state.deps.protocol.call_teardown_outbox;
    let id = store
        .enqueue(actor_only_intent())
        .await
        .expect("actor-only retry");
    let summary = drain_due(&state, 8).await.expect("drain");
    assert_eq!(summary.drained, 1);
    assert_eq!(
        store.find(&id).await.expect("find").expect("stored").status,
        CallTeardownStatus::Done
    );
    assert_eq!(
        snapshot_room(state.as_ref(), &room_jid)
            .await
            .room
            .muji_for_session("alice", &alice)
            .is_some(),
        new_registration
    );
    assert!(admin.remove_calls.lock().expect("calls").is_empty());
    if let Some(token) = token {
        assert!(!sfu.is_revoked(&token.jti));
        assert!(sfu.has_call_participant(&call, &identity));
    }
}

#[tokio::test]
async fn owner_retries_ghost_advertisement_cleanup_without_an_sfu_departure() {
    assert_owner_drains_only_the_actor_advertisement(false).await;
}

#[tokio::test]
async fn new_publication_makes_queued_actor_only_cleanup_obsolete() {
    assert_owner_drains_only_the_actor_advertisement(true).await;
}
