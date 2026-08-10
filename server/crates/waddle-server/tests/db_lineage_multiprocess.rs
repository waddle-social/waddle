//! Database-lineage readiness coverage using real server subprocesses.
//!
//! PostgreSQL is optional in local development, so every test skips unless
//! `WADDLE_TEST_POSTGRES_URL` is configured. Each test gets UUID-named schemas:
//! a schema is a lineage boundary because its identity is part of the attestation.

use std::{process::Command, sync::OnceLock, time::Duration};

use waddle_ws_test_support::TestServer;

const READINESS_TIMEOUT: Duration = Duration::from_secs(60);

/// These tests create durable tables and run several real server processes.
/// Serializing them keeps the optional shared PostgreSQL fixture responsive.
fn serial_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

struct SchemaFixture {
    admin: sqlx::PgPool,
    primary_admin: sqlx::PgPool,
    schemas: Vec<String>,
    database_url: String,
}

impl SchemaFixture {
    async fn new(prefix: &str, database_url: String) -> Self {
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect PostgreSQL test admin pool");
        let primary_schema = schema_name(prefix);
        sqlx::query(&format!("CREATE SCHEMA {primary_schema}"))
            .execute(&admin)
            .await
            .expect("create isolated PostgreSQL schema");
        let primary_url = postgres_url_with_search_path(&database_url, &primary_schema);
        let primary_admin = sqlx::PgPool::connect(&primary_url)
            .await
            .expect("connect isolated PostgreSQL schema admin pool");
        Self {
            admin,
            primary_admin,
            schemas: vec![primary_schema],
            database_url,
        }
    }

    async fn boundary(&mut self, prefix: &str) -> String {
        let schema = schema_name(prefix);
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&self.admin)
            .await
            .expect("create isolated PostgreSQL schema");
        self.schemas.push(schema.clone());
        postgres_url_with_search_path(&self.database_url, &schema)
    }

    fn boundary_url(&self) -> String {
        let schema = self
            .schemas
            .first()
            .expect("fixture creates its primary schema");
        postgres_url_with_search_path(&self.database_url, schema)
    }

    async fn lineage_uuid(&self) -> String {
        sqlx::query_scalar("SELECT lineage_uuid FROM _lineage WHERE id = 1")
            .fetch_one(&self.primary_admin)
            .await
            .expect("read enrolled lineage UUID")
    }

    async fn lineage_row_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM _lineage")
            .fetch_one(&self.primary_admin)
            .await
            .expect("count lineage rows")
    }

    async fn cleanup(self) {
        let Self {
            admin,
            primary_admin,
            schemas,
            ..
        } = self;
        primary_admin.close().await;
        for schema in schemas {
            sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
                .execute(&admin)
                .await
                .expect("drop isolated PostgreSQL schema");
        }
        admin.close().await;
    }
}

fn schema_name(prefix: &str) -> String {
    format!(
        "waddle_test_lineage_multiprocess_{prefix}_{}",
        uuid::Uuid::new_v4().simple()
    )
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse PostgreSQL URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}

fn lineage_envs(
    database_url: &str,
    deployment_uuid: &str,
    action: Option<&str>,
    mam_database_url: Option<&str>,
) -> Vec<(String, String)> {
    let mut envs = vec![
        ("WADDLE_DB_DRIVER".to_string(), "postgres".to_string()),
        ("WADDLE_DATABASE_URL".to_string(), database_url.to_string()),
        (
            "WADDLE_DEPLOYMENT_UUID".to_string(),
            deployment_uuid.to_string(),
        ),
    ];
    if let Some(action) = action {
        envs.push(("WADDLE_DB_LINEAGE_ACTION".to_string(), action.to_string()));
    }
    if let Some(mam_database_url) = mam_database_url {
        envs.push((
            "WADDLE_XMPP_MAM_DATABASE_URL".to_string(),
            mam_database_url.to_string(),
        ));
    }
    envs
}

async fn spawn_server(envs: Vec<(String, String)>) -> TestServer {
    tokio::task::spawn_blocking(move || {
        let env_refs: Vec<(&str, &str)> = envs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        TestServer::start_with_extra_envs(&[], &env_refs)
    })
    .await
    .expect("server spawn task completes")
}

