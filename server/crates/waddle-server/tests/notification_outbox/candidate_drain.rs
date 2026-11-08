//! Exercise T1 persistence through the production candidate-drain entry point.

use std::time::Duration;

use crate::ingress_support::IngressFixture;
use crate::support::{
    bare, candidate, drain_deps_with_noop_activity, noop_activity_reader, register_push_target,
    settings_projection, target_named, StubRoomPolicy,
};
use waddle_server::db::DatabaseDriver;
use waddle_server::ingress_uow::{
    CanonicalMessageRepository, DbRetryClass, EffectIntentRepository, IngressUowError,
};
use waddle_server::notification_outbox::{
    NoopDndReader, NotificationCandidate, NotificationOutboxError, NotificationOutboxStore,
};
use waddle_server::notification_settings_projection::NotificationSettingsProjectionStore;
use waddle_server::test_support::attach_notification_candidate_lineage_for_test;
use waddle_xmpp::ingress::{
    IngressEffectIntent, MessageKey, NotificationActivityMutation, NotificationCandidateOutcome,
    SemanticDigest,
};
use waddle_xmpp::push::InMemoryPushStore;
use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

struct DrainDependencies {
    push: InMemoryPushStore,
    blocking: InMemoryBlockingStorage,
    settings: NotificationSettingsProjectionStore,
    room_policy: StubRoomPolicy,
}

impl DrainDependencies {
    async fn new(candidate: &NotificationCandidate) -> Self {
        let push = InMemoryPushStore::new();
        for node in ["node-one", "node-two"] {
            register_push_target(&push, candidate.recipient_bare_jid(), &target_named(node)).await;
        }
        Self {
            push,
            blocking: InMemoryBlockingStorage::new(),
            settings: settings_projection().await,
            room_policy: StubRoomPolicy::new(),
        }
    }

    async fn drain(
        &self,
        store: &NotificationOutboxStore,
    ) -> Result<usize, NotificationOutboxError> {
        store
            .drain_pending_candidates_into_outbox(
                &self.push,
                &self.blocking,
                &self.settings,
                drain_deps_with_noop_activity(
                    &self.room_policy,
                    &NoopDndReader,
                    noop_activity_reader(),
                ),
                &bare("push.example.com"),
                16,
            )
            .await
    }
}

