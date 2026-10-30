use std::str::FromStr;
use std::sync::Arc;

use waddle_sfu::{
    CallId, Identity, LiveParticipant, ObservedCallSids, ParticipantSid, RoomOccupancy,
    SessionBinding, SfuService,
};

use super::{
    create_test_server_owner_session, drain_due, fixture_config, get_room_actor,
    handle_muc_join_with_occupancy_session, occupant_generation, participant_intent,
    state_with_executor, store, CallTeardownIntent, CallTeardownStatus, FullJid, RecordingAdmin,
    TeardownTarget,
};
use crate::call_teardown_outbox::{CallTeardownIntentId, CallTeardownOutboxStore};
use crate::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_xmpp::muc::room_actor::{GetSnapshot, RestoreLiveRoster};
use waddle_xmpp::muc::MucRoom;

fn retirement_intent() -> CallTeardownIntent {
    CallTeardownIntent {
        target: TeardownTarget::DisplacedParticipant {
            identity: FullJid::from_str("alice@example.test/device").expect("participant JID"),
            participant_sid: Some(
                ParticipantSid::try_new("PA_displaced").expect("participant SID"),
            ),
        },
        occupant: Some(occupant_generation()),
        session: Some(SessionBinding::try_new("displaced-session").expect("session")),
        ..participant_intent()
    }
}

fn unresolved_retirement_intent() -> CallTeardownIntent {
    CallTeardownIntent {
        target: TeardownTarget::DisplacedParticipant {
            identity: FullJid::from_str("alice@example.test/device").expect("participant JID"),
            participant_sid: None,
        },
        ..retirement_intent()
    }
}

#[tokio::test]
async fn unresolved_retirement_round_trips_its_generation_and_rejects_missing_evidence() {
    let store = store("unresolved-retirement-round-trip").await;
    let intent = unresolved_retirement_intent();
    let id = store.enqueue(intent.clone()).await.expect("enqueue");
    let reopened = CallTeardownOutboxStore::try_new(store.db.clone())
        .await
        .expect("reopen");
    assert_eq!(
        reopened
            .find(&id)
            .await
            .expect("find")
            .expect("stored")
            .intent,
        intent
    );
    let invalid = CallTeardownIntent {
        occupant: None,
        ..intent
    };
    assert!(store.enqueue(invalid.clone()).await.is_err());
    assert!(store.enqueue_batch(&[invalid]).await.is_err());
}

#[tokio::test]
async fn sqlite_mandatory_sid_upgrade_preserves_known_work_and_accepts_unresolved_generation() {
    let database = Database::in_memory("mandatory-sid-retirement-upgrade")
        .await
        .expect("database");
    let connection = database.guard().await.expect("connection");
    connection
        .execute_batch(include_str!("fixtures/mandatory_sid_retirement.sql"))
        .await
        .expect("mandatory SID schema");
    connection.execute(
        "INSERT INTO call_teardown_outbox \
         (intent_id, call_id, identity, participant_sid, action, status, created_at_ms, updated_at_ms) \
         VALUES ('known-retirement', 'alice@example.test:call-1', 'alice@example.test/device', \
         'PA_known', 'retire_displaced_participant', 'queued', 100, 200)", (),
    ).await.expect("historical SID-only retirement");
    drop(connection);
    let store = CallTeardownOutboxStore::try_new(database)
        .await
        .expect("upgrade");
    let known = store
        .find(&CallTeardownIntentId::from_stored(
            "known-retirement".into(),
        ))
        .await
        .expect("find")
        .expect("known work");
    assert_eq!(known.intent.occupant, None);
    assert!(matches!(
        known.intent.target,
        TeardownTarget::DisplacedParticipant {
            participant_sid: Some(_),
            ..
        }
    ));
    let unresolved = unresolved_retirement_intent();
    let id = store
        .enqueue(unresolved.clone())
        .await
        .expect("unresolved retirement");
    assert_eq!(
        store.find(&id).await.expect("find").expect("stored").intent,
        unresolved
    );
}

