//! Three-role, disposable PostgreSQL owner transition characterization.

use super::*;
use sha2::{Digest, Sha256};
use waddle_server::db::{
    run_disposable_epoch_transition_lab, DisposableEpochTransitionConfig,
    DisposableEpochTransitionError, DisposableEpochTransitionFault,
};

const CANDIDATE: &str =
    include_str!("../../src/db/migrations/owner/ingress_epoch_relation_guard_candidate.sql");

struct TransitionFixture {
    cluster: PgPool,
    admin: PgPool,
    migrator_options: PgConnectOptions,
    runtime_options: PgConnectOptions,
    database: String,
    schema: String,
    migrator: String,
    owner: String,
    runtime: String,
    lineage: AttestedLineage,
}

impl TransitionFixture {
    async fn new(url: &str) -> Self {
        let cluster = PgPool::connect(url).await.expect("cluster administrator");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database = format!("migration_transition_lab_d_{suffix}");
        let schema = format!("migration_transition_lab_s_{suffix}");
        let migrator = format!("migration_transition_lab_m_{suffix}");
        let owner = format!("migration_transition_lab_o_{suffix}");
        let runtime = format!("migration_transition_lab_r_{suffix}");
        sqlx::query(&format!("CREATE DATABASE {database}"))
            .execute(&cluster)
            .await
            .expect("create disposable database");
        for statement in [
            format!("CREATE ROLE {owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"),
            format!("CREATE ROLE {migrator} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"),
            format!("CREATE ROLE {runtime} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"),
            format!("GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
        ] {
            sqlx::query(&statement).execute(&cluster).await.expect("create separated roles");
        }
        let base = url_with_database(url, &database);
        let admin = PgPool::connect(&base)
            .await
            .expect("database administrator");
        for statement in [
            format!("REVOKE TEMP ON DATABASE {database} FROM PUBLIC"),
            format!("CREATE SCHEMA {schema} AUTHORIZATION {owner}"),
            format!("GRANT USAGE ON SCHEMA {schema} TO {runtime}"),
            format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
            format!("CREATE TABLE {schema}.waddle_epoch_transition_lab_marker (version TEXT PRIMARY KEY, checksum TEXT NOT NULL)"),
            format!("ALTER TABLE {schema}.waddle_epoch_transition_lab_marker OWNER TO {owner}"),
        ] {
            sqlx::query(&statement).execute(&admin).await.expect("provision disposable fixture");
        }
        let scoped = url_with_option(&base, "options", &format!("-c search_path={schema}"));
        let migrator_url = url_with_option(&scoped, "user", &migrator);
        let owner_url = url_with_option(
            &migrator_url,
            "options",
            &format!("-c search_path={schema} -c role={owner}"),
        );
        let migrator_options = PgConnectOptions::from_str(&migrator_url).expect("migrator options");
        let runtime_options = migrator_options.clone().username(&runtime);
        let db = Database::from_config(
            "owner transition fixture",
            &DatabaseConfig::new(DatabaseDriver::Postgres, owner_url),
        )
        .await
        .expect("owner role fixture connection");
        let lineage = lineage::enroll(
            &db,
            &LineageConfig {
                deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
                action: None,
            },
        )
        .await
        .expect("enroll independent expected lineage");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("install current registered catalog");
        drop(db);
        for statement in [
            format!("GRANT SELECT, UPDATE (id) ON {schema}.ingress_protocol_epoch TO {runtime}"),
            format!("GRANT INSERT ON {schema}.ingress_messages TO {runtime}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("provision V1009 runtime");
        }
        Self {
            cluster,
            admin,
            migrator_options,
            runtime_options,
            database,
            schema,
            migrator,
            owner,
            runtime,
            lineage,
        }
    }

    fn config(&self) -> DisposableEpochTransitionConfig {
        DisposableEpochTransitionConfig::new(
            self.lineage.clone(),
            PgRoleName::new(&self.migrator).expect("migrator"),
            PgRoleName::new(&self.owner).expect("owner"),
            PgRoleName::new(&self.runtime).expect("runtime"),
        )
        .expect("transition configuration")
    }

    async fn run(
        &self,
        fault: DisposableEpochTransitionFault,
    ) -> Result<(), DisposableEpochTransitionError> {
        run_disposable_epoch_transition_lab(
            &self.migrator_options,
            &self.runtime_options,
            &self.config(),
            fault,
        )
        .await
    }

    async fn admin(&self, statement: &str) {
        sqlx::query(statement)
            .execute(&self.admin)
            .await
            .unwrap_or_else(|error| panic!("fixture SQL failed: {statement}: {error}"));
    }

    async fn state(&self) -> (bool, bool, i64, i64) {
        sqlx::query_as(&format!(
            "SELECT pg_catalog.has_column_privilege($1, $2, 'id', 'UPDATE'), \
             (SELECT position('FOR SHARE' IN p.prosrc) > 0 FROM pg_catalog.pg_proc p \
              JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
              WHERE n.nspname = $3 AND p.proname = 'waddle_ingress_epoch_guard' AND p.pronargs = 0), \
             (SELECT count(*) FROM {}.waddle_epoch_transition_lab_marker), \
             (SELECT epoch FROM {}.ingress_protocol_epoch WHERE id = 1)", self.schema, self.schema
        )).bind(&self.runtime).bind(format!("{}.ingress_protocol_epoch", self.schema)).bind(&self.schema)
            .fetch_one(&self.admin).await.expect("observe transition state")
    }

    async fn ledger(&self) -> Vec<(i64, String)> {
        sqlx::query_as(&format!(
            "SELECT version, checksum FROM {}._migrations ORDER BY version",
            self.schema
        ))
        .fetch_all(&self.admin)
        .await
        .expect("ledger snapshot")
    }

    async fn cleanup(self) {
        self.admin.close().await;
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&self.cluster)
            .await
            .expect("drop disposable database");
        for role in [&self.migrator, &self.runtime, &self.owner] {
            sqlx::query(&format!("DROP ROLE {role}"))
                .execute(&self.cluster)
                .await
                .expect("drop disposable role");
        }
        self.cluster.close().await;
    }
}

