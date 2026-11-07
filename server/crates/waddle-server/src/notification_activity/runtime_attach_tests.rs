use super::schema::{ACTIVITY_OWNER_STATE_TABLE, ACTIVITY_SCHEMA_GATE};
use super::{
    DurableStoreAttachError, NotificationActivityReader, NotificationActivityStore,
    NotificationChatState,
};
use crate::db::Database;
use crate::push_registrations::DatabasePushRegistrationStore;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{bail, Result};
use sqlx::postgres::{PgConnectOptions, PgPool};
use waddle_xmpp::push::{PushSubscription, PushSubscriptionStore};

struct Fixture {
    cluster_admin: PgPool,
    admin: PgPool,
    base_url: String,
    admin_login: String,
    database: String,
    owner: String,
    definer: String,
    installer: String,
    runtime: String,
    target: String,
    trusted: String,
    had_public_control_execute: bool,
    operation_id: uuid::Uuid,
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
        let public_control_execute: bool = sqlx::query_scalar(
            "SELECT EXISTS (\
               SELECT 1 FROM pg_catalog.pg_proc p \
               CROSS JOIN LATERAL pg_catalog.aclexplode( \
                 COALESCE(p.proacl, pg_catalog.acldefault('f', p.proowner)) \
               ) acl \
               WHERE p.oid = 'pg_catalog.pg_control_system()'::regprocedure \
                 AND acl.grantee = 0 AND acl.privilege_type = 'EXECUTE' \
             )",
        )
        .fetch_one(&cluster_admin)
        .await
        .expect("capture original control-function public ACL");

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database = format!("notification_store_attach_{suffix}");
        let owner = format!("notification_store_o_{suffix}");
        let definer = format!("notification_store_d_{suffix}");
        let installer = format!("notification_store_i_{suffix}");
        let runtime = format!("notification_store_r_{suffix}");
        let target = format!("notification_store_t_{suffix}");
        let trusted = format!("notification_store_f_{suffix}");

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
            "GRANT {owner} TO \"{}\"",
            admin_login.replace('"', "\"\"")
        ))
        .execute(&cluster_admin)
        .await
        .expect("allow test administrator to assume owner role");
        sqlx::query(&format!(
            "GRANT {definer} TO {installer} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"
        ))
        .execute(&cluster_admin)
        .await
        .expect("grant restricted installer handoff");

        let base_url = notification_attach_url_with_database(&url, &database);
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
                .expect("grant only target-schema usage");
        }

        Self {
            cluster_admin,
            admin,
            base_url,
            admin_login,
            database,
            owner,
            definer,
            installer,
            runtime,
            target,
            trusted,
            had_public_control_execute: public_control_execute,
            operation_id: uuid::Uuid::new_v4(),
        }
    }

    async fn exercise(&self) -> Result<()> {
        use crate::config::LineageConfig;
        use crate::db::lineage::{self, DeploymentUuid};
        use crate::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY;
        use crate::db::physical_attestation::{
            attested_pool, install, PgRoleName, PhysicalAttestationConfig,
        };
        use crate::db::{DatabaseConfig, DatabaseDriver};

        let server_version: i32 =
            sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
                .fetch_one(&self.admin)
                .await?;
        if !(170_000..180_000).contains(&server_version) {
            bail!("notification-store reader witness requires PostgreSQL 17, got {server_version}");
        }

        let owner_url =
            notification_attach_url_with_role_and_schema(&self.base_url, &self.owner, &self.target);
        let owner_database = Database::from_config(
            "notification-store owner",
            &DatabaseConfig::new(DatabaseDriver::Postgres, owner_url),
        )
        .await?;
        let lineage = lineage::enroll(
            &owner_database,
            &LineageConfig {
                deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
                action: None,
            },
        )
        .await?;
        let _owner_activity =
            NotificationActivityStore::initialize_owner(owner_database.clone()).await?;
        let _owner_activity_again =
            NotificationActivityStore::initialize_owner(owner_database.clone()).await?;
        let _owner_push =
            DatabasePushRegistrationStore::initialize_owner(owner_database.clone()).await?;

        let attestation_config = PhysicalAttestationConfig::new(
            lineage,
            PgRoleName::new(self.installer.clone())?,
            PgRoleName::new(self.runtime.clone())?,
            PgRoleName::new(self.definer.clone())?,
            crate::db::lineage::PgSchemaName(self.trusted.clone()),
        )?;
        for statement in [
            "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC".to_owned(),
            format!(
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {}",
                self.owner
            ),
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
        let boundary_url = notification_attach_url_with_schema(&self.base_url, &self.target);
        let installer_options =
            PgConnectOptions::from_str(&boundary_url)?.username(&self.installer);
        install(
            &installer_options,
            attestation_config.clone(),
            self.operation_id,
        )
        .await?;

        sqlx::query(&format!(
            "GRANT SELECT, INSERT, UPDATE ON {}.notification_activity TO {}",
            self.target, self.runtime
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "GRANT SELECT ON {}.{} TO {}",
            self.target, ACTIVITY_OWNER_STATE_TABLE, self.runtime
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.push_registrations TO {}",
            self.target, self.runtime
        ))
        .execute(&self.admin)
        .await?;

        let owners_are_no_login: bool = sqlx::query_scalar(
            "SELECT bool_and(owner.rolname = $1 AND NOT owner.rolcanlogin) \
             FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             JOIN pg_catalog.pg_roles owner ON owner.oid = relation.relowner \
             WHERE namespace.nspname = $2 \
               AND relation.relname IN ('notification_activity', 'push_registrations') \
               AND relation.relkind = 'r'",
        )
        .bind(&self.owner)
        .bind(&self.target)
        .fetch_one(&self.admin)
        .await?;
        if !owners_are_no_login {
            bail!("notification tables are not owned by the NOLOGIN role");
        }

        let runtime_options = PgConnectOptions::from_str(&boundary_url)?.username(&self.runtime);
        let application_name = format!("notification_attach_{}", uuid::Uuid::new_v4().simple());
        let attested = attested_pool(
            runtime_options.application_name(&application_name),
            attestation_config,
            1,
            Duration::from_secs(3),
        )?;
        let runtime_database = crate::test_support::database_from_attested_postgres_pool(
            "notification-store runtime",
            attested,
        );
        match NotificationActivityStore::initialize_owner(runtime_database.clone()).await {
            Err(super::NotificationActivityError::OwnerBootstrapRequiresOwnerHandle) => {}
            Err(error) => bail!("attested activity owner bootstrap had wrong error: {error}"),
            Ok(_) => bail!("attested activity owner bootstrap unexpectedly succeeded"),
        }
        match DatabasePushRegistrationStore::initialize_owner(runtime_database.clone()).await {
            Err(error)
                if error
                    .to_string()
                    .contains("cannot use an attested runtime handle") => {}
            Err(error) => bail!("attested push owner bootstrap had wrong error: {error}"),
            Ok(_) => bail!("attested push owner bootstrap unexpectedly succeeded"),
        }

        sqlx::query(&format!(
            "UPDATE {}.{} SET prepared_revision = '0' WHERE singleton = '1'",
            self.target, ACTIVITY_OWNER_STATE_TABLE
        ))
        .execute(&self.admin)
        .await?;
        match NotificationActivityStore::attach_existing(runtime_database.clone()).await {
            Err(DurableStoreAttachError::OwnerPreparationIncomplete) => {}
            Err(error) => bail!("incomplete activity marker had wrong error: {error}"),
            Ok(_) => bail!("incomplete activity marker was accepted"),
        }
        let _owner_activity_after_incomplete =
            NotificationActivityStore::initialize_owner(owner_database.clone()).await?;

        sqlx::query(&format!(
            "UPDATE {}.{} SET schema_oid = schema_oid + 1 WHERE singleton = '1'",
            self.target, ACTIVITY_OWNER_STATE_TABLE
        ))
        .execute(&self.admin)
        .await?;
        match NotificationActivityStore::attach_existing(runtime_database.clone()).await {
            Err(DurableStoreAttachError::OwnerIdentityMismatch) => {}
            Err(error) => bail!("wrong activity marker identity had wrong error: {error}"),
            Ok(_) => bail!("wrong activity marker identity was accepted"),
        }
        sqlx::query(&format!(
            "UPDATE {}.{} SET schema_oid = (\
               SELECT oid::bigint FROM pg_catalog.pg_namespace WHERE nspname = $1\
             ) WHERE singleton = '1'",
            self.target, ACTIVITY_OWNER_STATE_TABLE
        ))
        .bind(&self.target)
        .execute(&self.admin)
        .await?;

        sqlx::query(&format!(
            "GRANT UPDATE ON {}.{} TO {}",
            self.target, ACTIVITY_OWNER_STATE_TABLE, self.runtime
        ))
        .execute(&self.admin)
        .await?;
        match NotificationActivityStore::attach_existing(runtime_database.clone()).await {
            Err(DurableStoreAttachError::UnsafeRuntimeRole) => {}
            Err(error) => bail!("activity marker UPDATE grant had wrong error: {error}"),
            Ok(_) => bail!("activity marker UPDATE grant was accepted"),
        }
        sqlx::query(&format!(
            "REVOKE UPDATE ON {}.{} FROM {}",
            self.target, ACTIVITY_OWNER_STATE_TABLE, self.runtime
        ))
        .execute(&self.admin)
        .await?;

        sqlx::query(&format!(
            "DROP TABLE {}.{}",
            self.target, ACTIVITY_OWNER_STATE_TABLE
        ))
        .execute(&self.admin)
        .await?;
        match NotificationActivityStore::attach_existing(runtime_database.clone()).await {
            Err(DurableStoreAttachError::OwnerPreparationIncomplete) => {}
            Err(error) => bail!("missing activity marker had wrong error: {error}"),
            Ok(_) => bail!("missing activity marker was accepted"),
        }
        let _owner_activity_after_missing =
            NotificationActivityStore::initialize_owner(owner_database.clone()).await?;
        sqlx::query(&format!(
            "GRANT SELECT ON {}.{} TO {}",
            self.target, ACTIVITY_OWNER_STATE_TABLE, self.runtime
        ))
        .execute(&self.admin)
        .await?;

        let mut ddl = self.admin.begin().await?;
        sqlx::query(&format!(
            "LOCK TABLE {}.notification_activity IN ACCESS EXCLUSIVE MODE",
            self.target
        ))
        .execute(&mut *ddl)
        .await?;
        let attach_database = runtime_database.clone();
        let attach_during_ddl = tokio::spawn(async move {
            NotificationActivityStore::attach_existing(attach_database).await
        });
        self.wait_for_catalog_fenced_relation_wait(
            &application_name,
            "notification_activity",
            true,
            CATALOG_MUTATION_ADVISORY_LOCK_KEY,
            ACTIVITY_SCHEMA_GATE,
        )
        .await?;
        sqlx::query(&format!(
            "CREATE FUNCTION {}.notification_activity_attach_race() \
             RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
            self.target
        ))
        .execute(&mut *ddl)
        .await?;
        sqlx::query(&format!(
            "CREATE TRIGGER notification_activity_attach_race \
             BEFORE INSERT ON {}.notification_activity FOR EACH ROW \
             EXECUTE FUNCTION {}.notification_activity_attach_race()",
            self.target, self.target
        ))
        .execute(&mut *ddl)
        .await?;
        ddl.commit().await?;
        match attach_during_ddl.await? {
            Err(DurableStoreAttachError::UnexpectedTrigger {
                table: "notification_activity",
            }) => {}
            Err(error) => bail!("activity trigger race had wrong error: {error}"),
            Ok(_) => bail!("activity trigger committed during attach was accepted"),
        }
        sqlx::query(&format!(
            "DROP TRIGGER notification_activity_attach_race ON {}.notification_activity",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "DROP FUNCTION {}.notification_activity_attach_race()",
            self.target
        ))
        .execute(&self.admin)
        .await?;

        // Exercise both public reader entry points. Push registrations share
        // catalog-S fencing but intentionally do not take the Activity gate.
        let activity = NotificationActivityStore::attach_existing(runtime_database.clone()).await?;
        let push = DatabasePushRegistrationStore::attach_existing(runtime_database.clone()).await?;

        let mut push_ddl = self.admin.begin().await?;
        sqlx::query(&format!(
            "LOCK TABLE {}.push_registrations IN ACCESS EXCLUSIVE MODE",
            self.target
        ))
        .execute(&mut *push_ddl)
        .await?;
        let attach_database = runtime_database.clone();
        let push_attach_during_ddl = tokio::spawn(async move {
            DatabasePushRegistrationStore::attach_existing(attach_database).await
        });
        self.wait_for_catalog_fenced_relation_wait(
            &application_name,
            "push_registrations",
            false,
            CATALOG_MUTATION_ADVISORY_LOCK_KEY,
            ACTIVITY_SCHEMA_GATE,
        )
        .await?;
        sqlx::query(&format!(
            "CREATE FUNCTION {}.push_registration_attach_race() \
             RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
            self.target
        ))
        .execute(&mut *push_ddl)
        .await?;
        sqlx::query(&format!(
            "CREATE TRIGGER push_registration_attach_race \
             BEFORE INSERT ON {}.push_registrations FOR EACH ROW \
             EXECUTE FUNCTION {}.push_registration_attach_race()",
            self.target, self.target
        ))
        .execute(&mut *push_ddl)
        .await?;
        push_ddl.commit().await?;
        match push_attach_during_ddl.await? {
            Err(DurableStoreAttachError::UnexpectedTrigger {
                table: "push_registrations",
            }) => {}
            Err(error) => bail!("push-registration trigger race had wrong error: {error}"),
            Ok(_) => bail!("push-registration trigger committed during attach was accepted"),
        }
        sqlx::query(&format!(
            "DROP TRIGGER push_registration_attach_race ON {}.push_registrations",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "DROP FUNCTION {}.push_registration_attach_race()",
            self.target
        ))
        .execute(&self.admin)
        .await?;

        let owner: jid::BareJid = "alice@example.net".parse()?;
        let conversation: jid::BareJid = "chat@example.net".parse()?;
        activity
            .record_chat_state(
                &owner,
                &conversation,
                NotificationChatState::Active,
                1_800_000_000,
            )
            .await?;
        if activity
            .read_activity(&owner, &conversation)
            .await?
            .is_none()
        {
            bail!("attested activity runtime could not round-trip DML");
        }

        sqlx::query(&format!(
            "CREATE TABLE {}.activity_dependents (\
               owner_bare_jid TEXT NOT NULL, conversation_jid TEXT NOT NULL, \
               FOREIGN KEY (owner_bare_jid, conversation_jid) \
                 REFERENCES {}.notification_activity (owner_bare_jid, conversation_jid))",
            self.target, self.target
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {}.activity_dependents VALUES ($1, $2)",
            self.target
        ))
        .bind(owner.to_string())
        .bind(conversation.to_string())
        .execute(&self.admin)
        .await?;
        match NotificationActivityStore::attach_existing(runtime_database.clone()).await {
            Err(DurableStoreAttachError::InboundForeignKey {
                table: "notification_activity",
            }) => {}
            Err(error) => bail!("activity inbound FK had wrong error: {error}"),
            Ok(_) => bail!("activity inbound FK was accepted"),
        }
        sqlx::query(&format!("DROP TABLE {}.activity_dependents", self.target))
            .execute(&self.admin)
            .await?;

        sqlx::query(&format!(
            "GRANT DELETE ON {}.notification_activity TO {}",
            self.target, self.runtime
        ))
        .execute(&self.admin)
        .await?;
        match NotificationActivityStore::attach_existing(runtime_database.clone()).await {
            Err(DurableStoreAttachError::UnsafeRuntimeRole) => {}
            Err(error) => bail!("activity DELETE grant had wrong error: {error}"),
            Ok(_) => bail!("activity DELETE grant was accepted"),
        }
        sqlx::query(&format!(
            "REVOKE DELETE ON {}.notification_activity FROM {}",
            self.target, self.runtime
        ))
        .execute(&self.admin)
        .await?;

        let registration = PushSubscription {
            user_jid: owner.to_string(),
            service_jid: "push.example.net".to_owned(),
            node: Some("n1".to_owned()),
            publish_options: None,
            endpoint: None,
            p256dh: None,
            auth_key: None,
        };
        push.register(registration).await?;
        if push.get_for_user(&owner.to_string()).await?.len() != 1 {
            bail!("attested push-registration runtime could not round-trip DML");
        }

        sqlx::query(&format!(
            "CREATE TABLE {}.push_registration_dependents (\
               owner_bare_jid TEXT NOT NULL, push_service_jid TEXT NOT NULL, node TEXT NOT NULL, \
               FOREIGN KEY (owner_bare_jid, push_service_jid, node) \
                 REFERENCES {}.push_registrations \
                   (owner_bare_jid, push_service_jid, node))",
            self.target, self.target
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {}.push_registration_dependents VALUES ($1, $2, $3)",
            self.target
        ))
        .bind(owner.to_string())
        .bind("push.example.net")
        .bind("n1")
        .execute(&self.admin)
        .await?;
        match DatabasePushRegistrationStore::attach_existing(runtime_database.clone()).await {
            Err(DurableStoreAttachError::InboundForeignKey {
                table: "push_registrations",
            }) => {}
            Err(error) => bail!("push-registration inbound FK had wrong error: {error}"),
            Ok(_) => bail!("push-registration inbound FK was accepted"),
        }
        sqlx::query(&format!(
            "DROP TABLE {}.push_registration_dependents",
            self.target
        ))
        .execute(&self.admin)
        .await?;

        sqlx::query(&format!(
            "DROP INDEX {}.idx_notification_activity_conversation_active",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        match NotificationActivityStore::attach_existing(runtime_database.clone()).await {
            Err(DurableStoreAttachError::UnexpectedIndex {
                table: "notification_activity",
            }) => {}
            Err(error) => bail!("missing activity index had wrong error: {error}"),
            Ok(_) => bail!("missing activity index was accepted"),
        }
        sqlx::query(&format!(
            "CREATE INDEX idx_notification_activity_conversation_active \
             ON {}.notification_activity (conversation_jid, last_active_at_ms)",
            self.target
        ))
        .execute(&self.admin)
        .await?;

        sqlx::query(&format!(
            "ALTER TABLE {}.push_registrations ALTER COLUMN failure_count SET DEFAULT 1",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        match DatabasePushRegistrationStore::attach_existing(runtime_database.clone()).await {
            Err(DurableStoreAttachError::InvalidColumn {
                table: "push_registrations",
                column: "failure_count",
            }) => {}
            Err(error) => bail!("unsafe registration default had wrong error: {error}"),
            Ok(_) => bail!("unsafe registration default was accepted"),
        }
        sqlx::query(&format!(
            "ALTER TABLE {}.push_registrations ALTER COLUMN failure_count SET DEFAULT 0",
            self.target
        ))
        .execute(&self.admin)
        .await?;

        sqlx::query(&format!(
            "GRANT TRUNCATE ON {}.push_registrations TO {}",
            self.target, self.runtime
        ))
        .execute(&self.admin)
        .await?;
        match DatabasePushRegistrationStore::attach_existing(runtime_database.clone()).await {
            Err(DurableStoreAttachError::UnsafeRuntimeRole) => {}
            Err(error) => bail!("unsafe registration grant had wrong error: {error}"),
            Ok(_) => bail!("unsafe registration grant was accepted"),
        }

        drop(push);
        drop(activity);
        drop(runtime_database);
        drop(_owner_push);
        drop(_owner_activity_after_missing);
        drop(_owner_activity_after_incomplete);
        drop(_owner_activity_again);
        drop(_owner_activity);
        drop(owner_database);
        Ok(())
    }

    async fn wait_for_catalog_fenced_relation_wait(
        &self,
        application_name: &str,
        table: &str,
        activity_gate_expected: bool,
        catalog_lock_key: i64,
        activity_gate: (i32, i32),
    ) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let operation: Option<(i32, bool, bool)> = sqlx::query_as(
                    "SELECT activity.pid, \
                       EXISTS (SELECT 1 FROM pg_catalog.pg_locks catalog_lock \
                         WHERE catalog_lock.pid = activity.pid \
                           AND catalog_lock.locktype = 'advisory' \
                           AND catalog_lock.objsubid = 1 \
                           AND catalog_lock.classid::bigint = ($4::bigint >> 32) \
                           AND catalog_lock.objid::bigint = ($4::bigint & 4294967295::bigint) \
                           AND catalog_lock.mode = 'ShareLock' \
                           AND catalog_lock.granted), \
                       EXISTS (SELECT 1 FROM pg_catalog.pg_locks activity_gate_lock \
                         WHERE activity_gate_lock.pid = activity.pid \
                           AND activity_gate_lock.locktype = 'advisory' \
                           AND activity_gate_lock.objsubid = 2 \
                           AND activity_gate_lock.classid::bigint = $5::bigint \
                           AND activity_gate_lock.objid::bigint = $6::bigint \
                           AND activity_gate_lock.mode = 'ShareLock' \
                           AND activity_gate_lock.granted) \
                     FROM pg_catalog.pg_stat_activity activity \
                     WHERE activity.application_name = $1 \
                       AND activity.state = 'active' \
                       AND activity.xact_start IS NOT NULL \
                       AND activity.wait_event_type = 'Lock' \
                       AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks relation_wait \
                         JOIN pg_catalog.pg_class relation \
                           ON relation.oid = relation_wait.relation \
                         JOIN pg_catalog.pg_namespace namespace \
                           ON namespace.oid = relation.relnamespace \
                         WHERE relation_wait.pid = activity.pid \
                           AND namespace.nspname = $2 \
                           AND relation.relname = $3 \
                           AND relation_wait.mode = 'ShareUpdateExclusiveLock' \
                           AND NOT relation_wait.granted)",
                )
                .bind(application_name)
                .bind(&self.target)
                .bind(table)
                .bind(catalog_lock_key)
                .bind(i64::from(activity_gate.0))
                .bind(i64::from(activity_gate.1))
                .fetch_optional(&self.admin)
                .await?;
                if let Some((pid, holds_catalog_s, holds_activity_gate)) = operation {
                    if !holds_catalog_s {
                        bail!("operation backend {pid} waited on {table} without catalog-S");
                    }
                    if holds_activity_gate != activity_gate_expected {
                        bail!(
                            "operation backend {pid} held activity gate: expected {activity_gate_expected}"
                        );
                    }
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        Ok(())
    }

    async fn cleanup(self) {
        self.admin.close().await;
        sqlx::query(&format!(
            "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {}",
            self.definer
        ))
        .execute(&self.cluster_admin)
        .await
        .expect("remove disposable definer function grant");
        sqlx::query(&format!(
            "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {}",
            self.owner
        ))
        .execute(&self.cluster_admin)
        .await
        .expect("remove disposable owner function grant");
        let restore_public = if self.had_public_control_execute {
            "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO PUBLIC"
        } else {
            "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC"
        };
        sqlx::query(restore_public)
            .execute(&self.cluster_admin)
            .await
            .expect("restore original control-function public ACL");
        sqlx::query(&format!(
            "REVOKE {} FROM \"{}\"",
            self.owner,
            self.admin_login.replace('"', "\"\"")
        ))
        .execute(&self.cluster_admin)
        .await
        .expect("remove disposable owner membership from test login");
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&self.cluster_admin)
            .await
            .expect("drop isolated test database");
        for role in [&self.runtime, &self.installer, &self.owner, &self.definer] {
            sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
                .execute(&self.cluster_admin)
                .await
                .expect("drop disposable test role");
        }
        self.cluster_admin.close().await;
    }
}

#[tokio::test]
async fn postgres_attached_notification_stores_validate_shape_and_runtime_grants() {
    static CONTROL_CATALOG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _serial = CONTROL_CATALOG_LOCK.lock().await;
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping PostgreSQL notification-store attachment test: test URL not set");
        return;
    };
    let fixture = Fixture::new(url).await;
    let result = fixture.exercise().await;
    fixture.cleanup().await;
    result.expect("owner-prepared PostgreSQL notification-store attach contract");
}

fn notification_attach_url_with_database(base: &str, database: &str) -> String {
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

fn notification_attach_url_with_schema(base: &str, schema: &str) -> String {
    notification_attach_url_with_options(base, &format!("-c search_path={schema}"))
}

fn notification_attach_url_with_role_and_schema(base: &str, role: &str, schema: &str) -> String {
    notification_attach_url_with_options(base, &format!("-c role={role} -c search_path={schema}"))
}

fn notification_attach_url_with_options(base: &str, options: &str) -> String {
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
