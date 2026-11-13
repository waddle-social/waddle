use super::{
    prepare_dark_room_observation_schema, verify_attested_room_observation_schema,
    RoomObservationSchemaError, ROOM_OBSERVATION_OWNER_GATE_LOCK,
    ROOM_OBSERVATION_OWNER_STATE_TABLE, ROOM_OBSERVATION_SCHEMA_LOCK,
    ROOM_OBSERVATION_SCHEMA_REVISION,
};
use anyhow::{bail, Result};
use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;

use crate::config::LineageConfig;
use crate::db::lineage::{self, AttestedLineage, DeploymentUuid, PgSchemaName};
use crate::db::physical_attestation::{
    attested_pool, install, PgRoleName, PhysicalAttestationConfig,
    CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use crate::db::Database;
use crate::db::{DatabaseConfig, DatabaseDriver, MigrationRunner};

struct Fixture {
    cluster_admin: PgPool,
    admin: PgPool,
    base_url: String,
    database: String,
    owner: String,
    migrator: String,
    runtime: String,
    installer: String,
    definer: String,
    outsider: String,
    target: String,
    trusted: String,
    secret: String,
    owner_database: Option<Database>,
    runtime_database: Option<Database>,
}

impl Fixture {
    async fn new(url: String) -> Self {
        let cluster_admin = PgPool::connect(&url)
            .await
            .expect("connect disposable PostgreSQL cluster");
        let suffix = Uuid::new_v4().simple().to_string();
        let database = format!("room_observer_{suffix}");
        let owner = format!("room_observer_o_{suffix}");
        let migrator = format!("room_observer_m_{suffix}");
        let runtime = format!("room_observer_r_{suffix}");
        let installer = format!("room_observer_i_{suffix}");
        let definer = format!("room_observer_d_{suffix}");
        let outsider = format!("room_observer_x_{suffix}");
        let target = format!("room_observer_t_{suffix}");
        let trusted = format!("room_observer_f_{suffix}");
        let secret = format!("room_observer_s_{suffix}");

        sqlx::query(&format!("CREATE DATABASE {database}"))
            .execute(&cluster_admin)
            .await
            .expect("create isolated test database");
        sqlx::query(&format!(
            "REVOKE CREATE, TEMPORARY ON DATABASE {database} FROM PUBLIC"
        ))
        .execute(&cluster_admin)
        .await
        .expect("restrict database defaults");
        for role in [&owner, &definer] {
            sqlx::query(&format!(
                "CREATE ROLE {role} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"
            ))
            .execute(&cluster_admin)
            .await
            .expect("create no-login role");
        }
        for role in [&migrator, &runtime, &installer, &outsider] {
            sqlx::query(&format!(
                "CREATE ROLE {role} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"
            ))
            .execute(&cluster_admin)
            .await
            .expect("create login role");
        }
        sqlx::query(&format!(
            "GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"
        ))
        .execute(&cluster_admin)
        .await
        .expect("allow the test migrator to assume the owner role");
        sqlx::query(&format!(
            "GRANT {definer} TO {installer} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"
        ))
        .execute(&cluster_admin)
        .await
        .expect("grant restricted attestation definer handoff");

        let base_url = url_with_database(&url, &database);
        let admin = PgPool::connect(&base_url)
            .await
            .expect("connect isolated database administrator");
        sqlx::query("REVOKE ALL ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC")
            .execute(&admin)
            .await
            .expect("deny runtime physical identity reads through PUBLIC");
        sqlx::query("REVOKE ALL ON SCHEMA public FROM PUBLIC")
            .execute(&admin)
            .await
            .expect("restrict public schema");
        sqlx::query(&format!("CREATE SCHEMA {target} AUTHORIZATION {owner}"))
            .execute(&admin)
            .await
            .expect("create owner-controlled schema");
        sqlx::query(&format!("CREATE SCHEMA {trusted} AUTHORIZATION {definer}"))
            .execute(&admin)
            .await
            .expect("create trusted definer schema");
        sqlx::query(&format!("CREATE SCHEMA {secret} AUTHORIZATION {definer}"))
            .execute(&admin)
            .await
            .expect("create second owner-controlled schema");
        sqlx::query(&format!(
            "GRANT USAGE, CREATE ON SCHEMA {secret} TO {owner}"
        ))
        .execute(&admin)
        .await
        .expect("allow target owner to create in a runtime-inaccessible schema");
        for role in [&owner, &definer] {
            sqlx::query(&format!(
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {role}"
            ))
            .execute(&admin)
            .await
            .expect("grant physical identity read");
        }

        Self {
            cluster_admin,
            admin,
            base_url,
            database,
            owner,
            migrator,
            runtime,
            installer,
            definer,
            outsider,
            target,
            trusted,
            secret,
            owner_database: None,
            runtime_database: None,
        }
    }

    async fn exercise(&mut self) -> Result<()> {
        let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
            .fetch_one(&self.admin)
            .await?;
        if version < 170_000 {
            bail!("room-observation owner/runtime fixture requires PostgreSQL 17");
        }

        let owner_url = url_with_options(
            &self.base_url,
            &format!("-c role={} -c search_path={}", self.owner, self.target),
        );
        let owner_url = url_with_username(&owner_url, &self.migrator);
        let mut owner_config = DatabaseConfig::new(DatabaseDriver::Postgres, owner_url);
        owner_config.pool_size = 1;
        owner_config.acquire_timeout = Duration::from_secs(3);
        let owner_database =
            Database::from_config("room-observation schema owner", &owner_config).await?;
        let lineage = lineage::enroll(
            &owner_database,
            &LineageConfig {
                deployment_uuid: Some(DeploymentUuid(Uuid::new_v4())),
                action: None,
            },
        )
        .await?;
        let identity = lineage
            .postgres_identity
            .clone()
            .expect("PostgreSQL lineage identity");
        let designated_migrator = PgRoleName::new(self.migrator.clone())?;
        let attestation_definer = PgRoleName::new(self.definer.clone())?;
        self.initialize_room_observations_with_catalog_blocker(&owner_database)
            .await?;
        MigrationRunner::single().run(&owner_database).await?;
        owner_database
            .execute(&format!(
                "GRANT USAGE ON SCHEMA {} TO {}, {}",
                self.target, self.runtime, self.definer
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT SELECT ON {}._lineage TO {}, {}",
                self.target, self.runtime, self.definer
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT SELECT ON {}._migrations TO {}",
                self.target, self.runtime
            ))
            .await?;

        let runtime_name = PgRoleName::new(self.runtime.clone())?;
        let installer_name = PgRoleName::new(self.installer.clone())?;
        let definer_name = PgRoleName::new(self.definer.clone())?;
        let attestation = PhysicalAttestationConfig::new(
            lineage.clone(),
            installer_name,
            runtime_name,
            definer_name,
            PgSchemaName(self.trusted.clone()),
        )?;
        let installer_url =
            url_with_options(&self.base_url, &format!("-c search_path={}", self.target));
        let installer_options =
            PgConnectOptions::from_str(&installer_url)?.username(&self.installer);
        install(&installer_options, attestation.clone(), Uuid::new_v4()).await?;

        let runtime_url =
            url_with_options(&self.base_url, &format!("-c search_path={}", self.target));
        let runtime_options = PgConnectOptions::from_str(&runtime_url)?.username(&self.runtime);
        let pool = attested_pool(runtime_options, attestation, 1, Duration::from_secs(3))?;
        let runtime_database = crate::test_support::database_from_attested_postgres_pool(
            "room-observer runtime",
            pool,
        );

        self.owner_database = Some(owner_database);
        self.runtime_database = Some(runtime_database);

        let owner_database = self.owner_database.as_ref().expect("owner database");
        let runtime_database = self.runtime_database.as_ref().expect("runtime database");
        let runtime_can_read_physical_identity: bool = sqlx::query_scalar(
            "SELECT pg_catalog.has_function_privilege($1, \
             'pg_catalog.pg_control_system()', 'EXECUTE')",
        )
        .bind(&self.runtime)
        .fetch_one(&self.admin)
        .await?;
        assert!(!runtime_can_read_physical_identity);

        let missing_marker = verify_attested_room_observation_schema(
            runtime_database,
            &lineage,
            &designated_migrator,
        )
        .await;
        assert!(
            matches!(
                &missing_marker,
                Err(RoomObservationSchemaError::OwnerPreparationIncomplete)
            ),
            "expected missing-marker rejection, got {missing_marker:?}"
        );
        assert!(!marker_exists(&self.admin, &self.target).await?);

        self.cancel_owner_waits_without_marker(owner_database, &lineage, &designated_migrator)
            .await?;
        assert!(!marker_exists(&self.admin, &self.target).await?);

        let ledger: (String, Option<String>) = sqlx::query_as(&format!(
            "SELECT description, checksum FROM {}._migrations WHERE version = 2000",
            self.target
        ))
        .fetch_one(&self.admin)
        .await?;
        sqlx::query(&format!(
            "UPDATE {}._migrations SET checksum = 'wrong' WHERE version = 2000",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            prepare_dark_room_observation_schema(
                owner_database,
                &lineage,
                &designated_migrator,
                &attestation_definer,
                &PgRoleName::new(self.runtime.clone())?,
            )
            .await,
            Err(RoomObservationSchemaError::Database)
        ));
        assert!(!marker_exists(&self.admin, &self.target).await?);
        sqlx::query(&format!(
            "UPDATE {}._migrations SET description = $1, checksum = $2 WHERE version = 2000",
            self.target
        ))
        .bind(&ledger.0)
        .bind(&ledger.1)
        .execute(&self.admin)
        .await?;

        let wrong_migrator = PgRoleName::new(self.outsider.clone())?;
        assert!(matches!(
            prepare_dark_room_observation_schema(
                owner_database,
                &lineage,
                &wrong_migrator,
                &attestation_definer,
                &PgRoleName::new(self.runtime.clone())?,
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeOwner)
        ));
        assert!(!marker_exists(&self.admin, &self.target).await?);

        let evidence = prepare_dark_room_observation_schema(
            owner_database,
            &lineage,
            &designated_migrator,
            &attestation_definer,
            &PgRoleName::new(self.runtime.clone())?,
        )
        .await
        .map_err(|error| anyhow::anyhow!("initial owner preparation failed: {error:?}"))?;
        assert_eq!(evidence.physical_identity(), &identity);
        assert_eq!(evidence.revision(), ROOM_OBSERVATION_SCHEMA_REVISION);
        prepare_dark_room_observation_schema(
            owner_database,
            &lineage,
            &designated_migrator,
            &attestation_definer,
            &PgRoleName::new(self.runtime.clone())?,
        )
        .await
        .unwrap_or_else(|error| panic!("owner preparation is not idempotent: {error:?}"));

        let complete = verify_attested_room_observation_schema(
            runtime_database,
            &lineage,
            &designated_migrator,
        )
        .await
        .map_err(|error| anyhow::anyhow!("completed schema verification failed: {error:?}"))?;
        assert_eq!(complete.physical_identity(), &identity);
        assert_eq!(complete.revision(), ROOM_OBSERVATION_SCHEMA_REVISION);
        self.verify_reader_catalog_and_ledger_contention(
            runtime_database,
            &lineage,
            &designated_migrator,
        )
        .await?;

        let stored_ledger: (String, Option<String>) = sqlx::query_as(&format!(
            "SELECT description, checksum FROM {}._migrations WHERE version = 2000",
            self.target
        ))
        .fetch_one(&self.admin)
        .await?;
        sqlx::query(&format!(
            "UPDATE {}._migrations SET description = 'wrong' WHERE version = 2000",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::Database)
        ));
        sqlx::query(&format!(
            "UPDATE {}._migrations SET description = $1 WHERE version = 2000",
            self.target
        ))
        .bind(&stored_ledger.0)
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "UPDATE {}._migrations SET checksum = 'wrong' WHERE version = 2000",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::Database)
        ));
        sqlx::query(&format!(
            "UPDATE {}._migrations SET checksum = $1 WHERE version = 2000",
            self.target
        ))
        .bind(&stored_ledger.1)
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "DELETE FROM {}._migrations WHERE version = 2000",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::Database)
        ));
        sqlx::query(&format!(
            "INSERT INTO {}._migrations (version, description, checksum) VALUES (2000, $1, $2)",
            self.target
        ))
        .bind(&stored_ledger.0)
        .bind(&stored_ledger.1)
        .execute(&self.admin)
        .await?;

        let mut wrong_identity = identity.clone();
        wrong_identity.system_identifier.0 ^= 1;
        let mut wrong_lineage_identity = lineage.clone();
        wrong_lineage_identity.postgres_identity = Some(wrong_identity);
        assert!(matches!(
            prepare_dark_room_observation_schema(
                owner_database,
                &wrong_lineage_identity,
                &designated_migrator,
                &attestation_definer,
                &PgRoleName::new(self.runtime.clone())?,
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeOwner)
                | Err(RoomObservationSchemaError::IdentityMismatch)
        ));

        sqlx::query(&format!(
            "DROP TABLE {}.{}",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::OwnerPreparationIncomplete)
        ));
        assert!(!marker_exists(&self.admin, &self.target).await?);
        prepare_dark_room_observation_schema(
            owner_database,
            &lineage,
            &designated_migrator,
            &attestation_definer,
            &PgRoleName::new(self.runtime.clone())?,
        )
        .await?;

        sqlx::query(&format!(
            "UPDATE {}.{} SET prepared_revision = '0' WHERE singleton = '1'",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::OwnerPreparationIncomplete)
        ));
        prepare_dark_room_observation_schema(
            owner_database,
            &lineage,
            &designated_migrator,
            &attestation_definer,
            &PgRoleName::new(self.runtime.clone())?,
        )
        .await?;

        sqlx::query(&format!(
            "ALTER TABLE {}.extension_room_sources ADD COLUMN unexpected TEXT",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        sqlx::query(&format!(
            "ALTER TABLE {}.extension_room_sources DROP COLUMN unexpected",
            self.target
        ))
        .execute(&self.admin)
        .await?;

        sqlx::query(&format!(
            "ALTER TABLE {}.extension_room_observation_work \
             ADD CONSTRAINT room_observation_bad_check CHECK (generation > 0)",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        sqlx::query(&format!(
            "ALTER TABLE {}.extension_room_observation_work \
             DROP CONSTRAINT room_observation_bad_check",
            self.target
        ))
        .execute(&self.admin)
        .await?;

        sqlx::query(&format!(
            "DROP INDEX {}.extension_room_observation_work_due",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        sqlx::query(&format!(
            "CREATE INDEX extension_room_observation_work_due \
             ON {}.extension_room_observation_work (plugin_id, room_jid, status, due_at_ms)",
            self.target
        ))
        .execute(&self.admin)
        .await?;

        sqlx::query(&format!(
            "DROP INDEX {}.extension_room_observation_work_settled",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        sqlx::query(&format!(
            "CREATE INDEX extension_room_observation_work_settled \
             ON {}.extension_room_observation_work (settled_at_ms, id) \
             WHERE status = 'completed'",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        sqlx::query(&format!(
            "DROP INDEX {}.extension_room_observation_work_settled",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "CREATE INDEX extension_room_observation_work_settled \
             ON {}.extension_room_observation_work (settled_at_ms, id) \
             WHERE status IN ('completed', 'terminal', 'stale')",
            self.target
        ))
        .execute(&self.admin)
        .await?;

        sqlx::query(&format!(
            "REVOKE SELECT ON TABLE {}.extension_room_sources FROM pg_monitor",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        sqlx::query(&format!(
            "GRANT SELECT ON TABLE {}.extension_room_sources TO pg_monitor",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        sqlx::query(&format!("GRANT USAGE ON SCHEMA {} TO PUBLIC", self.target))
            .execute(&self.admin)
            .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeRuntime)
        ));
        sqlx::query(&format!(
            "REVOKE USAGE ON SCHEMA {} FROM PUBLIC",
            self.target
        ))
        .execute(&self.admin)
        .await?;

        sqlx::query(&format!(
            "GRANT TRUNCATE ON {}.extension_room_observation_receipts TO {}",
            self.target, self.runtime
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeRuntime)
        ));
        sqlx::query(&format!(
            "REVOKE TRUNCATE ON {}.extension_room_observation_receipts FROM {}",
            self.target, self.runtime
        ))
        .execute(&self.admin)
        .await?;

        sqlx::query(&format!(
            "GRANT UPDATE (prepared_revision) ON {}.{} TO {}",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE, self.outsider
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeRuntime)
        ));
        sqlx::query(&format!(
            "REVOKE UPDATE (prepared_revision) ON {}.{} FROM {}",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE, self.outsider
        ))
        .execute(&self.admin)
        .await?;

        sqlx::query(&format!(
            "GRANT MAINTAIN ON {}.extension_room_observation_work TO {}",
            self.target, self.runtime
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeRuntime)
        ));
        sqlx::query(&format!(
            "REVOKE MAINTAIN ON {}.extension_room_observation_work FROM {}",
            self.target, self.runtime
        ))
        .execute(&self.admin)
        .await?;

        self.verify_waiting_attach_observes_completed_marker(
            runtime_database,
            &lineage,
            &designated_migrator,
        )
        .await?;

        let owner_membership = format!("REVOKE {} FROM {}", self.owner, self.migrator);
        sqlx::query(&owner_membership)
            .execute(&self.cluster_admin)
            .await?;
        sqlx::query(&format!(
            "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
            self.owner, self.outsider
        ))
        .execute(&self.cluster_admin)
        .await?;

        let attacker_url = url_with_options(
            &self.base_url,
            &format!("-c role={} -c search_path={}", self.owner, self.target),
        );
        let attacker_options = PgConnectOptions::from_str(&attacker_url)?.username(&self.outsider);
        let mut attacker = PgConnection::connect_with(&attacker_options).await?;
        sqlx::query(&format!(
            "UPDATE {}.{} SET schema_owner_member_oid = \
             (SELECT oid::bigint FROM pg_catalog.pg_roles WHERE rolname = session_user), \
             schema_owner_member_name = session_user WHERE singleton = '1'",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
        ))
        .execute(&mut attacker)
        .await?;
        attacker.close().await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::IdentityMismatch)
                | Err(RoomObservationSchemaError::UnsafeOwner)
        ));

        sqlx::query(&format!("REVOKE {} FROM {}", self.owner, self.outsider))
            .execute(&self.cluster_admin)
            .await?;
        sqlx::query(&format!(
            "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
            self.owner, self.migrator
        ))
        .execute(&self.cluster_admin)
        .await?;
        sqlx::query(&format!(
            "UPDATE {}.{} SET schema_owner_member_oid = \
             (SELECT oid::bigint FROM pg_catalog.pg_roles WHERE rolname = $1), \
             schema_owner_member_name = $1 WHERE singleton = '1'",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
        ))
        .bind(&self.migrator)
        .execute(&self.admin)
        .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        sqlx::query(&format!(
            "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
            self.migrator, self.outsider
        ))
        .execute(&self.cluster_admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeOwner)
        ));
        assert!(matches!(
            prepare_dark_room_observation_schema(
                owner_database,
                &lineage,
                &designated_migrator,
                &attestation_definer,
                &PgRoleName::new(self.runtime.clone())?,
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeOwner)
        ));
        sqlx::query(&format!("REVOKE {} FROM {}", self.migrator, self.outsider))
            .execute(&self.cluster_admin)
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        owner_database
            .execute(&format!(
                "CREATE TABLE {}.room_observation_unrelated_relation (id bigint PRIMARY KEY)",
                self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT SELECT ON {}.room_observation_unrelated_relation TO {}",
                self.target, self.runtime
            ))
            .await?;
        owner_database
            .execute(&format!(
                "CREATE VIEW {}.room_observation_unrelated_view AS \
                 SELECT id FROM {}.room_observation_unrelated_relation",
                self.target, self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT SELECT ON {}.room_observation_unrelated_view TO {}",
                self.target, self.runtime
            ))
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;
        owner_database
            .execute(&format!(
                "DROP VIEW {}.room_observation_unrelated_view",
                self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "DROP TABLE {}.room_observation_unrelated_relation",
                self.target
            ))
            .await?;

        owner_database
            .execute(&format!(
                "CREATE FUNCTION {}.room_observation_escalation() \
                 RETURNS integer LANGUAGE sql SECURITY DEFINER AS $$ SELECT 1 $$",
                self.target
            ))
            .await?;
        let runtime_can_execute_owner_definer: bool = sqlx::query_scalar(
            "SELECT pg_catalog.has_function_privilege($1, routine.oid, 'EXECUTE') \
             FROM pg_catalog.pg_proc routine \
             JOIN pg_catalog.pg_namespace namespace \
               ON namespace.oid = routine.pronamespace \
             WHERE namespace.nspname = $2 \
               AND routine.proname = 'room_observation_escalation'",
        )
        .bind(&self.runtime)
        .bind(&self.target)
        .fetch_one(&self.admin)
        .await?;
        assert!(runtime_can_execute_owner_definer);
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeRuntime)
        ));
        owner_database
            .execute(&format!(
                "DROP FUNCTION {}.room_observation_escalation()",
                self.target
            ))
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        owner_database
            .execute(&format!(
                "CREATE FUNCTION {}.room_observation_shadow_oid_eq(oid, oid) \
                 RETURNS boolean LANGUAGE sql IMMUTABLE AS $$ SELECT false $$",
                self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "CREATE OPERATOR {}.= (FUNCTION = {}.room_observation_shadow_oid_eq, \
                   LEFTARG = oid, RIGHTARG = oid)",
                self.target, self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "CREATE FUNCTION {}.room_observation_shadowed_escalation() \
                 RETURNS integer LANGUAGE sql SECURITY DEFINER AS $$ SELECT 1 $$",
                self.target
            ))
            .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeRuntime)
        ));
        owner_database
            .execute(&format!(
                "DROP FUNCTION {}.room_observation_shadowed_escalation()",
                self.target
            ))
            .await?;
        owner_database
            .execute(&format!("DROP OPERATOR {}.=(oid, oid)", self.target))
            .await?;
        owner_database
            .execute(&format!(
                "DROP FUNCTION {}.room_observation_shadow_oid_eq(oid, oid)",
                self.target
            ))
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        sqlx::query(&format!(
            "GRANT USAGE ON SCHEMA {} TO {}",
            self.secret, self.runtime
        ))
        .execute(&self.admin)
        .await?;
        owner_database
            .execute(&format!(
                "CREATE FUNCTION {}.room_observation_external_escalation() \
                 RETURNS integer LANGUAGE sql SECURITY DEFINER AS $$ SELECT 1 $$",
                self.secret
            ))
            .await?;
        let runtime_can_execute_external_owner_definer: bool = sqlx::query_scalar(
            "SELECT pg_catalog.has_function_privilege( \
               $1, pg_catalog.to_regprocedure($2), 'EXECUTE')",
        )
        .bind(&self.runtime)
        .bind(format!(
            "{}.room_observation_external_escalation()",
            self.secret
        ))
        .fetch_one(&self.admin)
        .await?;
        assert!(runtime_can_execute_external_owner_definer);
        runtime_database
            .execute(&format!(
                "SELECT {}.room_observation_external_escalation()",
                self.secret
            ))
            .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeRuntime)
        ));
        owner_database
            .execute(&format!(
                "DROP FUNCTION {}.room_observation_external_escalation()",
                self.secret
            ))
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        let runtime_can_read_ingress_messages: bool = sqlx::query_scalar(
            "SELECT pg_catalog.has_table_privilege( \
               $1, pg_catalog.to_regclass($2), 'SELECT')",
        )
        .bind(&self.runtime)
        .bind(format!("{}.ingress_messages", self.target))
        .fetch_one(&self.admin)
        .await?;
        assert!(!runtime_can_read_ingress_messages);
        owner_database
            .execute(&format!(
                "CREATE VIEW {}.room_observation_escalation_view AS \
                 SELECT message_key FROM {}.ingress_messages",
                self.target, self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT SELECT ON {}.room_observation_escalation_view TO {}",
                self.target, self.runtime
            ))
            .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        owner_database
            .execute(&format!(
                "DROP VIEW {}.room_observation_escalation_view",
                self.target
            ))
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        owner_database
            .execute(&format!(
                "CREATE VIEW {}.room_observation_column_escalation_view AS \
                 SELECT message_key FROM {}.ingress_messages",
                self.target, self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT SELECT (message_key) ON {}.room_observation_column_escalation_view TO {}",
                self.target, self.runtime
            ))
            .await?;
        let (runtime_has_view_select, runtime_has_column_select): (bool, bool) = sqlx::query_as(
            "SELECT pg_catalog.has_table_privilege($1, $2, 'SELECT'), \
                 pg_catalog.has_column_privilege($1, $2, 'message_key', 'SELECT')",
        )
        .bind(&self.runtime)
        .bind(format!(
            "{}.room_observation_column_escalation_view",
            self.target
        ))
        .fetch_one(&self.admin)
        .await?;
        assert!(!runtime_has_view_select);
        assert!(runtime_has_column_select);
        runtime_database
            .execute(&format!(
                "SELECT message_key FROM {}.room_observation_column_escalation_view",
                self.target
            ))
            .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        owner_database
            .execute(&format!(
                "DROP VIEW {}.room_observation_column_escalation_view",
                self.target
            ))
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        owner_database
            .execute(&format!(
                "CREATE VIEW {}.room_observation_external_escalation_view AS \
                 SELECT message_key FROM {}.ingress_messages",
                self.secret, self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT SELECT ON {}.room_observation_external_escalation_view TO {}",
                self.secret, self.runtime
            ))
            .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        owner_database
            .execute(&format!(
                "DROP VIEW {}.room_observation_external_escalation_view",
                self.secret
            ))
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;
        sqlx::query(&format!(
            "REVOKE USAGE ON SCHEMA {} FROM {}",
            self.secret, self.runtime
        ))
        .execute(&self.admin)
        .await?;

        owner_database
            .execute(&format!(
                "CREATE FUNCTION {}.room_observation_hidden_escalation() \
                 RETURNS integer LANGUAGE sql SECURITY DEFINER AS $$ SELECT 1 $$",
                self.secret
            ))
            .await?;
        owner_database
            .execute(&format!(
                "CREATE VIEW {}.room_observation_hidden_escalation_view AS \
                 SELECT {}.room_observation_hidden_escalation() AS value",
                self.target, self.secret
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT SELECT ON {}.room_observation_hidden_escalation_view TO {}",
                self.target, self.runtime
            ))
            .await?;
        let runtime_can_use_secret_schema: bool =
            sqlx::query_scalar("SELECT pg_catalog.has_schema_privilege($1, $2, 'USAGE')")
                .bind(&self.runtime)
                .bind(&self.secret)
                .fetch_one(&self.admin)
                .await?;
        let runtime_can_execute_hidden_function: bool = sqlx::query_scalar(
            "SELECT pg_catalog.has_function_privilege( \
               $1, pg_catalog.to_regprocedure($2), 'EXECUTE')",
        )
        .bind(&self.runtime)
        .bind(format!(
            "{}.room_observation_hidden_escalation()",
            self.secret
        ))
        .fetch_one(&self.admin)
        .await?;
        assert!(!runtime_can_use_secret_schema);
        assert!(runtime_can_execute_hidden_function);
        runtime_database
            .execute(&format!(
                "SELECT value FROM {}.room_observation_hidden_escalation_view",
                self.target
            ))
            .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeRuntime)
        ));
        owner_database
            .execute(&format!(
                "DROP VIEW {}.room_observation_hidden_escalation_view",
                self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "DROP FUNCTION {}.room_observation_hidden_escalation()",
                self.secret
            ))
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        owner_database
            .execute(&format!(
                "CREATE VIEW {}.room_observation_hidden_relation_view AS \
                 SELECT message_key FROM {}.ingress_messages",
                self.secret, self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT SELECT ON {}.room_observation_hidden_relation_view TO {}",
                self.secret, self.runtime
            ))
            .await?;
        owner_database
            .execute(&format!(
                "CREATE VIEW {}.room_observation_invoker_view \
                 WITH (security_invoker = true) AS \
                 SELECT message_key FROM {}.room_observation_hidden_relation_view",
                self.target, self.secret
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT SELECT ON {}.room_observation_invoker_view TO {}",
                self.target, self.runtime
            ))
            .await?;
        runtime_database
            .execute(&format!(
                "SELECT message_key FROM {}.room_observation_invoker_view",
                self.target
            ))
            .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        owner_database
            .execute(&format!(
                "DROP VIEW {}.room_observation_invoker_view",
                self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "DROP VIEW {}.room_observation_hidden_relation_view",
                self.secret
            ))
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        owner_database
            .execute(&format!(
                "CREATE MATERIALIZED VIEW {}.room_observation_escalation_matview AS \
                 SELECT message_key FROM {}.ingress_messages WITH DATA",
                self.target, self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT SELECT ON {}.room_observation_escalation_matview TO {}",
                self.target, self.runtime
            ))
            .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        owner_database
            .execute(&format!(
                "DROP MATERIALIZED VIEW {}.room_observation_escalation_matview",
                self.target
            ))
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        owner_database
            .execute(&format!(
                "CREATE VIEW {}.room_observation_update_escalation_view AS \
                 SELECT message_key FROM {}.ingress_messages",
                self.target, self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT UPDATE ON {}.room_observation_update_escalation_view TO {}",
                self.target, self.runtime
            ))
            .await?;
        let runtime_view_privileges: (bool, bool) = sqlx::query_as(
            "SELECT pg_catalog.has_table_privilege( \
               $1, pg_catalog.to_regclass($2), 'SELECT'), \
             pg_catalog.has_table_privilege( \
               $1, pg_catalog.to_regclass($2), 'UPDATE')",
        )
        .bind(&self.runtime)
        .bind(format!(
            "{}.room_observation_update_escalation_view",
            self.target
        ))
        .fetch_one(&self.admin)
        .await?;
        assert!(!runtime_view_privileges.0);
        assert!(runtime_view_privileges.1);
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        owner_database
            .execute(&format!(
                "DROP VIEW {}.room_observation_update_escalation_view",
                self.target
            ))
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        let schema_owner_name_before_trigger: String = sqlx::query_scalar(&format!(
            "SELECT schema_owner_name FROM {}.{} WHERE singleton = '1'",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
        ))
        .fetch_one(&self.admin)
        .await?;
        owner_database
            .execute(&format!(
                "CREATE TABLE {}.room_observation_trigger_source \
                 (id bigint NOT NULL, note text) PARTITION BY RANGE (id)",
                self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "CREATE TABLE {}.room_observation_trigger_source_leaf \
                 PARTITION OF {}.room_observation_trigger_source \
                 FOR VALUES FROM (0) TO (10)",
                self.target, self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT INSERT (id) ON {}.room_observation_trigger_source TO {}",
                self.target, self.runtime
            ))
            .await?;
        let parent_leaf_insert_privileges: (bool, bool, bool) = sqlx::query_as(
            "SELECT pg_catalog.has_table_privilege( \
               $1, pg_catalog.to_regclass($2), 'INSERT'), \
             pg_catalog.has_table_privilege( \
               $1, pg_catalog.to_regclass($3), 'INSERT'), \
             pg_catalog.has_column_privilege( \
               $1, pg_catalog.to_regclass($2), 'id', 'INSERT')",
        )
        .bind(&self.runtime)
        .bind(format!("{}.room_observation_trigger_source", self.target))
        .bind(format!(
            "{}.room_observation_trigger_source_leaf",
            self.target
        ))
        .fetch_one(&self.admin)
        .await?;
        assert!(!parent_leaf_insert_privileges.0);
        assert!(!parent_leaf_insert_privileges.1);
        assert!(parent_leaf_insert_privileges.2);
        owner_database
            .execute(&format!(
                r#"CREATE FUNCTION {}.room_observation_trigger_escalation()
                   RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $function$
                   BEGIN
                       UPDATE {}.{} SET schema_owner_name = schema_owner_name || '_trigger_escalated'
                       WHERE singleton = '1';
                       RETURN NEW;
                   END;
                   $function$"#,
                self.target, self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
            ))
            .await?;
        owner_database
            .execute(&format!(
                "CREATE TRIGGER room_observation_trigger_escalation \
                 BEFORE INSERT ON {}.room_observation_trigger_source_leaf \
                 FOR EACH ROW EXECUTE FUNCTION {}.room_observation_trigger_escalation()",
                self.target, self.target
            ))
            .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeRuntime)
        ));
        runtime_database
            .execute(&format!(
                "INSERT INTO {}.room_observation_trigger_source (id) VALUES (1)",
                self.target
            ))
            .await?;
        let escalated_schema_owner_name: String = sqlx::query_scalar(&format!(
            "SELECT schema_owner_name FROM {}.{} WHERE singleton = '1'",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
        ))
        .fetch_one(&self.admin)
        .await?;
        assert_eq!(
            escalated_schema_owner_name,
            format!("{schema_owner_name_before_trigger}_trigger_escalated")
        );
        sqlx::query(&format!(
            "UPDATE {}.{} SET schema_owner_name = $1 WHERE singleton = '1'",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
        ))
        .bind(&self.owner)
        .execute(&self.admin)
        .await?;
        owner_database
            .execute(&format!(
                "DROP TRIGGER room_observation_trigger_escalation \
                 ON {}.room_observation_trigger_source_leaf",
                self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "DROP FUNCTION {}.room_observation_trigger_escalation()",
                self.target
            ))
            .await?;
        sqlx::query(&format!(
            "INSERT INTO {}.room_observation_trigger_source (id, note) VALUES (2, 'before')",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        owner_database
            .execute(&format!(
                "GRANT SELECT (id) ON {}.room_observation_trigger_source TO {}",
                self.target, self.runtime
            ))
            .await?;
        owner_database
            .execute(&format!(
                "GRANT UPDATE (note) ON {}.room_observation_trigger_source TO {}",
                self.target, self.runtime
            ))
            .await?;
        let parent_update_privileges: (bool, bool) = sqlx::query_as(
            "SELECT pg_catalog.has_table_privilege( \
               $1, pg_catalog.to_regclass($2), 'UPDATE'), \
             pg_catalog.has_column_privilege( \
               $1, pg_catalog.to_regclass($2), 'note', 'UPDATE')",
        )
        .bind(&self.runtime)
        .bind(format!("{}.room_observation_trigger_source", self.target))
        .fetch_one(&self.admin)
        .await?;
        assert!(!parent_update_privileges.0);
        assert!(parent_update_privileges.1);
        let schema_owner_name_before_column_update: String = sqlx::query_scalar(&format!(
            "SELECT schema_owner_name FROM {}.{} WHERE singleton = '1'",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
        ))
        .fetch_one(&self.admin)
        .await?;
        owner_database
            .execute(&format!(
                r#"CREATE FUNCTION {}.room_observation_column_update_escalation()
                   RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $function$
                   BEGIN
                       UPDATE {}.{} SET schema_owner_name = schema_owner_name || '_column_update_escalated'
                       WHERE singleton = '1';
                       RETURN NEW;
                   END;
                   $function$"#,
                self.target, self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
            ))
            .await?;
        owner_database
            .execute(&format!(
                "CREATE TRIGGER room_observation_column_update_escalation \
                 BEFORE UPDATE OF note ON {}.room_observation_trigger_source_leaf \
                 FOR EACH ROW EXECUTE FUNCTION {}.room_observation_column_update_escalation()",
                self.target, self.target
            ))
            .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::UnsafeRuntime)
        ));
        runtime_database
            .execute(&format!(
                "UPDATE {}.room_observation_trigger_source SET note = 'after' WHERE id = 2",
                self.target
            ))
            .await?;
        let escalated_column_update_owner: String = sqlx::query_scalar(&format!(
            "SELECT schema_owner_name FROM {}.{} WHERE singleton = '1'",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
        ))
        .fetch_one(&self.admin)
        .await?;
        assert_eq!(
            escalated_column_update_owner,
            format!("{schema_owner_name_before_column_update}_column_update_escalated")
        );
        sqlx::query(&format!(
            "UPDATE {}.{} SET schema_owner_name = $1 WHERE singleton = '1'",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
        ))
        .bind(&self.owner)
        .execute(&self.admin)
        .await?;
        owner_database
            .execute(&format!(
                "DROP TRIGGER room_observation_column_update_escalation \
                 ON {}.room_observation_trigger_source_leaf",
                self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "DROP FUNCTION {}.room_observation_column_update_escalation()",
                self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "DROP TABLE {}.room_observation_trigger_source_leaf",
                self.target
            ))
            .await?;
        owner_database
            .execute(&format!(
                "DROP TABLE {}.room_observation_trigger_source",
                self.target
            ))
            .await?;
        verify_attested_room_observation_schema(runtime_database, &lineage, &designated_migrator)
            .await?;

        sqlx::query(&format!(
            "ALTER TABLE {}.extension_room_sources OWNER TO {}",
            self.target, self.outsider
        ))
        .execute(&self.admin)
        .await?;
        assert!(matches!(
            verify_attested_room_observation_schema(
                runtime_database,
                &lineage,
                &designated_migrator
            )
            .await,
            Err(RoomObservationSchemaError::InvalidSchemaShape)
        ));
        Ok(())
    }

    async fn cancel_owner_waits_without_marker(
        &self,
        owner_database: &Database,
        lineage: &AttestedLineage,
        designated_migrator: &PgRoleName,
    ) -> Result<()> {
        let admin_options = PgConnectOptions::from_str(&url_with_options(&self.base_url, ""))?;
        let mut gate = PgConnection::connect_with(&admin_options).await?;
        sqlx::query("SELECT pg_catalog.pg_advisory_lock_shared($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .execute(&mut gate)
            .await?;

        let owner_database = owner_database.clone();
        let lineage = lineage.clone();
        let designated_migrator = designated_migrator.clone();
        let runtime = PgRoleName::new(self.runtime.clone())?;
        let attestation_definer = PgRoleName::new(self.definer.clone())?;
        let started_at = activity_checkpoint(&self.admin).await?;
        let owner = tokio::spawn(async move {
            prepare_dark_room_observation_schema(
                &owner_database,
                &lineage,
                &designated_migrator,
                &attestation_definer,
                &runtime,
            )
            .await
        });
        let backend_pid = wait_for_catalog_try_and_rollback(&self.admin, &started_at).await?;
        let owner_pending = !owner.is_finished();
        let owner_marker_exists = marker_exists(&self.admin, &self.target).await?;
        let catalog_x_available: bool =
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                .fetch_one(&self.admin)
                .await?;
        let owner_gate_available: bool =
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                .bind(ROOM_OBSERVATION_OWNER_GATE_LOCK)
                .fetch_one(&self.admin)
                .await?;
        let lineage_gate_available: bool =
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                .bind(lineage::LINEAGE_ADVISORY_LOCK_KEY)
                .fetch_one(&self.admin)
                .await?;
        let ledger_gate_available: bool =
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                .bind(crate::db::MIGRATION_LEDGER_ADVISORY_LOCK_KEY)
                .fetch_one(&self.admin)
                .await?;
        let schema_gate_available: bool =
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)")
                .bind(ROOM_OBSERVATION_SCHEMA_LOCK)
                .fetch_one(&self.admin)
                .await?;
        if !owner_pending
            || owner_marker_exists
            || catalog_x_available
            || !owner_gate_available
            || !lineage_gate_available
            || !ledger_gate_available
            || !schema_gate_available
        {
            owner.abort();
            let _ = owner.await;
            sqlx::query("SELECT pg_catalog.pg_advisory_unlock_shared($1)")
                .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                .execute(&mut gate)
                .await?;
            gate.close().await?;
            bail!("room-observation catalog fence probe failed for backend {backend_pid}");
        }

        owner.abort();
        match owner.await {
            Err(error) => assert!(error.is_cancelled()),
            Ok(_) => bail!("cancelled owner preparation completed unexpectedly"),
        }
        sqlx::query("SELECT pg_catalog.pg_advisory_unlock_shared($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .execute(&mut gate)
            .await?;
        gate.close().await?;
        Ok(())
    }

    async fn initialize_room_observations_with_catalog_blocker(
        &self,
        owner_database: &Database,
    ) -> Result<()> {
        let admin_options = PgConnectOptions::from_str(&url_with_options(&self.base_url, ""))?;
        let mut catalog_blocker = PgConnection::connect_with(&admin_options).await?;
        sqlx::query("SELECT pg_catalog.pg_advisory_lock_shared($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .execute(&mut catalog_blocker)
            .await?;

        let initialization_database = owner_database.clone();
        let started_at = activity_checkpoint(&self.admin).await?;
        let initialization = tokio::spawn(async move {
            crate::ingress_uow::initialize_room_observations(&initialization_database).await
        });
        let backend_pid = wait_for_catalog_try_and_rollback(&self.admin, &started_at).await?;
        let initialization_pending = !initialization.is_finished();
        let catalog_x_available: bool =
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                .fetch_one(&self.admin)
                .await?;
        let schema_exists: bool = sqlx::query_scalar(
            "SELECT pg_catalog.to_regclass(format('%I.extension_room_observers', $1)) IS NOT NULL",
        )
        .bind(&self.target)
        .fetch_one(&self.admin)
        .await?;
        let owner_gate_available: bool =
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                .bind(ROOM_OBSERVATION_OWNER_GATE_LOCK)
                .fetch_one(&self.admin)
                .await?;
        let schema_gate_available: bool =
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)")
                .bind(ROOM_OBSERVATION_SCHEMA_LOCK)
                .fetch_one(&self.admin)
                .await?;

        if !initialization_pending
            || catalog_x_available
            || schema_exists
            || !owner_gate_available
            || !schema_gate_available
        {
            initialization.abort();
            let _ = initialization.await;
            sqlx::query("SELECT pg_catalog.pg_advisory_unlock_shared($1)")
                .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                .execute(&mut catalog_blocker)
                .await?;
            catalog_blocker.close().await?;
            bail!("raw room-observation catalog fence probe failed for backend {backend_pid}");
        }
        sqlx::query("SELECT pg_catalog.pg_advisory_unlock_shared($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .execute(&mut catalog_blocker)
            .await?;
        catalog_blocker.close().await?;
        initialization.await??;
        Ok(())
    }

    async fn verify_reader_catalog_and_ledger_contention(
        &self,
        runtime_database: &Database,
        lineage: &AttestedLineage,
        designated_migrator: &PgRoleName,
    ) -> Result<()> {
        for (gate_key, catalog_blocked) in [
            (CATALOG_MUTATION_ADVISORY_LOCK_KEY, true),
            (crate::db::MIGRATION_LEDGER_ADVISORY_LOCK_KEY, false),
        ] {
            let admin_options = PgConnectOptions::from_str(&url_with_options(&self.base_url, ""))?;
            let mut gate = PgConnection::connect_with(&admin_options).await?;
            sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
                .bind(gate_key)
                .execute(&mut gate)
                .await?;

            let runtime_database = runtime_database.clone();
            let lineage = lineage.clone();
            let designated_migrator = designated_migrator.clone();
            let started_at = activity_checkpoint(&self.admin).await?;
            let verifier = tokio::spawn(async move {
                verify_attested_room_observation_schema(
                    &runtime_database,
                    &lineage,
                    &designated_migrator,
                )
                .await
            });
            let backend_pid = wait_for_catalog_try_and_rollback(&self.admin, &started_at).await?;
            let verifier_pending = !verifier.is_finished();
            let marker_complete = marker_exists(&self.admin, &self.target).await?;
            let catalog_x_available = if catalog_blocked {
                sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                    .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                    .fetch_one(&self.admin)
                    .await?
            } else {
                // Hold a session catalog-X once the failed ledger try has
                // rolled back. This stabilizes the lower-gate probes against
                // the verifier's next short-lived retry.
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let acquired: bool =
                            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_lock($1)")
                                .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                                .fetch_one(&mut gate)
                                .await?;
                        if acquired {
                            return Ok::<(), sqlx::Error>(());
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await??;
                true
            };
            let lineage_x_available: bool =
                sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                    .bind(lineage::LINEAGE_ADVISORY_LOCK_KEY)
                    .fetch_one(&self.admin)
                    .await?;
            let owner_x_available: bool =
                sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                    .bind(ROOM_OBSERVATION_OWNER_GATE_LOCK)
                    .fetch_one(&self.admin)
                    .await?;
            let schema_x_available: bool =
                sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                    .bind(ROOM_OBSERVATION_SCHEMA_LOCK)
                    .fetch_one(&self.admin)
                    .await?;
            if !verifier_pending
                || !marker_complete
                || catalog_x_available == catalog_blocked
                || !lineage_x_available
                || !owner_x_available
                || !schema_x_available
            {
                verifier.abort();
                let _ = verifier.await;
                if !catalog_blocked {
                    sqlx::query("SELECT pg_catalog.pg_advisory_unlock($1)")
                        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                        .execute(&mut gate)
                        .await?;
                }
                sqlx::query("SELECT pg_catalog.pg_advisory_unlock($1)")
                    .bind(gate_key)
                    .execute(&mut gate)
                    .await?;
                gate.close().await?;
                bail!("room-observation reader fence probe failed for backend {backend_pid}");
            }
            if !catalog_blocked {
                sqlx::query("SELECT pg_catalog.pg_advisory_unlock($1)")
                    .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                    .execute(&mut gate)
                    .await?;
            }
            sqlx::query("SELECT pg_catalog.pg_advisory_unlock($1)")
                .bind(gate_key)
                .execute(&mut gate)
                .await?;
            gate.close().await?;
            verifier.await??;
        }
        Ok(())
    }

    async fn verify_waiting_attach_observes_completed_marker(
        &self,
        runtime_database: &Database,
        lineage: &AttestedLineage,
        designated_migrator: &PgRoleName,
    ) -> Result<()> {
        sqlx::query(&format!(
            "UPDATE {}.{} SET prepared_revision = '0' WHERE singleton = '1'",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
        ))
        .execute(&self.admin)
        .await?;

        let admin_options = PgConnectOptions::from_str(&url_with_options(&self.base_url, ""))?;
        let mut gate = PgConnection::connect_with(&admin_options).await?;
        sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
            .bind(ROOM_OBSERVATION_OWNER_GATE_LOCK)
            .execute(&mut gate)
            .await?;

        let runtime_database = runtime_database.clone();
        let lineage = lineage.clone();
        let designated_migrator = designated_migrator.clone();
        let started_at = activity_checkpoint(&self.admin).await?;
        let verifier = tokio::spawn(async move {
            verify_attested_room_observation_schema(
                &runtime_database,
                &lineage,
                &designated_migrator,
            )
            .await
        });
        let retry_seen = wait_for_catalog_try_and_rollback(&self.admin, &started_at)
            .await
            .is_ok()
            && !verifier.is_finished();

        sqlx::query(&format!(
            "UPDATE {}.{} SET prepared_revision = '1' WHERE singleton = '1'",
            self.target, ROOM_OBSERVATION_OWNER_STATE_TABLE
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query("SELECT pg_catalog.pg_advisory_unlock($1)")
            .bind(ROOM_OBSERVATION_OWNER_GATE_LOCK)
            .execute(&mut gate)
            .await?;
        gate.close().await?;

        if !retry_seen {
            bail!("attested verifier did not retry the owner session gate");
        }
        let evidence = verifier.await??;
        assert_eq!(evidence.revision(), ROOM_OBSERVATION_SCHEMA_REVISION);
        Ok(())
    }

    async fn cleanup(mut self) {
        self.runtime_database.take();
        self.owner_database.take();
        self.admin.close().await;
        for statement in [
            format!(
                "REVOKE {owner} FROM {migrator}",
                owner = self.owner,
                migrator = self.migrator
            ),
            format!("REVOKE {} FROM {}", self.owner, self.outsider),
            format!("REVOKE {} FROM {}", self.definer, self.installer),
            format!("DROP DATABASE {} WITH (FORCE)", self.database),
        ] {
            sqlx::query(&statement)
                .execute(&self.cluster_admin)
                .await
                .expect("remove disposable database roles and schema");
        }
        for role in [
            &self.runtime,
            &self.installer,
            &self.outsider,
            &self.migrator,
            &self.owner,
            &self.definer,
        ] {
            sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
                .execute(&self.cluster_admin)
                .await
                .expect("drop disposable role");
        }
        self.cluster_admin.close().await;
    }
}

#[tokio::test]
async fn postgres17_room_observation_owner_and_attested_verifier_contract() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping room-observation PostgreSQL fixture: test URL not set");
        return;
    };
    let mut fixture = Fixture::new(url).await;
    let result = fixture.exercise().await;
    fixture.cleanup().await;
    result.expect("room-observation PostgreSQL owner and verifier contract");
}

async fn activity_checkpoint(admin: &PgPool) -> Result<String> {
    Ok(
        sqlx::query_scalar("SELECT pg_catalog.clock_timestamp()::text")
            .fetch_one(admin)
            .await?,
    )
}

async fn wait_for_catalog_try_and_rollback(admin: &PgPool, started_at: &str) -> Result<i32> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let backend_pid = sqlx::query_scalar::<_, i32>(
                "SELECT pid FROM pg_catalog.pg_stat_activity \
                 WHERE datname = pg_catalog.current_database() \
                   AND pid <> pg_catalog.pg_backend_pid() AND state = 'idle' \
                   AND xact_start IS NULL AND query = 'ROLLBACK' \
                   AND query_start > $1::timestamptz \
                 ORDER BY query_start DESC LIMIT 1",
            )
            .bind(started_at)
            .fetch_optional(admin)
            .await?;
            if let Some(backend_pid) = backend_pid {
                return Ok::<i32, sqlx::Error>(backend_pid);
            }
            tokio::task::yield_now().await;
        }
    })
    .await?
    .map_err(Into::into)
}

async fn marker_exists(pool: &PgPool, schema: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class relation \
         JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
         WHERE namespace.nspname = $1 AND relation.relname = $2)",
    )
    .bind(schema)
    .bind(ROOM_OBSERVATION_OWNER_STATE_TABLE)
    .fetch_one(pool)
    .await?)
}

fn url_with_database(base: &str, database: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL test URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "dbname" && key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("dbname", database);
    url.to_string()
}

fn url_with_username(base: &str, username: &str) -> String {
    // A socket-directory URL has no authority to carry a username, and a
    // `user` parameter outranks the authority in libpq, so the login always
    // travels as that parameter.
    let mut url = url::Url::parse(base).expect("PostgreSQL test URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "user")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("user", username);
    url.to_string()
}

fn url_with_options(base: &str, options: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL test URL");
    let retained: Vec<_> = url
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