#[tokio::test]
async fn transition_commits_exact_candidate_marker_and_select_only_acl_at_epoch_zero() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = TransitionFixture::new(&url).await;
    let before = fixture.ledger().await;
    assert!(!before.iter().any(|(version, _)| *version == 1028));
    assert_eq!(fixture.state().await, (true, true, 0, 0));
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime LOGIN");
    let identity: (String, String) =
        sqlx::query_as("SELECT session_user::text, current_user::text")
            .fetch_one(&mut runtime)
            .await
            .expect("runtime identity");
    assert_eq!(
        (identity.0.as_str(), identity.1.as_str()),
        (fixture.runtime.as_str(), fixture.runtime.as_str())
    );
    let insert = format!(
        "INSERT INTO {}.ingress_messages (message_key, digest_version, digest) VALUES ($1, 1, $2)",
        fixture.schema
    );
    sqlx::query(&insert)
        .bind(uuid::Uuid::new_v4())
        .bind([0_u8; 32].as_slice())
        .execute(&mut runtime)
        .await
        .expect("V1009 writer works with UPDATE(id)");
    let error = sqlx::query(&format!(
        "UPDATE {}.ingress_protocol_epoch SET id = id WHERE id = 1",
        fixture.schema
    ))
    .execute(&mut runtime)
    .await
    .expect_err("id-only epoch update refused");
    assert!(error.to_string().contains("must advance exactly one step"));
    drop(runtime);
    fixture
        .run(DisposableEpochTransitionFault::None)
        .await
        .expect("commit laboratory transition");
    assert_eq!(fixture.state().await, (false, false, 1, 0));
    assert_eq!(
        fixture.ledger().await,
        before,
        "registered ledger remains unchanged"
    );
    let marker: (String, String) = sqlx::query_as(&format!(
        "SELECT version, checksum FROM {}.waddle_epoch_transition_lab_marker",
        fixture.schema
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("transactional marker");
    assert_eq!(
        marker,
        (
            "relation-guard-lab-1".to_string(),
            hex::encode(Sha256::digest(CANDIDATE.as_bytes()))
        )
    );
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime LOGIN after transition");
    let error = sqlx::query(&insert)
        .bind(uuid::Uuid::new_v4())
        .bind([0_u8; 32].as_slice())
        .execute(&mut runtime)
        .await
        .expect_err("proofless old writer refused");
    assert!(
        error.to_string().contains("epoch ACCESS SHARE prelock"),
        "{error}"
    );
    let error = sqlx::query(&format!(
        "SELECT epoch FROM {}.ingress_protocol_epoch WHERE id = 1 FOR SHARE",
        fixture.schema
    ))
    .execute(&mut runtime)
    .await
    .expect_err("runtime cannot row-lock epoch now");
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("42501")
    );
    let mut tx = runtime.begin().await.expect("candidate writer");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS SHARE MODE",
        fixture.schema
    ))
    .execute(&mut *tx)
    .await
    .expect("SELECT-only relation lock");
    sqlx::query("SELECT pg_catalog.set_config('waddle.protocol_epoch', '0', true), pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)").execute(&mut *tx).await.expect("candidate proof");
    sqlx::query(&insert)
        .bind(uuid::Uuid::new_v4())
        .bind([0_u8; 32].as_slice())
        .execute(&mut *tx)
        .await
        .expect("relation-guarded writer");
    tx.rollback().await.expect("discard test write");
    drop(runtime);
    fixture.cleanup().await;
}

