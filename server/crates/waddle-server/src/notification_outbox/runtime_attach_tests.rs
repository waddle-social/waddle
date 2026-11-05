use super::{
    schema::{OUTBOX_OWNER_PREPARED_REVISION, OUTBOX_OWNER_STATE_TABLE, OUTBOX_SCHEMA_GATE},
    NotificationOutboxRuntimeAttachError, NotificationOutboxStore, OwnerPreparationProof,
};
use crate::db::{Database, DatabaseConfig, DatabaseDriver};
use anyhow::{anyhow, bail, Result};
use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::Executor;
use std::str::FromStr;
use std::time::Duration;

struct Fixture {
    cluster_admin: PgPool,
    admin: PgPool,
    admin_login: String,
    base_url: String,
    database: String,
    owner: String,
    definer: String,
    installer: String,
    runtime: String,
    target: String,
    trusted: String,
    had_public_control_execute: bool,
}

impl Fixture {
    async fn new(url: String) -> Self {
        let cluster_admin = PgPool::connect(&url)
            .await
            .expect("connect disposable PostgreSQL cluster");
        let admin_login: String = sqlx::query_scalar("SELECT session_user::text")
            .fetch_one(&cluster_admin)
            .await
            .expect("read test cluster login");
        let had_public_control_execute: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p \
             CROSS JOIN LATERAL pg_catalog.aclexplode( \
               COALESCE(p.proacl, pg_catalog.acldefault('f', p.proowner)) \
             ) acl WHERE p.oid = 'pg_catalog.pg_control_system()'::regprocedure \
               AND acl.grantee = 0 AND acl.privilege_type = 'EXECUTE')",
        )
        .fetch_one(&cluster_admin)
        .await
        .expect("capture original control-function public ACL");

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database = format!("notification_outbox_attach_{suffix}");
        let owner = format!("notification_outbox_o_{suffix}");
        let definer = format!("notification_outbox_d_{suffix}");
        let installer = format!("notification_outbox_i_{suffix}");
        let runtime = format!("notification_outbox_r_{suffix}");
        let target = format!("notification_outbox_t_{suffix}");
        let trusted = format!("notification_outbox_f_{suffix}");

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
            .expect("create no-login owner or definer role");
        }
        for role in [&installer, &runtime] {
            sqlx::query(&format!("CREATE ROLE {role} LOGIN"))
                .execute(&cluster_admin)
                .await
                .expect("create test login role");
        }
        sqlx::query(&format!(
            "GRANT {owner} TO \"{}\" WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
            admin_login.replace('"', "\"\"")
        ))
        .execute(&cluster_admin)
        .await
        .expect("grant test administrator the owner role");
        sqlx::query(&format!(
            "GRANT {definer} TO {installer} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"
        ))
        .execute(&cluster_admin)
        .await
        .expect("grant installer the definer role");

        let base_url = outbox_attach_url_with_database(&url, &database);
        let admin = PgPool::connect(&base_url)
            .await
            .expect("connect isolated database administrator");
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
        for role in [&runtime, &definer] {
            sqlx::query(&format!("GRANT USAGE ON SCHEMA {target} TO {role}"))
                .execute(&admin)
                .await
                .expect("grant schema usage");
        }
        sqlx::query("REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC")
            .execute(&admin)
            .await
            .expect("restrict control-function access");
        for role in [&owner, &definer] {
            sqlx::query(&format!(
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {role}"
            ))
            .execute(&admin)
            .await
            .expect("grant control-function access to owner or definer");
        }

        Self {
            cluster_admin,
            admin,
            admin_login,
            base_url,
            database,
            owner,
            definer,
            installer,
            runtime,
            target,
            trusted,
            had_public_control_execute,
        }
    }

    async fn exercise(&self) -> Result<()> {
        use crate::config::LineageConfig;
        use crate::db::lineage::{self, DeploymentUuid};
        use crate::db::physical_attestation::{
            attested_pool, install, PgRoleName, PhysicalAttestationConfig,
        };

        let owner_url =
            outbox_attach_url_with_role_and_schema(&self.base_url, &self.owner, &self.target);
        let mut owner_config = DatabaseConfig::new(DatabaseDriver::Postgres, owner_url);
        owner_config.pool_size = 1;
        let owner_database =
            Database::from_config("notification outbox owner", &owner_config).await?;
        let lineage = lineage::enroll(
            &owner_database,
            &LineageConfig {
                deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
                action: None,
            },
        )
        .await?;
        let identity = lineage
            .postgres_identity
            .clone()
            .ok_or_else(|| anyhow!("PostgreSQL owner identity missing"))?;
        let _initial_owner_store =
            NotificationOutboxStore::prepare_owner(&owner_database, &identity).await?;
        sqlx::query(&format!(
            "ALTER TABLE {}.notification_candidates \
             DROP CONSTRAINT notification_candidates_reason_check",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "ALTER TABLE {}.notification_candidates \
             ADD CONSTRAINT stale_notification_candidates_reason \
             CHECK (reason IN ('offline_dm'))",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        let _constraint_upgrade_store =
            NotificationOutboxStore::prepare_owner(&owner_database, &identity).await?;

        let attestation = PhysicalAttestationConfig::new(
            lineage,
            PgRoleName::new(self.installer.clone())?,
            PgRoleName::new(self.runtime.clone())?,
            PgRoleName::new(self.definer.clone())?,
            crate::db::lineage::PgSchemaName(self.trusted.clone()),
        )?;
        for statement in [
            format!(
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {}",
                self.definer
            ),
            format!(
                "GRANT SELECT ON {}._lineage TO {}",
                self.target, self.definer
            ),
        ] {
            sqlx::query(&statement).execute(&self.admin).await?;
        }
        let boundary_url = outbox_attach_url_with_schema(&self.base_url, &self.target);
        let installer_options =
            PgConnectOptions::from_str(&boundary_url)?.username(&self.installer);
        install(
            &installer_options,
            attestation.clone(),
            uuid::Uuid::new_v4(),
        )
        .await?;
        sqlx::query(&format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.notification_candidates, \
             {}.notification_outbox, {}.notification_outbox_lineage TO {}",
            self.target, self.target, self.target, self.runtime
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "GRANT SELECT ON {}.{} TO {}",
            self.target, OUTBOX_OWNER_STATE_TABLE, self.runtime
        ))
        .execute(&self.admin)
        .await?;

        let runtime_options = PgConnectOptions::from_str(&boundary_url)?.username(&self.runtime);
        let attested = attested_pool(runtime_options, attestation, 2, Duration::from_secs(3))?;
        let runtime_database_pool = crate::test_support::database_pool_from_attested_postgres_pool(
            "notification outbox runtime",
            attested,
            1,
            1,
        )
        .await?;
        let runtime_database = runtime_database_pool.global().clone();

        match NotificationOutboxStore::prepare_owner(&runtime_database, &identity).await {
            Err(super::NotificationOutboxError::OwnerPreparationRequiresTrustedHandle) => {}
            Err(error) => bail!("attested owner-preparation error was unexpected: {error}"),
            Ok(_) => bail!("attested runtime unexpectedly prepared the outbox"),
        }
        let initially_attached =
            NotificationOutboxStore::attach_existing(runtime_database.clone()).await?;
        drop(initially_attached);
        let mut legacy_ddl_guard = self.admin.begin().await?;
        let legacy_candidate_lock = format!(
            "LOCK TABLE {}.notification_candidates IN ACCESS EXCLUSIVE MODE",
            self.target
        );
        legacy_ddl_guard
            .execute(legacy_candidate_lock.as_str())
            .await?;
        let legacy_owner_database = owner_database.clone();
        let legacy_owner_caller =
            tokio::spawn(
                async move { NotificationOutboxStore::try_new(legacy_owner_database).await },
            );
        self.wait_for_lock("granted").await?;
        self.wait_for_gate_lock_count(2).await?;
        self.wait_for_marker_revision(0).await?;
        legacy_ddl_guard.commit().await?;
        let legacy_owner_store =
            tokio::time::timeout(Duration::from_secs(20), legacy_owner_caller).await???;
        if self.marker_revision().await? != 0 {
            bail!("legacy try_new unexpectedly left a trusted preparation marker");
        }
        match NotificationOutboxStore::attach_existing(runtime_database.clone()).await {
            Err(NotificationOutboxRuntimeAttachError::OwnerPreparationIncomplete) => {}
            Err(error) => bail!("legacy owner path returned an unexpected attach error: {error}"),
            Ok(_) => bail!("legacy try_new unexpectedly certified owner preparation"),
        }
        drop(legacy_owner_store);
        let owner_store =
            NotificationOutboxStore::prepare_owner(&owner_database, &identity).await?;
        let attached = NotificationOutboxStore::attach_existing(runtime_database.clone()).await?;
        let ingress_lane = runtime_database_pool.ingress();
        match NotificationOutboxStore::attach_existing(ingress_lane).await {
            Err(NotificationOutboxRuntimeAttachError::MainLaneRequired) => {}
            Err(error) => bail!("wrong-lane attachment returned an unexpected error: {error}"),
            Ok(_) => bail!("wrong-lane attachment unexpectedly succeeded"),
        }

        let mut wrong_identity = identity.clone();
        wrong_identity.schema.oid = wrong_identity.schema.oid.wrapping_add(1);
        match NotificationOutboxStore::prepare_owner(&owner_database, &wrong_identity).await {
            Err(super::NotificationOutboxError::UnsafeOwnerBoundary) => {}
            Err(error) => bail!("wrong owner identity returned an unexpected error: {error}"),
            Ok(_) => bail!("wrong owner identity unexpectedly prepared the schema"),
        }

        let owner_conn = owner_database.guard().await?;
        owner_conn.execute(
            "INSERT INTO notification_candidates (recipient_bare_jid, conversation_jid, sender_jid, \
             stanza_id_by, stanza_id, class, reason, created_at_ms, delivery_id) \
             VALUES ('alice@example.net', 'bob@example.net', 'bob@example.net/phone', \
             'alice@example.net', 'legacy-candidate', 'dm', 'offline_dm', 11, NULL)",
            (),
        ).await?;
        owner_conn.execute(
            "INSERT INTO notification_outbox (job_id, recipient_bare_jid, push_service_jid, node, \
             conversation_jid, sender_jid, sender_jids, class, message_count, context_xml, \
             summary_sender_jid, summary_body, approved_payload_xml, approved_publish_options_xml, \
             queue_acceptance_may_exist, status, created_at_ms, updated_at_ms) \
             VALUES ('retained-job', 'alice@example.net', 'push.example.net', 'node', \
             'bob@example.net', 'bob@example.net/phone', '[\"bob@example.net/phone\"]', 'dm', 1, \
             '<context/>', 'bob@example.net', 'retained-summary', '<payload/>', '<options/>', \
             1, 'queued', 12, 13)",
            (),
        ).await?;
        owner_conn.execute(
            "INSERT INTO notification_outbox_lineage (candidate_delivery_id, job_id, settled_at_ms) \
             VALUES ('retained-candidate-id', 'retained-job', 14)",
            (),
        ).await?;
        let before = custody_snapshot(&owner_database).await?;
        drop(owner_conn);

        let legacy_url = outbox_attach_url_with_schema(&self.base_url, &self.target);
        let mut legacy_config = DatabaseConfig::new(DatabaseDriver::Postgres, legacy_url);
        legacy_config.pool_size = 1;
        let legacy_database =
            Database::from_config("notification outbox legacy owner", &legacy_config).await?;
        let temp_shadow = legacy_database.guard().await?;
        let mut schema_rows = temp_shadow
            .query(
                "SELECT n.oid::bigint, n.nspname::text, \
                        pg_catalog.array_to_json(pg_catalog.current_schemas(false))::text, \
                        pg_catalog.pg_my_temp_schema()::bigint, \
                        pg_catalog.pg_backend_pid()::integer \
                 FROM pg_catalog.pg_namespace n \
                 WHERE n.nspname = pg_catalog.current_schema()",
                (),
            )
            .await?;
        let schema_row = schema_rows
            .next()
            .await?
            .ok_or_else(|| anyhow!("temp-shadow connection has no active schema"))?;
        let gate_schema_identity = super::OwnerSchemaIdentity {
            oid: schema_row.get(0)?,
            name: schema_row.get(1)?,
            search_path_json: schema_row.get(2)?,
            temp_schema_oid: schema_row.get(3)?,
        };
        let temp_backend_pid: i32 = schema_row.get(4)?;
        drop(schema_rows);
        if gate_schema_identity.temp_schema_oid != 0 {
            bail!("fresh temp-shadow connection unexpectedly has a temp schema");
        }
        temp_shadow
            .execute(
                "CREATE TEMP TABLE notification_outbox_owner_state (\
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
                    prepared_revision BIGINT NOT NULL CHECK (prepared_revision >= 0)\
                 )",
                (),
            )
            .await?;
        drop(temp_shadow);

        let probe_key = super::fresh_owner_probe_key();
        let mut probe_gate = self.admin.begin().await?;
        sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1::integer, $2::integer)")
            .bind(probe_key.0)
            .bind(probe_key.1)
            .execute(&mut *probe_gate)
            .await?;
        let mut pooled_tx = legacy_database.begin().await?;
        let mut backend_rows = pooled_tx
            .query("SELECT pg_catalog.pg_backend_pid()::integer", ())
            .await?;
        let pooled_backend_pid = backend_rows
            .next()
            .await?
            .ok_or_else(|| anyhow!("temp-shadow transaction has no backend PID"))?
            .get::<i32>(0)?;
        drop(backend_rows);
        if pooled_backend_pid != temp_backend_pid {
            bail!("temp-shadow setup and owner transaction used different backends");
        }
        let pooled_store = NotificationOutboxStore {
            db: legacy_database.clone(),
            owner_preparation: Some(OwnerPreparationProof {
                identity: None,
                probe_key,
                schema_identity: std::sync::Arc::new(tokio::sync::Mutex::new(Some(
                    gate_schema_identity,
                ))),
            }),
        };
        let pooled_schema_check = pooled_store.verify_owner_transaction(&mut pooled_tx).await;
        pooled_tx.rollback().await?;
        probe_gate.rollback().await?;
        match pooled_schema_check {
            Err(error) if error.to_string().contains("changed its gate schema") => {}
            Err(error) => {
                bail!("temp-shadowed pooled transaction returned the wrong error: {error}")
            }
            Ok(()) => bail!("owner transaction accepted a temp marker shadow"),
        }

        match NotificationOutboxStore::try_new(legacy_database.clone()).await {
            Err(super::NotificationOutboxError::UnsafeOwnerBoundary) => {}
            Err(error) => bail!("temp-shadowed legacy startup returned the wrong error: {error}"),
            Ok(_) => bail!("legacy startup accepted a temp marker that shadowed durable state"),
        }
        if self.marker_revision().await? != OUTBOX_OWNER_PREPARED_REVISION {
            bail!("temp-shadowed legacy startup invalidated the durable marker");
        }
        if custody_snapshot(&owner_database).await? != before {
            bail!("temp-shadowed legacy startup changed persisted rows or ancestry");
        }
        let _after_temp_shadow =
            NotificationOutboxStore::attach_existing(runtime_database.clone()).await?;
        drop(legacy_database);

        let mut held = self.admin.begin().await?;
        let candidate_lock_sql = format!(
            "LOCK TABLE {}.notification_candidates IN ACCESS EXCLUSIVE MODE",
            self.target
        );
        held.execute(candidate_lock_sql.as_str()).await?;
        let owner_database_for_task = owner_database.clone();
        let owner_identity = identity.clone();
        let owner_caller = tokio::spawn(async move {
            NotificationOutboxStore::prepare_owner(&owner_database_for_task, &owner_identity).await
        });
        self.wait_for_lock("granted").await?;
        self.wait_for_marker_revision(0).await?;
        let runtime_for_task = runtime_database.clone();
        let attach_waiter = tokio::spawn(async move {
            NotificationOutboxStore::attach_existing(runtime_for_task).await
        });
        self.wait_for_lock("waiting").await?;
        owner_caller.abort();
        let owner_caller_result = owner_caller.await;
        if !matches!(owner_caller_result, Err(error) if error.is_cancelled()) {
            bail!("owner caller cancellation did not propagate");
        }
        held.commit().await?;
        let attached_after_owner =
            tokio::time::timeout(Duration::from_secs(20), attach_waiter).await??;
        attached_after_owner?;
        if self.marker_revision().await? != OUTBOX_OWNER_PREPARED_REVISION {
            bail!("durable owner marker was not completed after detached work");
        }
        let mut owner_marker_check = owner_database.begin().await?;
        NotificationOutboxStore::verify_owner_preparation_complete_tx(
            &mut owner_marker_check,
            &identity,
        )
        .await?;
        let mut competing_outbox_gate = self.admin.begin().await?;
        let competing_lock: bool = sqlx::query_scalar(
            "SELECT pg_catalog.pg_try_advisory_xact_lock($1::integer, $2::integer)",
        )
        .bind(OUTBOX_SCHEMA_GATE.0)
        .bind(OUTBOX_SCHEMA_GATE.1)
        .fetch_one(&mut *competing_outbox_gate)
        .await?;
        competing_outbox_gate.rollback().await?;
        if competing_lock {
            bail!("owner marker validation released its gate before transaction end");
        }
        owner_marker_check.rollback().await?;
        let mut released_outbox_gate = self.admin.begin().await?;
        let released_lock: bool = sqlx::query_scalar(
            "SELECT pg_catalog.pg_try_advisory_xact_lock($1::integer, $2::integer)",
        )
        .bind(OUTBOX_SCHEMA_GATE.0)
        .bind(OUTBOX_SCHEMA_GATE.1)
        .fetch_one(&mut *released_outbox_gate)
        .await?;
        released_outbox_gate.rollback().await?;
        if !released_lock {
            bail!("owner marker transaction did not release its outbox gate");
        }
        let after = custody_snapshot(&owner_database).await?;
        if after.0.is_none() {
            bail!("owner preparation did not assign the missing scheduler identity");
        }
        if after.1 != before.1
            || after.2 != before.2
            || after.3 != before.3
            || after.4 != before.4
            || after.5 != before.5
            || after.6 != before.6
        {
            bail!("owner preparation changed retained rows or exact lineage");
        }

        let marker_store =
            NotificationOutboxStore::attach_existing(runtime_database.clone()).await?;
        let marker_owner = owner_database.guard().await?;
        marker_owner
            .execute(
                &format!(
                    "UPDATE {}.{} SET prepared_revision = 0 WHERE singleton = 1",
                    self.target, OUTBOX_OWNER_STATE_TABLE
                ),
                (),
            )
            .await?;
        match NotificationOutboxStore::attach_existing(runtime_database.clone()).await {
            Err(NotificationOutboxRuntimeAttachError::OwnerPreparationIncomplete) => {}
            Err(error) => bail!("incomplete marker returned an unexpected error: {error}"),
            Ok(_) => bail!("incomplete owner marker unexpectedly attached"),
        }
        drop(marker_owner);
        let mut incomplete_marker_check = owner_database.begin().await?;
        let incomplete_marker_result =
            NotificationOutboxStore::verify_owner_preparation_complete_tx(
                &mut incomplete_marker_check,
                &identity,
            )
            .await;
        incomplete_marker_check.rollback().await?;
        match incomplete_marker_result {
            Err(NotificationOutboxRuntimeAttachError::OwnerPreparationIncomplete) => {}
            Err(error) => bail!("owner marker check returned an unexpected error: {error}"),
            Ok(()) => bail!("owner marker check accepted an incomplete preparation"),
        }
        let _reprepared =
            NotificationOutboxStore::prepare_owner(&owner_database, &identity).await?;

        let mut marker_guard = self.admin.begin().await?;
        let marker_guard_sql = format!(
            "LOCK TABLE {}.{} IN ACCESS EXCLUSIVE MODE",
            self.target, OUTBOX_OWNER_STATE_TABLE
        );
        marker_guard.execute(marker_guard_sql.as_str()).await?;
        let attach_for_race = runtime_database.clone();
        let racing_attach =
            tokio::spawn(
                async move { NotificationOutboxStore::attach_existing(attach_for_race).await },
            );
        self.wait_for_relation_lock("notification_outbox", "ShareUpdateExclusiveLock", true)
            .await?;
        self.wait_for_relation_lock(OUTBOX_OWNER_STATE_TABLE, "AccessShareLock", false)
            .await?;

        let racing_index_name =
            format!("notification_outbox_race_{}", uuid::Uuid::new_v4().simple());
        let racing_index_sql = format!(
            "CREATE INDEX CONCURRENTLY {racing_index_name} ON {}.notification_outbox (message_count)",
            self.target
        );
        let admin_for_index = self.admin.clone();
        let racing_index = tokio::spawn(async move {
            sqlx::query(racing_index_sql.as_str())
                .execute(&admin_for_index)
                .await
        });
        self.wait_for_relation_lock("notification_outbox", "ShareUpdateExclusiveLock", false)
            .await?;
        marker_guard.commit().await?;
        tokio::time::timeout(Duration::from_secs(20), racing_attach).await???;
        tokio::time::timeout(Duration::from_secs(20), racing_index).await???;
        match NotificationOutboxStore::attach_existing(runtime_database.clone()).await {
            Err(NotificationOutboxRuntimeAttachError::InvalidIndexes {
                table: "notification_outbox",
            }) => {}
            Err(error) => bail!("concurrent index was not detected after attach: {error}"),
            Ok(_) => bail!("attachment accepted an index added after its shape check"),
        }
        sqlx::query(&format!(
            "DROP INDEX CONCURRENTLY {}.{racing_index_name}",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        let _after_race_cleanup =
            NotificationOutboxStore::attach_existing(runtime_database.clone()).await?;

        sqlx::query(&format!(
            "DROP INDEX {}.idx_notification_candidates_identity",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "CREATE UNIQUE INDEX idx_notification_candidates_identity \
             ON {}.notification_candidates \
             (recipient_bare_jid, conversation_jid, thread_id, stanza_id COLLATE \"C\", class)",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        match NotificationOutboxStore::attach_existing(runtime_database.clone()).await {
            Err(NotificationOutboxRuntimeAttachError::InvalidIndexes {
                table: "notification_candidates",
            }) => {}
            Err(error) => bail!("custom index collation returned an unexpected error: {error}"),
            Ok(_) => bail!("attachment accepted a nondefault identity-index collation"),
        }
        sqlx::query(&format!(
            "DROP INDEX {}.idx_notification_candidates_identity",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "CREATE UNIQUE INDEX idx_notification_candidates_identity \
             ON {}.notification_candidates \
             (recipient_bare_jid, conversation_jid, thread_id, stanza_id, class)",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        let _after_collation_cleanup =
            NotificationOutboxStore::attach_existing(runtime_database.clone()).await?;

        sqlx::query(&format!(
            "ALTER TABLE {}.notification_outbox \
             ALTER COLUMN context_xml TYPE text COLLATE \"C\" USING context_xml::text",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        match NotificationOutboxStore::attach_existing(runtime_database.clone()).await {
            Err(NotificationOutboxRuntimeAttachError::InvalidColumns {
                table: "notification_outbox",
            }) => {}
            Err(error) => bail!("custom column collation returned an unexpected error: {error}"),
            Ok(_) => bail!("attachment accepted a nondefault column collation"),
        }
        sqlx::query(&format!(
            "ALTER TABLE {}.notification_outbox \
             ALTER COLUMN context_xml TYPE text USING context_xml::text",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        let _after_column_collation_cleanup =
            NotificationOutboxStore::attach_existing(runtime_database.clone()).await?;

        sqlx::query(&format!(
            "CREATE TABLE {}.notification_outbox_dependents (job_id text \
             REFERENCES {}.notification_outbox(job_id))",
            self.target, self.target
        ))
        .execute(&self.admin)
        .await?;
        match NotificationOutboxStore::attach_existing(runtime_database.clone()).await {
            Err(NotificationOutboxRuntimeAttachError::InvalidRelation {
                table: "notification_outbox",
            }) => {}
            Err(error) => bail!("inbound foreign key returned an unexpected error: {error}"),
            Ok(_) => bail!("attachment accepted an inbound foreign key"),
        }
        drop(marker_store);
        drop(attached);
        drop(owner_store);
        drop(runtime_database);
        drop(runtime_database_pool);
        drop(owner_database);
        Ok(())
    }

    async fn wait_for_lock(&self, mode: &str) -> Result<()> {
        let query = match mode {
            "granted" => format!(
                "SELECT COUNT(*)::bigint FROM pg_catalog.pg_locks WHERE locktype = 'advisory' \
                 AND classid = {}::oid AND objid = {}::oid AND granted",
                OUTBOX_SCHEMA_GATE.0, OUTBOX_SCHEMA_GATE.1
            ),
            "waiting" => format!(
                "SELECT COUNT(*)::bigint FROM pg_catalog.pg_locks WHERE locktype = 'advisory' \
                 AND classid = {}::oid AND objid = {}::oid AND NOT granted",
                OUTBOX_SCHEMA_GATE.0, OUTBOX_SCHEMA_GATE.1
            ),
            _ => bail!("unknown advisory lock mode"),
        };
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let count: i64 = sqlx::query_scalar(&query).fetch_one(&self.admin).await?;
                if count > 0 {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        Ok(())
    }

    async fn wait_for_gate_lock_count(&self, expected: i64) -> Result<()> {
        let query = format!(
            "SELECT COUNT(DISTINCT (held.classid, held.objid))::bigint \
             FROM pg_catalog.pg_locks fixed \
             JOIN pg_catalog.pg_locks held ON held.pid = fixed.pid \
             WHERE fixed.locktype = 'advisory' AND fixed.classid = {}::oid \
               AND fixed.objid = {}::oid AND fixed.granted \
               AND held.locktype = 'advisory' AND held.mode = 'ExclusiveLock' \
               AND held.granted",
            OUTBOX_SCHEMA_GATE.0, OUTBOX_SCHEMA_GATE.1
        );
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let count: i64 = sqlx::query_scalar(&query).fetch_one(&self.admin).await?;
                if count >= expected {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        Ok(())
    }

    async fn wait_for_relation_lock(
        &self,
        relation: &str,
        mode: &str,
        granted: bool,
    ) -> Result<()> {
        let qualified = format!("{}.{}", self.target, relation);
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let count: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*)::bigint FROM pg_catalog.pg_locks \
                     WHERE locktype = 'relation' AND relation = pg_catalog.to_regclass($1)::oid \
                       AND mode = $2 AND granted = $3",
                )
                .bind(&qualified)
                .bind(mode)
                .bind(granted)
                .fetch_one(&self.admin)
                .await?;
                if count > 0 {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        Ok(())
    }

    async fn marker_revision(&self) -> Result<i64> {
        Ok(sqlx::query_scalar(&format!(
            "SELECT prepared_revision FROM {}.{} WHERE singleton = 1",
            self.target, OUTBOX_OWNER_STATE_TABLE
        ))
        .fetch_one(&self.admin)
        .await?)
    }

    async fn wait_for_marker_revision(&self, expected: i64) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if self.marker_revision().await? == expected {
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        Ok(())
    }

    async fn cleanup(self) {
        self.admin.close().await;
        if self.had_public_control_execute {
            sqlx::query("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO PUBLIC")
                .execute(&self.cluster_admin)
                .await
                .expect("restore control-function public ACL");
        }
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&self.cluster_admin)
            .await
            .expect("drop isolated test database");
        sqlx::query(&format!(
            "REVOKE {} FROM \"{}\"",
            self.owner,
            self.admin_login.replace('"', "\"\"")
        ))
        .execute(&self.cluster_admin)
        .await
        .expect("remove disposable owner membership");
        for role in [&self.runtime, &self.installer, &self.owner, &self.definer] {
            sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
                .execute(&self.cluster_admin)
                .await
                .expect("drop disposable test role");
        }
        self.cluster_admin.close().await;
    }
}

async fn custody_snapshot(
    db: &Database,
) -> Result<(
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<i64>,
    String,
)> {
    let conn = db.guard().await?;
    let mut rows = conn
        .query(
            "SELECT c.delivery_id, o.approved_payload_xml, o.approved_publish_options_xml, \
         o.message_count, o.summary_body, l.settled_at_ms, l.candidate_delivery_id \
         FROM notification_candidates c \
         JOIN notification_outbox o ON o.job_id = 'retained-job' \
         JOIN notification_outbox_lineage l ON l.job_id = o.job_id \
         WHERE c.stanza_id = 'legacy-candidate'",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow!("retained row missing"))?;
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

#[tokio::test]
async fn postgres_owner_preparation_attaches_read_only_and_survives_cancellation() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping notification outbox attach test: test PostgreSQL URL not set");
        return;
    };
    let fixture = Fixture::new(url).await;
    let result = fixture.exercise().await;
    fixture.cleanup().await;
    result.expect("restricted notification outbox owner/runtime contract");
}

fn outbox_attach_url_with_database(base: &str, database: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL URL");
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

fn outbox_attach_url_with_schema(base: &str, schema: &str) -> String {
    outbox_attach_url_with_options(base, &format!("-c search_path={schema}"))
}

fn outbox_attach_url_with_role_and_schema(base: &str, role: &str, schema: &str) -> String {
    outbox_attach_url_with_options(base, &format!("-c role={role} -c search_path={schema}"))
}

fn outbox_attach_url_with_options(base: &str, options: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL URL");
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
