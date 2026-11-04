use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

use super::{ensure_postgres_schema, MamSchemaBootstrapError};
use super::{SqlxMamStorage, POSTGRES_SCHEMA_INIT_LOCK};
use crate::mam::MamSchemaVerificationError;

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn relation(schema: &str, name: &str) -> String {
    format!("{}.{name}", quoted(schema))
}

async fn direct(url: &str) -> PgConnection {
    PgConnection::connect(url).await.expect("direct connection")
}

async fn isolated(url: &str, prefix: &str) -> (PgConnection, String, String) {
    let mut conn = direct(url).await;
    let schema = format!("{prefix}_TEXT;\"{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {}", quoted(&schema)))
        .execute(&mut conn)
        .await
        .expect("isolated schema");
    let owner: String = sqlx::query_scalar("SELECT current_user::pg_catalog.text")
        .fetch_one(&mut conn)
        .await
        .expect("owner");
    (conn, schema, owner)
}

async fn verify(
    conn: &mut PgConnection,
    schema: &str,
    owner: &str,
) -> Result<(), MamSchemaVerificationError> {
    let mut tx = conn.begin().await.expect("verification transaction");
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .expect("read-only snapshot");
    sqlx::query("SET LOCAL statement_timeout = '5s'")
        .execute(&mut *tx)
        .await
        .expect("bounded timeout");
    let result = SqlxMamStorage::verify_postgres_schema_on_connection(&mut tx, schema, owner).await;
    tx.rollback().await.expect("rollback snapshot");
    result
}

async fn execute(conn: &mut PgConnection, sql: &str) {
    sqlx::query(sql).execute(conn).await.expect("fixture SQL");
}

async fn wait_backend_gone(conn: &mut PgConnection, pid: i32) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
            )
            .bind(pid)
            .fetch_one(&mut *conn)
            .await
            .expect("backend status");
            if !exists {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("backend closes after cancellation");
}

