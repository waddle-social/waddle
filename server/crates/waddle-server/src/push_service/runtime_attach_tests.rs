use super::owner::PUSH_SCHEMA_ADVISORY_LOCK_KEY;
use super::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration, PushServiceAttachError,
    PushServiceOwnerError,
};
use crate::db::lineage::{self, DeploymentUuid};
use crate::db::physical_attestation::{
    attested_pool, install, PgRoleName, PhysicalAttestationConfig,
};
use crate::db::{Database, DatabaseConfig, DatabaseDriver};
use anyhow::{bail, Context, Result};
use sqlx::postgres::{PgConnectOptions, PgPool};
use std::str::FromStr;
use std::time::Duration;

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
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database = format!("push_store_owner_{suffix}");
        let owner = format!("push_store_o_{suffix}");
        let definer = format!("push_store_d_{suffix}");
        let installer = format!("push_store_i_{suffix}");
        let runtime = format!("push_store_r_{suffix}");
        let target = format!("push_store_t_{suffix}");
        let trusted = format!("push_store_f_{suffix}");

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
            .expect("create no-login owner or definer");
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

        let base_url = push_attach_url_with_database(&url, &database);
        let admin = PgPool::connect(&base_url)
            .await
            .expect("connect isolated database administrator");
        let had_public_control_execute: bool = sqlx::query_scalar(
            "SELECT EXISTS (\
               SELECT 1 FROM pg_catalog.pg_proc procedure \
               CROSS JOIN LATERAL pg_catalog.aclexplode( \
                 COALESCE(procedure.proacl, pg_catalog.acldefault('f', procedure.proowner)) \
               ) acl \
               WHERE procedure.oid = 'pg_catalog.pg_control_system()'::regprocedure \
                 AND acl.grantee = 0 AND acl.privilege_type = 'EXECUTE' \
             )",
        )
        .fetch_one(&admin)
        .await
        .expect("capture pg_control_system public ACL in test database");
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
                .expect("grant target-schema usage");
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
            had_public_control_execute,
            operation_id: uuid::Uuid::new_v4(),
        }
    }

    async fn exercise(&self) -> Result<()> {
        let owner_url =
            push_attach_url_with_role_and_schema(&self.base_url, &self.owner, &self.target);
        let owner_database = Database::from_config(
            "push-service owner",
            &DatabaseConfig::new(DatabaseDriver::Postgres, owner_url),
        )
        .await?;
        let temp_test_database = Database::from_config(
            "push-service temp-shadow test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, self.base_url.clone()),
        )
        .await?;
        sqlx::query("REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC")
            .execute(&self.admin)
            .await?;
        for role in [&self.owner, &self.definer] {
            sqlx::query(&format!(
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {role}"
            ))
            .execute(&self.admin)
            .await?;
        }
        let lineage = lineage::enroll(
            &owner_database,
            &crate::config::LineageConfig {
                deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
                action: None,
            },
        )
        .await?;
        let identity = lineage
            .postgres_identity
            .clone()
            .ok_or_else(|| anyhow::anyhow!("owner lineage did not include PostgreSQL identity"))?;

        let mut wrong_identity = identity.clone();
        wrong_identity.schema.oid = wrong_identity.schema.oid.saturating_add(1);
        match DatabasePushServiceStore::initialize_owner(&owner_database, &wrong_identity).await {
            Err(PushServiceOwnerError::UnsafeOwnerBoundary) => {}
            Err(error) => bail!("wrong owner schema identity had wrong error: {error}"),
            Ok(()) => bail!("wrong owner schema identity was accepted"),
        }
        let unexpected_push_tables: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = $1 AND relation.relname LIKE 'push_%'",
        )
        .bind(&self.target)
        .fetch_one(&self.admin)
        .await?;
        if unexpected_push_tables != 0 {
            bail!("wrong identity created Push Service relations");
        }
        verify_owner_preparation_cancellation(
            &owner_database,
            &self.admin,
            &self.target,
            &identity,
        )
        .await?;

        sqlx::query(&format!(
            "GRANT SELECT ON {}._lineage TO {}",
            self.target, self.definer
        ))
        .execute(&self.admin)
        .await?;
        let attestation_config = PhysicalAttestationConfig::new(
            lineage,
            PgRoleName::new(self.installer.clone())?,
            PgRoleName::new(self.runtime.clone())?,
            PgRoleName::new(self.definer.clone())?,
            crate::db::lineage::PgSchemaName(self.trusted.clone()),
        )?;
        let boundary_url = push_attach_url_with_schema(&self.base_url, &self.target);
        let installer_options =
            PgConnectOptions::from_str(&boundary_url)?.username(&self.installer);
        install(
            &installer_options,
            attestation_config.clone(),
            self.operation_id,
        )
        .await?;
        let runtime_options = PgConnectOptions::from_str(&boundary_url)?.username(&self.runtime);
        let attested = attested_pool(
            runtime_options.clone(),
            attestation_config.clone(),
            1,
            Duration::from_secs(3),
        )?;
        let runtime_database = crate::test_support::database_from_attested_postgres_pool(
            "push-service runtime",
            attested,
        );
        let second_attested = attested_pool(
            runtime_options,
            attestation_config,
            1,
            Duration::from_secs(3),
        )?;
        let second_runtime_database = crate::test_support::database_from_attested_postgres_pool(
            "second push-service runtime",
            second_attested,
        );

        DatabasePushServiceStore::initialize_owner(&owner_database, &identity).await?;
        if marker_phase(&self.admin, &self.target).await? != 0 {
            bail!("local owner preparation did not leave an incomplete marker");
        }
        let secret = [7_u8; 32];
        grant_push_runtime_privileges(&self.admin, &self.target, &self.runtime).await?;
        assert_invalid_marker(
            DatabasePushServiceStore::attach_existing_without_pubsub(
                runtime_database.clone(),
                &secret,
            )
            .await,
        )?;
        match DatabasePushServiceStore::adopt_notification_ancestry_owner(
            &owner_database,
            &identity,
        )
        .await
        {
            Err(PushServiceOwnerError::NotificationOutboxNotPrepared) => {}
            Err(error) => bail!("early ancestry adoption had wrong error: {error}"),
            Ok(()) => bail!("ancestry adoption succeeded before outbox preparation"),
        }
        if marker_phase(&self.admin, &self.target).await? != 0 {
            bail!("failed ancestry adoption changed the incomplete marker");
        }

        let _legacy_outbox =
            crate::notification_outbox::NotificationOutboxStore::try_new(owner_database.clone())
                .await?;
        match DatabasePushServiceStore::adopt_notification_ancestry_owner(
            &owner_database,
            &identity,
        )
        .await
        {
            Err(PushServiceOwnerError::NotificationOutboxNotPrepared) => {}
            Err(error) => bail!("incomplete outbox marker had wrong error: {error}"),
            Ok(()) => bail!("incomplete outbox marker completed Push readiness"),
        }
        if marker_phase(&self.admin, &self.target).await? != 0 {
            bail!("incomplete outbox marker changed the Push owner marker");
        }
        let _outbox = crate::notification_outbox::NotificationOutboxStore::prepare_owner(
            &owner_database,
            &identity,
        )
        .await?;
        DatabasePushServiceStore::adopt_notification_ancestry_owner(&owner_database, &identity)
            .await?;
        if marker_phase(&self.admin, &self.target).await? != 1 {
            bail!("cross-store owner adoption did not complete the marker");
        }
        verify_marker_temp_shadow_is_rejected_and_qualified(
            &temp_test_database,
            &self.admin,
            &self.target,
        )
        .await?;

        match DatabasePushServiceStore::initialize_owner(&runtime_database, &identity).await {
            Err(PushServiceOwnerError::OwnerHandleRequired) => {}
            Err(error) => bail!("attested owner initialization had wrong error: {error}"),
            Ok(()) => bail!("attested runtime handle performed owner initialization"),
        }

        // Two independently attested nodes may start together. Both must
        // attach even though their catalog checks take conflicting SUE locks.
        let (store, _) = tokio::try_join!(
            DatabasePushServiceStore::attach_existing_without_pubsub(
                runtime_database.clone(),
                &secret,
            ),
            DatabasePushServiceStore::attach_existing_without_pubsub(
                second_runtime_database,
                &secret,
            ),
        )?;
        let owner: jid::BareJid = "alice@example.net".parse()?;
        let node = store
            .ensure_node(&owner, "push-service-attach-test")
            .await?;
        if store.get_node(node.node()).await?.is_none() {
            bail!("single-slot attested Push Service store could not round-trip node DML");
        }
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new(
                    "device-1",
                    node.node(),
                    PushDevicePlatform::Web,
                    "test",
                ),
            )
            .await?;

        assert_rejected_publish_jobs_index_shape(
            &self.admin,
            runtime_database.clone(),
            &secret,
            &self.target,
            "ancestry_job_id text_pattern_ops",
        )
        .await?;
        assert_rejected_publish_jobs_index_shape(
            &self.admin,
            runtime_database.clone(),
            &secret,
            &self.target,
            "ancestry_job_id COLLATE \"C\"",
        )
        .await?;

        sqlx::query(&format!(
            "CREATE TABLE public.push_nodes_dependent (node TEXT NOT NULL, \
             FOREIGN KEY (node) REFERENCES {}.push_nodes(node) ON DELETE CASCADE)",
            self.target
        ))
        .execute(&self.admin)
        .await?;
        sqlx::query("INSERT INTO public.push_nodes_dependent (node) VALUES ($1)")
            .bind(node.node())
            .execute(&self.admin)
            .await?;
        match DatabasePushServiceStore::attach_existing_without_pubsub(
            runtime_database.clone(),
            &secret,
        )
        .await
        {
            Err(PushServiceAttachError::InboundForeignKey {
                table: "push_nodes",
            }) => {}
            Err(error) => bail!("cross-schema inbound FK had wrong error: {error}"),
            Ok(_) => bail!("cross-schema inbound FK was accepted"),
        }
        sqlx::query("DROP TABLE public.push_nodes_dependent")
            .execute(&self.admin)
            .await?;

        sqlx::query(&format!(
            "GRANT TRUNCATE ON {}.push_nodes TO {}",
            self.target, self.runtime
        ))
        .execute(&self.admin)
        .await?;
        match DatabasePushServiceStore::attach_existing_without_pubsub(
            runtime_database.clone(),
            &secret,
        )
        .await
        {
            Err(PushServiceAttachError::UnsafeRuntimePrivileges {
                component: "push_nodes",
            }) => {}
            Err(error) => bail!("unexpected TRUNCATE grant had wrong error: {error}"),
            Ok(_) => bail!("unexpected TRUNCATE grant was accepted"),
        }
        sqlx::query(&format!(
            "REVOKE TRUNCATE ON {}.push_nodes FROM {}",
            self.target, self.runtime
        ))
        .execute(&self.admin)
        .await?;

        run_concurrent_index_race(
            self.admin.clone(),
            runtime_database.clone(),
            &self.target,
            &secret,
        )
        .await?;

        // Exercise the preserved raw constructor after owner preparation. It
        // invalidates completion before its own DDL/data migration and leaves
        // the explicit owner ancestry phase to restore readiness.
        let _legacy =
            DatabasePushServiceStore::new_with_secret_key(owner_database.clone(), &secret).await?;
        if marker_phase(&self.admin, &self.target).await? != 0 {
            bail!("legacy raw initialization left the owner marker complete");
        }
        assert_invalid_marker(
            DatabasePushServiceStore::attach_existing_without_pubsub(
                runtime_database.clone(),
                &secret,
            )
            .await,
        )?;
        DatabasePushServiceStore::initialize_owner(&owner_database, &identity).await?;
        DatabasePushServiceStore::adopt_notification_ancestry_owner(&owner_database, &identity)
            .await?;
        let after_legacy = DatabasePushServiceStore::attach_existing_without_pubsub(
            runtime_database.clone(),
            &secret,
        )
        .await?;
        let second_node = after_legacy
            .ensure_node(&owner, "after-legacy-init")
            .await?;
        if after_legacy.get_node(second_node.node()).await?.is_none() {
            bail!("reattached store could not use the same one-slot runtime pool");
        }

        verify_legacy_adoption_waits_for_push_gate(
            _legacy.clone(),
            &owner_database,
            &self.admin,
            runtime_database.clone(),
            &secret,
            &self.target,
        )
        .await?;
        DatabasePushServiceStore::initialize_owner(&owner_database, &identity).await?;
        DatabasePushServiceStore::adopt_notification_ancestry_owner(&owner_database, &identity)
            .await?;
        let after_raw_adoption = DatabasePushServiceStore::attach_existing_without_pubsub(
            runtime_database.clone(),
            &secret,
        )
        .await?;
        if after_raw_adoption
            .get_node(second_node.node())
            .await?
            .is_none()
        {
            bail!("owner re-preparation after legacy adoption lost Push Service state");
        }

        drop(after_raw_adoption);
        drop(after_legacy);
        drop(store);
        drop(runtime_database);
        drop(_legacy);
        drop(_outbox);
        drop(owner_database);
        Ok(())
    }

    async fn cleanup(self) {
        for role in [&self.owner, &self.definer] {
            sqlx::query(&format!(
                "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {role}"
            ))
            .execute(&self.admin)
            .await
            .expect("remove disposable control-function grants");
        }
        let restore_public = if self.had_public_control_execute {
            "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO PUBLIC"
        } else {
            "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC"
        };
        sqlx::query(restore_public)
            .execute(&self.admin)
            .await
            .expect("restore original control-function public ACL");
        self.admin.close().await;
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
async fn postgres_push_owner_preparation_and_attested_attach_contract() {
    static CONTROL_CATALOG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _serial = CONTROL_CATALOG_LOCK.lock().await;
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping PostgreSQL Push Service owner/attach test: test URL not set");
        return;
    };
    let fixture = Fixture::new(url).await;
    let result = fixture.exercise().await;
    fixture.cleanup().await;
    result.expect("owner-prepared PostgreSQL Push Service runtime attach contract");
}

