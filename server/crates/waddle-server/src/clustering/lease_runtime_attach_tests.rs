use super::{
    Database, KeypairSlotLease, LeaseAttachError, LeaseError, LeaseIdentity, NodeId,
    PostgresKeypairSlotLease,
};

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
        let database = format!("keypair_slot_attach_{suffix}");
        let owner = format!("keypair_slot_o_{suffix}");
        let definer = format!("keypair_slot_d_{suffix}");
        let installer = format!("keypair_slot_i_{suffix}");
        let runtime = format!("keypair_slot_r_{suffix}");
        let target = format!("keypair_slot_s_{suffix}");
        let trusted = format!("keypair_slot_t_{suffix}");

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

        let base_url = keypair_attach_url(&url, &database, "-c search_path=public");
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
                .expect("grant target schema usage");
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

    async fn owner_prepare(
        &self,
    ) -> Result<(
        Database,
        crate::db::physical_attestation::PhysicalAttestationConfig,
        crate::db::lineage::PgIdentity,
        Vec<String>,
        uuid::Uuid,
    )> {
        use crate::config::LineageConfig;
        use crate::db::lineage::{self, DeploymentUuid};
        use crate::db::physical_attestation::{install, PgRoleName, PhysicalAttestationConfig};
        use crate::db::{DatabaseConfig, DatabaseDriver};

        let owner_url =
            keypair_attach_url_with_role(&self.base_url, &self.database, &self.owner, &self.target);
        let owner_database = Database::from_config(
            "keypair-slot owner",
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
        let identity = lineage
            .postgres_identity
            .as_ref()
            .context("enrolled owner lineage lacks PostgreSQL identity")?;
        let owner_identity = identity.clone();
        use base64::Engine;
        let keys = (0..2)
            .map(|_| {
                let keypair = libp2p::identity::ed25519::Keypair::generate();
                let seed =
                    base64::engine::general_purpose::STANDARD.encode(keypair.secret().as_ref());
                let peer_id = libp2p::identity::Keypair::from(keypair)
                    .public()
                    .to_peer_id();
                (seed, peer_id)
            })
            .collect::<Vec<_>>();
        let configured_pool = keys
            .iter()
            .map(|(seed, _)| seed.clone())
            .collect::<Vec<_>>();
        let peer_ids = keys.iter().map(|(_, peer_id)| *peer_id).collect::<Vec<_>>();
        let generation = uuid::Uuid::new_v4();
        PostgresKeypairSlotLease::initialize_keypool_owner(
            &owner_database,
            identity,
            generation,
            &peer_ids,
            &self.runtime,
            Duration::ZERO,
        )
        .await?;

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
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {}, {}",
                self.owner, self.definer
            ),
            format!(
                "GRANT SELECT ON {}._lineage TO {}",
                self.target, self.definer
            ),
        ] {
            sqlx::query(&statement).execute(&self.admin).await?;
        }
        let boundary_url = keypair_attach_url(
            &self.base_url,
            &self.database,
            &format!("-c search_path={}", self.target),
        );
        let installer_options =
            PgConnectOptions::from_str(&boundary_url)?.username(&self.installer);
        install(
            &installer_options,
            attestation_config.clone(),
            self.operation_id,
        )
        .await?;

        Ok((
            owner_database,
            attestation_config,
            owner_identity,
            configured_pool,
            generation,
        ))
    }

    async fn runtime_database(
        &self,
        config: crate::db::physical_attestation::PhysicalAttestationConfig,
        control_schema: &str,
    ) -> Result<Database> {
        use crate::db::physical_attestation::attested_pool;

        let boundary_url = keypair_attach_url(
            &self.base_url,
            &self.database,
            &format!("-c search_path={}", self.target),
        );
        let runtime_options = PgConnectOptions::from_str(&boundary_url)?.username(&self.runtime);
        let main = attested_pool(
            runtime_options.clone(),
            config.clone(),
            2,
            Duration::from_secs(3),
        )?;
        let control_url = keypair_attach_url(
            &self.base_url,
            &self.database,
            &format!("-c search_path={control_schema}"),
        );
        let control_options = PgConnectOptions::from_str(&control_url)?.username(&self.runtime);
        let control = attested_pool(control_options, config, 1, Duration::from_secs(3))?;
        Ok(
            Database::from_attested_postgres_pool_with_ingress_and_control_plane(
                "keypair-slot restricted runtime",
                main,
                Some((control, 1)),
                1,
                1,
            )
            .await?,
        )
    }

    async fn cleanup(self) {
        self.admin.close().await;
        sqlx::query(&format!(
            "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {}, {}",
            self.owner, self.definer
        ))
        .execute(&self.cluster_admin)
        .await
        .expect("remove disposable definer function grant");
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
async fn postgres_attached_keypair_lease_validates_both_pools_and_contract() {
    static CONTROL_CATALOG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _serial = CONTROL_CATALOG_LOCK.lock().await;
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping PostgreSQL keypair-slot attachment test: test URL not set");
        return;
    };
    let fixture = Fixture::new(url).await;
    let result = async {
        let (owner_database, config, owner_identity, configured_pool, generation) =
            fixture.owner_prepare().await?;
        let database = fixture
            .runtime_database(config.clone(), &fixture.target)
            .await?;
        match PostgresKeypairSlotLease::initialize_owner(database.clone()).await {
            Err(LeaseError::OwnerBootstrapRequiresOwnerHandle) => {}
            Err(error) => bail!("runtime owner bootstrap had wrong error: {error}"),
            Ok(_) => bail!("attested runtime unexpectedly initialized owner schema"),
        }
        match PostgresKeypairSlotLease::new(database.clone())
            .acquire(&ident(), 1, Duration::from_secs(30))
            .await
        {
            Err(LeaseError::RuntimeAttachRequired) => {}
            Err(error) => bail!("unverified runtime operation had wrong error: {error}"),
            Ok(_) => bail!("unverified runtime operation unexpectedly succeeded"),
        }

        match PostgresKeypairSlotLease::attach_existing(database.clone(), &configured_pool).await {
            Err(LeaseAttachError::KeypoolGenerationPending) => {}
            Err(error) => bail!("pending generation had wrong error: {error}"),
            Ok(_) => bail!("pending keypool generation unexpectedly attached"),
        }
        PostgresKeypairSlotLease::activate_keypool_owner(
            &owner_database,
            &owner_identity,
            generation,
        )
        .await?;

        let immutable_map = sqlx::query(&format!(
            "UPDATE {}.clustering_keypool_slots SET peer_id = peer_id \
             WHERE generation = '{}' AND slot_index = 0",
            fixture.target, generation
        ))
        .execute(&fixture.admin)
        .await;
        if immutable_map.is_ok() {
            bail!("active owner-seeded keypair mapping was mutable");
        }
        let generation_rewind = sqlx::query(&format!(
            "UPDATE {}.clustering_keypool SET state = 'pending' \
             WHERE singleton AND generation = '{}'",
            fixture.target, generation
        ))
        .execute(&fixture.admin)
        .await;
        if generation_rewind.is_ok() {
            bail!("active keypair generation could return to pending");
        }
        let truncate_map = sqlx::query(&format!(
            "TRUNCATE {}.clustering_keypool_slots",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await;
        if truncate_map.is_ok() {
            bail!("active owner-seeded keypair mapping could be truncated");
        }

        let mut reverse = configured_pool.clone();
        reverse.reverse();
        let second_node_database = fixture
            .runtime_database(config.clone(), &fixture.target)
            .await?;
        match PostgresKeypairSlotLease::attach_existing(second_node_database, &reverse).await {
            Err(LeaseAttachError::KeypairPoolOrderMismatch) => {}
            Err(error) => bail!("reversed keypool order had wrong error: {error}"),
            Ok(_) => bail!("reversed keypool order unexpectedly attached"),
        }
        let no_rows: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {}.clustering_keypair_slots",
            fixture.target
        ))
        .fetch_one(&fixture.admin)
        .await?;
        if no_rows != 0 {
            bail!("configuration refusal happened after slot acquisition");
        }

        let lease =
            PostgresKeypairSlotLease::attach_existing(database.clone(), &configured_pool).await?;
        let runtime_sql = PgPool::connect_with(
            PgConnectOptions::from_str(&keypair_attach_url(
                &fixture.base_url,
                &fixture.database,
                &format!("-c search_path={}", fixture.target),
            ))?
            .username(&fixture.runtime),
        )
        .await?;
        let legacy_insert = sqlx::query(&format!(
            "INSERT INTO {}.clustering_keypair_slots \
             (slot_index, holder_node, holder_epoch, heartbeat, expired) \
             VALUES (0, 'old-node', 'old-epoch', now(), false)",
            fixture.target
        ))
        .execute(&runtime_sql)
        .await;
        if legacy_insert.is_ok() {
            bail!("legacy writer insert unexpectedly succeeded");
        }

        let identity = ident();
        match lease.acquire(&identity, 1, Duration::from_secs(30)).await {
            Err(LeaseError::InvalidKeypairPool) => {}
            Err(error) => bail!("wrong keypool size had wrong error: {error}"),
            Ok(_) => bail!("wrong keypool size unexpectedly acquired a slot"),
        }
        let slot = lease.acquire(&identity, 2, Duration::from_secs(30)).await?;
        let legacy_heartbeat = sqlx::query(&format!(
            "UPDATE {}.clustering_keypair_slots SET heartbeat = now() WHERE slot_index = $1",
            fixture.target
        ))
        .bind(i64::from(slot.slot_index))
        .execute(&runtime_sql)
        .await;
        if legacy_heartbeat.is_ok() {
            bail!("legacy heartbeat SQL unexpectedly bypassed the enrollment revision guard");
        }
        lease
            .heartbeat(&identity, slot, Duration::from_secs(30))
            .await?;
        lease.release(&identity, slot).await?;
        let delete_slot = sqlx::query(&format!(
            "DELETE FROM {}.clustering_keypair_slots WHERE slot_index = $1",
            fixture.target
        ))
        .bind(i64::from(slot.slot_index))
        .execute(&fixture.admin)
        .await;
        if delete_slot.is_ok() {
            bail!("durable keypair slot row could be deleted while active");
        }
        runtime_sql.close().await;

        let invalid_control = fixture.runtime_database(config.clone(), "public").await?;
        if PostgresKeypairSlotLease::attach_existing(invalid_control, &configured_pool)
            .await
            .is_ok()
        {
            bail!("a control-plane pool with the wrong active schema was accepted");
        }

        sqlx::query(&format!(
            "GRANT DELETE ON {}.clustering_keypair_slots TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await?;
        match PostgresKeypairSlotLease::attach_existing(database.clone(), &configured_pool).await {
            Err(LeaseAttachError::UnsafeRuntimeRole) => {}
            Err(error) => bail!("unsafe runtime DELETE grant had wrong error: {error}"),
            Ok(_) => bail!("runtime DELETE grant was accepted"),
        }
        sqlx::query(&format!(
            "REVOKE DELETE ON {}.clustering_keypair_slots FROM {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await?;

        sqlx::query(&format!(
            "ALTER TABLE {}.clustering_keypair_slots \
             DROP CONSTRAINT clustering_keypair_slots_pkey",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await?;
        match PostgresKeypairSlotLease::attach_existing(database.clone(), &configured_pool).await {
            Err(LeaseAttachError::InvalidSchemaShape) => {}
            Err(error) => bail!("missing keypair-slot PK had wrong error: {error}"),
            Ok(_) => bail!("missing keypair-slot PK was accepted"),
        }
        sqlx::query(&format!(
            "ALTER TABLE {}.clustering_keypair_slots \
             ADD CONSTRAINT clustering_keypair_slots_pkey PRIMARY KEY (slot_index)",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await?;

        sqlx::query(&format!(
            "ALTER TABLE {}.clustering_keypool_slots \
             DROP CONSTRAINT clustering_keypool_slots_generation_peer_id_key",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await?;
        match PostgresKeypairSlotLease::attach_existing(database.clone(), &configured_pool).await {
            Err(LeaseAttachError::InvalidKeypairPoolShape) => {}
            Err(error) => bail!("missing keypool uniqueness index had wrong error: {error}"),
            Ok(_) => bail!("missing keypool uniqueness index was accepted"),
        }
        sqlx::query(&format!(
            "ALTER TABLE {}.clustering_keypool_slots \
             ADD CONSTRAINT clustering_keypool_slots_generation_peer_id_key \
             UNIQUE (generation, peer_id)",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await?;

        drop(lease);
        drop(database);
        drop(owner_database);
        Ok::<(), anyhow::Error>(())
    }
    .await;
    fixture.cleanup().await;
    result.expect("owner-prepared PostgreSQL keypair-slot attach contract");
}

fn ident() -> LeaseIdentity {
    LeaseIdentity {
        node_id: NodeId::generate(),
        node_epoch: uuid::Uuid::new_v4(),
    }
}

fn keypair_attach_url(base: &str, database: &str, options: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL test URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "dbname" && key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("dbname", database)
        .append_pair("options", options);
    url.to_string()
}

fn keypair_attach_url_with_role(base: &str, database: &str, role: &str, schema: &str) -> String {
    keypair_attach_url(
        base,
        database,
        &format!("-c role={role} -c search_path={schema}"),
    )
}