#[tokio::test]
async fn postgres_owned_bootstrap_and_snapshot_verifier_reject_drift() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let (mut conn, schema, owner) = isolated(&url, "mam_verify").await;
    let messages = relation(&schema, "mam_messages");
    let sequences = relation(&schema, "mam_archive_sequences");
    let mut observer = direct(&url).await;
    let id: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut conn)
        .await
        .expect("backend id");
    const OTHER_LOCK: i64 = 0x7761_6464_6f74_6831;
    sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
        .bind(OTHER_LOCK)
        .execute(&mut conn)
        .await
        .expect("earlier owner lock");
    // A temp shadow and hardened search path cannot redirect schema-qualified DDL.
    execute(&mut conn, "CREATE TEMP TABLE mam_messages (decoy int)").await;
    execute(&mut conn, "SET search_path = pg_catalog, pg_temp").await;
    let mut conn =
        SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(conn, &schema, &owner)
            .await
            .expect("bootstrap");
    let after: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut conn)
        .await
        .expect("same backend");
    assert_eq!(after, id);
    let earlier_free: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_lock($1)")
        .bind(OTHER_LOCK)
        .fetch_one(&mut observer)
        .await
        .expect("probe earlier lock");
    assert!(!earlier_free);
    let owner_holds_mam: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid = $1 AND locktype = 'advisory' AND granted AND objsubid = 1 AND ((classid::pg_catalog.int8 << 32) | objid::pg_catalog.int8) = $2)",
    )
    .bind(id)
    .bind(POSTGRES_SCHEMA_INIT_LOCK)
    .fetch_one(&mut observer)
    .await
    .expect("probe MAM lock on owner session");
    assert!(!owner_holds_mam);
    verify(&mut conn, &schema, &owner)
        .await
        .expect("fresh snapshot");
    assert!(matches!(
        SqlxMamStorage::verify_postgres_schema_on_connection(&mut conn, &schema, &owner).await,
        Err(MamSchemaVerificationError::TransactionRequired)
    ));
    assert!(matches!(
        verify(&mut conn, &schema, "different-owner").await,
        Err(MamSchemaVerificationError::RelationDrift)
    ));
    let mut tx = conn.begin().await.expect("wrong mode transaction");
    sqlx::query("SET LOCAL statement_timeout = '5s'")
        .execute(&mut *tx)
        .await
        .expect("timeout");
    assert!(matches!(
        SqlxMamStorage::verify_postgres_schema_on_connection(&mut tx, &schema, &owner).await,
        Err(MamSchemaVerificationError::TransactionMode)
    ));
    tx.rollback().await.expect("rollback");
    let mut tx = conn.begin().await.expect("unbounded transaction");
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .expect("mode");
    sqlx::query("SET LOCAL statement_timeout = 0")
        .execute(&mut *tx)
        .await
        .expect("timeout");
    assert!(matches!(
        SqlxMamStorage::verify_postgres_schema_on_connection(&mut tx, &schema, &owner).await,
        Err(MamSchemaVerificationError::UnboundedTimeout)
    ));
    tx.rollback().await.expect("rollback");

    // Zero is valid for an empty archive, and counters may exceed surviving rows.
    execute(
        &mut conn,
        &format!("INSERT INTO {sequences} VALUES ('empty@example.org', 0)"),
    )
    .await;
    verify(&mut conn, &schema, &owner)
        .await
        .expect("empty counter");
    execute(&mut conn, &format!("INSERT INTO {messages} (id, room_jid, timestamp, from_jid, to_jid, origin_id, archive_seq) VALUES ('first', 'room@example.org', CURRENT_TIMESTAMP, 'alice@example.org', 'room@example.org', 'duplicate', 1)")).await;
    assert!(matches!(
        verify(&mut conn, &schema, &owner).await,
        Err(MamSchemaVerificationError::MissingOrStaleCounter)
    ));
    execute(
        &mut conn,
        &format!("INSERT INTO {sequences} VALUES ('room@example.org', 0)"),
    )
    .await;
    assert!(matches!(
        verify(&mut conn, &schema, &owner).await,
        Err(MamSchemaVerificationError::MissingOrStaleCounter)
    ));
    execute(
        &mut conn,
        &format!("UPDATE {sequences} SET next_seq = 9 WHERE archive_jid = 'room@example.org'"),
    )
    .await;
    verify(&mut conn, &schema, &owner)
        .await
        .expect("high water above maximum");
    execute(
        &mut conn,
        &format!("UPDATE {sequences} SET next_seq = -1 WHERE archive_jid = 'room@example.org'"),
    )
    .await;
    assert!(matches!(
        verify(&mut conn, &schema, &owner).await,
        Err(MamSchemaVerificationError::NegativeCounter)
    ));
    execute(&mut conn, &format!("UPDATE {sequences} SET next_seq = 9223372036854775807 WHERE archive_jid = 'room@example.org'")).await;
    assert!(matches!(
        verify(&mut conn, &schema, &owner).await,
        Err(MamSchemaVerificationError::ExhaustedCounter)
    ));
    execute(
        &mut conn,
        &format!("UPDATE {sequences} SET next_seq = 9 WHERE archive_jid = 'room@example.org'"),
    )
    .await;
    execute(
        &mut conn,
        &format!("UPDATE {messages} SET archive_seq = 0 WHERE id = 'first'"),
    )
    .await;
    assert!(matches!(
        verify(&mut conn, &schema, &owner).await,
        Err(MamSchemaVerificationError::NonpositiveOrdinal)
    ));
    execute(
        &mut conn,
        &format!("UPDATE {messages} SET archive_seq = 1 WHERE id = 'first'"),
    )
    .await;

    execute(
        &mut conn,
        &format!("UPDATE {messages} SET body = 'body' WHERE body IS NULL"),
    )
    .await;
    execute(
        &mut conn,
        &format!("ALTER TABLE {messages} ALTER COLUMN body SET NOT NULL"),
    )
    .await;
    assert!(matches!(
        verify(&mut conn, &schema, &owner).await,
        Err(MamSchemaVerificationError::ColumnDrift)
    ));
    execute(
        &mut conn,
        &format!("ALTER TABLE {messages} ALTER COLUMN body DROP NOT NULL"),
    )
    .await;
    execute(
        &mut conn,
        &format!("ALTER TABLE {messages} ADD CONSTRAINT fixture_check CHECK (archive_seq > 0)"),
    )
    .await;
    assert!(matches!(
        verify(&mut conn, &schema, &owner).await,
        Err(MamSchemaVerificationError::ConstraintDrift)
    ));
    execute(
        &mut conn,
        &format!("ALTER TABLE {messages} DROP CONSTRAINT fixture_check"),
    )
    .await;
    execute(
        &mut conn,
        &format!("CREATE INDEX idx_mam_origin_direct_unique ON {messages} (origin_id)"),
    )
    .await;
    assert!(matches!(
        verify(&mut conn, &schema, &owner).await,
        Err(MamSchemaVerificationError::ObsoleteIndex)
    ));
    execute(
        &mut conn,
        &format!(
            "DROP INDEX {}",
            relation(&schema, "idx_mam_origin_direct_unique")
        ),
    )
    .await;
    execute(
        &mut conn,
        &format!("DROP INDEX {}", relation(&schema, "idx_mam_room_origin")),
    )
    .await;
    execute(
        &mut conn,
        &format!("CREATE INDEX idx_mam_room_origin ON {messages} (origin_id, room_jid)"),
    )
    .await;
    assert!(matches!(
        verify(&mut conn, &schema, &owner).await,
        Err(MamSchemaVerificationError::IndexDrift)
    ));
    execute(
        &mut conn,
        &format!("DROP INDEX {}", relation(&schema, "idx_mam_room_origin")),
    )
    .await;
    execute(&mut conn, &format!("INSERT INTO {messages} (id, room_jid, timestamp, from_jid, to_jid, origin_id, archive_seq) VALUES ('second', 'room@example.org', CURRENT_TIMESTAMP, 'alice@example.org', 'room@example.org', 'duplicate', 2)")).await;
    let invalid = sqlx::query(&format!(
        "CREATE UNIQUE INDEX CONCURRENTLY idx_mam_room_origin ON {messages} (origin_id)"
    ))
    .execute(&mut conn)
    .await;
    assert!(invalid.is_err());
    assert!(matches!(
        verify(&mut conn, &schema, &owner).await,
        Err(MamSchemaVerificationError::IndexDrift)
    ));
    execute(
        &mut conn,
        &format!("DROP INDEX {}", relation(&schema, "idx_mam_room_origin")),
    )
    .await;
    execute(
        &mut conn,
        &format!("CREATE INDEX idx_mam_room_origin ON {messages} (room_jid, origin_id)"),
    )
    .await;
    verify(&mut conn, &schema, &owner)
        .await
        .expect("repaired snapshot");

    sqlx::query("SELECT pg_catalog.pg_advisory_unlock($1)")
        .bind(OTHER_LOCK)
        .execute(&mut conn)
        .await
        .expect("release earlier lock");
    execute(
        &mut conn,
        &format!("DROP SCHEMA {} CASCADE", quoted(&schema)),
    )
    .await;
}