#[tokio::test]
async fn every_precommit_fault_rolls_back_candidate_acl_and_marker() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = TransitionFixture::new(&url).await;
    let ledger = fixture.ledger().await;
    for fault in [
        DisposableEpochTransitionFault::AfterCandidate,
        DisposableEpochTransitionFault::AfterRevoke,
        DisposableEpochTransitionFault::AfterMarker,
    ] {
        assert!(
            matches!(
                fixture.run(fault).await,
                Err(DisposableEpochTransitionError::Injected)
            ),
            "{fault:?}"
        );
        assert_eq!(
            fixture.state().await,
            (true, true, 0, 0),
            "rollback after {fault:?}"
        );
        assert_eq!(fixture.ledger().await, ledger);
    }
    fixture.cleanup().await;
}

#[tokio::test]
async fn transition_refuses_wrong_identity_role_paths_and_catalog_drift() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = TransitionFixture::new(&url).await;
    let ledger = fixture.ledger().await;
    let mut wrong = fixture.lineage.clone();
    wrong
        .postgres_identity
        .as_mut()
        .expect("physical identity")
        .schema
        .oid += 1;
    let wrong = DisposableEpochTransitionConfig::new(
        wrong,
        PgRoleName::new(&fixture.migrator).expect("migrator"),
        PgRoleName::new(&fixture.owner).expect("owner"),
        PgRoleName::new(&fixture.runtime).expect("runtime"),
    )
    .expect("wrong-identity config");
    assert!(run_disposable_epoch_transition_lab(
        &fixture.migrator_options,
        &fixture.runtime_options,
        &wrong,
        DisposableEpochTransitionFault::None
    )
    .await
    .is_err());
    let mut wrong_system = fixture.lineage.clone();
    wrong_system
        .postgres_identity
        .as_mut()
        .expect("physical identity")
        .system_identifier
        .0 ^= 1;
    let mut wrong_lineage = fixture.lineage.clone();
    wrong_lineage.lineage_uuid.0 = uuid::Uuid::new_v4();
    for wrong in [wrong_system, wrong_lineage] {
        let wrong = DisposableEpochTransitionConfig::new(
            wrong,
            PgRoleName::new(&fixture.migrator).expect("migrator"),
            PgRoleName::new(&fixture.owner).expect("owner"),
            PgRoleName::new(&fixture.runtime).expect("runtime"),
        )
        .expect("independent wrong identity");
        assert!(matches!(
            run_disposable_epoch_transition_lab(
                &fixture.migrator_options,
                &fixture.runtime_options,
                &wrong,
                DisposableEpochTransitionFault::None
            )
            .await,
            Err(DisposableEpochTransitionError::Identity)
        ));
    }
    let wrong_current = fixture
        .migrator_options
        .clone()
        .options([("role", fixture.owner.as_str())]);
    assert!(matches!(
        run_disposable_epoch_transition_lab(
            &wrong_current,
            &fixture.runtime_options,
            &fixture.config(),
            DisposableEpochTransitionFault::None
        )
        .await,
        Err(DisposableEpochTransitionError::Roles)
    ));
    assert!(matches!(
        run_disposable_epoch_transition_lab(
            &fixture.runtime_options,
            &fixture.runtime_options,
            &fixture.config(),
            DisposableEpochTransitionFault::None
        )
        .await,
        Err(DisposableEpochTransitionError::Roles)
    ));

    let outsider = format!(
        "migration_transition_lab_x_{}",
        uuid::Uuid::new_v4().simple()
    );
    fixture
        .admin(&format!("CREATE ROLE {outsider} LOGIN"))
        .await;
    let mut outsider_login =
        PgConnection::connect_with(&fixture.migrator_options.clone().username(&outsider))
            .await
            .expect("independent fourth login");
    for role in [&fixture.owner, &fixture.migrator, &fixture.runtime] {
        fixture
            .admin(&format!(
                "GRANT {role} TO {outsider} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"
            ))
            .await;
        sqlx::query(&format!("SET ROLE {role}"))
            .execute(&mut outsider_login)
            .await
            .expect("fourth login can assume the granted role");
        let identity: (String, String) =
            sqlx::query_as("SELECT session_user::text, current_user::text")
                .fetch_one(&mut outsider_login)
                .await
                .expect("fourth login role identity");
        assert_eq!(
            (identity.0.as_str(), identity.1.as_str()),
            (outsider.as_str(), role.as_str())
        );
        if role == &fixture.migrator {
            sqlx::query(&format!("SET ROLE {}", fixture.owner))
                .execute(&mut outsider_login)
                .await
                .expect("fourth login can also assume the owner through migrator");
        }
        sqlx::query("RESET ROLE")
            .execute(&mut outsider_login)
            .await
            .expect("reset fourth login role");
        assert!(matches!(
            fixture.run(DisposableEpochTransitionFault::None).await,
            Err(DisposableEpochTransitionError::Roles)
        ));
        fixture
            .admin(&format!("REVOKE {role} FROM {outsider}"))
            .await;
    }
    fixture
        .admin(&format!(
            "GRANT USAGE ON SCHEMA {} TO {outsider}",
            fixture.schema
        ))
        .await;
    for role in [&fixture.migrator, &outsider] {
        for (privileges, grant_option) in [
            ("INSERT, UPDATE, DELETE", ""),
            ("SELECT", " WITH GRANT OPTION"),
            ("UPDATE (checksum)", " WITH GRANT OPTION"),
            ("INSERT (version, checksum)", ""),
        ] {
            fixture
                .admin(&format!(
                    "GRANT {privileges} ON {}.waddle_epoch_transition_lab_marker TO {role}{grant_option}",
                    fixture.schema
                ))
                .await;
            if role == &outsider && privileges == "INSERT, UPDATE, DELETE" {
                let mut write_tx = outsider_login
                    .begin()
                    .await
                    .expect("outsider write transaction");
                for statement in [
                    format!("INSERT INTO {}.waddle_epoch_transition_lab_marker VALUES ('forged', 'checksum')", fixture.schema),
                    format!("UPDATE {}.waddle_epoch_transition_lab_marker SET checksum = 'modified'", fixture.schema),
                    format!("DELETE FROM {}.waddle_epoch_transition_lab_marker", fixture.schema),
                ] {
                    assert_eq!(sqlx::query(&statement).execute(&mut *write_tx).await.expect("outsider marker write").rows_affected(), 1);
                }
                write_tx.rollback().await.expect("discard outsider writes");
            }
            assert!(
                matches!(
                    fixture.run(DisposableEpochTransitionFault::None).await,
                    Err(DisposableEpochTransitionError::Marker)
                ),
                "accepted marker {privileges}{grant_option} for {role}"
            );
            fixture
                .admin(&format!(
                    "REVOKE {privileges} ON {}.waddle_epoch_transition_lab_marker FROM {role}",
                    fixture.schema
                ))
                .await;
            assert_eq!(fixture.state().await, (true, true, 0, 0));
            assert_eq!(fixture.ledger().await, ledger);
        }
    }
    fixture
        .admin(&format!(
            "REVOKE USAGE ON SCHEMA {} FROM {outsider}",
            fixture.schema
        ))
        .await;
    drop(outsider_login);
    fixture.admin(&format!("DROP ROLE {outsider}")).await;

    for (change, restore) in [
        (
            format!("ALTER ROLE {} LOGIN", fixture.owner),
            format!("ALTER ROLE {} NOLOGIN", fixture.owner),
        ),
        (
            format!("ALTER ROLE {} CREATEROLE", fixture.migrator),
            format!("ALTER ROLE {} NOCREATEROLE", fixture.migrator),
        ),
        (
            format!(
                "GRANT pg_read_all_stats TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
                fixture.migrator
            ),
            format!("REVOKE pg_read_all_stats FROM {}", fixture.migrator),
        ),
        (
            format!(
                "GRANT pg_read_all_stats TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
                fixture.owner
            ),
            format!("REVOKE pg_read_all_stats FROM {}", fixture.owner),
        ),
        (
            format!(
                "GRANT TEMP ON DATABASE {} TO {}",
                fixture.database, fixture.migrator
            ),
            format!(
                "REVOKE TEMP ON DATABASE {} FROM {}",
                fixture.database, fixture.migrator
            ),
        ),
        (
            format!(
                "GRANT TEMP ON DATABASE {} TO {}",
                fixture.database, fixture.owner
            ),
            format!(
                "REVOKE TEMP ON DATABASE {} FROM {}",
                fixture.database, fixture.owner
            ),
        ),
        (
            format!(
                "GRANT TEMP ON DATABASE {} TO {}",
                fixture.database, fixture.runtime
            ),
            format!(
                "REVOKE TEMP ON DATABASE {} FROM {}",
                fixture.database, fixture.runtime
            ),
        ),
        (
            format!(
                "GRANT UPDATE (epoch) ON {}.ingress_protocol_epoch TO {}",
                fixture.schema, fixture.runtime
            ),
            format!(
                "REVOKE UPDATE (epoch) ON {}.ingress_protocol_epoch FROM {}",
                fixture.schema, fixture.runtime
            ),
        ),
        (
            format!(
                "GRANT SELECT ON {}.ingress_protocol_epoch TO PUBLIC",
                fixture.schema
            ),
            format!(
                "REVOKE SELECT ON {}.ingress_protocol_epoch FROM PUBLIC",
                fixture.schema
            ),
        ),
        (
            format!(
                "GRANT SELECT ON {}.waddle_epoch_transition_lab_marker TO {}",
                fixture.schema, fixture.runtime
            ),
            format!(
                "REVOKE SELECT ON {}.waddle_epoch_transition_lab_marker FROM {}",
                fixture.schema, fixture.runtime
            ),
        ),
    ] {
        fixture.admin(&change).await;
        assert!(
            fixture
                .run(DisposableEpochTransitionFault::None)
                .await
                .is_err(),
            "accepted {change}"
        );
        fixture.admin(&restore).await;
        assert_eq!(
            fixture.state().await,
            (true, true, 0, 0),
            "mutated after {change}"
        );
        assert_eq!(fixture.ledger().await, ledger);
    }
    for options in [
        "INHERIT TRUE, SET TRUE, ADMIN FALSE",
        "INHERIT FALSE, SET FALSE, ADMIN FALSE",
        "INHERIT FALSE, SET TRUE, ADMIN TRUE",
    ] {
        fixture
            .admin(&format!(
                "REVOKE {} FROM {}",
                fixture.owner, fixture.migrator
            ))
            .await;
        fixture
            .admin(&format!(
                "GRANT {} TO {} WITH {options}",
                fixture.owner, fixture.migrator
            ))
            .await;
        assert!(
            matches!(
                fixture.run(DisposableEpochTransitionFault::None).await,
                Err(DisposableEpochTransitionError::Roles)
            ),
            "accepted membership {options}"
        );
    }
    fixture
        .admin(&format!(
            "REVOKE {} FROM {}",
            fixture.owner, fixture.migrator
        ))
        .await;
    fixture
        .admin(&format!(
            "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
            fixture.owner, fixture.migrator
        ))
        .await;
    fixture.admin(&format!("CREATE FUNCTION {}.lab_marker_hook() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$", fixture.schema)).await;
    fixture.admin(&format!("CREATE TRIGGER lab_marker_hook BEFORE INSERT ON {}.waddle_epoch_transition_lab_marker FOR EACH ROW EXECUTE FUNCTION {}.lab_marker_hook()", fixture.schema, fixture.schema)).await;
    assert!(matches!(
        fixture.run(DisposableEpochTransitionFault::None).await,
        Err(DisposableEpochTransitionError::Marker)
    ));
    fixture
        .admin(&format!(
            "DROP TRIGGER lab_marker_hook ON {}.waddle_epoch_transition_lab_marker",
            fixture.schema
        ))
        .await;
    fixture
        .admin(&format!(
            "DROP FUNCTION {}.lab_marker_hook()",
            fixture.schema
        ))
        .await;
    fixture
        .admin(&format!(
            "CREATE INDEX lab_marker_extra ON {}.waddle_epoch_transition_lab_marker (checksum)",
            fixture.schema
        ))
        .await;
    assert!(matches!(
        fixture.run(DisposableEpochTransitionFault::None).await,
        Err(DisposableEpochTransitionError::Marker)
    ));
    fixture
        .admin(&format!("DROP INDEX {}.lab_marker_extra", fixture.schema))
        .await;
    for table in waddle_server::ingress_substrate::EPOCH_GUARDED_TABLES {
        let trigger = format!("{table}_epoch_guard_dml");
        fixture
            .admin(&format!(
                "ALTER TABLE {}.{table} DISABLE TRIGGER {trigger}",
                fixture.schema
            ))
            .await;
        assert!(
            fixture
                .run(DisposableEpochTransitionFault::None)
                .await
                .is_err(),
            "accepted disabled guard on {table}"
        );
        fixture
            .admin(&format!(
                "ALTER TABLE {}.{table} ENABLE ALWAYS TRIGGER {trigger}",
                fixture.schema
            ))
            .await;
    }
    assert_eq!(fixture.state().await, (true, true, 0, 0));
    assert_eq!(fixture.ledger().await, ledger);
    fixture.cleanup().await;
}