async fn marker_phase(admin: &PgPool, schema: &str) -> Result<i64> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT owner_phase FROM {schema}.push_service_schema_marker WHERE marker_id = 1"
    ))
    .fetch_one(admin)
    .await?)
}

async fn verify_marker_temp_shadow_is_rejected_and_qualified(
    owner_database: &Database,
    admin: &PgPool,
    schema: &str,
) -> Result<()> {
    let mut tx = owner_database.begin().await?;
    tx.execute(
        "CREATE TEMP TABLE push_service_schema_marker (\
           marker_id INTEGER PRIMARY KEY, owner_phase BIGINT NOT NULL)",
        (),
    )
    .await?;
    tx.execute(
        "INSERT INTO pg_temp.push_service_schema_marker (marker_id, owner_phase) VALUES (1, 1)",
        (),
    )
    .await?;
    match super::owner::verify_no_temp_push_relations_tx(&mut tx).await {
        Err(PushServiceOwnerError::UnsafeTemporaryRelation) => {}
        Err(error) => bail!("temp push relation check had wrong error: {error}"),
        Ok(()) => bail!("raw Push Service startup accepted a temp shadow relation"),
    }
    super::owner::invalidate_completion_marker_tx(&mut tx, schema).await?;
    let mut rows = tx
        .query(
            &format!(
                "SELECT owner_phase FROM {schema}.push_service_schema_marker WHERE marker_id = 1"
            ),
            (),
        )
        .await?;
    let main_phase = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("owner marker disappeared during temp shadow test"))?
        .get::<i64>(0)?;
    drop(rows);
    let mut rows = tx
        .query(
            "SELECT owner_phase FROM pg_temp.push_service_schema_marker WHERE marker_id = 1",
            (),
        )
        .await?;
    let temp_phase = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("temp marker disappeared during shadow test"))?
        .get::<i64>(0)?;
    drop(rows);
    tx.rollback().await?;
    if main_phase != 0 || temp_phase != 1 || marker_phase(admin, schema).await? != 1 {
        bail!(
            "qualified marker invalidation crossed a temp shadow: main={main_phase}, temp={temp_phase}"
        );
    }
    Ok(())
}