#[tokio::test]
async fn retirement_round_trips_and_deduplicates_separately_from_departure() {
    let store = store("displaced-participant-round-trip").await;
    let retirement = retirement_intent();
    let id = store.enqueue(retirement.clone()).await.expect("enqueue");
    assert_eq!(store.enqueue(retirement.clone()).await.expect("dedupe"), id);
    let mut departure = retirement.clone();
    departure.target = TeardownTarget::Participant {
        identity: FullJid::from_str("alice@example.test/device").expect("participant JID"),
        participant_sid: Some(ParticipantSid::try_new("PA_displaced").expect("SID")),
    };
    assert_ne!(store.enqueue(departure).await.expect("departure"), id);
    assert_eq!(
        store.find(&id).await.expect("find").expect("stored").intent,
        retirement
    );
}

#[tokio::test]
async fn sqlite_legacy_outbox_upgrade_preserves_jobs_indexes_and_triggers() {
    let database = Database::in_memory("legacy-retirement-upgrade")
        .await
        .expect("database");
    let connection = database.guard().await.expect("connection");
    connection
        .execute_batch(include_str!("fixtures/pre_displaced_retirement.sql"))
        .await
        .expect("legacy schema");
    connection
        .execute_batch(
            "INSERT INTO call_teardown_outbox \
                (intent_id, call_id, identity, action, status, attempt_count, \
                 next_attempt_at_ms, created_at_ms, updated_at_ms, participant_sid, \
                 session_binding, claim_token, producing_node) \
             VALUES ('old-job', 'alice@example.test:call-1', 'alice@example.test/device', \
                 'remove_participant', 'in-progress', 3, 1234, 100, 200, 'PA_old', \
                 'old-session', 'old-claim', '[\"old-node\",\"old-epoch\"]'); \
             CREATE INDEX old_outbox_status ON call_teardown_outbox(status); \
             CREATE TABLE retirement_upgrade_audit (intent_id TEXT NOT NULL);",
        )
        .await
        .expect("legacy data and dependent schema");
    connection
        .execute(
            "CREATE TRIGGER old_outbox_audit AFTER INSERT ON call_teardown_outbox \
             BEGIN INSERT INTO retirement_upgrade_audit VALUES (NEW.intent_id); END",
            (),
        )
        .await
        .expect("legacy trigger");
    drop(connection);

    let store = CallTeardownOutboxStore::try_new(database.clone())
        .await
        .expect("upgrade");
    let job = store
        .find(&CallTeardownIntentId::from_stored("old-job".into()))
        .await
        .expect("find")
        .expect("preserved job");
    assert_eq!(job.status, CallTeardownStatus::InProgress);
    assert_eq!(job.attempt_count, 3);
    assert_eq!(job.next_attempt_at_ms, Some(1234));
    assert_eq!(job.created_at_ms, 100);
    assert_eq!(job.claim_token.expect("claim").as_str(), "old-claim");
    assert_eq!(
        job.intent.session,
        Some(SessionBinding::try_new("old-session").expect("session"))
    );
    assert_eq!(
        job.intent.target,
        TeardownTarget::Participant {
            identity: FullJid::from_str("alice@example.test/device").expect("JID"),
            participant_sid: Some(ParticipantSid::try_new("PA_old").expect("SID")),
        }
    );
    let new_id = store
        .enqueue(retirement_intent())
        .await
        .expect("new action");
    // Repeated startup is harmless and retains both old and new work.
    let reopened = CallTeardownOutboxStore::try_new(database.clone())
        .await
        .expect("idempotent initialization");
    assert!(reopened.find(&new_id).await.expect("find").is_some());
    let connection = database.guard().await.expect("connection");
    let mut rows = connection
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' \
             AND name IN ('old_outbox_status', 'idx_call_teardown_outbox_status_due', \
                 'idx_call_teardown_outbox_dedupe')",
            (),
        )
        .await
        .expect("indexes");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("count"),
        3
    );
    let mut rows = connection
        .query("SELECT intent_id FROM retirement_upgrade_audit", ())
        .await
        .expect("trigger output");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("audit entry")
            .get::<String>(0)
            .expect("intent ID"),
        new_id.as_str()
    );
}