async fn attach_parent(fixture: &IngressFixture, candidate: &NotificationCandidate) -> MessageKey {
    let key = MessageKey::new();
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: candidate.recipient_bare_jid().clone(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: candidate.conversation_jid().clone(),
            archive_stanza_id: candidate.archive_stanza_id().clone(),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    let mut transaction = fixture.uow.begin().await.expect("canonical transaction");
    CanonicalMessageRepository::record_message(
        &mut transaction,
        key,
        &SemanticDigest::from_storage(1, [23; 32]).expect("digest"),
        None,
    )
    .await
    .expect("record parent");
    EffectIntentRepository::reconcile(&mut transaction, key, std::slice::from_ref(&intent), false)
        .await
        .expect("record notification intent");
    transaction.commit().await.expect("commit parent");
    attach_notification_candidate_lineage_for_test(
        &fixture.uow,
        key,
        &intent.semantic_key(),
        candidate,
    )
    .await
    .expect("attach candidate through owner path");
    key
}

async fn install_second_target_failure(fixture: &IngressFixture) {
    match fixture.db.driver() {
        DatabaseDriver::Sqlite => {
            fixture
                .execute(
                    "CREATE TRIGGER fail_second_t1_target BEFORE INSERT ON notification_outbox \
                 WHEN NEW.node = 'node-two' BEGIN SELECT RAISE(FAIL, 'fail second target'); END",
                    (),
                )
                .await;
        }
        DatabaseDriver::Postgres => {
            fixture.execute(
                "CREATE FUNCTION fail_second_t1_target() RETURNS trigger LANGUAGE plpgsql AS $$ \
                 BEGIN IF NEW.node = 'node-two' THEN RAISE EXCEPTION 'fail second target'; \
                 END IF; RETURN NEW; END $$",
                (),
            ).await;
            fixture
                .execute(
                    "CREATE TRIGGER fail_second_t1_target BEFORE INSERT ON notification_outbox \
                 FOR EACH ROW EXECUTE FUNCTION fail_second_t1_target()",
                    (),
                )
                .await;
        }
    }
}

async fn remove_second_target_failure(fixture: &IngressFixture) {
    match fixture.db.driver() {
        DatabaseDriver::Sqlite => {
            fixture
                .execute("DROP TRIGGER fail_second_t1_target", ())
                .await
        }
        DatabaseDriver::Postgres => {
            fixture
                .execute(
                    "DROP TRIGGER fail_second_t1_target ON notification_outbox",
                    (),
                )
                .await;
            fixture
                .execute("DROP FUNCTION fail_second_t1_target()", ())
                .await;
        }
    }
}

async fn rollback_fanout_coalescing_and_late_parent(fixture: IngressFixture) {
    let outbox = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("outbox");
    let first = candidate("drain-atomic-first");
    outbox
        .insert_candidate(&first)
        .await
        .expect("first candidate");
    attach_parent(&fixture, &first).await;
    let deps = DrainDependencies::new(&first).await;
    install_second_target_failure(&fixture).await;
    let error = deps
        .drain(&outbox)
        .await
        .expect_err("second target must fail");
    assert!(error.to_string().contains("fail second target"), "{error}");
    assert_eq!(fixture.count("notification_outbox").await, 0);
    assert_eq!(fixture.count("notification_outbox_lineage").await, 0);
    assert_eq!(
        fixture
            .count("notification_candidates WHERE outboxed_at_ms IS NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    remove_second_target_failure(&fixture).await;
    assert_eq!(deps.drain(&outbox).await.expect("retry full fanout"), 1);
    assert_eq!(fixture.count("notification_outbox").await, 2);
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        2
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        2
    );

    let second = candidate("drain-atomic-second");
    outbox
        .insert_candidate(&second)
        .await
        .expect("second candidate");
    attach_parent(&fixture, &second).await;
    assert_eq!(
        deps.drain(&outbox)
            .await
            .expect("coalesce second candidate"),
        1
    );
    let jobs = outbox.pending_outbox_jobs().await.expect("coalesced jobs");
    assert_eq!(jobs.len(), 2);
    assert!(jobs.iter().all(|job| job.message_count() == 2));
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        4
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        4
    );
    attach_parent(&fixture, &first).await;
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        6
    );
    drop(outbox);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_fanout_rollback_coalescing_and_late_parent() {
    rollback_fanout_coalescing_and_late_parent(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_fanout_rollback_coalescing_and_late_parent() {
    if let Some(fixture) = IngressFixture::postgres("t1_fanout_atomic").await {
        rollback_fanout_coalescing_and_late_parent(fixture).await;
    }
}

async fn malformed_candidate_disposition(fixture: IngressFixture) {
    let outbox = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("outbox");
    let malformed = candidate("drain-malformed-parent");
    outbox
        .insert_candidate(&malformed)
        .await
        .expect("candidate");
    attach_parent(&fixture, &malformed).await;
    fixture
        .execute(
            "UPDATE notification_candidates SET sender_jid = ? WHERE stanza_id = ?",
            waddle_server::db_params!["invalid sender", "drain-malformed-parent"],
        )
        .await;
    let deps = DrainDependencies::new(&malformed).await;
    assert_eq!(
        deps.drain(&outbox)
            .await
            .expect("discard malformed candidate"),
        0
    );
    assert_eq!(
        fixture
            .count("notification_candidates WHERE outboxed_at_ms IS NOT NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        0
    );
    assert_eq!(fixture.count("notification_outbox").await, 0);

    let bad_id = candidate("drain-malformed-id");
    outbox
        .insert_candidate(&bad_id)
        .await
        .expect("candidate with bad id");
    attach_parent(&fixture, &bad_id).await;
    fixture.execute("UPDATE notification_candidates SET sender_jid = ?, delivery_id = ? WHERE stanza_id = ?",
        waddle_server::db_params!["invalid sender", "not-a-uuid", "drain-malformed-id"]).await;
    assert!(matches!(
        deps.drain(&outbox).await,
        Err(NotificationOutboxError::InvalidDeliveryIdentity)
    ));
    assert_eq!(
        fixture
            .count("notification_candidates WHERE outboxed_at_ms IS NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    drop(outbox);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_malformed_candidate_settles_valid_identity_and_rejects_invalid_id() {
    malformed_candidate_disposition(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_malformed_candidate_settles_valid_identity_and_rejects_invalid_id() {
    if let Some(fixture) = IngressFixture::postgres("t1_malformed").await {
        malformed_candidate_disposition(fixture).await;
    }
}

#[tokio::test]
async fn postgres_parent_contention_and_epoch_wait_leave_candidate_pending() {
    let Some(fixture) = IngressFixture::postgres("t1_parent_epoch").await else {
        return;
    };
    let outbox = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("outbox");
    let item = candidate("drain-parent-epoch");
    outbox.insert_candidate(&item).await.expect("candidate");
    let parent = attach_parent(&fixture, &item).await;
    let deps = DrainDependencies::new(&item).await;
    let mut held = fixture.uow.begin().await.expect("hold canonical parent");
    assert!(CanonicalMessageRepository::lock(&mut held, parent)
        .await
        .expect("lock parent"));
    let error = tokio::time::timeout(Duration::from_secs(2), deps.drain(&outbox))
        .await
        .expect("parent conflict must not wait")
        .expect_err("held parent rejects drain");
    assert!(
        matches!(
            &error,
            NotificationOutboxError::Ancestry(IngressUowError::Database {
                retry_class: DbRetryClass::CanonicalLockContention,
            })
        ),
        "{error:?}"
    );
    assert_eq!(fixture.count("notification_outbox").await, 0);
    assert_eq!(
        fixture
            .count("notification_candidates WHERE outboxed_at_ms IS NULL")
            .await,
        1
    );
    held.commit().await.expect("release parent");

    // Hold the candidate row before starting the drain. Observing the drain
    // waiting on epoch while this lock is held proves it has not waited for
    // or modified the candidate first.
    let mut candidate_probe = fixture.db.begin().await.expect("candidate lock probe");
    let rows = candidate_probe
        .query(
            "SELECT stanza_id FROM notification_candidates WHERE stanza_id = ? FOR UPDATE NOWAIT",
            waddle_server::db_params!["drain-parent-epoch"],
        )
        .await
        .expect("hold candidate before starting drain");
    drop(rows);

    let mut epoch = fixture.db.begin().await.expect("epoch blocker");
    let mut rows = epoch
        .query("SELECT pg_backend_pid()", ())
        .await
        .expect("blocker pid");
    let pid: i32 = rows
        .next()
        .await
        .expect("pid query")
        .expect("pid row")
        .get(0)
        .expect("pid");
    drop(rows);
    let rows = epoch
        .query(
            "SELECT epoch FROM ingress_protocol_epoch WHERE id = 1 FOR UPDATE",
            (),
        )
        .await
        .expect("hold epoch");
    drop(rows);
    let mut drain = Box::pin(deps.drain(&outbox));
    let waiting = async {
        loop {
            let query = format!("pg_stat_activity WHERE {pid} = ANY(pg_blocking_pids(pid)) \
                AND wait_event_type = 'Lock' AND query LIKE '%SELECT epoch FROM ingress_protocol_epoch%'");
            if fixture.count(&query).await > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::select! {
        _ = waiting => {},
        result = &mut drain => panic!("drain ended before epoch waiter was observed: {result:?}"),
    }
    candidate_probe
        .rollback()
        .await
        .expect("release candidate probe");
    let failed = tokio::time::timeout(Duration::from_secs(2), &mut drain)
        .await
        .expect("bounded epoch wait");
    assert!(
        matches!(
            &failed,
            Err(NotificationOutboxError::Ancestry(
                IngressUowError::Substrate(
                    waddle_server::ingress_substrate::IngressSubstrateError::Timeout
                )
            ))
        ),
        "held epoch must return the existing typed timeout: {failed:?}"
    );
    drop(drain);
    epoch.rollback().await.expect("release epoch");
    assert_eq!(
        deps.drain(&outbox)
            .await
            .expect("retry after epoch release"),
        1
    );
    assert_eq!(fixture.count("notification_outbox").await, 2);
    drop(outbox);
    fixture.close().await;
}

async fn foundation_absent_drain(database: waddle_server::db::Database) {
    let outbox = NotificationOutboxStore::try_new(database.clone())
        .await
        .expect("standalone outbox");
    let query = match database.driver() {
        DatabaseDriver::Sqlite => "SELECT NOT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ingress_effect_descendants')",
        DatabaseDriver::Postgres => "SELECT pg_catalog.to_regclass('ingress_effect_descendants') IS NULL",
    };
    let mut rows = database
        .guard()
        .await
        .expect("foundation probe")
        .query(query, ())
        .await
        .expect("probe foundation");
    let absent: bool = rows
        .next()
        .await
        .expect("probe row")
        .expect("exists")
        .get(0)
        .expect("absent");
    assert!(absent, "control must exercise a Foundation-absent store");
    drop(rows);
    if database.driver() == DatabaseDriver::Postgres {
        let guard = database.guard().await.expect("timeout trigger guard");
        guard.execute(
            "CREATE FUNCTION verify_t1_standalone_timeouts() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN IF current_setting('lock_timeout')::interval <> interval '100 milliseconds' \
             OR current_setting('statement_timeout')::interval <> interval '250 milliseconds' \
             THEN RAISE EXCEPTION 'standalone ancestry probe timeouts changed'; END IF; RETURN NEW; END $$",
            (),
        ).await.expect("create timeout check");
        guard.execute(
            "CREATE TRIGGER verify_t1_standalone_timeouts BEFORE INSERT ON notification_outbox \
             FOR EACH ROW EXECUTE FUNCTION verify_t1_standalone_timeouts()",
            (),
        ).await.expect("install timeout check");
    }
    let item = candidate("standalone-no-foundation");
    outbox.insert_candidate(&item).await.expect("candidate");
    let deps = DrainDependencies::new(&item).await;
    assert_eq!(deps.drain(&outbox).await.expect("standalone drain"), 1);
    assert_eq!(outbox.pending_outbox_jobs().await.expect("jobs").len(), 2);
}

#[tokio::test]
async fn sqlite_foundation_absent_standalone_drain() {
    let database = waddle_server::db::Database::in_memory("t1-no-foundation")
        .await
        .expect("database");
    foundation_absent_drain(database).await;
}

#[tokio::test]
async fn postgres_foundation_absent_standalone_drain() {
    use waddle_server::db::{Database, DatabaseConfig};
    let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let admin = sqlx::PgPool::connect(&database_url).await.expect("admin");
    let schema = format!("waddle_t1_standalone_{}", uuid::Uuid::new_v4().simple());
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
        .append_pair(
            "options",
            &format!("-c search_path={schema} -c lock_timeout=2s -c statement_timeout=3s"),
        );
    let database = Database::from_config(
        "t1-no-foundation",
        &DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string()),
    )
    .await
    .expect("database");
    foundation_absent_drain(database).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop schema");
    admin.close().await;
}

async fn exhausted_policy_settles_candidate(fixture: IngressFixture) {
    let outbox = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("outbox");
    let item = candidate("drain-exhausted-policy");
    outbox.insert_candidate(&item).await.expect("candidate");
    attach_parent(&fixture, &item).await;
    fixture
        .execute(
            "UPDATE notification_candidates SET policy_error_count = 47 WHERE stanza_id = ?",
            waddle_server::db_params!["drain-exhausted-policy"],
        )
        .await;
    let deps = DrainDependencies::new(&item).await;
    let outcome = outbox
        .drain_pending_candidates_into_outbox(
            &deps.push,
            &crate::support::FailingBlockingStorage,
            &deps.settings,
            drain_deps_with_noop_activity(
                &deps.room_policy,
                &NoopDndReader,
                noop_activity_reader(),
            ),
            &bare("push.example.com"),
            16,
        )
        .await
        .expect("terminal policy disposition");
    assert_eq!(outcome, 1);
    assert_eq!(fixture.count("notification_candidates WHERE policy_error_count = 48 AND outboxed_at_ms IS NOT NULL AND suppressed_reason = 'policy_retries_exhausted'").await, 1);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        0
    );
    assert_eq!(fixture.count("notification_outbox").await, 0);
    drop(outbox);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_exhausted_policy_settles_candidate_ancestry() {
    exhausted_policy_settles_candidate(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_exhausted_policy_settles_candidate_ancestry() {
    if let Some(fixture) = IngressFixture::postgres("t1_exhausted_policy").await {
        exhausted_policy_settles_candidate(fixture).await;
    }
}