async fn verify_owner_preparation_cancellation(
    owner_database: &Database,
    admin: &PgPool,
    schema: &str,
    identity: &lineage::PgIdentity,
) -> Result<()> {
    let mut gate = owner_database.begin().await?;
    gate.query(
        "SELECT pg_catalog.pg_advisory_xact_lock(?)",
        crate::db_params![PUSH_SCHEMA_ADVISORY_LOCK_KEY],
    )
    .await?;

    let preparation_database = owner_database.clone();
    let preparation_identity = identity.clone();
    let preparation = tokio::spawn(async move {
        DatabasePushServiceStore::initialize_owner(&preparation_database, &preparation_identity)
            .await
    });
    let waiting = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE datname = current_database() AND wait_event_type = 'Lock' \
                   AND wait_event = 'advisory' \
                   AND query LIKE '%pg_advisory_xact_lock%')",
            )
            .fetch_one(admin)
            .await?;
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok::<(), sqlx::Error>(())
    })
    .await;
    match waiting {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            preparation.abort();
            gate.rollback().await?;
            return Err(error.into());
        }
        Err(error) => {
            preparation.abort();
            gate.rollback().await?;
            return Err(error.into());
        }
    }
    preparation.abort();
    match preparation.await {
        Err(error) if error.is_cancelled() => {}
        Err(error) => return Err(error.into()),
        Ok(result) => bail!("owner preparation completed before cancellation: {result:?}"),
    }
    gate.rollback().await?;

    let marker_exists: bool = sqlx::query_scalar(
        "SELECT pg_catalog.to_regclass(format('%I.push_service_schema_marker', $1)) IS NOT NULL",
    )
    .bind(schema)
    .fetch_one(admin)
    .await?;
    if marker_exists {
        bail!("cancelled owner preparation left a completion marker");
    }
    Ok(())
}

