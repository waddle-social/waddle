use super::*;
use crate::db::{DatabaseConfig, DatabaseDriver};
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::{
    CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
};
use std::sync::Arc;
use waddle_xmpp::ingress::{
    EffectMessageIdentity, IngressEffectIntent, MessageKey, SemanticDigest,
};

async fn scalar(db: &Database, sql: &str) -> i64 {
    let conn = db.guard().await.expect("schema inspection");
    let mut rows = conn.query(sql, ()).await.expect("scalar query");
    rows.next()
        .await
        .expect("scalar row")
        .expect("scalar value")
        .get(0)
        .expect("integer")
}

async fn independent_database(fixture: &IngressFixture) -> Database {
    let mut config = DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url());
    config.pool_size = 2;
    Database::from_config("independent-outbox-boot", &config)
        .await
        .expect("independent pool")
}

async fn retain_unknown_work(fixture: &IngressFixture) {
    let recipient = "alice@example.com".parse::<BareJid>().expect("recipient");
    let intent = IngressEffectIntent::RouteDirect {
        recipient: recipient.clone(),
        fanout: vec![],
        route_identity: EffectMessageIdentity::capture_ordinal(1),
        prepared: None,
    };
    let key = MessageKey::new();
    let frozen = uuid::Uuid::new_v4();
    let legacy = uuid::Uuid::new_v4();
    let mut canonical = fixture.uow.begin().await.expect("canonical custody");
    CanonicalMessageRepository::record_message(
        &mut canonical,
        key,
        &SemanticDigest::from_storage(1, [73; 32]).expect("digest"),
        None,
    )
    .await
    .expect("parent");
    EffectIntentRepository::reconcile(&mut canonical, key, std::slice::from_ref(&intent), false)
        .await
        .expect("recorded intent");
    for id in [frozen, legacy] {
        EffectDescendantRepository::attach(&mut canonical, key, &intent.semantic_key(), id)
            .await
            .expect("pending descendant");
    }
    canonical.commit().await.expect("custody committed");
    let payload = Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build();
    let options = Element::builder("x", waddle_xmpp::xep::NS_DATA_FORMS).build();
    let context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS).build();
    let mut tx = fixture.db.begin_immediate().await.expect("unknown work");
    for (id, node, xml, form) in [
        (
            frozen,
            "frozen",
            Some(String::from(&payload)),
            Some(String::from(&options)),
        ),
        (legacy, "legacy", None, None),
    ] {
        tx.execute("INSERT INTO notification_outbox (job_id,recipient_bare_jid,push_service_jid,node,conversation_jid,sender_jid,sender_jids,class,message_count,context_xml,summary_sender_jid,summary_body,approved_payload_xml,approved_publish_options_xml,queue_acceptance_may_exist,status,created_at_ms,updated_at_ms) VALUES(?,?,'push.example.com',?,'bob@example.com','bob@example.com/phone','[\"bob@example.com/phone\"]','dm',3,?,'bob@example.com','retained summary',?,?,1,'queued',1,1)",crate::db_params![id.to_string(),recipient.to_string(),node,String::from(&context),xml,form]).await.expect("retained unknown row");
        tx.execute(
            "INSERT INTO notification_outbox_lineage(candidate_delivery_id,job_id) VALUES(?,?)",
            crate::db_params![uuid::Uuid::new_v4().to_string(), id.to_string()],
        )
        .await
        .expect("unsettled lineage");
    }
    tx.commit().await.expect("unknown work committed");
}

#[derive(Debug, PartialEq, Eq)]
struct RetainedJob {
    id: String,
    context: String,
    count: i64,
    payload: Option<String>,
    options: Option<String>,
    summary: String,
}

async fn snapshot(db: &Database) -> Vec<RetainedJob> {
    let conn = db.guard().await.expect("custody snapshot");
    let mut rows = conn.query("SELECT job_id,context_xml,message_count,approved_payload_xml,approved_publish_options_xml,summary_body FROM notification_outbox ORDER BY job_id",()).await.expect("frozen rows");
    let mut data = Vec::new();
    while let Some(row) = rows.next().await.expect("snapshot row") {
        data.push(RetainedJob {
            id: row.get(0).expect("id"),
            context: row.get(1).expect("context"),
            count: row.get(2).expect("count"),
            payload: row.get(3).expect("payload"),
            options: row.get(4).expect("options"),
            summary: row.get(5).expect("summary"),
        });
    }
    data
}