#[tokio::test]
async fn transition_acquires_advisory_and_epoch_locks_in_required_order() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    const LINEAGE_LOCK: i64 = 6_841_445_497_037_937_993;
    const MIGRATION_LOCK: i64 = 6_841_445_497_037_937_992;
    let fixture = TransitionFixture::new(&url).await;
    let mut migration_holder = fixture.admin.begin().await.expect("migration lock holder");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *migration_holder)
        .await
        .expect("hold migration lock");
    let migration_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *migration_holder)
        .await
        .expect("migration holder PID");
    let mut epoch_holder = fixture.admin.begin().await.expect("epoch lock holder");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *epoch_holder)
    .await
    .expect("hold epoch lock");
    let epoch_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *epoch_holder)
        .await
        .expect("epoch holder PID");
    let mut marker_holder = fixture.admin.begin().await.expect("marker lock holder");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.waddle_epoch_transition_lab_marker IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *marker_holder)
    .await
    .expect("hold marker lock");
    let marker_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *marker_holder)
        .await
        .expect("marker holder PID");
    let application = format!("transition-locks-{}", uuid::Uuid::new_v4().simple());
    let migrator = fixture
        .migrator_options
        .clone()
        .application_name(&application);
    let runtime = fixture.runtime_options.clone();
    let config = fixture.config();
    tokio::task::LocalSet::new().run_until(async {
        let operation = tokio::task::spawn_local(async move {
            run_disposable_epoch_transition_lab(&migrator, &runtime, &config, DisposableEpochTransitionFault::None).await
        });
        let pid = wait_for_application(&fixture.admin, &application, migration_pid).await;
        let mut probe = fixture.admin.begin().await.expect("lineage probe");
        let acquired: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
            .bind(LINEAGE_LOCK).fetch_one(&mut *probe).await.expect("probe lineage lock");
        assert!(!acquired, "lineage lock must precede migration lock");
        probe.rollback().await.expect("release probe");
        migration_holder.rollback().await.expect("release migration lock");
        assert_eq!(wait_for_application(&fixture.admin, &application, epoch_pid).await, pid);
        let mut probe = fixture.admin.begin().await.expect("migration probe");
        let acquired: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
            .bind(MIGRATION_LOCK).fetch_one(&mut *probe).await.expect("probe migration lock");
        assert!(!acquired, "migration lock must precede epoch lock");
        probe.rollback().await.expect("release probe");
        epoch_holder.rollback().await.expect("release epoch lock");
        assert_eq!(wait_for_application(&fixture.admin, &application, marker_pid).await, pid);
        let modes: Vec<String> = sqlx::query_scalar("SELECT mode::text FROM pg_catalog.pg_locks WHERE pid = $1 AND relation = pg_catalog.to_regclass($2) AND granted")
            .bind(pid).bind(format!("{}.ingress_protocol_epoch", fixture.schema)).fetch_all(&fixture.admin).await.expect("observe held epoch locks");
        assert!(modes.iter().any(|mode| mode == "AccessExclusiveLock"), "{modes:?}");
        assert!(modes.iter().any(|mode| mode == "AccessShareLock"), "{modes:?}");
        marker_holder.rollback().await.expect("release marker lock");
        operation.await.expect("transition task").expect("commit after locks release");
    }).await;
    assert_eq!(fixture.state().await, (false, false, 1, 0));
    fixture.cleanup().await;
}

