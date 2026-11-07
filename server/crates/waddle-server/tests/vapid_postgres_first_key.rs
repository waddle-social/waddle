//! Public-entry PostgreSQL first-key and legacy-owner regression tests.
//!
//! Each bootstrap environment is installed in a fresh subprocess before its
//! runtime starts. The database URL must point at a disposable PG17 cluster.

#[path = "../src/test_support/postgres_proxy.rs"]
pub mod postgres_proxy;

use std::ffi::OsString;
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use sqlx::postgres::PgConnectOptions;
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;
use tokio::sync::Notify;
use waddle_server::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::push_service::vapid_storage::VapidStorage;
use waddle_xmpp::push::types::{VapidDbWriteStage, VapidLoadError};

const ROOT_KEY: &[u8] = b"vapid-postgres-test-root-key-32b";
const ENV_KEY: &str = "WADDLE_VAPID_PRIVATE_KEY";
const CHILD_MODE: &str = "WADDLE_VAPID_PG_TEST_MODE";
const CHILD_URL: &str = "WADDLE_VAPID_PG_TEST_URL";
const CHILD_SCHEMA: &str = "WADDLE_VAPID_PG_TEST_SCHEMA";
const PUSH_GATE: i64 = 6_841_445_497_037_937_998;

struct Fixture {
    admin: PgPool,
    schema: String,
    url: String,
}

impl Fixture {
    async fn new(url: &str) -> Self {
        let admin = PgPool::connect(url)
            .await
            .expect("connect disposable cluster");
        let version: i32 =
            sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
                .fetch_one(&admin)
                .await
                .expect("read PostgreSQL version");
        assert!((170_000..180_000).contains(&version), "requires PG17");
        let schema = format!("vapid_first_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated schema");
        let url = url_with_options(url, &format!("-c search_path={schema}"));
        Self { admin, schema, url }
    }

    async fn count(&self) -> i64 {
        sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {}.push_service_vapid_keys",
            self.schema
        ))
        .fetch_one(&self.admin)
        .await
        .expect("count stored keys")
    }

    async fn cleanup(self) {
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .expect("drop isolated schema");
        self.admin.close().await;
    }
}

fn url_with_options(url: &str, options: &str) -> String {
    let mut url = url::Url::parse(url).expect("PostgreSQL URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", options);
    url.to_string()
}

fn spawn_child_with_schema(
    url: &str,
    mode: &str,
    key: Option<OsString>,
    schema: Option<&str>,
) -> Child {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", "pg17_vapid_subprocess_entry", "--nocapture"])
        .env(CHILD_MODE, mode)
        .env(CHILD_URL, url)
        .env_remove(ENV_KEY)
        .env_remove(CHILD_SCHEMA)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(key) = key {
        command.env(ENV_KEY, key);
    }
    if let Some(schema) = schema {
        command.env(CHILD_SCHEMA, schema);
    }
    command.spawn().expect("start isolated VAPID process")
}

fn spawn_child(url: &str, mode: &str, key: Option<OsString>) -> Child {
    spawn_child_with_schema(url, mode, key, None)
}

