//! Restricted-role owner-rejection grants on the real attested ingress lane.

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_server::config::{IngressConfig, LineageConfig};
use waddle_server::db::physical_attestation::{attested_pool, PgRoleName};
use waddle_server::db::{
    grant_dark_owner_rejection_runtime_access, inspect_dark_owner_rejection_runtime_access,
    DarkMigrationOwnerConfig, DarkMigrationOwnerError, Database, DatabasePool,
    OwnerRejectionGrantError, OwnerRejectionGrantState, Transaction,
};
use waddle_server::ingress::{IngressAuthority, IngressStartupError};
use waddle_server::ingress_uow::IngressUnitOfWork;

use super::{
    prepare_candidate_application_fixture, test_url, url_with_search_path, Fixture,
    CATALOG_GRANT_LOCK,
};

async fn guarded<'a>(database: &'a Database, schema: &str) -> Transaction<'a> {
    let mut tx = database
        .begin()
        .await
        .expect("attested ingress transaction");
    tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
        .await
        .expect("read committed");
    tx.execute(
        &format!("LOCK TABLE ONLY \"{schema}\".ingress_protocol_epoch IN ACCESS SHARE MODE"),
        (),
    )
    .await
    .expect("epoch first");
    tx.execute(
        "SELECT pg_catalog.set_config('waddle.protocol_epoch', '0', true), \
         pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)",
        (),
    )
    .await
    .expect("epoch proof");
    tx
}

async fn seed_messages(fixture: &Fixture) -> (Uuid, Uuid) {
    let source = Uuid::new_v4();
    let child = Uuid::new_v4();
    let mut connection = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("seed owner");
    let mut tx = connection.begin().await.expect("seed transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY \"{}\".ingress_protocol_epoch IN ACCESS SHARE MODE",
        fixture.target
    ))
    .execute(&mut *tx)
    .await
    .expect("seed epoch first");
    sqlx::query(
        "SELECT pg_catalog.set_config('waddle.protocol_epoch', '0', true), \
         pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)",
    )
    .execute(&mut *tx)
    .await
    .expect("seed epoch proof");
    sqlx::query(&format!(
        "INSERT INTO \"{}\".ingress_messages \
         (message_key, digest_version, digest, source_binding_version, source_bare_jid, \
          source_auth_context_id, source_auth_context_version, source_auth_epoch) \
         VALUES ($1, 1, $2, 1, 'source@example.test', $3, 1, 1)",
        fixture.target
    ))
    .bind(source)
    .bind([1_u8; 32].as_slice())
    .bind(Uuid::new_v4())
    .execute(&mut *tx)
    .await
    .expect("bound source");
    sqlx::query(&format!(
        "UPDATE \"{}\".ingress_messages SET owner_decision = 0 WHERE message_key = $1",
        fixture.target
    ))
    .bind(source)
    .execute(&mut *tx)
    .await
    .expect("undecided source");
    sqlx::query(&format!(
        "INSERT INTO \"{}\".ingress_messages (message_key, digest_version, digest) VALUES ($1, 1, $2)",
        fixture.target
    ))
    .bind(child)
    .bind([2_u8; 32].as_slice())
    .execute(&mut *tx)
    .await
    .expect("child");
    tx.commit().await.expect("seed commit");
    (source, child)
}

