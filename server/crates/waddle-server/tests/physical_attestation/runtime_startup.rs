//! PG17 and real-binary checks for restricted startup preparation.

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::PgConnectOptions;
use sqlx::Row;
use uuid::Uuid;
use waddle_server::config::LineageConfig;
use waddle_server::db::bootstrap::RuntimePoolLimits;
use waddle_server::db::runtime_startup::RuntimeStartupIdentity;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};

use super::{dark_attestor_fixture, test_url, url_with_schema, Fixture, CATALOG_GRANT_LOCK};

const RELEASE_GIT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

#[tokio::test]
async fn restricted_startup_runs_pinned_preflight_then_refuses_store_activation() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(base_url) = test_url() else { return };
    let fixture = Fixture::new(base_url).await;
    let (document, database_url) = runtime_handoff(&fixture).await;
    let lineage = LineageConfig {
        deployment_uuid: Some(fixture.lineage.deployment_uuid),
        action: None,
    };
    let limits = RuntimePoolLimits {
        main_connections: 1,
        ingress_connections: 1,
        acquire_timeout: Duration::from_secs(5),
    };
    let mut wrong_socket_document: serde_json::Value =
        serde_json::from_str(&document).expect("structured handoff JSON");
    wrong_socket_document["cutover"]["primary"]["endpoint_socket_dir"] =
        "/private/wrong-socket".into();
    let wrong_socket_identity = RuntimeStartupIdentity::from_json(
        &wrong_socket_document.to_string(),
        &lineage,
        RELEASE_GIT_SHA,
    )
    .expect("valid but mismatched socket handoff");
    assert!(matches!(
        wrong_socket_identity.preflight(&database_url, limits).await,
        Err(waddle_server::db::runtime_startup::RuntimeStartupError::PrimaryEndpointMismatch)
    ));
    let identity = RuntimeStartupIdentity::from_json(&document, &lineage, RELEASE_GIT_SHA)
        .expect("v3 operator procedure handoff");

    let preparation = identity
        .preflight(&database_url, limits)
        .await
        .expect("pinned preflight on the runtime login");
    assert!(preparation.transition_id() != Uuid::nil());

    let temp = tempfile::tempdir().expect("startup gate directory");
    let port_file = temp.path().join("port");
    let mut server = tokio::process::Command::new(env!("CARGO_BIN_EXE_waddle-server"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("WADDLE_")
            || key.to_string_lossy().starts_with("OTEL_")
        {
            server.env_remove(key);
        }
    }
    server
        .env(
            "WADDLE_SESSION_KEY",
            "integration-test-session-key-32-bytes-long",
        )
        .env(
            "WADDLE_OCCUPANT_ID_SECRET",
            "integration-test-occupant-id-secret-32-bytes-long",
        )
        .env("WADDLE_DB_DRIVER", "postgres")
        .env("WADDLE_DATABASE_URL", &database_url)
        .env(
            "WADDLE_DEPLOYMENT_UUID",
            fixture.lineage.deployment_uuid.to_string(),
        )
        .env("WADDLE_GIT_SHA", RELEASE_GIT_SHA)
        .env("WADDLE_DB_RUNTIME_ATTESTATION", &document)
        .env("WADDLE_HTTP_ADDR", "127.0.0.1:0")
        .env("WADDLE_HTTP_PORT_FILE", &port_file)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(40), server.output())
        .await
        .expect("restricted startup finishes")
        .expect("start server binary");
    assert!(
        !output.status.success(),
        "restricted startup remains closed after preparation"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let output_text = format!("{stdout}{stderr}");
    assert!(
        output_text.contains("enabled stores and transaction entry remain unconverted"),
        "startup must retain the all-store refusal: {output_text}"
    );
    assert!(
        !port_file.exists(),
        "the HTTP listener must not be published"
    );

    fixture.cleanup().await;
}