fn finish_child_with_timeout(mut child: Child, wait: Duration) {
    let deadline = Instant::now() + wait;
    loop {
        if child.try_wait().expect("poll VAPID process").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().expect("terminate stalled VAPID process");
            let _ = child.wait();
            panic!("VAPID subprocess exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("wait for VAPID process");
    assert!(
        output.status.success(),
        "VAPID subprocess failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn finish_child(child: Child) {
    finish_child_with_timeout(child, Duration::from_secs(60));
}

fn run_child(url: &str, mode: &str, key: Option<OsString>) {
    finish_child(spawn_child(url, mode, key));
}

fn encoded_key(byte: u8) -> OsString {
    URL_SAFE_NO_PAD.encode([byte; 32]).into()
}

#[tokio::test]
async fn pg17_vapid_subprocess_entry() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    let url = std::env::var(CHILD_URL).expect("fixture URL");
    let schema = std::env::var(CHILD_SCHEMA).ok();
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, url);
    if schema.is_some() || matches!(mode.as_str(), "commit_unknown" | "fence_error") {
        config.pool_size = 1;
    }
    let db = Database::from_config("VAPID PostgreSQL public-entry test", &config)
        .await
        .expect("open VAPID test database");
    if let Some(schema) = schema {
        let connection = db.guard().await.expect("single physical checkout");
        let path = format!("\"{}\"", schema.replace('"', "\"\""));
        connection
            .query(
                "SELECT pg_catalog.set_config('search_path', ?, false)",
                waddle_server::db_params![path],
            )
            .await
            .expect("set quoted search path on checkout");
        drop(connection);
    }
    if mode == "concurrent" {
        let mut tasks = Vec::new();
        for _ in 0..12 {
            let db = db.clone();
            tasks.push(tokio::spawn(async move {
                VapidStorage::load_or_provision(db, ROOT_KEY).await
            }));
        }
        let mut kid = None;
        for task in tasks {
            let signer = task.await.expect("join bootstrap").expect("bootstrap");
            let observed = signer.current_kid();
            if let Some(expected) = kid {
                assert_eq!(observed, expected);
            } else {
                kid = Some(observed);
            }
        }
        return;
    }
    let result = VapidStorage::load_or_provision(db.clone(), ROOT_KEY).await;
    match mode.as_str() {
        "bootstrap" => {
            let signer = result.expect("bootstrap or reuse environment key");
            let expected = p256::SecretKey::from_slice(&[0xAE; 32])
                .expect("test scalar")
                .public_key()
                .to_encoded_point(false);
            assert_eq!(
                signer.current_public_key().to_encoded_point(false),
                expected
            );
            let again = VapidStorage::load_or_provision(db, ROOT_KEY)
                .await
                .expect("reuse matching environment key");
            assert_eq!(signer.current_kid(), again.current_kid());
        }
        "reuse" => {
            result.expect("reuse active key");
        }
        "mismatch" => assert!(matches!(result, Err(VapidLoadError::BootstrapKeyMismatch))),
        "malformed" => assert!(matches!(result, Err(VapidLoadError::EnvParse(_)))),
        "nonunicode" => assert!(matches!(
            result,
            Err(VapidLoadError::InvalidBootstrapEnvironment)
        )),
        "retired" => assert!(matches!(result, Err(VapidLoadError::RotationRequired))),
        "multiple" => assert!(matches!(result, Err(VapidLoadError::MultipleActiveKeys))),
        "stored" => assert!(matches!(result, Err(VapidLoadError::InvalidStoredKey))),
        "marker" => assert!(matches!(result, Err(VapidLoadError::InvalidStorageState))),
        "identity_denied" => match result {
            Err(VapidLoadError::DbWrite {
                stage: VapidDbWriteStage::FencedProvision,
                source,
            }) => assert!(source
                .to_string()
                .contains("permission denied for function pg_control_system")),
            _ => panic!("marker identity check did not fail on the denied function"),
        },
        "commit_unknown" => assert!(matches!(result, Err(VapidLoadError::CommitIndeterminate))),
        "fence_error" => assert!(matches!(
            result,
            Err(VapidLoadError::DbWrite {
                stage: VapidDbWriteStage::FencedProvision,
                ..
            })
        )),
        _ => panic!("unknown VAPID test mode"),
    }
}

#[tokio::test]
async fn pg17_concurrent_first_key_and_integrity_failures() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let f = Fixture::new(&url).await;
    run_child(&f.url, "concurrent", None);
    assert_eq!(f.count().await, 1);
    run_child(&f.url, "reuse", None);

    let public_key: Vec<u8> = sqlx::query_scalar(&format!(
        "SELECT public_key FROM {}.push_service_vapid_keys",
        f.schema
    ))
    .fetch_one(&f.admin)
    .await
    .expect("read public key");
    sqlx::query(&format!(
        "UPDATE {}.push_service_vapid_keys SET public_key = $1",
        f.schema
    ))
    .bind(vec![4_u8])
    .execute(&f.admin)
    .await
    .expect("tamper public key");
    run_child(&f.url, "stored", None);
    sqlx::query(&format!(
        "UPDATE {}.push_service_vapid_keys SET public_key = $1, retired_at_ms = 1",
        f.schema
    ))
    .bind(public_key)
    .execute(&f.admin)
    .await
    .expect("restore public key and retire it");
    run_child(&f.url, "retired", None);
    sqlx::query(&format!(
        "UPDATE {}.push_service_vapid_keys SET retired_at_ms = NULL",
        f.schema
    ))
    .execute(&f.admin)
    .await
    .expect("restore active key");
    sqlx::query(&format!(
        "INSERT INTO {0}.push_service_vapid_keys \
         SELECT $1, sealed_private_key, public_key, root_key_version, created_at_ms, NULL \
         FROM {0}.push_service_vapid_keys",
        f.schema
    ))
    .bind(uuid::Uuid::new_v4().to_string())
    .execute(&f.admin)
    .await
    .expect("construct multiple-active fixture");
    run_child(&f.url, "multiple", None);
    assert_eq!(f.count().await, 2);
    f.cleanup().await;
}