#[tokio::test]
async fn postgres_owned_bootstrap_resumes_legacy_backfill_and_rejects_preheld_lock() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let (mut conn, schema, owner) = isolated(&url, "mam_legacy").await;
    let messages = relation(&schema, "mam_messages");
    let sequences = relation(&schema, "mam_archive_sequences");
    execute(&mut conn, &format!("CREATE TABLE {messages} (id TEXT PRIMARY KEY, room_jid TEXT NOT NULL, timestamp TIMESTAMPTZ NOT NULL, from_jid TEXT NOT NULL, to_jid TEXT NOT NULL, body TEXT NOT NULL, stanza_id TEXT, thread_id TEXT, reply_to_id TEXT, reply_to_jid TEXT, origin_id TEXT, message_type TEXT NOT NULL DEFAULT 'chat', archive_seq BIGINT, created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, origin_dedup_fingerprint TEXT)")).await;
    execute(
        &mut conn,
        &format!(
            "CREATE TABLE {sequences} (archive_jid TEXT PRIMARY KEY, next_seq BIGINT NOT NULL)"
        ),
    )
    .await;
    execute(
        &mut conn,
        &format!("INSERT INTO {sequences} VALUES ('room@example.org', 10)"),
    )
    .await;
    execute(&mut conn, &format!("INSERT INTO {messages} (id, room_jid, timestamp, from_jid, to_jid, body, archive_seq) VALUES ('b', 'room@example.org', '2020-01-01 00:00:00+00', 'a@example.org', 'room@example.org', 'b', NULL), ('a', 'room@example.org', '2020-01-01 00:00:00+00', 'a@example.org', 'room@example.org', 'a', 1)")).await;
    execute(&mut conn, &format!("CREATE INDEX idx_mam_origin_direct_content_unique ON {messages} (origin_dedup_fingerprint)")).await;
    let mut conn =
        SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(conn, &schema, &owner)
            .await
            .expect("legacy bootstrap");
    let rows: Vec<(String, i64)> = sqlx::query_as(&format!(
        "SELECT id, archive_seq FROM {messages} ORDER BY archive_seq"
    ))
    .fetch_all(&mut conn)
    .await
    .expect("backfill rows");
    assert_eq!(rows, vec![("a".into(), 1), ("b".into(), 2)]);
    let counter: i64 = sqlx::query_scalar(&format!(
        "SELECT next_seq FROM {sequences} WHERE archive_jid = 'room@example.org'"
    ))
    .fetch_one(&mut conn)
    .await
    .expect("counter");
    assert_eq!(counter, 10);
    verify(&mut conn, &schema, &owner)
        .await
        .expect("legacy verification");
    let mut conn =
        SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(conn, &schema, &owner)
            .await
            .expect("idempotent bootstrap");
    verify(&mut conn, &schema, &owner)
        .await
        .expect("repeat verification");
    let preheld_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut conn)
        .await
        .expect("backend id");
    sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
        .bind(POSTGRES_SCHEMA_INIT_LOCK)
        .execute(&mut conn)
        .await
        .expect("prehold");
    assert!(matches!(
        SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(conn, &schema, &owner).await,
        Err(MamSchemaBootstrapError::AlreadyHeld)
    ));
    let mut observer = direct(&url).await;
    wait_backend_gone(&mut observer, preheld_pid).await;
    execute(
        &mut observer,
        &format!("DROP SCHEMA {} CASCADE", quoted(&schema)),
    )
    .await;
}