async fn postgres_upgrade(fixture: &'static str) {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (outbox action upgrade)");
        return;
    };
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("Postgres admin");
    let schema = format!("waddle_test_retirement_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("isolated schema");
    let mut url = url::Url::parse(&database_url).expect("database URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair("options", &format!("-c search_path={schema}"));
    let database = Database::from_config(
        "legacy-retirement-postgres-upgrade",
        &DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string()),
    )
    .await
    .expect("isolated database");
    let connection = database.guard().await.expect("connection");
    connection
        .execute_batch(&fixture.replace(" INTEGER", " BIGINT"))
        .await
        .expect("legacy schema");
    connection
        .execute(
            "INSERT INTO call_teardown_outbox \
            (intent_id, call_id, identity, action, status, attempt_count, \
             next_attempt_at_ms, created_at_ms, updated_at_ms, participant_sid) \
         VALUES ('old-job', 'alice@example.test:call-1', 'alice@example.test/device', \
             'remove_participant', 'queued', 3, 1234, 100, 200, 'PA_old')",
            (),
        )
        .await
        .expect("legacy job");
    drop(connection);
    let store = CallTeardownOutboxStore::try_new(database.clone())
        .await
        .expect("upgrade");
    let old_id = CallTeardownIntentId::from_stored("old-job".into());
    assert_eq!(
        store
            .find(&old_id)
            .await
            .expect("find")
            .expect("old job")
            .attempt_count,
        3
    );
    let new_id = store
        .enqueue(unresolved_retirement_intent())
        .await
        .expect("retirement");
    let reopened = CallTeardownOutboxStore::try_new(database.clone())
        .await
        .expect("idempotent upgrade");
    assert!(reopened.find(&new_id).await.expect("find").is_some());
    drop(reopened);
    drop(store);
    drop(database);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("remove isolated schema");
}

#[tokio::test]
async fn postgres_legacy_outbox_upgrade_preserves_jobs_and_accepts_retirement() {
    postgres_upgrade(include_str!("fixtures/pre_displaced_retirement.sql")).await;
}

#[tokio::test]
async fn postgres_mandatory_sid_upgrade_accepts_unresolved_generation() {
    postgres_upgrade(include_str!("fixtures/mandatory_sid_retirement.sql")).await;
}

#[derive(Clone, Copy)]
enum CanonicalSeat {
    Successor,
    Candidate,
    MissingGeneration,
    Absent,
}

#[derive(Clone, Copy)]
enum RetirementEvidence {
    SidAndGeneration,
    GenerationOnly,
    SidOnly,
}