#[tokio::test]
async fn pg17_environment_bootstrap_is_idempotent_and_mismatches_fail_closed() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let f = Fixture::new(&url).await;
    run_child(&f.url, "bootstrap", Some(encoded_key(0xAE)));
    run_child(&f.url, "bootstrap", Some(encoded_key(0xAE)));
    run_child(&f.url, "mismatch", Some(encoded_key(0xAC)));
    run_child(&f.url, "malformed", Some("invalid scalar".into()));
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        run_child(&f.url, "nonunicode", Some(OsString::from_vec(vec![0xFF])));
    }
    run_child(&f.url, "reuse", None);
    assert_eq!(f.count().await, 1);
    f.cleanup().await;
}

#[tokio::test]
async fn pg17_raw_role_may_own_vapid_table_in_another_roles_schema() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let f = Fixture::new(&url).await;
    let role = format!("vapid_raw_{}", uuid::Uuid::new_v4().simple());
    let login: String = sqlx::query_scalar("SELECT session_user::text")
        .fetch_one(&f.admin)
        .await
        .expect("session user");
    let login = login.replace('"', "\"\"");
    for sql in [
        format!("CREATE ROLE {role} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE"),
        format!("GRANT {role} TO \"{login}\""),
        format!("GRANT USAGE, CREATE ON SCHEMA {} TO {role}", f.schema),
    ] {
        sqlx::query(&sql)
            .execute(&f.admin)
            .await
            .expect("prepare raw role");
    }
    let role_url = url_with_options(&url, &format!("-c role={role} -c search_path={}", f.schema));
    run_child(&role_url, "reuse", None);
    let owner: String = sqlx::query_scalar(&format!(
        "SELECT pg_catalog.pg_get_userbyid(relowner)::text FROM pg_catalog.pg_class \
         WHERE oid = '{}.push_service_vapid_keys'::regclass",
        f.schema
    ))
    .fetch_one(&f.admin)
    .await
    .expect("table owner");
    assert_eq!(owner, role);
    let kid: String = sqlx::query_scalar(&format!(
        "SELECT kid FROM {}.push_service_vapid_keys",
        f.schema
    ))
    .fetch_one(&f.admin)
    .await
    .expect("first kid");
    sqlx::query(&format!(
        "DROP INDEX {}.idx_push_service_vapid_keys_active",
        f.schema
    ))
    .execute(&f.admin)
    .await
    .expect("remove legacy optional index");
    run_child(&role_url, "reuse", None);
    let after: String = sqlx::query_scalar(&format!(
        "SELECT kid FROM {}.push_service_vapid_keys",
        f.schema
    ))
    .fetch_one(&f.admin)
    .await
    .expect("reused kid");
    assert_eq!(kid, after);
    let repaired: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(format!("{}.idx_push_service_vapid_keys_active", f.schema))
        .fetch_one(&f.admin)
        .await
        .expect("index repair");
    assert!(repaired);
    sqlx::query(&format!("REVOKE {role} FROM \"{login}\""))
        .execute(&f.admin)
        .await
        .expect("revoke role membership");
    sqlx::query(&format!("DROP SCHEMA {} CASCADE", f.schema))
        .execute(&f.admin)
        .await
        .expect("drop test schema");
    sqlx::query(&format!("DROP ROLE {role}"))
        .execute(&f.admin)
        .await
        .expect("drop test role");
    f.admin.close().await;
}