async fn assert_custody(fixture: &IngressFixture, before: &[RetainedJob]) {
    assert_eq!(
        snapshot(&fixture.db).await,
        before,
        "startup preserves exact job identity and frozen bytes"
    );
    assert_eq!(
        fixture
            .count("notification_outbox WHERE status='queued' AND queue_acceptance_may_exist=1")
            .await,
        2
    );
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
    assert_eq!(fixture.count("ingress_messages").await, 1);
}

async fn stable_current_boot(fixture: IngressFixture) {
    let initial = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("initial schema");
    retain_unknown_work(&fixture).await;
    let before = snapshot(&fixture.db).await;
    let sql = match fixture.db.driver() {
        DatabaseDriver::Sqlite => {
            "SELECT rowid FROM sqlite_schema WHERE name='idx_notification_outbox_queued_coalesce'"
        }
        DatabaseDriver::Postgres => {
            "SELECT to_regclass('idx_notification_outbox_queued_coalesce')::oid::bigint"
        }
    };
    let original = scalar(&fixture.db, sql).await;
    let restarted = NotificationOutboxStore::try_new(independent_database(&fixture).await)
        .await
        .expect("current boot");
    assert_custody(&fixture, &before).await;
    assert_eq!(
        scalar(&fixture.db, sql).await,
        original,
        "an already-current boot must not drop/recreate schema objects"
    );
    drop(restarted);
    drop(initial);
    fixture.close().await;
}

#[derive(Clone, Copy)]
enum SchemaState {
    Fresh,
    Current,
    Stale,
}

async fn stale_schema(fixture: &IngressFixture) {
    let conn = fixture.db.guard().await.expect("old schema");
    if fixture.db.driver() == DatabaseDriver::Postgres {
        conn.execute(
            "ALTER TABLE notification_outbox DROP CONSTRAINT notification_outbox_class_check",
            (),
        )
        .await
        .expect("old class check");
        conn.execute("ALTER TABLE notification_outbox ADD CONSTRAINT notification_outbox_class_check CHECK(class IN('dm','personal_mention'))",()).await.expect("legacy class values");
    } else {
        conn.execute(
            "ALTER TABLE notification_outbox RENAME TO notification_outbox_pre_upgrade",
            (),
        )
        .await
        .expect("old table");
        conn.execute(
            &notification_outbox_table_sql("INTEGER", false).replace(
                NOTIFICATION_OUTBOX_CLASS_CHECK_SQL,
                "class IN ('dm','personal_mention')",
            ),
            (),
        )
        .await
        .expect("legacy class schema");
        conn.execute(
            "INSERT INTO notification_outbox SELECT * FROM notification_outbox_pre_upgrade",
            (),
        )
        .await
        .expect("retained work copied");
        conn.execute("DROP TABLE notification_outbox_pre_upgrade", ())
            .await
            .expect("remove setup copy");
    }
    conn.execute(
        "DROP INDEX IF EXISTS idx_notification_outbox_queued_coalesce",
        (),
    )
    .await
    .expect("old coalescing definition");
    conn.execute("CREATE UNIQUE INDEX idx_notification_outbox_queued_coalesce ON notification_outbox(recipient_bare_jid,push_service_jid,node,conversation_jid,thread_id,class) WHERE status='queued' AND approved_payload_xml IS NULL",()).await.expect("legacy index predicate");
}

