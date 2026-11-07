//! Public PostgreSQL inbox-owner catalog and legacy recovery migration witness.

use std::time::Duration;

use sqlx::PgPool;
use waddle_server::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::inbox::DatabaseInboxStorage;

const MESSAGE_KEY_GATE: &str = "groupchat_notification_recovery.message_key";

fn scoped_url(base: &str, schema: &str, application: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| !matches!(key.as_ref(), "options" | "application_name"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"))
        .append_pair("application_name", application);
    url.to_string()
}

async fn wait_for<F, Fut>(mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if check().await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wait for PostgreSQL witness");
}

#[tokio::test]
async fn pg17_inbox_owner_fences_legacy_migration_on_the_mutating_backend() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let admin = PgPool::connect(&url)
        .await
        .expect("connect disposable cluster");
    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
        .fetch_one(&admin)
        .await
        .expect("read PostgreSQL version");
    assert!((170_000..180_000).contains(&version), "requires PG17");
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let schema = format!("inbox_fence_{suffix}");
    let application = format!("inbox_owner_{suffix}");
    for statement in [
        format!("CREATE SCHEMA {schema}"),
        format!(
            "CREATE TABLE {schema}.inbox_entries (\
             user_jid TEXT NOT NULL, partner_jid TEXT NOT NULL, thread_id TEXT NOT NULL DEFAULT '', \
             kind TEXT NOT NULL, last_stanza_id TEXT NOT NULL, last_updated INTEGER NOT NULL, \
             unread INTEGER NOT NULL DEFAULT 0, preview TEXT, thread_title TEXT, \
             reply_count INTEGER NOT NULL DEFAULT 0, author TEXT, \
             PRIMARY KEY (user_jid, partner_jid, thread_id))"
        ),
        format!(
            "CREATE TABLE {schema}.groupchat_notification_recovery (\
             recipient_bare_jid TEXT NOT NULL, room_jid TEXT NOT NULL, thread_id TEXT NOT NULL DEFAULT '', \
             stanza_id_by TEXT NOT NULL, stanza_id TEXT NOT NULL, sender_jid TEXT NOT NULL, \
             is_live_occupant INTEGER NOT NULL, room_members_only INTEGER NOT NULL, \
             created_at_ms BIGINT NOT NULL, \
             PRIMARY KEY (recipient_bare_jid, room_jid, thread_id, stanza_id_by, stanza_id))"
        ),
        format!(
            "INSERT INTO {schema}.groupchat_notification_recovery \
             (recipient_bare_jid, room_jid, stanza_id_by, stanza_id, sender_jid, \
              is_live_occupant, room_members_only, created_at_ms) \
             VALUES ('u@example.com', 'r@example.com', 'r@example.com', 'old', \
                     'r@example.com/a', 1, 0, 1)"
        ),
    ] {
        sqlx::query(&statement).execute(&admin).await.expect("prepare legacy schema");
    }
    let mut config = DatabaseConfig::new(
        DatabaseDriver::Postgres,
        scoped_url(&url, &schema, &application),
    );
    config.pool_size = 1;
    let db = Database::from_config("inbox owner fence witness", &config)
        .await
        .expect("one-slot owner database");

    let mut gate = admin.begin().await.expect("begin old gate holder");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext($1))")
        .bind(MESSAGE_KEY_GATE)
        .execute(&mut *gate)
        .await
        .expect("hold original one-argument hashtext gate");
    let mut blocker = admin.begin().await.expect("begin recovery table blocker");
    sqlx::query(&format!(
        "LOCK TABLE {schema}.groupchat_notification_recovery IN ACCESS EXCLUSIVE MODE"
    ))
    .execute(&mut *blocker)
    .await
    .expect("block recovery mutation");
    let task = tokio::spawn({
        let db = db.clone();
        async move { DatabaseInboxStorage::from_database(db).await }
    });
    wait_for(|| async {
        let retry: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
             WHERE application_name = $1 AND state = 'idle' \
               AND xact_start IS NULL AND query = 'ROLLBACK')",
        )
        .bind(&application)
        .fetch_one(&admin)
        .await
        .expect("observe gate rollback");
        let catalog_available: bool =
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                .fetch_one(&admin)
                .await
                .expect("probe catalog release");
        retry && catalog_available
    })
    .await;
    gate.commit().await.expect("release old gate");

    // The exact backend blocked on the recovery table must hold both the
    // catalog fence and the old signed-bigint hash gate, not a two-int gate.
    wait_for(|| async {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity a \
             WHERE a.application_name = $1 AND a.state = 'active' \
               AND a.xact_start IS NOT NULL AND a.wait_event_type = 'Lock' \
               AND a.query LIKE '%groupchat_notification_recovery%' \
               AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = a.pid \
                 AND l.locktype = 'advisory' AND l.granted AND l.objsubid = 1 \
                 AND l.classid::bigint = (($2::bigint >> 32) & 4294967295) \
                 AND l.objid::bigint = ($2::bigint & 4294967295) AND l.mode = 'ExclusiveLock') \
               AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = a.pid \
                 AND l.locktype = 'advisory' AND l.granted AND l.objsubid = 1 \
                 AND l.classid::bigint = ((pg_catalog.hashtext($3)::bigint >> 32) & 4294967295) \
                 AND l.objid::bigint = (pg_catalog.hashtext($3)::bigint & 4294967295) \
                 AND l.mode = 'ExclusiveLock'))",
        )
        .bind(&application)
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .bind(MESSAGE_KEY_GATE)
        .fetch_one(&admin)
        .await
        .expect("observe owner backend and advisory locks")
    })
    .await;
    task.abort();
    match task.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("blocked owner was not cancelled"),
    }
    blocker.rollback().await.expect("release recovery table");
    wait_for(|| async {
        sqlx::query_scalar::<_, bool>("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&admin)
            .await
            .expect("observe transaction release")
    })
    .await;
    let data_type: String = sqlx::query_scalar(
        "SELECT data_type FROM information_schema.columns \
         WHERE table_schema = $1 AND table_name = 'inbox_entries' AND column_name = 'last_updated'",
    )
    .bind(&schema)
    .fetch_one(&admin)
    .await
    .expect("inspect rolled-back widening");
    assert_eq!(data_type, "integer");
    let legacy_count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {schema}.groupchat_notification_recovery"
    ))
    .fetch_one(&admin)
    .await
    .expect("inspect legacy rows");
    assert_eq!(legacy_count, 1);
    let key_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = $1 \
         AND table_name = 'groupchat_notification_recovery' AND column_name = 'message_key')",
    )
    .bind(&schema)
    .fetch_one(&admin)
    .await
    .expect("inspect rolled-back message key");
    assert!(!key_exists);

    // A later index failure occurs after the conditional DELETE and key ADD.
    // The whole migration, including widening and deletion, must roll back.
    assert!(DatabaseInboxStorage::from_database(db.clone())
        .await
        .is_err());
    let after_failure: (i64, bool, bool, String) = sqlx::query_as(&format!(
        "SELECT (SELECT count(*) FROM {schema}.groupchat_notification_recovery), \
         EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = $1 \
           AND table_name = 'groupchat_notification_recovery' AND column_name = 'message_key'), \
         EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = $1 \
           AND table_name = 'groupchat_notification_recovery' \
           AND column_name = 'sender_can_broadcast_channel_mention'), \
         (SELECT data_type::text FROM information_schema.columns WHERE table_schema = $1 \
           AND table_name = 'inbox_entries' AND column_name = 'last_updated')"
    ))
    .bind(&schema)
    .fetch_one(&admin)
    .await
    .expect("inspect failed migration rollback");
    assert_eq!(after_failure, (1, false, false, "integer".to_owned()));
    sqlx::query(&format!(
        "ALTER TABLE {schema}.groupchat_notification_recovery ADD COLUMN completed_at_ms BIGINT"
    ))
    .execute(&admin)
    .await
    .expect("complete legacy recovery fixture");

    drop(
        DatabaseInboxStorage::from_database(db.clone())
            .await
            .expect("migrate legacy schema"),
    );
    let data_type: String = sqlx::query_scalar(
        "SELECT data_type FROM information_schema.columns \
         WHERE table_schema = $1 AND table_name = 'inbox_entries' AND column_name = 'last_updated'",
    )
    .bind(&schema)
    .fetch_one(&admin)
    .await
    .expect("inspect widened type");
    assert_eq!(data_type, "bigint");
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns \
         WHERE table_schema = $1 AND table_name = 'groupchat_notification_recovery' \
         AND column_name = 'message_key'",
    )
    .bind(&schema)
    .fetch_one(&admin)
    .await
    .expect("inspect new key");
    assert_eq!(nullable, "NO");
    let remaining: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {schema}.groupchat_notification_recovery"
    ))
    .fetch_one(&admin)
    .await
    .expect("count deleted legacy rows");
    assert_eq!(remaining, 0);
    sqlx::query(&format!(
        "INSERT INTO {schema}.groupchat_notification_recovery \
         (message_key, recipient_bare_jid, room_jid, stanza_id_by, stanza_id, sender_jid, \
          is_live_occupant, room_members_only, created_at_ms) \
         VALUES ('key', 'u@example.com', 'r@example.com', 'r@example.com', 'new', \
                 'r@example.com/a', 1, 0, 2)"
    ))
    .execute(&admin)
    .await
    .expect("insert keyed recovery");
    drop(
        DatabaseInboxStorage::from_database(db.clone())
            .await
            .expect("reopen inbox"),
    );
    let retained: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {schema}.groupchat_notification_recovery WHERE message_key = 'key'"
    ))
    .fetch_one(&admin)
    .await
    .expect("count retained keyed recovery");
    assert_eq!(retained, 1);

    // A same-name updatable view must be refused before the legacy DELETE.
    for statement in [
        format!("DROP TABLE {schema}.groupchat_notification_recovery"),
        format!("CREATE TABLE {schema}.legacy_target (id INTEGER NOT NULL)"),
        format!("INSERT INTO {schema}.legacy_target VALUES (1)"),
        format!("CREATE VIEW {schema}.groupchat_notification_recovery AS SELECT * FROM {schema}.legacy_target"),
    ] {
        sqlx::query(&statement)
            .execute(&admin)
            .await
            .expect("prepare unsafe relation");
    }
    let error = match DatabaseInboxStorage::from_database(db.clone()).await {
        Ok(_) => panic!("owner accepted recovery view"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("unexpected PostgreSQL relation kind"));
    let retained: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {schema}.legacy_target"))
        .fetch_one(&admin)
        .await
        .expect("read view target");
    assert_eq!(retained, 1);

    drop(db);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop fixture schema");
    admin.close().await;
}