#[tokio::test]
async fn postgres_owner_preflight_rejects_foreign_table_before_any_ddl() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let (mut conn, schema, owner) = isolated(&url, "mam_foreign").await;
    let role = format!("mam_foreign_{}", Uuid::new_v4().simple());
    let sequences = relation(&schema, "mam_archive_sequences");
    execute(&mut conn, &format!("CREATE ROLE {} NOLOGIN", quoted(&role))).await;
    execute(
        &mut conn,
        &format!(
            "CREATE TABLE {sequences} (archive_jid TEXT PRIMARY KEY, next_seq BIGINT NOT NULL)"
        ),
    )
    .await;
    execute(
        &mut conn,
        &format!("ALTER TABLE {sequences} OWNER TO {}", quoted(&role)),
    )
    .await;
    assert!(matches!(
        SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(conn, &schema, &owner).await,
        Err(MamSchemaBootstrapError::PreexistingObjectConflict)
    ));
    let mut observer = direct(&url).await;
    let messages_exist: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = 'mam_messages')",
    ).bind(&schema).fetch_one(&mut observer).await.expect("check no DDL prefix");
    assert!(!messages_exist);
    execute(
        &mut observer,
        &format!("DROP SCHEMA {} CASCADE", quoted(&schema)),
    )
    .await;
    execute(&mut observer, &format!("DROP ROLE {}", quoted(&role))).await;
}