#[tokio::test]
async fn pg17_push_marker_committed_during_gate_wait_is_rechecked_and_invalidated() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let f = Fixture::new(&url).await;
    let mut owner = f.admin.begin().await.expect("owner transaction");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(PUSH_GATE)
        .execute(&mut *owner)
        .await
        .expect("hold Push gate");
    sqlx::query(&format!(
        "CREATE TABLE {}.push_service_schema_marker (\
           marker_id INTEGER PRIMARY KEY CHECK (marker_id = 1), \
           version BIGINT NOT NULL CHECK (version = 1), \
           owner_phase BIGINT NOT NULL CHECK (owner_phase IN (0, 1)), \
           system_identifier TEXT NOT NULL, database_oid BIGINT NOT NULL, \
           database_name TEXT NOT NULL, schema_oid BIGINT NOT NULL, schema_name TEXT NOT NULL)",
        f.schema
    ))
    .execute(&mut *owner)
    .await
    .expect("create strict marker while holding Push gate");
    sqlx::query(&format!(
        "INSERT INTO {}.push_service_schema_marker \
         SELECT 1, 1, 1, (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
           database.oid::bigint, database.datname::text, namespace.oid::bigint, namespace.nspname::text \
         FROM pg_catalog.pg_database database, pg_catalog.pg_namespace namespace \
         WHERE database.datname = pg_catalog.current_database() AND namespace.nspname = $1",
        f.schema
    ))
    .bind(&f.schema)
    .execute(&mut *owner)
    .await
    .expect("stage completed marker");

    let application = format!("vapid_wait_{}", uuid::Uuid::new_v4().simple());
    let mut child_url = url::Url::parse(&f.url).expect("fixture URL");
    child_url
        .query_pairs_mut()
        .append_pair("application_name", &application);
    let mut child = spawn_child(child_url.as_str(), "reuse", None);
    let retry_observed = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let saw_retry: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE application_name = $1 AND state = 'idle' \
                   AND xact_start IS NULL AND query = 'ROLLBACK')",
            )
            .bind(&application)
            .fetch_one(&f.admin)
            .await
            .expect("observe acknowledged gate retry");
            let catalog_available: bool =
                sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                    .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                    .fetch_one(&f.admin)
                    .await
                    .expect("probe released catalog fence");
            if saw_retry && catalog_available {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if retry_observed.is_err() {
        child.kill().expect("terminate stalled VAPID process");
        let _ = child.wait();
        panic!("provisioning did not retry without holding catalog-X");
    }
    let absent: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NULL")
        .bind(format!("{}.push_service_vapid_keys", f.schema))
        .fetch_one(&f.admin)
        .await
        .expect("read pre-commit table state");
    assert!(absent);
    owner.commit().await.expect("commit completed marker");
    finish_child(child);
    let phase: i64 = sqlx::query_scalar(&format!(
        "SELECT owner_phase FROM {}.push_service_schema_marker",
        f.schema
    ))
    .fetch_one(&f.admin)
    .await
    .expect("read marker phase");
    assert_eq!(phase, 0);
    assert_eq!(f.count().await, 1);

    // A valid owner-written VAPID marker remains usable, but a marker naming
    // any other active key must prevent a later raw load or insert.
    sqlx::query(&format!(
        "CREATE TABLE {}.push_service_vapid_schema_marker (\
           marker_id INTEGER NOT NULL PRIMARY KEY CHECK (marker_id = 1), \
           version BIGINT NOT NULL CHECK (version = 1), system_identifier TEXT NOT NULL, \
           database_oid BIGINT NOT NULL, database_name TEXT NOT NULL, \
           schema_oid BIGINT NOT NULL, schema_name TEXT NOT NULL, initial_kid TEXT NOT NULL)",
        f.schema
    ))
    .execute(&f.admin)
    .await
    .expect("create strict VAPID marker");
    sqlx::query(&format!(
        "INSERT INTO {0}.push_service_vapid_schema_marker \
         SELECT 1, 1, marker.system_identifier, marker.database_oid, marker.database_name, \
           marker.schema_oid, marker.schema_name, key.kid \
         FROM {0}.push_service_schema_marker marker CROSS JOIN {0}.push_service_vapid_keys key",
        f.schema
    ))
    .execute(&f.admin)
    .await
    .expect("mark actual active key");
    run_child(&f.url, "reuse", None);
    sqlx::query(&format!(
        "UPDATE {}.push_service_vapid_schema_marker SET initial_kid = 'wrong'",
        f.schema
    ))
    .execute(&f.admin)
    .await
    .expect("corrupt VAPID marker");
    run_child(&f.url, "marker", None);
    assert_eq!(f.count().await, 1);
    f.cleanup().await;
}