async fn verify_legacy_adoption_waits_for_push_gate(
    raw_store: DatabasePushServiceStore,
    owner_database: &Database,
    admin: &PgPool,
    runtime_database: Database,
    secret: &[u8],
    schema: &str,
) -> Result<()> {
    let mut gate = owner_database.begin().await?;
    gate.query(
        "SELECT pg_catalog.pg_advisory_xact_lock(?)",
        crate::db_params![PUSH_SCHEMA_ADVISORY_LOCK_KEY],
    )
    .await?;

    let adoption = tokio::spawn(async move { raw_store.adopt_notification_ancestry().await });
    let waiting = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE datname = current_database() AND wait_event_type = 'Lock' \
                   AND wait_event = 'advisory' \
                   AND query LIKE '%pg_advisory_xact_lock%')",
            )
            .fetch_one(admin)
            .await?;
            if waiting {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    if !matches!(waiting, Ok(Ok(()))) {
        adoption.abort();
        gate.rollback().await?;
        let _ = adoption.await;
        return Err(anyhow::anyhow!(
            "legacy ancestry adoption did not wait on the Push Service gate"
        ));
    }

    let phase_while_blocked = marker_phase(admin, schema).await;
    gate.rollback().await?;
    adoption.await??;
    let phase_while_blocked = phase_while_blocked?;
    let phase_after_adoption = marker_phase(admin, schema).await?;
    if phase_while_blocked != 1 || phase_after_adoption != 0 {
        bail!(
            "legacy ancestry adoption changed owner phase unexpectedly: while blocked={phase_while_blocked}, after={phase_after_adoption}"
        );
    }
    assert_invalid_marker(
        DatabasePushServiceStore::attach_existing_without_pubsub(runtime_database, secret).await,
    )?;
    Ok(())
}