async fn concurrent_boot(fixture: IngressFixture, state: SchemaState) {
    let retained = if matches!(state, SchemaState::Fresh) {
        None
    } else {
        NotificationOutboxStore::try_new(fixture.db.clone())
            .await
            .expect("existing schema");
        retain_unknown_work(&fixture).await;
        if matches!(state, SchemaState::Stale) {
            stale_schema(&fixture).await;
        }
        Some(snapshot(&fixture.db).await)
    };
    let workers = 8;
    let barrier = Arc::new(tokio::sync::Barrier::new(workers));
    let mut tasks = Vec::new();
    for _ in 0..workers {
        let database = independent_database(&fixture).await;
        let start = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            start.wait().await;
            NotificationOutboxStore::try_new(database).await
        }));
    }
    let results = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut failures = Vec::new();
        for task in tasks {
            if let Err(error) = task.await.expect("initializer task") {
                failures.push(error.to_string());
            }
        }
        failures
    })
    .await
    .expect("concurrent initialization finishes within startup budget");
    if let Some(before) = retained {
        assert_custody(&fixture, &before).await;
    }
    assert!(
        results.is_empty(),
        "independent initializers must all succeed: {results:#?}"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_current_boot_preserves_coalescing_index_catalog_identity() {
    stable_current_boot(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_current_boot_preserves_coalescing_index_oid() {
    if let Some(f) = IngressFixture::postgres("current_boot_oid").await {
        stable_current_boot(f).await;
    }
}
#[tokio::test]
async fn postgres_concurrent_fresh_outbox_initializers_succeed() {
    if let Some(f) = IngressFixture::postgres("fresh_boot_race").await {
        concurrent_boot(f, SchemaState::Fresh).await;
    }
}
#[tokio::test]
async fn postgres_concurrent_current_boots_preserve_custody() {
    if let Some(f) = IngressFixture::postgres("current_boot_race").await {
        concurrent_boot(f, SchemaState::Current).await;
    }
}
#[tokio::test]
async fn postgres_concurrent_stale_upgrade_preserves_custody() {
    if let Some(f) = IngressFixture::postgres("stale_boot_race").await {
        concurrent_boot(f, SchemaState::Stale).await;
    }
}

async fn tagged_database(fixture: &IngressFixture, pool_size: u32) -> (Database, String) {
    let tag = format!("outbox_startup_{}", uuid::Uuid::new_v4().simple());
    let mut url = url::Url::parse(fixture.db.database_url()).expect("fixture URL");
    url.query_pairs_mut().append_pair("application_name", &tag);
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string());
    config.pool_size = pool_size;
    (
        Database::from_config("tagged-startup-pool", &config)
            .await
            .expect("exact tagged pool"),
        tag,
    )
}

#[derive(Clone, Copy)]
enum IndexScenario {
    Missing,
    LiteralCase,
    Collation,
    Rollback,
}

async fn index_upgrade_controls(fixture: IngressFixture, scenario: IndexScenario) {
    let store = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("schema");
    retain_unknown_work(&fixture).await;
    let before = snapshot(&fixture.db).await;
    let conn = fixture.db.guard().await.expect("index fixture");
    {
        let mut setup = fixture
            .db
            .begin_immediate()
            .await
            .expect("one physical setup connection");
        setup
            .execute("DROP INDEX idx_notification_outbox_queued_coalesce", ())
            .await
            .expect("old index removed once");
        match scenario {
            IndexScenario::Missing => {}
            IndexScenario::LiteralCase => {
                setup.execute("CREATE UNIQUE INDEX idx_notification_outbox_queued_coalesce ON notification_outbox(recipient_bare_jid,push_service_jid,node,conversation_jid,thread_id,class) WHERE status='QUEUED' AND approved_payload_xml IS NULL AND queue_acceptance_may_exist=0",()).await.expect("wrong literal case");
            }
            IndexScenario::Collation => {
                setup.execute("CREATE UNIQUE INDEX idx_notification_outbox_queued_coalesce ON notification_outbox(recipient_bare_jid,push_service_jid,node,conversation_jid,thread_id COLLATE NOCASE,class) WHERE status='queued' AND approved_payload_xml IS NULL AND queue_acceptance_may_exist=0",()).await.expect("incorrect thread collation");
            }
            IndexScenario::Rollback => {
                setup.execute("CREATE INDEX idx_notification_outbox_queued_coalesce ON notification_outbox(job_id)",()).await.expect("wrong-key index");
            }
        }
        setup
            .commit()
            .await
            .expect("complete setup before verification and cleanup");
    }
    match scenario {
        IndexScenario::Missing => {
            store
                .migrate_coalescing_index()
                .await
                .expect("repair missing index");
        }
        IndexScenario::LiteralCase => {
            store
                .migrate_coalescing_index()
                .await
                .expect("repair case-sensitive predicate");
            let sql=match fixture.db.driver() {DatabaseDriver::Postgres=>"SELECT pg_get_expr(indpred,indrelid) FROM pg_index WHERE indexrelid=to_regclass('idx_notification_outbox_queued_coalesce')",DatabaseDriver::Sqlite=>"SELECT sql FROM sqlite_master WHERE name='idx_notification_outbox_queued_coalesce'"};
            let mut rows = conn.query(sql, ()).await.expect("corrected predicate");
            let definition: String = rows
                .next()
                .await
                .expect("row")
                .expect("index")
                .get(0)
                .expect("definition");
            assert!(definition.contains("'queued'"));
            assert!(!definition.contains("'QUEUED'"));
        }
        IndexScenario::Collation => {
            store
                .migrate_coalescing_index()
                .await
                .expect("restore binary thread identity");
            let context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS).build();
            for thread in ["Branch", "branch"] {
                conn.execute("INSERT INTO notification_outbox(job_id,recipient_bare_jid,push_service_jid,node,conversation_jid,sender_jid,sender_jids,thread_id,class,message_count,context_xml,queue_acceptance_may_exist,status,created_at_ms,updated_at_ms) VALUES(?,'alice@example.com','push.example.com','case-control','bob@example.com','bob@example.com/phone','[\"bob@example.com/phone\"]',?,'dm',1,?,0,'queued',1,1)",crate::db_params![uuid::Uuid::new_v4().to_string(),thread,String::from(&context)]).await.expect("case-distinct threads coexist");
            }
            conn.execute(
                "DELETE FROM notification_outbox WHERE node='case-control'",
                (),
            )
            .await
            .expect("remove only collation fixture rows");
        }
        IndexScenario::Rollback => {
            let oid_sql=match fixture.db.driver() { DatabaseDriver::Postgres=>"SELECT to_regclass('idx_notification_outbox_queued_coalesce')::oid::bigint",DatabaseDriver::Sqlite=>"SELECT rowid FROM sqlite_master WHERE name='idx_notification_outbox_queued_coalesce'"};
            let original = scalar(&fixture.db, oid_sql).await;
            let context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS).build();
            for _ in 0..2 {
                conn.execute("INSERT INTO notification_outbox(job_id,recipient_bare_jid,push_service_jid,node,conversation_jid,sender_jid,sender_jids,class,message_count,context_xml,queue_acceptance_may_exist,status,created_at_ms,updated_at_ms) VALUES(?,'alice@example.com','push.example.com','duplicate','bob@example.com','bob@example.com/phone','[\"bob@example.com/phone\"]','dm',1,?,0,'queued',1,1)",crate::db_params![uuid::Uuid::new_v4().to_string(),String::from(&context)]).await.expect("known unaccepted duplicate heads");
            }
            assert!(
                store.migrate_coalescing_index().await.is_err(),
                "new unique index rejects duplicate heads"
            );
            assert_eq!(
                scalar(&fixture.db, oid_sql).await,
                original,
                "failed migration restores the old index atomically"
            );
            if fixture.db.driver() == DatabaseDriver::Postgres {
                assert!(store
                    .initialize_postgres_schema(OUTBOX_SCHEMA_GATE_WAIT)
                    .await
                    .is_err());
                assert_gate_free(&fixture).await;
            }
            conn.execute("DELETE FROM notification_outbox WHERE node='duplicate'", ())
                .await
                .expect("remove only fixture conflict");
            store
                .migrate_coalescing_index()
                .await
                .expect("retry atomic upgrade");
        }
    }
    assert_custody(&fixture, &before).await;
    drop(conn);
    drop(store);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_index_missing_stale_case_and_failure_rollback_preserve_custody() {
    for scenario in [
        IndexScenario::Missing,
        IndexScenario::LiteralCase,
        IndexScenario::Collation,
        IndexScenario::Rollback,
    ] {
        index_upgrade_controls(IngressFixture::sqlite().await, scenario).await;
    }
}
#[tokio::test]
async fn postgres_index_missing_stale_case_and_failure_rollback_preserve_custody() {
    for scenario in [
        IndexScenario::Missing,
        IndexScenario::LiteralCase,
        IndexScenario::Rollback,
    ] {
        let Some(f) = IngressFixture::postgres("index_upgrade_atomic").await else {
            return;
        };
        index_upgrade_controls(f, scenario).await;
    }
}

async fn wait_scalar(db: &Database, sql: &str, expected: i64) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if scalar(db, sql).await == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("observable database barrier reached");
}