#[tokio::test]
async fn lost_commit_ack_is_unknown_and_never_replayed_as_success() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = TransitionFixture::new(&url).await;
    let ledger = fixture.ledger().await;
    assert!(matches!(
        fixture
            .run(DisposableEpochTransitionFault::CommitAcknowledgementLost)
            .await,
        Err(DisposableEpochTransitionError::CommitUnknown)
    ));
    assert_eq!(
        fixture.state().await,
        (false, false, 1, 0),
        "fixture observed a committed transaction despite the synthetic lost ACK"
    );
    assert_eq!(fixture.ledger().await, ledger);
    assert!(matches!(
        fixture.run(DisposableEpochTransitionFault::None).await,
        Err(DisposableEpochTransitionError::Marker)
    ));
    assert_eq!(
        fixture.state().await,
        (false, false, 1, 0),
        "replay did not alter committed state"
    );
    fixture.cleanup().await;
}

#[cfg(unix)]
struct CommitProxy {
    options: PgConnectOptions,
    committed: tokio::sync::oneshot::Receiver<()>,
    release: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

#[cfg(unix)]
async fn relay_until_commit<S>(
    client: tokio::net::TcpStream,
    upstream: S,
    committed: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
) -> std::io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut server_read, mut server_write) = tokio::io::split(upstream);
    let client_to_server =
        tokio::spawn(async move { tokio::io::copy(&mut client_read, &mut server_write).await });
    loop {
        let mut header = [0_u8; 5];
        server_read.read_exact(&mut header).await?;
        let length = u32::from_be_bytes(header[1..5].try_into().expect("message length")) as usize;
        let mut payload = vec![0; length - 4];
        server_read.read_exact(&mut payload).await?;
        if header[0] == b'C' && payload == b"COMMIT\0" {
            let _ = committed.send(());
            let _ = release.await;
            break;
        }
        client_write.write_all(&header).await?;
        client_write.write_all(&payload).await?;
    }
    client_to_server.abort();
    Ok(())
}

