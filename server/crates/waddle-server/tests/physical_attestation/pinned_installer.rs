use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, Row};

use super::{
    attestor_probe_was_called, create_attestor_probe, lose_commit_reply_proxy, postgres_proxy,
    test_url, Fixture, CATALOG_GRANT_LOCK,
};
use waddle_server::db::physical_attestation::{
    inspect_pinned_installation, prepare_pinned_install, DarkInstallationStatus,
    DarkInstallerError, DarkInstallerPins,
};

async fn installer_pins(fixture: &Fixture) -> DarkInstallerPins {
    sqlx::query(&format!(
        "GRANT SELECT ON {}._lineage TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("independently provision runtime lineage lock privilege");
    // These values are read by the fixture administrator before any hostile
    // mutation, never learned from the installer connection under test.
    let row = sqlx::query(
        "SELECT db.datdba::bigint, n.nspowner::bigint, r.oid::bigint, i.oid::bigint, \
                d.oid::bigint, trusted.oid::bigint, c.oid::bigint, idx.indexrelid::bigint \
         FROM pg_catalog.pg_database db \
         JOIN pg_catalog.pg_namespace n ON n.nspname=$1 \
         JOIN pg_catalog.pg_class c ON c.relnamespace=n.oid AND c.relname='_lineage' \
         JOIN pg_catalog.pg_index idx ON idx.indrelid=c.oid AND idx.indisprimary \
         JOIN pg_catalog.pg_namespace trusted ON trusted.nspname=$2 \
         JOIN pg_catalog.pg_roles r ON r.rolname=$3 \
         JOIN pg_catalog.pg_roles i ON i.rolname=$4 \
         JOIN pg_catalog.pg_roles d ON d.rolname=$5 \
         WHERE db.datname=pg_catalog.current_database()",
    )
    .bind(&fixture.target)
    .bind(&fixture.trusted)
    .bind(&fixture.runtime)
    .bind(&fixture.installer)
    .bind(&fixture.definer)
    .fetch_one(&fixture.admin)
    .await
    .expect("record independent administrator pins");
    let oid = |index| u32::try_from(row.get::<i64, _>(index)).expect("OID fits u32");
    DarkInstallerPins {
        database_owner_role_oid: oid(0),
        schema_owner_role_oid: oid(1),
        runtime_role_oid: oid(2),
        installer_role_oid: oid(3),
        definer_role_oid: oid(4),
        trusted_schema_oid: oid(5),
        lineage_relation_oid: oid(6),
        lineage_primary_index_oid: oid(7),
        existing_function_oid: None,
        operation_id: fixture.operation_id,
    }
}

fn installer_options(fixture: &Fixture) -> PgConnectOptions {
    PgConnectOptions::from_str(&fixture.base_url)
        .expect("fixture connection options")
        .username(&fixture.installer)
}

async fn commit_prepared(fixture: &Fixture, pins: DarkInstallerPins) -> DarkInstallationStatus {
    let prepared = prepare_pinned_install(&installer_options(fixture), &fixture.config(), pins)
        .await
        .expect("prepare pinned installation");
    assert_eq!(prepared.fence().operation_id, pins.operation_id);
    assert!(prepared.fence().backend_pid > 0);
    // A production caller durably records this exact fence before commit.
    let _recorded_fence = prepared.fence().clone();
    prepared
        .commit()
        .await
        .expect("commit and verify installer")
}

#[tokio::test]
async fn pinned_installer_first_install_retry_pins_and_acl_fail_closed() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pins = installer_pins(&fixture).await;
    let options = installer_options(&fixture);
    assert_eq!(
        inspect_pinned_installation(&options, &fixture.config(), pins)
            .await
            .expect("observe absent function"),
        DarkInstallationStatus::Indeterminate
    );
    let prepared = prepare_pinned_install(&options, &fixture.config(), pins)
        .await
        .expect("prepare first installation");
    let recorded_fence = prepared.fence().clone();
    let visible: i64 = sqlx::query_scalar(
        "SELECT pg_catalog.count(*) FROM pg_catalog.pg_proc p \
         WHERE p.pronamespace=$1 AND p.proname='read_physical_identity'",
    )
    .bind(sqlx::postgres::types::Oid(pins.trusted_schema_oid))
    .fetch_one(&fixture.admin)
    .await
    .expect("check visibility before commit");
    assert_eq!(visible, 0);
    let committed = prepared.commit().await.expect("commit first installation");
    let DarkInstallationStatus::Committed {
        observed_function_oid,
    } = committed
    else {
        panic!("installation unexpectedly indeterminate");
    };
    assert!(observed_function_oid != 0);
    assert_eq!(recorded_fence.operation_id, pins.operation_id);
    assert_eq!(
        inspect_pinned_installation(&options, &fixture.config(), pins)
            .await
            .expect("fresh exact readback"),
        committed
    );
    let mut pinned = pins;
    pinned.existing_function_oid = Some(observed_function_oid);
    assert_eq!(commit_prepared(&fixture, pinned).await, committed);
    pinned.existing_function_oid = Some(observed_function_oid ^ 1);
    assert!(matches!(
        inspect_pinned_installation(&options, &fixture.config(), pinned).await,
        Err(DarkInstallerError::Conflict)
    ));
    let mut conflict = pins;
    conflict.operation_id = uuid::Uuid::new_v4();
    assert!(matches!(
        prepare_pinned_install(&options, &fixture.config(), conflict).await,
        Err(DarkInstallerError::Conflict)
    ));
    sqlx::query(&format!(
        "GRANT UPDATE ON {}._lineage TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("introduce runtime write grant drift");
    assert!(matches!(
        inspect_pinned_installation(&options, &fixture.config(), pins).await,
        Err(DarkInstallerError::CatalogDrift)
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_installer_rejects_view_and_cast_before_they_execute() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url.clone()).await;
    let pins = installer_pins(&fixture).await;
    create_attestor_probe(&fixture).await;
    sqlx::query(&format!(
        "CREATE FUNCTION {}.attestor_probe_fn() RETURNS integer LANGUAGE sql VOLATILE \
         SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS \
         'SELECT pg_catalog.nextval(''{}.attestor_probe''::pg_catalog.regclass)::integer'",
        fixture.target, fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("create view execution witness");
    sqlx::query(&format!(
        "ALTER TABLE {}._lineage RENAME TO original_lineage",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("rename original heap");
    sqlx::query(&format!(
        "CREATE VIEW {}._lineage AS SELECT l.id + {}.attestor_probe_fn() AS id, \
         l.format,l.lineage_uuid,l.deployment_uuid,l.pg_system_identifier,l.pg_database_oid, \
         l.pg_database_name,l.pg_schema_oid,l.pg_schema_name,l.stamped_at \
         FROM {}.original_lineage l",
        fixture.target, fixture.target, fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("replace lineage with hostile view");
    for role in [&fixture.runtime, &fixture.definer] {
        sqlx::query(&format!(
            "GRANT SELECT ON {}._lineage TO {}",
            fixture.target, role
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant view access");
    }
    assert!(matches!(
        prepare_pinned_install(&installer_options(&fixture), &fixture.config(), pins).await,
        Err(DarkInstallerError::CatalogDrift)
    ));
    assert!(!attestor_probe_was_called(&fixture).await);
    fixture.cleanup().await;

    let fixture = Fixture::new(url).await;
    let pins = installer_pins(&fixture).await;
    create_attestor_probe(&fixture).await;
    sqlx::query(&format!(
        "CREATE FUNCTION {}.attestor_cast(text) RETURNS bigint LANGUAGE sql VOLATILE \
         SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS \
         'SELECT pg_catalog.nextval(''{}.attestor_probe''::pg_catalog.regclass)'",
        fixture.target, fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("create cast witness");
    sqlx::query(&format!(
        "CREATE CAST (text AS bigint) WITH FUNCTION {}.attestor_cast(text) AS IMPLICIT",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("install hostile cast");
    sqlx::query(&format!(
        "ALTER TABLE {}._lineage ALTER COLUMN format TYPE text USING format::text",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("replace lineage format type");
    assert!(matches!(
        prepare_pinned_install(&installer_options(&fixture), &fixture.config(), pins).await,
        Err(DarkInstallerError::CatalogDrift)
    ));
    assert!(!attestor_probe_was_called(&fixture).await);
    sqlx::query("SELECT '1'::text::bigint")
        .execute(&fixture.admin)
        .await
        .expect("hostile cast would execute");
    assert!(attestor_probe_was_called(&fixture).await);
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_installer_never_uses_inherited_hostile_operator() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pins = installer_pins(&fixture).await;
    create_attestor_probe(&fixture).await;
    let poison = format!("{}_poison", fixture.target);
    sqlx::query(&format!("CREATE SCHEMA {poison}"))
        .execute(&fixture.admin)
        .await
        .expect("create hostile schema");
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA {poison} TO {}",
        fixture.installer
    ))
    .execute(&fixture.admin)
    .await
    .expect("expose hostile schema");
    sqlx::query(&format!(
        r#"CREATE FUNCTION {poison}.poison_equal(pg_catalog.name, pg_catalog.text)
RETURNS boolean LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path=pg_catalog,pg_temp
AS $body$ BEGIN
  PERFORM pg_catalog.nextval('{}.attestor_probe'::pg_catalog.regclass);
  RETURN $1::pg_catalog.text OPERATOR(pg_catalog.=) $2;
END $body$"#,
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("create hostile operator function");
    sqlx::query(&format!(
        "CREATE OPERATOR {poison}.= (FUNCTION={poison}.poison_equal, \
         LEFTARG=pg_catalog.name, RIGHTARG=pg_catalog.text)"
    ))
    .execute(&fixture.admin)
    .await
    .expect("create hostile operator");
    sqlx::query(&format!(
        "ALTER ROLE {} SET search_path={poison},pg_catalog",
        fixture.installer
    ))
    .execute(&fixture.admin)
    .await
    .expect("install hostile inherited path");
    let mut direct = PgConnection::connect_with(&installer_options(&fixture))
        .await
        .expect("connect with hostile path");
    let equal: bool =
        sqlx::query_scalar("SELECT 'same'::pg_catalog.name = 'same'::pg_catalog.text")
            .fetch_one(&mut direct)
            .await
            .expect("hostile operator executes outside installer");
    assert!(equal && attestor_probe_was_called(&fixture).await);
    drop(direct);
    sqlx::query(&format!(
        "SELECT pg_catalog.setval('{}.attestor_probe'::pg_catalog.regclass, 1, false)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("reset execution witness");
    let prepared = prepare_pinned_install(&installer_options(&fixture), &fixture.config(), pins)
        .await
        .expect("installer uses catalog-first path");
    assert!(!attestor_probe_was_called(&fixture).await);
    drop(prepared);
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_installer_rejects_replaced_function_without_invocation() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pins = installer_pins(&fixture).await;
    commit_prepared(&fixture, pins).await;
    create_attestor_probe(&fixture).await;
    sqlx::query(&format!(
        r#"CREATE OR REPLACE FUNCTION {}.read_physical_identity()
RETURNS TABLE (
    lineage_uuid text, deployment_uuid text,
    stored_system_identifier text, stored_database_oid text, stored_database_name text,
    stored_schema_oid text, stored_schema_name text,
    live_system_identifier text, live_database_oid text, live_database_name text,
    live_schema_oid text, live_schema_name text, stored_lineage_format bigint
)
LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $body$ SELECT pg_catalog.nextval('{}.attestor_probe'::pg_catalog.regclass)::text,
    'x'::text, 'x'::text, 'x'::text, 'x'::text, 'x'::text, 'x'::text,
    'x'::text, 'x'::text, 'x'::text, 'x'::text, 'x'::text, 1::bigint $body$"#,
        fixture.trusted, fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("replace function while preserving OID, marker and ACL");
    assert!(matches!(
        inspect_pinned_installation(&installer_options(&fixture), &fixture.config(), pins).await,
        Err(DarkInstallerError::Conflict)
    ));
    assert!(!attestor_probe_was_called(&fixture).await);
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_installer_rejects_enabled_event_trigger_before_ddl() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pins = installer_pins(&fixture).await;
    create_attestor_probe(&fixture).await;
    sqlx::query(&format!(
        "CREATE FUNCTION {}.ddl_probe() RETURNS event_trigger LANGUAGE plpgsql \
         SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS \
         $body$ BEGIN PERFORM pg_catalog.nextval('{}.attestor_probe'::pg_catalog.regclass); END $body$",
        fixture.target, fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("create event trigger witness");
    sqlx::query(&format!(
        "CREATE EVENT TRIGGER pinned_installer_probe ON ddl_command_start \
         EXECUTE FUNCTION {}.ddl_probe()",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("install hostile event trigger");
    assert!(matches!(
        prepare_pinned_install(&installer_options(&fixture), &fixture.config(), pins).await,
        Err(DarkInstallerError::CatalogDrift)
    ));
    assert!(!attestor_probe_was_called(&fixture).await);
    sqlx::query(&format!(
        "CREATE TABLE {}.event_trigger_witness (id integer)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("confirm event trigger would execute on DDL");
    assert!(attestor_probe_was_called(&fixture).await);
    sqlx::query("DROP EVENT TRIGGER pinned_installer_probe")
        .execute(&fixture.admin)
        .await
        .expect("remove test event trigger");
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_installer_rechecks_relation_after_waiting_on_replacement() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pins = installer_pins(&fixture).await;
    let mut holder = fixture.admin.begin().await.expect("begin DDL holder");
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .expect("holder PID");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}._lineage IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *holder)
    .await
    .expect("lock original lineage");
    let app_name = format!("pinned_install_{}", uuid::Uuid::new_v4().simple());
    let options = installer_options(&fixture).application_name(&app_name);
    let config = fixture.config();
    let task = tokio::spawn(async move { prepare_pinned_install(&options, &config, pins).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name=$1 AND $2=ANY(pg_catalog.pg_blocking_pids(a.pid)))",
            )
            .bind(&app_name)
            .bind(holder_pid)
            .fetch_one(&fixture.admin)
            .await
            .expect("observe blocked installer");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("installer must wait on original relation");
    sqlx::query(&format!(
        "ALTER TABLE {}._lineage RENAME TO original_lineage",
        fixture.target
    ))
    .execute(&mut *holder)
    .await
    .expect("rename original");
    sqlx::query(&format!(
        "CREATE TABLE {}._lineage (LIKE {}.original_lineage INCLUDING ALL)",
        fixture.target, fixture.target
    ))
    .execute(&mut *holder)
    .await
    .expect("create matching replacement");
    sqlx::query(&format!(
        "INSERT INTO {}._lineage SELECT * FROM {}.original_lineage",
        fixture.target, fixture.target
    ))
    .execute(&mut *holder)
    .await
    .expect("copy matching values");
    for role in [&fixture.runtime, &fixture.definer] {
        sqlx::query(&format!(
            "GRANT SELECT ON {}._lineage TO {}",
            fixture.target, role
        ))
        .execute(&mut *holder)
        .await
        .expect("copy lineage grants");
    }
    holder.commit().await.expect("publish replacement");
    assert!(matches!(
        task.await.expect("join installer"),
        Err(DarkInstallerError::CatalogDrift)
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_installer_lost_commit_ack_keeps_original_fence() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pins = installer_pins(&fixture).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind commit proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let reply_held = Arc::new(tokio::sync::Notify::new());
    let disconnect = Arc::new(tokio::sync::Notify::new());
    let proxy = tokio::spawn(lose_commit_reply_proxy(
        listener,
        fixture.admin_options.clone(),
        reply_held.clone(),
        disconnect.clone(),
    ));
    let mut proxy_url =
        postgres_proxy::local_proxy_url(&fixture.base_url, port).expect("proxy URL");
    proxy_url
        .query_pairs_mut()
        .append_pair("user", &fixture.installer)
        .append_pair("sslmode", "disable");
    let options = PgConnectOptions::from_str(proxy_url.as_str()).expect("proxy installer options");
    let prepared = prepare_pinned_install(&options, &fixture.config(), pins)
        .await
        .expect("prepare via proxy");
    let recorded_fence = prepared.fence().clone();
    let pending = tokio::spawn(async move { prepared.commit().await });
    tokio::time::timeout(Duration::from_secs(5), reply_held.notified())
        .await
        .expect("hold COMMIT acknowledgement");
    let observed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_proc p \
         WHERE p.pronamespace=$1 AND p.proname='read_physical_identity' \
           AND pg_catalog.obj_description(p.oid, 'pg_proc') LIKE $2)",
    )
    .bind(sqlx::postgres::types::Oid(pins.trusted_schema_oid))
    .bind(format!("waddle-attestation-v1:{}:%", pins.operation_id))
    .fetch_one(&fixture.admin)
    .await
    .expect("observe committed marker");
    assert!(observed);
    disconnect.notify_one();
    proxy.await.expect("join proxy").expect("proxy I/O");
    assert!(matches!(
        pending.await.expect("join installer"),
        Err(DarkInstallerError::CommitUnknown { fence }) if fence == recorded_fence
    ));
    let observed =
        inspect_pinned_installation(&installer_options(&fixture), &fixture.config(), pins)
            .await
            .expect("same-operation fresh reconciliation");
    assert!(matches!(observed, DarkInstallationStatus::Committed { .. }));
    assert_eq!(commit_prepared(&fixture, pins).await, observed);
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_installer_cancelled_commit_retains_pre_recorded_fence() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pins = installer_pins(&fixture).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind commit proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let reply_held = Arc::new(tokio::sync::Notify::new());
    let disconnect = Arc::new(tokio::sync::Notify::new());
    let proxy = tokio::spawn(lose_commit_reply_proxy(
        listener,
        fixture.admin_options.clone(),
        reply_held.clone(),
        disconnect.clone(),
    ));
    let mut proxy_url =
        postgres_proxy::local_proxy_url(&fixture.base_url, port).expect("proxy URL");
    proxy_url
        .query_pairs_mut()
        .append_pair("user", &fixture.installer)
        .append_pair("sslmode", "disable");
    let options = PgConnectOptions::from_str(proxy_url.as_str()).expect("proxy installer options");
    let prepared = prepare_pinned_install(&options, &fixture.config(), pins)
        .await
        .expect("prepare via proxy");
    // The caller records this before handing the consuming commit future to a
    // cancellable task. Cancellation cannot erase its original identity.
    let recorded_fence = prepared.fence().clone();
    let pending = tokio::spawn(async move { prepared.commit().await });
    tokio::time::timeout(Duration::from_secs(5), reply_held.notified())
        .await
        .expect("hold actual COMMIT acknowledgement");
    let observed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_proc p \
         WHERE p.pronamespace=$1 AND p.proname='read_physical_identity' \
           AND pg_catalog.obj_description(p.oid, 'pg_proc') LIKE $2)",
    )
    .bind(sqlx::postgres::types::Oid(pins.trusted_schema_oid))
    .bind(format!("waddle-attestation-v1:{}:%", pins.operation_id))
    .fetch_one(&fixture.admin)
    .await
    .expect("observe committed marker");
    assert!(observed);
    pending.abort();
    assert!(pending
        .await
        .expect_err("commit future cancelled")
        .is_cancelled());
    disconnect.notify_one();
    proxy.await.expect("join proxy").expect("proxy I/O");
    assert_eq!(recorded_fence.operation_id, pins.operation_id);
    assert!(recorded_fence.backend_pid > 0);
    assert!(matches!(
        inspect_pinned_installation(&installer_options(&fixture), &fixture.config(), pins).await,
        Ok(DarkInstallationStatus::Committed { .. })
    ));
    fixture.cleanup().await;
}