async fn detached_gate(fixture: &IngressFixture) -> sqlx::PgConnection {
    let mut connection = fixture
        .db
        .detached_postgres_startup_connection()
        .await
        .expect("physical gate probe");
    sqlx::query("SET lock_timeout='30s'")
        .execute(&mut connection)
        .await
        .expect("bounded probe wait");
    sqlx::query("SELECT pg_advisory_lock($1,$2)")
        .bind(OUTBOX_SCHEMA_GATE.0)
        .bind(OUTBOX_SCHEMA_GATE.1)
        .execute(&mut connection)
        .await
        .expect("gate acquired");
    connection
}

async fn assert_gate_free(fixture: &IngressFixture) {
    use sqlx::Connection;
    let mut connection = fixture
        .db
        .detached_postgres_startup_connection()
        .await
        .expect("gate probe");
    sqlx::query("SET lock_timeout='10s'")
        .execute(&mut connection)
        .await
        .expect("bounded cleanup probe");
    sqlx::query("SELECT pg_advisory_lock($1,$2)")
        .bind(OUTBOX_SCHEMA_GATE.0)
        .bind(OUTBOX_SCHEMA_GATE.1)
        .execute(&mut connection)
        .await
        .expect("completed startup does not leak its gate");
    connection.close().await.expect("close physical probe");
}