async fn drain_retirement(
    live_sid: &str,
    fail_remove: bool,
    evidence: RetirementEvidence,
    canonical_seat: CanonicalSeat,
) {
    let unresolved_sid = matches!(evidence, RetirementEvidence::GenerationOnly);
    let jid: FullJid = "alice@example.com/web".parse().expect("participant JID");
    let identity = Identity::from_jid(jid.clone());
    let candidate = occupant_generation();
    let candidate_is_current = matches!(canonical_seat, CanonicalSeat::Candidate);
    let successor = if candidate_is_current {
        candidate
    } else {
        occupant_generation()
    };
    let provider_generation = if live_sid == "PA_displaced" {
        candidate
    } else {
        successor
    };
    let admin = Arc::new(RecordingAdmin {
        fail_remove,
        occupancy: std::sync::Mutex::new(RoomOccupancy {
            waddle: vec![LiveParticipant {
                identity: identity.clone(),
                participant_sid: Some(ParticipantSid::try_new(live_sid).expect("live SID")),
                media_generation: Some(provider_generation),
            }],
            foreign: 0,
        }),
        ..RecordingAdmin::default()
    });
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        admin.clone() as Arc<_>,
    ));
    let state = state_with_executor(sfu.clone()).await;
    let room_jid = "retirement-authority@muc.example.com"
        .parse()
        .expect("room JID");
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        (successor, Some(&owner_session)),
    )
    .await;
    if matches!(
        canonical_seat,
        CanonicalSeat::MissingGeneration | CanonicalSeat::Absent
    ) {
        let actor = get_room_actor(state.as_ref(), &room_jid)
            .await
            .expect("room actor");
        let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
        let mut room = MucRoom::new(
            room_jid.clone(),
            snapshot.room.waddle_id.clone(),
            snapshot.room.channel_id.clone(),
            snapshot.room.config.clone(),
        );
        if matches!(canonical_seat, CanonicalSeat::MissingGeneration) {
            // Simulate a legacy restored seat whose signaling generation is
            // unavailable; media observations cannot supply that authority.
            room.occupants = snapshot.room.occupants.clone();
        }
        actor
            .ask(RestoreLiveRoster {
                room,
                occupancy_revision: snapshot.occupancy_revision,
                departures: snapshot.departures,
                live_roster_restore_attempt: snapshot.live_roster_restore_attempt,
                occupancy_incarnation: snapshot.occupancy_incarnation,
                pending_affiliation_departures: snapshot.pending_affiliation_departures,
                pending_admin_projection: None,
                admin_mutation_resolutions: snapshot.admin_mutation_resolutions,
            })
            .await
            .expect("restore canonical seat");
    }
    let mut intent = CallTeardownIntent {
        call_id: CallId::try_new(room_jid.to_string()).expect("call ID"),
        target: TeardownTarget::DisplacedParticipant {
            identity: jid,
            participant_sid: (!unresolved_sid)
                .then(|| ParticipantSid::try_new("PA_displaced").expect("SID")),
        },
        occupant: (!matches!(evidence, RetirementEvidence::SidOnly)).then_some(candidate),
        ..retirement_intent()
    };
    let successor_session = SessionBinding::try_new("successor-session").expect("session");
    // Deliberately disagree with the authoritative actor in the stale-producer
    // case: local SFU generation must never authorize retiring the actor's seat.
    let cached_generation = if candidate_is_current {
        occupant_generation()
    } else {
        successor
    };
    let has_cache = !unresolved_sid || candidate_is_current;
    if has_cache {
        sfu.register_call_participant_observed(
            &intent.call_id,
            &identity,
            &ObservedCallSids::new(
                intent.room_sid.clone(),
                Some(ParticipantSid::try_new("PA_displaced").expect("old SID")),
            ),
        );
        sfu.register_call_participant_with_session(
            &intent.call_id,
            &identity,
            &successor_session,
            cached_generation,
        );
    } else {
        // A restarted owner has neither the producer's SFU registry nor its
        // observed SIDs. Its persisted generation must resolve the target.
        intent.room_sid = None;
    }
    let store = &state.deps.protocol.call_teardown_outbox;
    let id = store.enqueue(intent.clone()).await.expect("enqueue");
    // Reopening proves unresolved work survives beyond the producer's cache.
    let reopened = CallTeardownOutboxStore::try_new(store.db.clone())
        .await
        .expect("reopen");
    assert_eq!(
        reopened
            .find(&id)
            .await
            .expect("find")
            .expect("stored")
            .intent,
        intent
    );
    let summary = drain_due(&state, 8).await.expect("drain");
    let stored = store.find(&id).await.expect("find").expect("stored");
    let authority_unknown = matches!(canonical_seat, CanonicalSeat::MissingGeneration);
    let should_remove = live_sid == "PA_displaced" && !candidate_is_current && !authority_unknown;
    assert_eq!(
        admin.remove_calls.lock().expect("calls").len(),
        usize::from(should_remove)
    );
    if (should_remove && fail_remove) || authority_unknown {
        assert_eq!(summary.requeued, 1);
        assert_eq!(stored.status, CallTeardownStatus::Queued);
        assert_eq!(stored.attempt_count, 1);
    } else {
        assert_eq!(summary.drained, 1);
        assert_eq!(stored.status, CallTeardownStatus::Done);
    }
    assert_eq!(
        sfu.participant_occupant_session(&intent.call_id, &identity),
        has_cache.then_some(cached_generation)
    );
    assert_eq!(
        sfu.participant_session_binding(&intent.call_id, &identity),
        has_cache.then_some(successor_session)
    );
}