async fn runtime_handoff(fixture: &Fixture) -> (String, String) {
    let database_url = runtime_database_url(fixture);
    let migration_url = url_with_schema(&fixture.base_url, &fixture.target);
    let owner = Database::from_config(
        "restricted startup fixture owner",
        &DatabaseConfig::new(DatabaseDriver::Postgres, migration_url),
    )
    .await
    .expect("owner fixture database");
    MigrationRunner::single()
        .run(&owner)
        .await
        .expect("apply V1030 ledger");
    drop(owner);

    for relation in [
        "_migrations",
        "ingress_protocol_epoch",
        "ingress_epoch_guard_manifest",
    ] {
        sqlx::query(&format!(
            "GRANT SELECT ON {}.{relation} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant runtime read-only pinned entry access");
    }
    let (_config, attestor) = dark_attestor_fixture(fixture).await;
    let pins_row = sqlx::query(
        "SELECT ledger.oid::bigint, ledger_idx.indexrelid::bigint, \
                epoch.oid::bigint, epoch_idx.indexrelid::bigint, manifest.oid::bigint, \
                old_runtime.oid::bigint \
         FROM pg_catalog.pg_namespace n \
         JOIN pg_catalog.pg_class ledger ON ledger.relnamespace=n.oid AND ledger.relname='_migrations' \
         JOIN pg_catalog.pg_index ledger_idx ON ledger_idx.indrelid=ledger.oid AND ledger_idx.indisprimary \
         JOIN pg_catalog.pg_class epoch ON epoch.relnamespace=n.oid AND epoch.relname='ingress_protocol_epoch' \
         JOIN pg_catalog.pg_index epoch_idx ON epoch_idx.indrelid=epoch.oid AND epoch_idx.indisprimary \
         JOIN pg_catalog.pg_class manifest ON manifest.relnamespace=n.oid AND manifest.relname='ingress_epoch_guard_manifest' \
         JOIN pg_catalog.pg_roles old_runtime ON old_runtime.rolname=$2 \
         WHERE n.nspname=$1",
    )
    .bind(&fixture.target)
    .bind(&fixture.outsider)
    .fetch_one(&fixture.admin)
    .await
    .expect("independently record startup pins and old role");
    let oid = |index| u32::try_from(pins_row.get::<i64, _>(index)).expect("OID fits u32");
    let physical = fixture
        .lineage
        .postgres_identity
        .as_ref()
        .expect("physical PostgreSQL identity");
    let endpoint = PgConnectOptions::from_str(&database_url).expect("runtime URL options");
    let document = serde_json::json!({
        "version": 3,
        "release_git_sha": RELEASE_GIT_SHA,
        "deployment_uuid": fixture.lineage.deployment_uuid.to_string(),
        "lineage_uuid": fixture.lineage.lineage_uuid.to_string(),
        "system_identifier": physical.system_identifier.to_string(),
        "database_oid": physical.database.oid,
        "database_name": physical.database.name.0,
        "schema_oid": physical.schema.oid,
        "schema_name": physical.schema.name.0,
        "installer_login": fixture.installer,
        "runtime_login": fixture.runtime,
        "definer_role": fixture.definer,
        "trusted_schema": fixture.trusted,
        "cutover": {
            "transition_id": Uuid::new_v4().to_string(),
            "primary": {
                "provider": "test-postgresql",
                "cluster_id": "disposable-cluster",
                "instance_id": "disposable-primary",
                "endpoint_host": endpoint.get_host(),
                "endpoint_port": endpoint.get_port(),
                "endpoint_socket_dir": endpoint.get_socket().map(|path| path.to_string_lossy().into_owned()),
                "observation_record_id": Uuid::new_v4().to_string(),
            },
            "old_runtime": {
                "login": fixture.outsider,
                "role_oid": oid(5),
                "credential_cutoff_record_id": Uuid::new_v4().to_string(),
            },
            "listener_cutoff_record_id": Uuid::new_v4().to_string(),
            "backend_cutoff_record_id": Uuid::new_v4().to_string(),
            "prepared_transaction_cutoff_record_id": Uuid::new_v4().to_string(),
            "mutator_policy": {
                "mode": "quiesced_for_serving_interval",
                "record_id": Uuid::new_v4().to_string(),
            },
        },
        "pins": {
            "database_owner_role_oid": attestor.database_owner_role_oid,
            "schema_owner_role_oid": attestor.schema_owner_role_oid,
            "runtime_role_oid": attestor.runtime_role_oid,
            "installer_role_oid": attestor.installer_role_oid,
            "definer_role_oid": attestor.definer_role_oid,
            "trusted_schema_oid": attestor.trusted_schema_oid,
            "lineage_relation_oid": attestor.lineage_relation_oid,
            "lineage_primary_index_oid": attestor.lineage_primary_index_oid,
            "attestor_function_oid": attestor.attestor_function_oid,
            "installation_operation_id": attestor.installation_operation_id.to_string(),
            "ledger_relation_oid": oid(0),
            "ledger_primary_index_oid": oid(1),
            "epoch_relation_oid": oid(2),
            "epoch_primary_index_oid": oid(3),
            "guard_manifest_relation_oid": oid(4),
        }
    });
    (document.to_string(), database_url)
}

fn runtime_database_url(fixture: &Fixture) -> String {
    let mut url = url::Url::parse(&url_with_schema(&fixture.base_url, &fixture.target))
        .expect("fixture database URL");
    let retained = url
        .query_pairs()
        .filter(|(key, _)| key != "user")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("user", &fixture.runtime);
    url.to_string()
}