#[tokio::test]
async fn postgres_owner_preflight_rejects_wrong_index_target_before_any_ddl() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let (mut conn, schema, owner) = isolated(&url, "mam_wrong_index").await;
    execute(
        &mut conn,
        &format!("CREATE TABLE {} (value TEXT)", relation(&schema, "decoy")),
    )
    .await;
    execute(
        &mut conn,
        &format!(
            "CREATE INDEX idx_mam_room_origin ON {} (value)",
            relation(&schema, "decoy")
        ),
    )
    .await;
    assert!(matches!(
        SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(conn, &schema, &owner).await,
        Err(MamSchemaBootstrapError::PreexistingObjectConflict)
    ));
    let mut observer = direct(&url).await;
    let messages_exist: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = 'mam_messages')",
    ).bind(&schema).fetch_one(&mut observer).await.expect("check no DDL prefix");
    assert!(!messages_exist);
    execute(
        &mut observer,
        &format!("DROP SCHEMA {} CASCADE", quoted(&schema)),
    )
    .await;
}

async fn cancellation_case(url: &str, pooled: bool) {
    let (conn, schema, owner) = isolated(url, "mam_cancel").await;
    let mut conn =
        SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(conn, &schema, &owner)
            .await
            .expect("initial bootstrap");
    execute(
        &mut conn,
        &format!("DROP INDEX {}", relation(&schema, "idx_mam_room_origin")),
    )
    .await;
    drop(conn);

    let mut blocker = direct(url).await;
    let mut tx = blocker.begin().await.expect("blocking transaction");
    sqlx::query(&format!(
        "LOCK TABLE {} IN ACCESS SHARE MODE",
        relation(&schema, "mam_messages")
    ))
    .execute(&mut *tx)
    .await
    .expect("block ALTER TABLE");
    let mut observer = direct(url).await;
    let (pid, handle, pool) = if pooled {
        let set_path = format!("SET search_path TO {}", quoted(&schema));
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .after_connect(move |conn, _| {
                let set_path = set_path.clone();
                Box::pin(async move {
                    sqlx::query(&set_path).execute(conn).await?;
                    Ok(())
                })
            })
            .connect(url)
            .await
            .expect("single connection pool");
        let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(&pool)
            .await
            .expect("pool backend");
        let task_pool = pool.clone();
        let handle = tokio::spawn(async move {
            let _ = ensure_postgres_schema(&task_pool).await;
        });
        (pid, handle, Some(pool))
    } else {
        let mut owner_conn = direct(url).await;
        let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(&mut owner_conn)
            .await
            .expect("owner backend");
        let target = schema.clone();
        let expected_owner = owner.clone();
        let handle = tokio::spawn(async move {
            let _ = SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(
                owner_conn,
                &target,
                &expected_owner,
            )
            .await;
        });
        (pid, handle, None)
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let committed_prefix: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = 'idx_mam_room_origin')",
            ).bind(&schema).fetch_one(&mut observer).await.expect("observe prefix");
            if committed_prefix { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("autocommitted prefix");
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let ddl_waiting: bool = sqlx::query_scalar(
                "SELECT COALESCE((SELECT state = 'active' AND wait_event_type = 'Lock' AND query LIKE '%ADD COLUMN IF NOT EXISTS rich_payload%' FROM pg_catalog.pg_stat_activity WHERE pid = $1), false)",
            )
            .bind(pid)
            .fetch_one(&mut observer)
            .await
            .expect("observe dispatched ALTER");
            if ddl_waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("owner dispatched ALTER and is waiting for the blocker");
    assert!(!handle.is_finished(), "ALTER must wait for the blocker");
    handle.abort();
    assert!(handle.await.expect_err("cancelled task").is_cancelled());
    // PostgreSQL can finish an in-flight statement before noticing a dropped
    // client socket, so release the blocker before observing backend exit.
    tx.rollback().await.expect("release table lock");
    wait_backend_gone(&mut observer, pid).await;
    if let Some(pool) = pool {
        let replacement_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(&pool)
            .await
            .expect("replacement pool backend");
        assert_ne!(
            replacement_pid, pid,
            "cancelled lock-bearing session must not be pooled"
        );
        pool.close().await;
    }
    execute(
        &mut observer,
        &format!("DROP SCHEMA {} CASCADE", quoted(&schema)),
    )
    .await;
}

#[tokio::test]
async fn postgres_cancelled_owned_and_pooled_bootstraps_do_not_recycle_sessions() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    cancellation_case(&url, false).await;
    cancellation_case(&url, true).await;
}
