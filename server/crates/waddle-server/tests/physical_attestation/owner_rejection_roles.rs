//! Restricted-role owner-rejection grants on the real attested ingress lane.

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_server::config::{IngressConfig, LineageConfig};
use waddle_server::db::physical_attestation::{
    attested_pool, PgRoleName, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
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

const LINEAGE_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_993;
const LEDGER_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_992;

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
        format!("ALTER TABLE {}.ingress_protocol_epoch OWNER TO {owner}", fixture.target),
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

    exercise_catalog_fence(&fixture, &owner_options, &config, &migrator_role).await;
    exercise_epoch_barrier(&fixture, &owner_options, &config, &migrator_role).await;

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

async fn wait_relation_waiter(
    admin: &PgPool,
    application: &str,
    relation: &str,
    mode: &str,
) -> i32 {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT a.pid FROM pg_catalog.pg_stat_activity a \
                 JOIN pg_catalog.pg_locks l ON l.pid=a.pid \
                 WHERE a.application_name=$1 AND l.relation=$2::regclass \
                   AND l.mode=$3 AND NOT l.granted",
            )
            .bind(application)
            .bind(relation)
            .bind(mode)
            .fetch_optional(admin)
            .await
            .expect("observe real relation waiter");
            if let Some(pid) = pid {
                break pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("relation waiter before timeout")
}

async fn wait_idle_retry(admin: &PgPool, application: &str) -> i32 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_stat_activity \
                 WHERE application_name=$1 AND state='idle' AND xact_start IS NULL \
                   AND backend_xmin IS NULL AND query='ROLLBACK'",
            )
            .bind(application)
            .fetch_optional(admin)
            .await
            .expect("observe acknowledged retry rollback");
            if let Some(pid) = pid {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("owner attempt rolls back before retry")
}

async fn assert_advisory_lock(admin: &PgPool, pid: i32, key: i64, mode: &str) {
    let held: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
         WHERE pid=$1 AND locktype='advisory' AND granted AND mode=$2 \
           AND classid=(($3::pg_catalog.int8 >> 32) & 4294967295)::pg_catalog.oid \
           AND objid=($3::pg_catalog.int8 & 4294967295)::pg_catalog.oid \
           AND objsubid=1)",
    )
    .bind(pid)
    .bind(mode)
    .bind(key)
    .fetch_one(admin)
    .await
    .expect("inspect exact owner backend advisory lock");
    assert!(held, "backend {pid} must hold {mode} on {key}");
}

fn spawn_owner_entry(
    owner: PgConnectOptions,
    runtime: PgConnectOptions,
    config: DarkMigrationOwnerConfig,
    migrator: PgRoleName,
    grant: bool,
) -> tokio::task::JoinHandle<Result<OwnerRejectionGrantState, OwnerRejectionGrantError>> {
    tokio::spawn(async move {
        if grant {
            grant_dark_owner_rejection_runtime_access(&owner, &runtime, &config, &migrator)
                .await
                .map(|()| OwnerRejectionGrantState::Granted)
        } else {
            inspect_dark_owner_rejection_runtime_access(&owner, &runtime, &config, &migrator).await
        }
    })
}