async fn wait_for_attested(server: &TestServer) {
    let deadline = tokio::time::Instant::now() + READINESS_TIMEOUT;
    let client = reqwest::Client::new();
    let mut last = String::new();
    while tokio::time::Instant::now() < deadline {
        match client
            .get(format!("{}/readyz", server.http_base_url()))
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                let body = response
                    .json::<serde_json::Value>()
                    .await
                    .expect("decode readiness JSON");
                if status == reqwest::StatusCode::OK && body["lineage"] == "attested" {
                    return;
                }
                last = format!("status={status}, body={body}");
            }
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("server did not become lineage-attested: {last}");
}

async fn wait_for_lineage_failure(server: &TestServer, store: &str, expected: &str) {
    let deadline = tokio::time::Instant::now() + READINESS_TIMEOUT;
    let client = reqwest::Client::new();
    let mut last = String::new();
    while tokio::time::Instant::now() < deadline {
        match client
            .get(format!("{}/readyz", server.http_base_url()))
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                let body = response
                    .json::<serde_json::Value>()
                    .await
                    .expect("decode readiness JSON");
                if status == reqwest::StatusCode::SERVICE_UNAVAILABLE
                    && body["lineage"][store] == expected
                {
                    return;
                }
                last = format!("status={status}, body={body}");
            }
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("server did not report lineage {store}={expected}: {last}");
}

async fn assert_liveness(server: &TestServer) {
    let response = reqwest::Client::new()
        .get(format!("{}/healthz", server.http_base_url()))
        .send()
        .await
        .expect("request liveness endpoint");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
}

/// Startup errors from an invalid adopt action occur before the HTTP listener
/// is bound, so this path intentionally mirrors TestServer's env-driven spawn
/// but observes the process's clean non-success exit directly.
async fn spawn_invalid_adopt_and_wait_for_exit(envs: Vec<(String, String)>) {
    let status = tokio::task::spawn_blocking(move || {
        let binary = env!("CARGO_BIN_EXE_waddle-server");
        let port_file = std::env::temp_dir().join(format!(
            "waddle-lineage-invalid-adopt-port-{}",
            uuid::Uuid::new_v4()
        ));
        let upload_dir = std::env::temp_dir().join(format!(
            "waddle-lineage-invalid-adopt-uploads-{}",
            uuid::Uuid::new_v4()
        ));
        let mut command = Command::new(binary);
        for (key, _) in std::env::vars() {
            if key.starts_with("WADDLE_") || key.starts_with("OTEL_") {
                command.env_remove(key);
            }
        }
        command
            .env("WADDLE_CERTS_EPHEMERAL", "true")
            .env("WADDLE_TEST_FIXED_ACCOUNT_ENABLED", "true")
            .env(
                "WADDLE_TEST_FIXED_ACCOUNT_PASSWORD",
                "lineage-test-password",
            )
            .env("WADDLE_TEST_PROFILE_PUBLISH_TOKEN", "lineage-test-token")
            .env("WADDLE_SERVER_OWNER_LOCALPARTS", "admin")
            .env("WADDLE_HTTP_ADDR", "127.0.0.1:0")
            .env("WADDLE_XMPP_DOMAIN", "localhost")
            .env("WADDLE_XMPP_PUSH_SERVICE_ALLOW_IN_MEMORY", "true")
            .env("WADDLE_XMPP_MAM_ALLOW_IN_MEMORY", "true")
            .env("WADDLE_XMPP_MAM_DATABASE_URL", "sqlite::memory:")
            .env("WADDLE_XMPP_INBOX_DATABASE_URL", "sqlite::memory:")
            .env("WADDLE_XMPP_SM_DATABASE_URL", "sqlite::memory:")
            .env(
                "WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL",
                "sqlite::memory:",
            )
            .env("WADDLE_XMPP_PUBSUB_DATABASE_URL", "sqlite::memory:")
            .env("WADDLE_UPLOAD_DIR", &upload_dir)
            .env("WADDLE_GIT_SHA", waddle_ws_test_support::TEST_GIT_SHA)
            .env(
                "WADDLE_SESSION_KEY",
                "integration-test-session-key-32-bytes-long",
            )
            .env(
                "WADDLE_OCCUPANT_ID_SECRET",
                "integration-test-occupant-id-secret-32-bytes-long",
            )
            .env("WADDLE_HTTP_PORT_FILE", &port_file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        for (key, value) in envs {
            command.env(key, value);
        }
        let status = command
            .spawn()
            .expect("spawn invalid adopt server")
            .wait()
            .expect("wait for invalid adopt server exit");
        let _ = std::fs::remove_file(port_file);
        let _ = std::fs::remove_dir_all(upload_dir);
        status
    })
    .await
    .expect("invalid adopt process task completes");
    assert!(
        !status.success(),
        "invalid adoption must fail startup cleanly"
    );
}

#[tokio::test]
async fn two_replicas_same_database_both_ready() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let _serial = serial_lock().lock().await;
    let fixture = SchemaFixture::new("same_database", postgres_url).await;
    let deployment_uuid = uuid::Uuid::new_v4().to_string();

    let first = spawn_server(lineage_envs(
        &fixture.boundary_url(),
        &deployment_uuid,
        Some("enroll"),
        None,
    ))
    .await;
    wait_for_attested(&first).await;

    let second = spawn_server(lineage_envs(
        &fixture.boundary_url(),
        &deployment_uuid,
        None,
        None,
    ))
    .await;
    wait_for_attested(&second).await;

    drop(second);
    drop(first);
    fixture.cleanup().await;
}