#[tokio::test]
async fn pg17_quoted_raw_schema_accepts_current_role_owned_table() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let admin = PgPool::connect(&url)
        .await
        .expect("connect disposable cluster");
    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
        .fetch_one(&admin)
        .await
        .expect("server version");
    assert!((170_000..180_000).contains(&version), "requires PG17");
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let role = format!("vapid_quote_{suffix}");
    let schema = format!("Vapid-\"?{suffix}");
    let quoted_schema = format!("\"{}\"", schema.replace('"', "\"\""));
    let login: String = sqlx::query_scalar("SELECT session_user::text")
        .fetch_one(&admin)
        .await
        .expect("session user");
    let login = login.replace('"', "\"\"");
    for sql in [
        format!("CREATE ROLE {role} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE"),
        format!("GRANT {role} TO \"{login}\""),
        format!("CREATE SCHEMA {quoted_schema}"),
        format!("GRANT USAGE, CREATE ON SCHEMA {quoted_schema} TO {role}"),
    ] {
        sqlx::query(&sql)
            .execute(&admin)
            .await
            .expect("prepare quoted schema");
    }
    let role_url = url_with_options(&url, &format!("-c role={role}"));
    finish_child(spawn_child_with_schema(
        &role_url,
        "reuse",
        None,
        Some(&schema),
    ));
    finish_child(spawn_child_with_schema(
        &role_url,
        "reuse",
        None,
        Some(&schema),
    ));
    let keys: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {quoted_schema}.push_service_vapid_keys"
    ))
    .fetch_one(&admin)
    .await
    .expect("count quoted-schema keys");
    assert_eq!(keys, 1);
    sqlx::query(&format!("DROP SCHEMA {quoted_schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop quoted schema");
    sqlx::query(&format!("REVOKE {role} FROM \"{login}\""))
        .execute(&admin)
        .await
        .expect("revoke role membership");
    sqlx::query(&format!("DROP ROLE {role}"))
        .execute(&admin)
        .await
        .expect("drop raw role");
    admin.close().await;
}