async fn exercise_catalog_fence(
    fixture: &Fixture,
    owner_options: &PgConnectOptions,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
) {
    let version: i32 = sqlx::query_scalar(
        "SELECT pg_catalog.current_setting('server_version_num')::pg_catalog.int4",
    )
    .fetch_one(&fixture.admin)
    .await
    .expect("PostgreSQL version");
    assert!((170_000..180_000).contains(&version), "PG17 is required");

    // The separately pinned runtime correlation session must not retain an
    // inherited repeatable-read snapshot while its owner waits for catalog-X.
    sqlx::query(&format!(
        "ALTER ROLE {} SET default_transaction_isolation = 'repeatable read'",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("make runtime session inherit repeatable read");

    for grant in [false, true] {
        let mut coordinator = PgConnection::connect_with(&fixture.admin_options)
            .await
            .expect("catalog and epoch coordinator");
        sqlx::query("BEGIN")
            .execute(&mut coordinator)
            .await
            .expect("begin epoch barrier");
        sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .execute(&mut coordinator)
            .await
            .expect("hold catalog-X");
        let epoch = format!("{}.ingress_protocol_epoch", fixture.target);
        let held_mode = if grant {
            "ACCESS SHARE"
        } else {
            "ACCESS EXCLUSIVE"
        };
        sqlx::query(&format!("LOCK TABLE ONLY {epoch} IN {held_mode} MODE"))
            .execute(&mut coordinator)
            .await
            .expect("hold epoch barrier");

        // A pre-fence role check would refuse this deliberately invalid role.
        sqlx::query(&format!("ALTER ROLE {} CREATEROLE", migrator.as_str()))
            .execute(&fixture.admin)
            .await
            .expect("temporarily invalidate migrator role");
        let application = format!("owner-catalog-{}", Uuid::new_v4().simple());
        let task = spawn_owner_entry(
            owner_options.clone().application_name(&application),
            fixture.runtime_options.clone(),
            config.clone(),
            migrator.clone(),
            grant,
        );
        let idle_pid = wait_idle_retry(&fixture.admin, &application).await;
        assert!(!task.is_finished(), "the role check must follow catalog");
        let held: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_catalog.pg_locks \
             WHERE pid=$1 AND locktype='advisory' AND granted",
        )
        .bind(idle_pid)
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect failed catalog attempt");
        assert_eq!(held, 0, "failed catalog try releases all gates");
        let runtime_live_without_snapshot: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
             WHERE usename=$1 AND application_name LIKE 'waddle-owner-rejection-%' \
               AND state='idle in transaction' AND xact_start IS NOT NULL \
               AND backend_xmin IS NULL)",
        )
        .bind(&fixture.runtime)
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect separate runtime correlation session");
        assert!(
            runtime_live_without_snapshot,
            "runtime correlation session must precede owner checkout without an inherited snapshot"
        );
        sqlx::query(&format!("ALTER ROLE {} NOCREATEROLE", migrator.as_str()))
            .execute(&fixture.admin)
            .await
            .expect("restore migrator role while fenced");
        let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&mut coordinator)
            .await
            .expect("release catalog fence");
        assert!(unlocked);
        let wait_mode = if grant {
            "AccessExclusiveLock"
        } else {
            "AccessShareLock"
        };
        let pid = wait_relation_waiter(&fixture.admin, &application, &epoch, wait_mode).await;
        let advisory_mode = if grant { "ExclusiveLock" } else { "ShareLock" };
        for key in [
            CATALOG_MUTATION_ADVISORY_LOCK_KEY,
            LINEAGE_ADVISORY_LOCK_KEY,
            LEDGER_ADVISORY_LOCK_KEY,
        ] {
            assert_advisory_lock(&fixture.admin, pid, key, advisory_mode).await;
        }
        sqlx::query("ROLLBACK")
            .execute(&mut coordinator)
            .await
            .expect("release epoch barrier");
        let expected = if grant {
            OwnerRejectionGrantState::Granted
        } else {
            OwnerRejectionGrantState::Missing
        };
        assert_eq!(
            task.await.expect("join owner entry").expect("owner entry"),
            expected
        );
    }

    // A ledger-only blocker forces a lower-gate failure after catalog and
    // lineage have been acquired. A competing catalog-X can make progress
    // while the ledger remains held, proving the failed transaction ended.
    for grant in [false, true] {
        let mut blocker = PgConnection::connect_with(&fixture.admin_options)
            .await
            .expect("ledger blocker");
        sqlx::query("BEGIN")
            .execute(&mut blocker)
            .await
            .expect("begin ledger blocker");
        sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
            .bind(LEDGER_ADVISORY_LOCK_KEY)
            .execute(&mut blocker)
            .await
            .expect("hold ledger-X");
        let application = format!("owner-ledger-{}", Uuid::new_v4().simple());
        let task = spawn_owner_entry(
            owner_options.clone().application_name(&application),
            fixture.runtime_options.clone(),
            config.clone(),
            migrator.clone(),
            grant,
        );
        wait_idle_retry(&fixture.admin, &application).await;
        assert!(!task.is_finished(), "ledger blocker prevents completion");
        let mut observer = PgConnection::connect_with(&fixture.admin_options)
            .await
            .expect("independent catalog observer");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let acquired: bool =
                    sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_lock($1)")
                        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                        .fetch_one(&mut observer)
                        .await
                        .expect("try catalog-X");
                if acquired {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("lower contention releases catalog-X or catalog-S");
        let pid = wait_idle_retry(&fixture.admin, &application).await;
        let held: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_catalog.pg_locks \
             WHERE pid=$1 AND locktype='advisory' AND granted",
        )
        .bind(pid)
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect retried owner gates");
        assert_eq!(held, 0, "rollback also releases the lineage gate");
        sqlx::query("ROLLBACK")
            .execute(&mut blocker)
            .await
            .expect("release ledger blocker");
        let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&mut observer)
            .await
            .expect("release observer catalog fence");
        assert!(unlocked);
        assert_eq!(
            task.await
                .expect("join retried entry")
                .expect("retry succeeds"),
            OwnerRejectionGrantState::Granted
        );
    }
    sqlx::query(&format!(
        "REVOKE SELECT, INSERT ON {}.ingress_owner_rejections FROM {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("reset scoped fixture grant");
    sqlx::query(&format!(
        "ALTER ROLE {} RESET default_transaction_isolation",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore runtime default isolation");
}

fn spawn_grant(
    owner: PgConnectOptions,
    runtime: PgConnectOptions,
    config: DarkMigrationOwnerConfig,
    migrator: PgRoleName,
) -> tokio::task::JoinHandle<Result<(), OwnerRejectionGrantError>> {
    tokio::spawn(async move {
        grant_dark_owner_rejection_runtime_access(&owner, &runtime, &config, &migrator).await
    })
}

async fn exercise_epoch_barrier(
    fixture: &Fixture,
    owner_options: &PgConnectOptions,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
) {
    let schema = &fixture.target;
    let epoch = format!("{schema}.ingress_protocol_epoch");
    let mapping = format!("{schema}.ingress_owner_rejections");
    let function = format!("{schema}.waddle_ingress_owner_rejection_insert_guard()");
    let original: String = sqlx::query_scalar("SELECT pg_get_functiondef($1::regprocedure)")
        .bind(&function)
        .fetch_one(&fixture.admin)
        .await
        .expect("original guard");
    for mutation in ["body", "ACL", "cancel"] {
        let mut reader = PgConnection::connect_with(&fixture.runtime_options)
            .await
            .expect("reader session");
        let mut held = reader.begin().await.expect("reader transaction");
        sqlx::query(&format!("LOCK TABLE ONLY {epoch} IN ACCESS SHARE MODE"))
            .execute(&mut *held)
            .await
            .expect("runtime AS barrier");
        let application = format!("owner-ax-{}", Uuid::new_v4().simple());
        let grant = spawn_grant(
            owner_options.clone().application_name(&application),
            fixture.runtime_options.clone(),
            config.clone(),
            migrator.clone(),
        );
        let pid =
            wait_relation_waiter(&fixture.admin, &application, &epoch, "AccessExclusiveLock").await;
        let early_locks: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_catalog.pg_locks WHERE pid=$1 AND relation IN \
             ($2::regclass,$3::regclass,$4::regclass)",
        )
        .bind(pid)
        .bind(format!("{schema}._lineage"))
        .bind(format!("{schema}._migrations"))
        .bind(&mapping)
        .fetch_one(&fixture.admin)
        .await
        .expect("covered lock order");
        assert_eq!(
            early_locks, 0,
            "AX must precede lineage, ledger and mapping reads"
        );
        if mutation == "cancel" {
            grant.abort();
            assert!(grant
                .await
                .expect_err("cancelled grant task")
                .is_cancelled());
            // Fence the exact observed backend before treating absence as a
            // rollback verdict; client cancellation by itself proves no fence.
            let _: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
                .bind(pid)
                .fetch_one(&fixture.admin)
                .await
                .expect("fence cancelled attempt backend");
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let exists: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)",
                    )
                    .bind(pid)
                    .fetch_one(&fixture.admin)
                    .await
                    .expect("observe fence completion");
                    if !exists {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("cancelled backend is gone before reconciliation");
        } else {
            let sql = if mutation == "body" {
                format!("CREATE OR REPLACE FUNCTION {function} RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog AS $$ BEGIN RETURN NEW; END $$")
            } else {
                format!("GRANT UPDATE ON {mapping} TO {}", fixture.runtime)
            };
            sqlx::raw_sql(&sql)
                .execute(&fixture.admin)
                .await
                .expect("same-epoch hostile drift");
            held.rollback().await.expect("release runtime AS");
            assert!(
                grant.await.expect("join refused grant").is_err(),
                "post-wait {mutation} drift must refuse"
            );
            let restore = if mutation == "body" {
                original.clone()
            } else {
                format!("REVOKE UPDATE ON {mapping} FROM {}", fixture.runtime)
            };
            sqlx::raw_sql(&restore)
                .execute(&fixture.admin)
                .await
                .expect("restore hostile drift");
            continue;
        }
        held.rollback().await.expect("release cancelled barrier");
    }
    assert_eq!(
        inspect_dark_owner_rejection_runtime_access(
            owner_options,
            &fixture.runtime_options,
            config,
            migrator
        )
        .await
        .expect("refusals and cancellation did not grant"),
        OwnerRejectionGrantState::Missing
    );

    // Hold the mapping only in this disposable coordinator. Once the owner
    // reaches it, the owner must already hold epoch AX and exclude new AS.
    let mut coordinator = fixture.admin.begin().await.expect("coordinator");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {mapping} IN ACCESS EXCLUSIVE MODE"
    ))
    .execute(&mut *coordinator)
    .await
    .expect("hold mapping");
    let application = format!("owner-ax-{}", Uuid::new_v4().simple());
    let grant = spawn_grant(
        owner_options.clone().application_name(&application),
        fixture.runtime_options.clone(),
        config.clone(),
        migrator.clone(),
    );
    let pid = wait_relation_waiter(&fixture.admin, &application, &mapping, "AccessShareLock").await;
    let holds_ax: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE pid=$1 AND relation=$2::regclass AND mode='AccessExclusiveLock' AND granted)")
        .bind(pid).bind(&epoch).fetch_one(&fixture.admin).await.expect("owner epoch AX");
    assert!(holds_ax);
    let reader_name = format!("runtime-as-{}", Uuid::new_v4().simple());
    let reader_options = fixture
        .runtime_options
        .clone()
        .application_name(&reader_name);
    let reader_epoch = epoch.clone();
    let reader = tokio::spawn(async move {
        let mut connection = PgConnection::connect_with(&reader_options)
            .await
            .expect("new runtime reader");
        let mut tx = connection.begin().await.expect("new runtime transaction");
        sqlx::query(&format!(
            "LOCK TABLE ONLY {reader_epoch} IN ACCESS SHARE MODE"
        ))
        .execute(&mut *tx)
        .await
        .expect("runtime waits for owner AX");
        tx.rollback().await.expect("runtime read-only rollback");
    });
    wait_relation_waiter(&fixture.admin, &reader_name, &epoch, "AccessShareLock").await;
    coordinator
        .rollback()
        .await
        .expect("let fixed owner operation finish");
    grant
        .await
        .expect("join mutation")
        .expect("AX grant commits");
    reader.await.expect("join runtime AS reader");

    // A separate externally held AX barrier lets the owner diagnostic reach
    // its AS wait without first queuing behind the mutator's advisory locks.
    let mut epoch_holder = fixture.admin.begin().await.expect("epoch coordinator");
    sqlx::query(&format!("LOCK TABLE ONLY {epoch} IN ACCESS EXCLUSIVE MODE"))
        .execute(&mut *epoch_holder)
        .await
        .expect("hold epoch AX for observer");
    let observer_name = format!("owner-as-{}", Uuid::new_v4().simple());
    let observer_owner = owner_options.clone().application_name(&observer_name);
    let runtime = fixture.runtime_options.clone();
    let observer_config = config.clone();
    let observer_migrator = migrator.clone();
    let observer = tokio::spawn(async move {
        inspect_dark_owner_rejection_runtime_access(
            &observer_owner,
            &runtime,
            &observer_config,
            &observer_migrator,
        )
        .await
    });
    wait_relation_waiter(&fixture.admin, &observer_name, &epoch, "AccessShareLock").await;
    epoch_holder
        .rollback()
        .await
        .expect("release observer barrier");
    assert_eq!(
        observer
            .await
            .expect("join AS observer")
            .expect("AS read-only result"),
        OwnerRejectionGrantState::Granted
    );
    sqlx::query(&format!(
        "REVOKE SELECT, INSERT ON {mapping} FROM {}",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("reset scoped fixture grant");
}