#[tokio::test]
async fn unenrolled_database_stays_unready() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let _serial = serial_lock().lock().await;
    let fixture = SchemaFixture::new("unenrolled", postgres_url).await;
    let deployment_uuid = uuid::Uuid::new_v4().to_string();
    let mut server = spawn_server(lineage_envs(
        &fixture.boundary_url(),
        &deployment_uuid,
        None,
        None,
    ))
    .await;

    wait_for_lineage_failure(&server, "global", "missing_lineage").await;
    assert!(!server.wait_for_exit(Duration::from_secs(1)).await);
    assert_liveness(&server).await;

    drop(server);
    fixture.cleanup().await;
}

#[tokio::test]
async fn split_deployment_rejected() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let _serial = serial_lock().lock().await;
    let mut fixture = SchemaFixture::new("split_deployment_global", postgres_url).await;
    let database_a = fixture.boundary_url();
    let database_b = fixture.boundary("split_deployment_mam").await;
    let deployment_one = uuid::Uuid::new_v4().to_string();
    let deployment_two = uuid::Uuid::new_v4().to_string();

    // In single-node mode, separately enrolled durable stores are legitimate.
    // The clustered colocation rule is covered by the PG-gated lineage registry
    // unit test because the full cluster bootstrap is deliberately feature-gated.
    let enrolled = spawn_server(lineage_envs(
        &database_a,
        &deployment_one,
        Some("enroll"),
        Some(&database_b),
    ))
    .await;
    wait_for_attested(&enrolled).await;

    let conflicting = spawn_server(lineage_envs(&database_a, &deployment_two, None, None)).await;
    wait_for_lineage_failure(&conflicting, "global", "deployment_uuid_mismatch").await;

    let independently_provisioned = spawn_server(lineage_envs(
        &database_a,
        &deployment_one,
        None,
        Some(&database_b),
    ))
    .await;
    wait_for_attested(&independently_provisioned).await;

    drop(independently_provisioned);
    drop(conflicting);
    drop(enrolled);
    fixture.cleanup().await;
}

#[tokio::test]
async fn enroll_action_is_one_shot_and_idempotent() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let _serial = serial_lock().lock().await;
    let fixture = SchemaFixture::new("enroll_idempotent", postgres_url).await;
    let deployment_uuid = uuid::Uuid::new_v4().to_string();
    let database_url = fixture.boundary_url();

    let first = spawn_server(lineage_envs(
        &database_url,
        &deployment_uuid,
        Some("enroll"),
        None,
    ))
    .await;
    wait_for_attested(&first).await;
    let first_uuid = fixture.lineage_uuid().await;
    assert_eq!(fixture.lineage_row_count().await, 1);
    drop(first);

    let second = spawn_server(lineage_envs(
        &database_url,
        &deployment_uuid,
        Some("enroll"),
        None,
    ))
    .await;
    wait_for_attested(&second).await;
    assert_eq!(fixture.lineage_uuid().await, first_uuid);
    assert_eq!(fixture.lineage_row_count().await, 1);
    drop(second);

    let wrong = uuid::Uuid::new_v4();
    let invalid_adopt_action = format!("adopt={wrong}");
    spawn_invalid_adopt_and_wait_for_exit(lineage_envs(
        &database_url,
        &deployment_uuid,
        Some(&invalid_adopt_action),
        None,
    ))
    .await;

    fixture.cleanup().await;
}