#[tokio::test]
async fn postgres_one_slot_pool_completes_schema_initialization_without_leaked_gate() {
    let Some(f) = IngressFixture::postgres("one_slot_startup").await else {
        return;
    };
    let (database, tag) = tagged_database(&f, 1).await;
    let store = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        NotificationOutboxStore::try_new(database),
    )
    .await
    .expect("detached gate returns the only pool slot")
    .expect("schema startup");
    wait_scalar(
        &f.db,
        &format!("SELECT COUNT(*) FROM pg_stat_activity WHERE application_name='{tag}'"),
        1,
    )
    .await;
    assert_gate_free(&f).await;
    drop(store);
    f.close().await;
}

#[tokio::test]
async fn postgres_gate_timeout_closes_detached_session_and_preserves_custody() {
    use sqlx::Connection;
    let Some(f) = IngressFixture::postgres("gate_timeout").await else {
        return;
    };
    NotificationOutboxStore::try_new(f.db.clone())
        .await
        .expect("schema");
    let (database, tag) = tagged_database(&f, 1).await;
    let store = NotificationOutboxStore { db: database };
    retain_unknown_work(&f).await;
    let before = snapshot(&f.db).await;
    let held = detached_gate(&f).await;
    assert!(store
        .initialize_postgres_schema(std::time::Duration::from_millis(100))
        .await
        .is_err());
    // Only this pool's replacement session remains after the detached
    // waiter closes; unrelated fixture schemas may legitimately be waiting.
    store
        .query("SELECT 1", ())
        .await
        .expect("replacement pool slot available");
    wait_scalar(
        &f.db,
        &format!("SELECT COUNT(*) FROM pg_stat_activity WHERE application_name='{tag}'"),
        1,
    )
    .await;
    held.close().await.expect("release held gate");
    assert_gate_free(&f).await;
    assert_custody(&f, &before).await;
    drop(store);
    f.close().await;
}

#[tokio::test]
async fn postgres_canceled_caller_keeps_gate_until_outstanding_schema_work_finishes() {
    let Some(f) = IngressFixture::postgres("canceled_schema").await else {
        return;
    };
    let store = NotificationOutboxStore::try_new(f.db.clone())
        .await
        .expect("schema");
    retain_unknown_work(&f).await;
    let before = snapshot(&f.db).await;
    let mut held =
        f.db.begin_immediate()
            .await
            .expect("hold table before schema read");
    held.execute(
        "LOCK TABLE notification_candidates IN ACCESS EXCLUSIVE MODE",
        (),
    )
    .await
    .expect("hold actual relation");
    let database = independent_database(&f).await;
    let caller = tokio::spawn(async move { NotificationOutboxStore::try_new(database).await });
    let blocked="SELECT COUNT(*) FROM pg_locks WHERE relation=to_regclass('notification_candidates') AND NOT granted";
    wait_scalar(&f.db, blocked, 1).await;
    let gate_owner = scalar(&f.db, &format!("SELECT pid::bigint FROM pg_locks WHERE locktype='advisory' AND classid={}::oid AND objid={}::oid AND granted", OUTBOX_SCHEMA_GATE.0, OUTBOX_SCHEMA_GATE.1)).await;
    caller.abort();
    match caller.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("caller must be canceled"),
    }
    let gate=format!("SELECT COUNT(*) FROM pg_locks WHERE locktype='advisory' AND classid={}::oid AND objid={}::oid AND granted AND pid={gate_owner}",OUTBOX_SCHEMA_GATE.0,OUTBOX_SCHEMA_GATE.1);
    assert_eq!(
        scalar(&f.db, &gate).await,
        1,
        "owned schema task retains gate after caller cancellation"
    );
    held.rollback().await.expect("let outstanding DDL complete");
    wait_scalar(&f.db, &gate, 0).await;
    wait_scalar(
        &f.db,
        &format!("SELECT COUNT(*) FROM pg_stat_activity WHERE pid={gate_owner}"),
        0,
    )
    .await;
    assert_gate_free(&f).await;
    assert_custody(&f, &before).await;
    drop(store);
    f.close().await;
}