#[tokio::test]
async fn pg17_raw_role_without_control_privilege_fails_closed_on_marked_schema() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let cluster = PgPool::connect(&url)
        .await
        .expect("connect disposable cluster");
    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
        .fetch_one(&cluster)
        .await
        .expect("server version");
    assert!((170_000..180_000).contains(&version), "requires PG17");
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let database = format!("vapid_identity_{suffix}");
    let role = format!("vapid_identity_r_{suffix}");
    let schema = format!("vapid_identity_s_{suffix}");
    let login: String = sqlx::query_scalar("SELECT session_user::text")
        .fetch_one(&cluster)
        .await
        .expect("session user");
    let login = login.replace('"', "\"\"");
    for sql in [
        format!("CREATE DATABASE {database}"),
        format!("CREATE ROLE {role} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT"),
        format!("GRANT {role} TO \"{login}\""),
    ] {
        sqlx::query(&sql)
            .execute(&cluster)
            .await
            .expect("prepare isolated database and role");
    }
    let mut database_url = url::Url::parse(&url).expect("PostgreSQL URL");
    database_url.set_path(&format!("/{database}"));
    let admin = PgPool::connect(database_url.as_str())
        .await
        .expect("connect isolated database");
    for sql in [
        "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC".to_owned(),
        format!("CREATE SCHEMA {schema}"),
        format!("GRANT USAGE, CREATE ON SCHEMA {schema} TO {role}"),
    ] {
        sqlx::query(&sql)
            .execute(&admin)
            .await
            .expect("prepare restricted schema");
    }
    let can_read_identity: bool = sqlx::query_scalar(
        "SELECT pg_catalog.has_function_privilege($1, 'pg_catalog.pg_control_system()', 'EXECUTE')",
    )
    .bind(&role)
    .fetch_one(&admin)
    .await
    .expect("inspect raw role privilege");
    assert!(!can_read_identity);
    let role_url = url_with_options(
        database_url.as_str(),
        &format!("-c role={role} -c search_path={schema}"),
    );
    run_child(&role_url, "reuse", None);
    let kid_before: String =
        sqlx::query_scalar(&format!("SELECT kid FROM {schema}.push_service_vapid_keys"))
            .fetch_one(&admin)
            .await
            .expect("unmarked bootstrap key");
    sqlx::query(&format!(
        "CREATE TABLE {schema}.push_service_schema_marker (\
           marker_id INTEGER PRIMARY KEY CHECK (marker_id = 1), \
           version BIGINT NOT NULL CHECK (version = 1), \
           owner_phase BIGINT NOT NULL CHECK (owner_phase IN (0, 1)), \
           system_identifier TEXT NOT NULL, database_oid BIGINT NOT NULL, \
           database_name TEXT NOT NULL, schema_oid BIGINT NOT NULL, schema_name TEXT NOT NULL)"
    ))
    .execute(&admin)
    .await
    .expect("create strict Push marker");
    sqlx::query(&format!(
        "INSERT INTO {schema}.push_service_schema_marker \
         SELECT 1, 1, 1, (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
           database.oid::bigint, database.datname::text, namespace.oid::bigint, namespace.nspname::text \
         FROM pg_catalog.pg_database database, pg_catalog.pg_namespace namespace \
         WHERE database.datname = pg_catalog.current_database() AND namespace.nspname = $1"
    )).bind(&schema).execute(&admin).await.expect("insert completed marker");
    sqlx::query(&format!(
        "GRANT SELECT, UPDATE ON {schema}.push_service_schema_marker TO {role}"
    ))
    .execute(&admin)
    .await
    .expect("permit marker lock without identity privilege");
    run_child(&role_url, "identity_denied", None);
    let (phase, kid_after, count): (i64, String, i64) = sqlx::query_as(&format!(
        "SELECT marker.owner_phase, min(key.kid), count(*) \
         FROM {schema}.push_service_schema_marker marker CROSS JOIN {schema}.push_service_vapid_keys key \
         GROUP BY marker.owner_phase"
    )).fetch_one(&admin).await.expect("inspect unchanged marker and key");
    assert_eq!((phase, kid_after, count), (1, kid_before, 1));
    admin.close().await;
    sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .execute(&cluster)
        .await
        .expect("drop isolated database");
    sqlx::query(&format!("REVOKE {role} FROM \"{login}\""))
        .execute(&cluster)
        .await
        .expect("revoke role membership");
    sqlx::query(&format!("DROP ROLE {role}"))
        .execute(&cluster)
        .await
        .expect("drop restricted role");
    cluster.close().await;
}

async fn withhold_commit_reply(
    listener: tokio::net::TcpListener,
    target: PgConnectOptions,
    held: Arc<Notify>,
    release: Arc<Notify>,
) -> std::io::Result<()> {
    let (client, _) = listener.accept().await?;
    let server = postgres_proxy::connect_local_postgres(&target).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let forward = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
    });
    loop {
        let tag = server_read.read_u8().await?;
        let length = server_read.read_u32().await?;
        if !(4..=1_048_576).contains(&length) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid PostgreSQL response length",
            ));
        }
        let mut body = vec![0; (length - 4) as usize];
        server_read.read_exact(&mut body).await?;
        if tag == b'C' && body == b"COMMIT\0" {
            held.notify_one();
            release.notified().await;
            break;
        }
        client_write.write_u8(tag).await?;
        client_write.write_u32(length).await?;
        client_write.write_all(&body).await?;
    }
    forward.abort();
    let _ = forward.await;
    Ok(())
}