#[cfg(unix)]
async fn commit_proxy(fixture: &TransitionFixture) -> CommitProxy {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy listener");
    let address = listener.local_addr().expect("proxy address");
    let socket = fixture.migrator_options.get_socket().cloned();
    let host = fixture.migrator_options.get_host().to_owned();
    let port = fixture.migrator_options.get_port();
    let (committed_tx, committed) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (client, _) = listener.accept().await?;
        if let Some(socket) = socket {
            let upstream =
                tokio::net::UnixStream::connect(socket.join(format!(".s.PGSQL.{port}"))).await?;
            relay_until_commit(client, upstream, committed_tx, release_rx).await
        } else {
            let upstream = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
            relay_until_commit(client, upstream, committed_tx, release_rx).await
        }
    });
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(address.port())
        .username(&fixture.migrator)
        .database(&fixture.database)
        .options([("search_path", fixture.schema.as_str())])
        .ssl_mode(sqlx::postgres::PgSslMode::Disable);
    CommitProxy {
        options,
        committed,
        release,
        task,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn real_lost_commit_response_is_unknown_without_replay() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = TransitionFixture::new(&url).await;
    let ledger = fixture.ledger().await;
    let CommitProxy {
        options,
        committed,
        release,
        task: proxy,
    } = commit_proxy(&fixture).await;
    let runtime = fixture.runtime_options.clone();
    let config = fixture.config();
    tokio::task::LocalSet::new()
        .run_until(async {
            let operation = tokio::task::spawn_local(async move {
                run_disposable_epoch_transition_lab(
                    &options,
                    &runtime,
                    &config,
                    DisposableEpochTransitionFault::None,
                )
                .await
            });
            tokio::time::timeout(Duration::from_secs(10), committed)
                .await
                .expect("server emitted COMMIT response")
                .expect("proxy active");
            assert_eq!(
                fixture.state().await,
                (false, false, 1, 0),
                "independent session observes committed state"
            );
            release.send(()).expect("drop withheld response");
            let result = tokio::time::timeout(Duration::from_secs(10), operation)
                .await
                .expect("operation finishes")
                .expect("operation task");
            assert!(
                matches!(result, Err(DisposableEpochTransitionError::CommitUnknown)),
                "{result:?}"
            );
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .expect("proxy ends")
        .expect("proxy task")
        .expect("proxy connection");
    assert_eq!(fixture.ledger().await, ledger);
    assert!(matches!(
        fixture.run(DisposableEpochTransitionFault::None).await,
        Err(DisposableEpochTransitionError::Marker)
    ));
    fixture.cleanup().await;
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_while_commit_response_is_withheld_requires_reconciliation() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = TransitionFixture::new(&url).await;
    let ledger = fixture.ledger().await;
    let CommitProxy {
        options,
        committed,
        release,
        task: proxy,
    } = commit_proxy(&fixture).await;
    let runtime = fixture.runtime_options.clone();
    let config = fixture.config();
    tokio::task::LocalSet::new()
        .run_until(async {
            let operation = tokio::task::spawn_local(async move {
                run_disposable_epoch_transition_lab(
                    &options,
                    &runtime,
                    &config,
                    DisposableEpochTransitionFault::None,
                )
                .await
            });
            tokio::time::timeout(Duration::from_secs(10), committed)
                .await
                .expect("server emitted COMMIT response")
                .expect("proxy active");
            operation.abort();
            assert!(operation
                .await
                .expect_err("caller cancellation")
                .is_cancelled());
            release.send(()).expect("release proxy");
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .expect("proxy ends")
        .expect("proxy task")
        .expect("proxy connection");
    assert_eq!(
        fixture.state().await,
        (false, false, 1, 0),
        "independent observation requires reconciliation after cancellation"
    );
    assert_eq!(fixture.ledger().await, ledger);
    fixture.cleanup().await;
}