fn assert_invalid_marker<T>(result: Result<T, PushServiceAttachError>) -> Result<()> {
    match result {
        Err(PushServiceAttachError::InvalidMarker) => Ok(()),
        Err(error) => bail!("incomplete owner marker had wrong error: {error}"),
        Ok(_) => bail!("incomplete owner marker was accepted"),
    }
}

async fn assert_rejected_publish_jobs_index_shape(
    admin: &PgPool,
    runtime_database: Database,
    secret: &[u8],
    schema: &str,
    index_columns: &str,
) -> Result<()> {
    sqlx::query(&format!(
        "DROP INDEX {schema}.idx_push_publish_jobs_canonical_delivery"
    ))
    .execute(admin)
    .await?;
    sqlx::query(&format!(
        "CREATE UNIQUE INDEX idx_push_publish_jobs_canonical_delivery \
         ON {schema}.push_publish_jobs ({index_columns}) \
         WHERE ancestry_job_id IS NOT NULL"
    ))
    .execute(admin)
    .await?;
    match DatabasePushServiceStore::attach_existing_without_pubsub(runtime_database, secret).await {
        Err(PushServiceAttachError::InvalidIndex {
            table: "push_publish_jobs",
        }) => {}
        Err(error) => bail!("unexpected index shape had wrong error: {error}"),
        Ok(_) => bail!("unexpected index opclass or collation was accepted"),
    }
    sqlx::query(&format!(
        "DROP INDEX {schema}.idx_push_publish_jobs_canonical_delivery"
    ))
    .execute(admin)
    .await?;
    sqlx::query(&format!(
        "CREATE UNIQUE INDEX idx_push_publish_jobs_canonical_delivery \
         ON {schema}.push_publish_jobs (ancestry_job_id) \
         WHERE ancestry_job_id IS NOT NULL"
    ))
    .execute(admin)
    .await?;
    Ok(())
}