#[tokio::test]
async fn pg17_commit_reply_timeout_is_indeterminate_and_retry_reuses_committed_key() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let target = PgConnectOptions::from_str(&url).expect("PostgreSQL connection options");
    if postgres_proxy::ensure_local_target(&target).is_err() {
        eprintln!("skipping plaintext proxy witness for nonlocal PostgreSQL target");
        return;
    }
    let f = Fixture::new(&url).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local PostgreSQL proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let held = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let proxy = tokio::spawn(withhold_commit_reply(
        listener,
        target,
        Arc::clone(&held),
        Arc::clone(&release),
    ));
    let proxy_url = postgres_proxy::local_proxy_url(&f.url, port).expect("proxy URL");
    let mut child = spawn_child(proxy_url.as_str(), "commit_unknown", None);
    if tokio::time::timeout(Duration::from_secs(10), held.notified())
        .await
        .is_err()
    {
        if child.try_wait().expect("poll VAPID process").is_none() {
            child.kill().expect("terminate stalled VAPID process");
        }
        let output = child
            .wait_with_output()
            .expect("read VAPID subprocess output");
        let proxy_result = if proxy.is_finished() {
            Some(proxy.await)
        } else {
            proxy.abort();
            None
        };
        panic!(
            "proxy did not observe a COMMIT reply; proxy={proxy_result:?}; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(f.count().await, 1, "server committed before reply was held");
    let committed_kid: String = sqlx::query_scalar(&format!(
        "SELECT kid FROM {}.push_service_vapid_keys",
        f.schema
    ))
    .fetch_one(&f.admin)
    .await
    .expect("read committed key");
    tokio::time::timeout(
        Duration::from_secs(14),
        tokio::task::spawn_blocking(move || {
            finish_child_with_timeout(child, Duration::from_secs(12))
        }),
    )
    .await
    .expect("COMMIT wait exceeded its bound")
    .expect("join VAPID process wait");
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .expect("proxy ends")
        .expect("join proxy")
        .expect("proxy I/O");

    // This fixture has one known primary; the retry itself must reacquire both
    // fences and reread the durable row before returning the existing signer.
    run_child(&f.url, "reuse", None);
    let retried_kid: String = sqlx::query_scalar(&format!(
        "SELECT kid FROM {}.push_service_vapid_keys",
        f.schema
    ))
    .fetch_one(&f.admin)
    .await
    .expect("read key after fenced retry");
    assert_eq!(committed_kid, retried_kid);
    assert_eq!(f.count().await, 1);
    f.cleanup().await;
}

async fn hold_catalog_lock_reply(
    listener: tokio::net::TcpListener,
    target: PgConnectOptions,
    held: Arc<Notify>,
    release: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let (client, _) = listener.accept().await?;
    let server = postgres_proxy::connect_local_postgres(&target).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let catalog_query_seen = Arc::new(AtomicBool::new(false));
    let response_held = Arc::new(AtomicBool::new(false));
    let mut upstream_release = release.clone();
    let downstream_seen = Arc::clone(&catalog_query_seen);
    let upstream_held = Arc::clone(&response_held);
    let upstream = async move {
        let mut buffer = [0_u8; 8192];
        let mut tail = Vec::new();
        loop {
            if upstream_held.load(Ordering::SeqCst) {
                let _ = upstream_release.changed().await;
                return Ok::<(), std::io::Error>(());
            }
            let count = client_read.read(&mut buffer).await?;
            if count == 0 {
                // Keep the server side open even after client cancellation.
                let _ = upstream_release.changed().await;
                return Ok(());
            }
            if upstream_held.load(Ordering::SeqCst) {
                let _ = upstream_release.changed().await;
                return Ok(());
            }
            let mut scan = tail;
            scan.extend_from_slice(&buffer[..count]);
            let needle = b"pg_try_advisory_xact_lock(";
            if scan.windows(needle.len()).any(|part| part == needle) {
                catalog_query_seen.store(true, Ordering::SeqCst);
            }
            server_write.write_all(&buffer[..count]).await?;
            tail = scan[scan.len().saturating_sub(needle.len() - 1)..].to_vec();
        }
    };
    let mut downstream_release = release;
    let downstream = async move {
        loop {
            let tag = server_read.read_u8().await?;
            let length = server_read.read_u32().await?;
            if !(4..=1_048_576).contains(&length) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid PostgreSQL response length",
                ));
            }
            let mut body = vec![0; (length - 4) as usize];
            server_read.read_exact(&mut body).await?;
            let acquired =
                body.len() == 7 && body[..6] == [0, 1, 0, 0, 0, 1] && matches!(body[6], 1 | b't');
            if downstream_seen.load(Ordering::SeqCst) && tag == b'D' && acquired {
                response_held.store(true, Ordering::SeqCst);
                held.notify_one();
                let _ = downstream_release.changed().await;
                return Ok(());
            }
            client_write.write_u8(tag).await?;
            client_write.write_u32(length).await?;
            client_write.write_all(&body).await?;
        }
    };
    tokio::select! { result = upstream => result, result = downstream => result }
}

