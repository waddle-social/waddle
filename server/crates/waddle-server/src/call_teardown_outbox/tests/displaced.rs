use std::str::FromStr;
use std::sync::Arc;

use waddle_sfu::{
    Identity, ObservedCallSids, ParticipantSid, RoomOccupancy, SessionBinding, SfuService,
};

use super::{
    drain_due, fixture_config, occupant_generation, participant_intent, state_with_executor, store,
    CallTeardownIntent, CallTeardownStatus, FullJid, RecordingAdmin, TeardownTarget,
};
use crate::call_teardown_outbox::{CallTeardownIntentId, CallTeardownOutboxStore};
use crate::db::{Database, DatabaseConfig, DatabaseDriver};

fn retirement_intent() -> CallTeardownIntent {
    CallTeardownIntent {
        target: TeardownTarget::DisplacedParticipant {
            identity: FullJid::from_str("alice@example.test/device").expect("participant JID"),
            participant_sid: ParticipantSid::try_new("PA_displaced").expect("participant SID"),
        },
        occupant: Some(occupant_generation()),
        session: Some(SessionBinding::try_new("displaced-session").expect("session")),
        ..participant_intent()
    }
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

#[tokio::test]
async fn postgres_legacy_outbox_upgrade_preserves_jobs_and_accepts_retirement() {
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
        .execute_batch(
            &include_str!("fixtures/pre_displaced_retirement.sql").replace(" INTEGER", " BIGINT"),
        )
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
        .enqueue(retirement_intent())
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

async fn drain_retirement(live_sid: &str, fail_remove: bool) {
    let identity = Identity::from_jid(
        FullJid::from_str("alice@example.test/device").expect("participant JID"),
    );
    let admin = Arc::new(RecordingAdmin {
        fail_remove,
        occupancy: std::sync::Mutex::new(RoomOccupancy {
            waddle: vec![(
                identity.clone(),
                Some(ParticipantSid::try_new(live_sid).expect("live SID")),
            )],
            foreign: 0,
        }),
        ..RecordingAdmin::default()
    });
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        admin.clone() as Arc<_>,
    ));
    let state = state_with_executor(sfu.clone()).await;
    let intent = retirement_intent();
    let successor = occupant_generation();
    let successor_session = SessionBinding::try_new("successor-session").expect("session");
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
        successor,
    );
    let id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(intent.clone())
        .await
        .expect("enqueue");
    let summary = drain_due(&state, 8).await.expect("drain");
    let stored = state
        .deps
        .protocol
        .call_teardown_outbox
        .find(&id)
        .await
        .expect("find")
        .expect("stored");
    let should_remove = live_sid == "PA_displaced";
    assert_eq!(
        admin.remove_calls.lock().expect("calls").len(),
        usize::from(should_remove)
    );
    if should_remove && fail_remove {
        assert_eq!(summary.requeued, 1);
        assert_eq!(stored.status, CallTeardownStatus::Queued);
        assert_eq!(stored.attempt_count, 1);
    } else {
        assert_eq!(summary.drained, 1);
        assert_eq!(stored.status, CallTeardownStatus::Done);
    }
    assert_eq!(
        sfu.participant_occupant_session(&intent.call_id, &identity),
        Some(successor)
    );
    assert_eq!(
        sfu.participant_session_binding(&intent.call_id, &identity),
        Some(successor_session)
    );
}

#[tokio::test]
async fn retirement_drain_is_not_suppressed_by_the_successor_session() {
    drain_retirement("PA_displaced", false).await;
}

#[tokio::test]
async fn retirement_drain_skips_a_connected_successor() {
    drain_retirement("PA_successor", false).await;
}

#[tokio::test]
async fn retirement_drain_requeues_failed_admin_removal_without_clearing_successor() {
    drain_retirement("PA_displaced", true).await;
}