#[tokio::test]
async fn owner_grant_and_attested_ingress_lab() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url.clone()).await;
    let attestation = prepare_candidate_application_fixture(&fixture).await;
    let owner = format!("owner_grant_{}", Uuid::new_v4().simple());
    let migrator = format!("migrator_grant_{}", Uuid::new_v4().simple());
    let migrator_role = PgRoleName::new(&migrator).expect("migrator name");
    for statement in [
        format!("CREATE ROLE {owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"),
        format!("CREATE ROLE {migrator} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"),
        format!("GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
        format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
        format!("ALTER SCHEMA {} OWNER TO {owner}", fixture.target),
        format!("ALTER TABLE {}.ingress_owner_rejections OWNER TO {owner}", fixture.target),
        format!("GRANT SELECT ON {}._lineage, {}._migrations TO {owner}", fixture.target, fixture.target),
        format!("GRANT SELECT ON {}.ingress_protocol_epoch, {}.ingress_messages TO {owner}", fixture.target, fixture.target),
        format!("GRANT UPDATE (id) ON {}.ingress_protocol_epoch TO {}", fixture.target, fixture.runtime),
        format!("GRANT DELETE ON {}.ingress_messages TO {}", fixture.target, fixture.runtime),
    ] {
        sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .expect("provision isolated owner and required child DML");
    }
    for function in [
        "waddle_ingress_owner_rejection_immutable",
        "waddle_ingress_owner_rejection_insert_guard",
        "waddle_ingress_owner_rejection_delete_guard",
        "waddle_ingress_epoch_guard",
        "waddle_ingress_truncate_guard",
    ] {
        sqlx::query(&format!(
            "ALTER FUNCTION {}.{function}() OWNER TO {owner}",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("align guard function ownership with migration owner");
    }
    let owner_options = fixture.admin_options.clone().username(&migrator);
    let config = DarkMigrationOwnerConfig::new(
        fixture.lineage.clone(),
        PgRoleName::new(&owner).expect("owner name"),
        PgRoleName::new(&fixture.runtime).expect("runtime name"),
    )
    .expect("independent role config");
    let wrong_runtime_options = fixture.runtime_options.clone().username(&fixture.outsider);
    assert!(inspect_dark_owner_rejection_runtime_access(
        &owner_options,
        &wrong_runtime_options,
        &config,
        &migrator_role,
    )
    .await
    .is_err());
    let other_database = format!("owner_rejection_other_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {other_database}"))
        .execute(&fixture.admin)
        .await
        .expect("create other disposable database");
    sqlx::query(&format!(
        "GRANT CONNECT ON DATABASE {other_database} TO {}",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant runtime access to other database");
    let wrong_database_options = fixture.runtime_options.clone().database(&other_database);
    let wrong_database = PgConnection::connect_with(&wrong_database_options)
        .await
        .expect("runtime login in different database");
    drop(wrong_database);
    assert!(matches!(
        inspect_dark_owner_rejection_runtime_access(
            &owner_options,
            &wrong_database_options,
            &config,
            &migrator_role,
        )
        .await,
        Err(OwnerRejectionGrantError::Owner(
            DarkMigrationOwnerError::RuntimeSessionUnverified
        ))
    ));
    sqlx::query(&format!("DROP DATABASE {other_database} WITH (FORCE)"))
        .execute(&fixture.admin)
        .await
        .expect("drop other disposable database");
    sqlx::query(&format!(
        "GRANT SET ON PARAMETER session_replication_role TO {}",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant unique runtime parameter privilege");
    assert!(
        matches!(
            grant_dark_owner_rejection_runtime_access(
                &owner_options,
                &fixture.runtime_options,
                &config,
                &migrator_role,
            )
            .await,
            Err(OwnerRejectionGrantError::UnsafeRuntime)
        ),
        "runtime parameter grant must be refused"
    );
    sqlx::query(&format!(
        "REVOKE SET ON PARAMETER session_replication_role FROM {}",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke unique runtime parameter privilege");
    // Parameter grants are cluster-wide. Keep the PUBLIC probe uncommitted so
    // concurrent sessions never observe it and pre-existing grants are retained.
    let mut public_probe = fixture
        .admin
        .begin()
        .await
        .expect("PUBLIC probe transaction");
    sqlx::query("GRANT SET ON PARAMETER session_replication_role TO PUBLIC")
        .execute(&mut *public_probe)
        .await
        .expect("uncommitted PUBLIC grant");
    let effective: bool = sqlx::query_scalar(
        "SELECT pg_catalog.has_parameter_privilege($1::pg_catalog.name, 'session_replication_role', 'SET')",
    )
    .bind(&fixture.runtime)
    .fetch_one(&mut *public_probe)
    .await
    .expect("PUBLIC grant is effective inside transaction");
    assert!(effective);
    public_probe.rollback().await.expect("discard PUBLIC grant");
    sqlx::query(&format!(
        "ALTER ROLE {} SET session_replication_role = 'replica'",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("set replica default");
    let mut replica = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime with replica default");
    let mode: String = sqlx::query_scalar("SHOW session_replication_role")
        .fetch_one(&mut replica)
        .await
        .expect("runtime mode");
    assert_eq!(mode, "replica");
    drop(replica);
    assert!(
        matches!(
            grant_dark_owner_rejection_runtime_access(
                &owner_options,
                &fixture.runtime_options,
                &config,
                &migrator_role,
            )
            .await,
            Err(OwnerRejectionGrantError::Owner(
                DarkMigrationOwnerError::RuntimeSessionUnverified
            ))
        ),
        "replica runtime cannot receive grant"
    );
    assert!(
        matches!(
            inspect_dark_owner_rejection_runtime_access(
                &owner_options,
                &fixture.runtime_options,
                &config,
                &migrator_role,
            )
            .await,
            Err(OwnerRejectionGrantError::Owner(
                DarkMigrationOwnerError::RuntimeSessionUnverified
            ))
        ),
        "replica runtime cannot receive positive readback"
    );
    sqlx::query(&format!(
        "ALTER ROLE {} RESET session_replication_role",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("reset replica default");
    assert_eq!(
        inspect_dark_owner_rejection_runtime_access(
            &owner_options,
            &fixture.runtime_options,
            &config,
            &migrator_role
        )
        .await
        .expect("initial owner probe"),
        OwnerRejectionGrantState::Missing
    );
    assert!(
        grant_dark_owner_rejection_runtime_access(
            &fixture.runtime_options,
            &fixture.runtime_options,
            &config,
            &migrator_role
        )
        .await
        .is_err(),
        "runtime cannot invoke owner operation"
    );

    let mut wrong_lineage = fixture.lineage.clone();
    wrong_lineage
        .postgres_identity
        .as_mut()
        .expect("identity")
        .schema
        .oid ^= 1;
    let wrong_schema = DarkMigrationOwnerConfig::new(
        wrong_lineage,
        PgRoleName::new(&owner).expect("owner"),
        PgRoleName::new(&fixture.runtime).expect("runtime"),
    )
    .expect("wrong schema config");
    assert!(grant_dark_owner_rejection_runtime_access(
        &owner_options,
        &fixture.runtime_options,
        &wrong_schema,
        &migrator_role
    )
    .await
    .is_err());
    let wrong_runtime = DarkMigrationOwnerConfig::new(
        fixture.lineage.clone(),
        PgRoleName::new(&owner).expect("owner"),
        PgRoleName::new(&fixture.outsider).expect("outsider"),
    )
    .expect("wrong runtime config");
    assert!(grant_dark_owner_rejection_runtime_access(
        &owner_options,
        &fixture.runtime_options,
        &wrong_runtime,
        &migrator_role
    )
    .await
    .is_err());
    sqlx::query(&format!("ALTER TABLE {}.ingress_owner_rejections DISABLE TRIGGER ingress_owner_rejections_insert_guard", fixture.target))
        .execute(&fixture.admin).await.expect("disable mapping guard");
    assert!(
        grant_dark_owner_rejection_runtime_access(
            &owner_options,
            &fixture.runtime_options,
            &config,
            &migrator_role
        )
        .await
        .is_err(),
        "disabled V1028 guard prevents granting"
    );
    sqlx::query(&format!("ALTER TABLE {}.ingress_owner_rejections ENABLE TRIGGER ingress_owner_rejections_insert_guard", fixture.target))
        .execute(&fixture.admin).await.expect("restore mapping guard");
    sqlx::query(&format!("ALTER TABLE {}.ingress_owner_rejections DROP CONSTRAINT ingress_owner_rejections_child_message_key_fkey", fixture.target))
        .execute(&fixture.admin).await.expect("drop child FK");
    assert!(
        grant_dark_owner_rejection_runtime_access(
            &owner_options,
            &fixture.runtime_options,
            &config,
            &migrator_role
        )
        .await
        .is_err(),
        "missing child FK prevents granting"
    );
    sqlx::query(&format!("ALTER TABLE {}.ingress_owner_rejections ADD CONSTRAINT ingress_owner_rejections_child_message_key_fkey FOREIGN KEY (child_message_key) REFERENCES {}.ingress_messages(message_key) ON DELETE CASCADE", fixture.target, fixture.target))
        .execute(&fixture.admin).await.expect("restore child FK");
    sqlx::query(&format!(
        "ALTER FUNCTION {}.waddle_ingress_owner_rejection_insert_guard() SECURITY DEFINER",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("change function security");
    assert!(
        grant_dark_owner_rejection_runtime_access(
            &owner_options,
            &fixture.runtime_options,
            &config,
            &migrator_role
        )
        .await
        .is_err(),
        "definer-mode guard prevents granting"
    );
    sqlx::query(&format!(
        "ALTER FUNCTION {}.waddle_ingress_owner_rejection_insert_guard() SECURITY INVOKER",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore invoker mode");
    sqlx::query(&format!(
        "GRANT SELECT ON {}.ingress_owner_rejections TO PUBLIC",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("inject public grant");
    assert!(
        grant_dark_owner_rejection_runtime_access(
            &owner_options,
            &fixture.runtime_options,
            &config,
            &migrator_role
        )
        .await
        .is_err(),
        "unexpected public grant prevents granting"
    );
    sqlx::query(&format!(
        "REVOKE SELECT ON {}.ingress_owner_rejections FROM PUBLIC",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove public grant");
    sqlx::query(&format!(
        "GRANT UPDATE (source_message_key) ON {}.ingress_owner_rejections TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("inject unsafe column privilege");
    assert!(matches!(
        grant_dark_owner_rejection_runtime_access(
            &owner_options,
            &fixture.runtime_options,
            &config,
            &migrator_role
        )
        .await,
        Err(OwnerRejectionGrantError::UnsafeRuntime)
    ));
    sqlx::query(&format!(
        "REVOKE UPDATE (source_message_key) ON {}.ingress_owner_rejections FROM {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove unsafe test privilege");
    assert_eq!(
        inspect_dark_owner_rejection_runtime_access(
            &owner_options,
            &fixture.runtime_options,
            &config,
            &migrator_role
        )
        .await
        .expect("negative checks left ACL untouched"),
        OwnerRejectionGrantState::Missing
    );

    let hostile_owner =
        PgConnectOptions::from_str(&url_with_search_path(&fixture.base_url, "public,pg_temp"))
            .expect("hostile owner options")
            .username(&migrator);
    grant_dark_owner_rejection_runtime_access(
        &hostile_owner,
        &fixture.runtime_options,
        &config,
        &migrator_role,
    )
    .await
    .expect("owner grant");
    grant_dark_owner_rejection_runtime_access(
        &owner_options,
        &fixture.runtime_options,
        &config,
        &migrator_role,
    )
    .await
    .expect("idempotent owner grant");
    assert_eq!(
        inspect_dark_owner_rejection_runtime_access(
            &owner_options,
            &fixture.runtime_options,
            &config,
            &migrator_role
        )
        .await
        .expect("fresh committed readback"),
        OwnerRejectionGrantState::Granted
    );

    let hostile_runtime = PgConnectOptions::from_str(&url_with_search_path(
        &fixture.base_url,
        &format!("{},public,pg_temp", fixture.target),
    ))
    .expect("hostile runtime options")
    .username(&fixture.runtime);
    let pool = attested_pool(
        hostile_runtime,
        attestation.clone(),
        2,
        Duration::from_millis(500),
    )
    .expect("attested pool");
    let database_pool = DatabasePool::from_attested_postgres_pool("owner-lab", pool, 1, 1)
        .await
        .expect("one shared pool, disjoint lanes");
    let ingress = database_pool.ingress();
    let lineage = LineageConfig {
        deployment_uuid: Some(fixture.lineage.deployment_uuid),
        action: None,
    };
    assert!(PgConnectOptions::from_str(ingress.database_url()).is_err());
    assert!(matches!(
        IngressAuthority::try_new(
            IngressConfig {
                pool_size: 1,
                retry_attempts: 1
            },
            database_pool.global().clone(),
            lineage.clone(),
            #[cfg(feature = "clustering")]
            None,
        )
        .await,
        Err(IngressStartupError::AttestedIngressPoolRequired)
    ));
    let authority = IngressAuthority::try_new_with_attested_database(
        IngressConfig {
            pool_size: 1,
            retry_attempts: 1,
        },
        ingress.clone(),
        lineage.clone(),
        #[cfg(feature = "clustering")]
        None,
    )
    .await
    .expect("real attested ingress authority assembly");
    assert!(authority.drain_and_join(Duration::from_secs(1)).await);
    drop(authority);

    let uow = IngressUnitOfWork::open(ingress.clone(), lineage).expect("ingress UoW");
    let held = uow.begin().await.expect("one ingress slot");
    assert!(
        uow.begin_with_timeouts(Duration::from_millis(150), Duration::from_millis(300))
            .await
            .is_err(),
        "a second ingress operation cannot fall back to a DSN or main lane"
    );
    let main = database_pool
        .global()
        .begin()
        .await
        .expect("main lane remains available");
    drop(main);
    held.commit().await.expect("release ingress slot");

    let (source, child) = seed_messages(&fixture).await;
    let mut tx = guarded(&ingress, &fixture.target).await;
    tx.execute(
        &format!("INSERT INTO \"{}\".ingress_owner_rejections (source_message_key, child_message_key) VALUES (?::uuid, ?::uuid)", fixture.target),
        (source.to_string(), child.to_string()),
    ).await.expect("restricted role can insert mapping on attested ingress lane");
    tx.commit().await.expect("insert mapping commit");
    let guard = ingress.guard().await.expect("attested read");
    let mut rows = guard
        .query(
            &format!(
                "SELECT count(*) FROM \"{}\".ingress_owner_rejections",
                fixture.target
            ),
            (),
        )
        .await
        .expect("restricted SELECT");
    assert_eq!(
        rows.next()
            .await
            .expect("read")
            .expect("count")
            .get::<i64>(0)
            .expect("count type"),
        1
    );
    drop(rows);
    drop(guard);
    for statement in [
        format!(
            "UPDATE \"{}\".ingress_owner_rejections SET child_message_key = child_message_key",
            fixture.target
        ),
        format!(
            "DELETE FROM \"{}\".ingress_owner_rejections",
            fixture.target
        ),
    ] {
        let mut tx = guarded(&ingress, &fixture.target).await;
        assert!(
            tx.execute(&statement, ()).await.is_err(),
            "mapping mutation is denied"
        );
        drop(tx);
    }
    let mut tx = guarded(&ingress, &fixture.target).await;
    tx.execute(
        &format!(
            "DELETE FROM \"{}\".ingress_messages WHERE message_key = ?::uuid",
            fixture.target
        ),
        waddle_server::db_params![child.to_string()],
    )
    .await
    .expect("restricted child DELETE and FK cascade");
    tx.commit().await.expect("child deletion commits");
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.ingress_owner_rejections",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect FK cascade");
    assert_eq!(
        count, 0,
        "child FK cascade removes the mapping without direct mapping DELETE"
    );

    let old_pids: Vec<i32> = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname = $1 AND usename = $2",
    )
    .bind(&fixture.database_name)
    .bind(&fixture.runtime)
    .fetch_all(&fixture.admin)
    .await
    .expect("enumerate disposable runtime sessions");
    assert!(!old_pids.is_empty());
    for pid in &old_pids {
        let terminated: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
            .bind(pid)
            .fetch_one(&fixture.admin)
            .await
            .expect("terminate disposable runtime session");
        assert!(terminated);
    }
    let mut replacement = ingress.begin().await.expect("attested reconnect");
    let mut pid_rows = replacement
        .query("SELECT pg_catalog.pg_backend_pid()::bigint", ())
        .await
        .expect("replacement PID");
    let new_pid: i64 = pid_rows
        .next()
        .await
        .expect("PID row")
        .expect("PID")
        .get(0)
        .expect("PID type");
    drop(pid_rows);
    assert!(!old_pids.iter().any(|pid| i64::from(*pid) == new_pid));
    replacement.commit().await.expect("release replacement");

    sqlx::query(&format!(
        "GRANT TEMPORARY ON DATABASE {} TO {}",
        fixture.database_name, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("inject TEMP privilege");
    assert!(matches!(
        inspect_dark_owner_rejection_runtime_access(
            &owner_options,
            &fixture.runtime_options,
            &config,
            &migrator_role
        )
        .await,
        Err(OwnerRejectionGrantError::Owner(_))
    ));
    let mut shadow = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime shadow session");
    sqlx::query("CREATE TEMP TABLE ingress_owner_rejections (source_message_key uuid, child_message_key uuid)")
        .execute(&mut shadow).await.expect("create hostile shadow");
    assert!(
        ingress.begin().await.is_err(),
        "attested pool refuses TEMP drift"
    );
    drop(shadow);
    sqlx::query(&format!(
        "REVOKE TEMPORARY ON DATABASE {} FROM {}",
        fixture.database_name, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore TEMP denial");
    ingress
        .begin()
        .await
        .expect("fresh/replacement attested session recovers")
        .commit()
        .await
        .expect("release replacement");

    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_owner_rejections RENAME TO owner_rejections_hidden",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("hide relation");
    assert!(
        inspect_dark_owner_rejection_runtime_access(
            &owner_options,
            &fixture.runtime_options,
            &config,
            &migrator_role
        )
        .await
        .is_err(),
        "missing relation fails closed"
    );
    sqlx::query(&format!(
        "ALTER TABLE {}.owner_rejections_hidden RENAME TO ingress_owner_rejections",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore relation");

    drop(uow);
    drop(ingress);
    drop(database_pool);
    fixture.cleanup().await;
    let cluster = PgPool::connect(&url).await.expect("cleanup cluster");
    sqlx::query(&format!("DROP ROLE {migrator}"))
        .execute(&cluster)
        .await
        .expect("drop migrator role");
    sqlx::query(&format!("DROP ROLE {owner}"))
        .execute(&cluster)
        .await
        .expect("drop owner role");
    cluster.close().await;
}