async fn assert_catalog_locked(admin: &PgPool, expected_locked: bool) {
    let available: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(admin)
        .await
        .expect("probe catalog fence");
    assert_eq!(!available, expected_locked);
}

async fn exercise_pre_fence_idle_cap(url: &str, initial_idle: &str) {
    let f = Fixture::new(url).await;
    let target = PgConnectOptions::from_str(url).expect("PostgreSQL options");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let held = Arc::new(Notify::new());
    let (release, release_receiver) = watch::channel(false);
    let proxy = tokio::spawn(hold_catalog_lock_reply(
        listener,
        target,
        Arc::clone(&held),
        release_receiver,
    ));
    let scoped = url_with_options(
        url,
        &format!(
            "-c search_path={} -c idle_in_transaction_session_timeout={initial_idle}",
            f.schema
        ),
    );
    let mut proxy_url = postgres_proxy::local_proxy_url(&scoped, port).expect("proxy URL");
    let application = format!("vapid_idle_{}", uuid::Uuid::new_v4().simple());
    proxy_url
        .query_pairs_mut()
        .append_pair("application_name", &application);
    let mut child = spawn_child(proxy_url.as_str(), "fence_error", None);
    if tokio::time::timeout(Duration::from_secs(10), held.notified())
        .await
        .is_err()
    {
        if child.try_wait().expect("poll VAPID process").is_none() {
            child.kill().expect("terminate stalled VAPID process");
        }
        let output = child.wait_with_output().expect("read VAPID process output");
        proxy.abort();
        panic!(
            "proxy did not hold catalog response; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let pid: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name = $1",
    )
    .bind(&application)
    .fetch_one(&f.admin)
    .await
    .expect("identify fenced backend");
    assert_catalog_locked(&f.admin, true).await;
    let mut child_wait = tokio::task::spawn_blocking(move || {
        finish_child_with_timeout(child, Duration::from_secs(25));
    });
    if initial_idle == "0" {
        // The client times out first. The proxy deliberately retains the
        // server socket, so socket closure is not evidence of lock release.
        tokio::time::timeout(Duration::from_secs(25), &mut child_wait)
            .await
            .expect("client deadline")
            .expect("VAPID process wait");
        let still_present: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
        )
        .bind(pid)
        .fetch_one(&f.admin)
        .await
        .expect("inspect original backend");
        assert!(still_present, "backend must still be held by the proxy");
        assert_catalog_locked(&f.admin, true).await;
    }
    let backend_deadline = if initial_idle == "0" {
        Duration::from_secs(15)
    } else {
        Duration::from_secs(8)
    };
    tokio::time::timeout(backend_deadline, async {
        loop {
            let present: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
            )
            .bind(pid)
            .fetch_one(&f.admin)
            .await
            .expect("inspect idle backend");
            if !present {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("server idle transaction bound releases backend");
    assert_catalog_locked(&f.admin, false).await;
    release.send(true).expect("release held proxy sockets");
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .expect("proxy ends")
        .expect("join proxy")
        .expect("proxy I/O");
    if initial_idle != "0" {
        tokio::time::timeout(Duration::from_secs(8), &mut child_wait)
            .await
            .expect("tight-timeout client exits")
            .expect("VAPID process wait");
    }
    let absent: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NULL")
        .bind(format!("{}.push_service_vapid_keys", f.schema))
        .fetch_one(&f.admin)
        .await
        .expect("check no key mutation before fence acknowledgement");
    assert!(absent);
    run_child(&f.url, "reuse", None);
    assert_eq!(f.count().await, 1);
    f.cleanup().await;
}

#[tokio::test]
async fn pg17_catalog_lock_reply_stall_respects_idle_transaction_caps() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let target = PgConnectOptions::from_str(&url).expect("PostgreSQL options");
    if postgres_proxy::ensure_local_target(&target).is_err() {
        eprintln!("skipping plaintext proxy witness for nonlocal PostgreSQL target");
        return;
    }
    exercise_pre_fence_idle_cap(&url, "0").await;
    exercise_pre_fence_idle_cap(&url, "2000ms").await;
}