#[tokio::test]
async fn retirement_drain_is_not_suppressed_by_the_successor_session() {
    drain_retirement(
        "PA_displaced",
        false,
        RetirementEvidence::SidAndGeneration,
        CanonicalSeat::Successor,
    )
    .await;
}

#[tokio::test]
async fn retirement_drain_skips_a_connected_successor() {
    drain_retirement(
        "PA_successor",
        false,
        RetirementEvidence::SidAndGeneration,
        CanonicalSeat::Successor,
    )
    .await;
}

#[tokio::test]
async fn retirement_drain_requeues_failed_admin_removal_without_clearing_successor() {
    drain_retirement(
        "PA_displaced",
        true,
        RetirementEvidence::SidAndGeneration,
        CanonicalSeat::Successor,
    )
    .await;
}

#[tokio::test]
async fn retirement_reopened_without_sid_resolves_the_tagged_displaced_generation() {
    drain_retirement(
        "PA_displaced",
        false,
        RetirementEvidence::GenerationOnly,
        CanonicalSeat::Successor,
    )
    .await;
}

#[tokio::test]
async fn retirement_never_removes_the_current_actor_generation_despite_stale_sfu_cache() {
    drain_retirement(
        "PA_displaced",
        false,
        RetirementEvidence::GenerationOnly,
        CanonicalSeat::Candidate,
    )
    .await;
}

#[tokio::test]
async fn known_sid_retirement_cannot_remove_the_current_actor_generation() {
    drain_retirement(
        "PA_displaced",
        false,
        RetirementEvidence::SidAndGeneration,
        CanonicalSeat::Candidate,
    )
    .await;
}

#[tokio::test]
async fn unowned_retirement_keeps_its_responsibility_after_the_ownership_deadline() {
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::new(RecordingAdmin::default()),
    ));
    let state = state_with_executor(sfu).await;
    let intent = CallTeardownIntent {
        call_id: CallId::try_new("unowned-retirement@muc.example.com").expect("room ID"),
        ..unresolved_retirement_intent()
    };
    let store = &state.deps.protocol.call_teardown_outbox;
    let id = store
        .enqueue_at(intent, crate::time::now_ms() - 48 * 60 * 60 * 1000)
        .await
        .expect("enqueue");
    let summary = drain_due(&state, 8).await.expect("drain");
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.drained, 0);
    let stored = store.find(&id).await.expect("find").expect("stored");
    assert_eq!(stored.status, CallTeardownStatus::Queued);
    assert_eq!(stored.attempt_count, 0);
}

#[tokio::test]
async fn retirement_retries_a_seated_actor_with_unknown_generation() {
    drain_retirement(
        "PA_displaced",
        false,
        RetirementEvidence::GenerationOnly,
        CanonicalSeat::MissingGeneration,
    )
    .await;
}

#[tokio::test]
async fn retirement_can_remove_a_proven_absent_occupant() {
    drain_retirement(
        "PA_displaced",
        false,
        RetirementEvidence::GenerationOnly,
        CanonicalSeat::Absent,
    )
    .await;
}

#[tokio::test]
async fn historical_sid_only_retirement_preserves_the_current_tagged_generation() {
    drain_retirement(
        "PA_displaced",
        false,
        RetirementEvidence::SidOnly,
        CanonicalSeat::Candidate,
    )
    .await;
}