async fn grant_push_runtime_privileges(admin: &PgPool, schema: &str, runtime: &str) -> Result<()> {
    for (table, privileges) in [
        ("push_service_schema_marker", "SELECT"),
        ("push_owner_locks", "SELECT, INSERT, UPDATE"),
        ("push_nodes", "SELECT, INSERT, UPDATE, DELETE"),
        ("push_node_locks", "SELECT, INSERT, UPDATE"),
        ("push_devices", "SELECT, INSERT, UPDATE, DELETE"),
        ("push_delivery_attempts", "SELECT, INSERT, DELETE"),
        ("push_publish_jobs", "SELECT, INSERT, UPDATE, DELETE"),
        ("push_publication_orders", "SELECT, INSERT, UPDATE"),
    ] {
        sqlx::query(&format!(
            "GRANT {privileges} ON {schema}.{table} TO {runtime}"
        ))
        .execute(admin)
        .await?;
    }
    Ok(())
}

async fn run_concurrent_index_race(
    admin: PgPool,
    runtime_database: Database,
    schema: &str,
    secret: &[u8],
) -> Result<()> {
    sqlx::query(&format!(
        "INSERT INTO {schema}.push_nodes \
           (node, owner_bare_jid, app_id, status, created_at_ms, updated_at_ms) \
         SELECT 'race-' || sequence::text, 'race@example.net', 'app-' || sequence::text, \
           'active', 1, 1 FROM generate_series(1, 300000) sequence"
    ))
    .execute(&admin)
    .await?;
    let target = format!("{schema}.push_nodes");
    let build_target = target.clone();
    let build_admin = admin.clone();
    let build = tokio::spawn(async move {
        sqlx::query(&format!(
            "CREATE INDEX CONCURRENTLY idx_push_nodes_race_probe \
             ON {build_target} (md5(node))"
        ))
        .execute(&build_admin)
        .await
    });
    let mut observed = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let in_progress: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_progress_create_index \
                 WHERE relid = $1::regclass)",
            )
            .bind(&target)
            .fetch_one(&admin)
            .await?;
            if in_progress {
                observed = true;
                break;
            }
            if build.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        Ok::<(), sqlx::Error>(())
    })
    .await??;
    if !observed {
        build.await??;
        bail!("could not observe concurrent index creation in progress");
    }

    let secret = secret.to_vec();
    let attach_database = runtime_database.clone();
    let attach_secret = secret.clone();
    let mut attach = tokio::spawn(async move {
        DatabasePushServiceStore::attach_existing_without_pubsub(attach_database, &attach_secret)
            .await
    });
    let early_result = tokio::time::timeout(Duration::from_secs(10), &mut attach)
        .await
        .context("Push Service attach did not refuse concurrent index creation")??;
    match early_result {
        Err(PushServiceAttachError::ConcurrentSchemaChange {
            table: "push_nodes",
        }) => {}
        Err(error) => bail!("concurrent index build had wrong refusal: {error}"),
        Ok(_) => bail!("Push Service attach accepted an active concurrent index build"),
    }
    build.await??;
    match DatabasePushServiceStore::attach_existing_without_pubsub(runtime_database, &secret).await
    {
        Err(PushServiceAttachError::InvalidIndex {
            table: "push_nodes",
        }) => {}
        Err(error) => bail!("completed concurrent index build had wrong error: {error}"),
        Ok(_) => bail!("Push Service attach accepted an unexpected concurrent index"),
    }
    sqlx::query(&format!("DROP INDEX {schema}.idx_push_nodes_race_probe"))
        .execute(&admin)
        .await?;
    Ok(())
}

fn push_attach_url_with_database(base: &str, database: &str) -> String {
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

fn push_attach_url_with_schema(base: &str, schema: &str) -> String {
    push_attach_url_with_options(base, &format!("-c search_path={schema}"))
}

fn push_attach_url_with_role_and_schema(base: &str, role: &str, schema: &str) -> String {
    push_attach_url_with_options(base, &format!("-c role={role} -c search_path={schema}"))
}

fn push_attach_url_with_options(base: &str, options: &str) -> String {
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