#[tokio::test]
async fn postgres_resolved_index_namespace_preserves_same_name_decoy() {
    let Some(f) = IngressFixture::postgres("index_namespace").await else {
        return;
    };
    let store = NotificationOutboxStore::try_new(f.db.clone())
        .await
        .expect("actual schema");
    let mut tx = f.db.begin_immediate().await.expect("catalog control");
    let mut rows = tx
        .query("SELECT current_schema()", ())
        .await
        .expect("actual namespace");
    let actual: String = rows
        .next()
        .await
        .expect("row")
        .expect("namespace")
        .get(0)
        .expect("name");
    drop(rows);
    let decoy = format!("decoy_{}", uuid::Uuid::new_v4().simple());
    tx.execute(&format!("CREATE SCHEMA {decoy}"), ())
        .await
        .expect("decoy schema");
    tx.execute(
        &format!("CREATE TABLE {decoy}.other_table(job_id TEXT)"),
        (),
    )
    .await
    .expect("foreign relation");
    tx.execute(
        &format!(
            "CREATE INDEX idx_notification_outbox_queued_coalesce ON {decoy}.other_table(job_id)"
        ),
        (),
    )
    .await
    .expect("decoy index");
    tx.execute(&format!("SET LOCAL search_path={decoy},{actual}"), ())
        .await
        .expect("decoy comes first");
    let mut rows = tx
        .query(
            "SELECT to_regclass('idx_notification_outbox_queued_coalesce')::oid::bigint",
            (),
        )
        .await
        .expect("decoy OID");
    let oid: i64 = rows
        .next()
        .await
        .expect("row")
        .expect("OID")
        .get(0)
        .expect("number");
    drop(rows);
    migrate_coalescing_index_tx(&mut tx)
        .await
        .expect("resolved actual table namespace");
    let mut rows = tx
        .query(
            "SELECT to_regclass('idx_notification_outbox_queued_coalesce')::oid::bigint",
            (),
        )
        .await
        .expect("unchanged decoy");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("OID")
            .get::<i64>(0)
            .expect("number"),
        oid
    );
    drop(rows);
    tx.rollback()
        .await
        .expect("remove only transactional decoy");
    let mut tx =
        f.db.begin_immediate()
            .await
            .expect("same-namespace foreign owner control");
    tx.execute("DROP INDEX idx_notification_outbox_queued_coalesce", ())
        .await
        .expect("make canonical name available");
    tx.execute("CREATE TABLE foreign_index_owner(job_id TEXT)", ())
        .await
        .expect("unrelated table");
    tx.execute(
        "CREATE INDEX idx_notification_outbox_queued_coalesce ON foreign_index_owner(job_id)",
        (),
    )
    .await
    .expect("name owned by other table");
    assert!(
        migrate_coalescing_index_tx(&mut tx).await.is_err(),
        "never delete another table's index"
    );
    let mut rows=tx.query("SELECT COUNT(*) FROM pg_index WHERE indexrelid=to_regclass('idx_notification_outbox_queued_coalesce') AND indrelid=to_regclass('foreign_index_owner')",()).await.expect("foreign ownership retained");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("number"),
        1
    );
    drop(rows);
    tx.rollback().await.expect("restore actual index");
    drop(store);
    f.close().await;
}
