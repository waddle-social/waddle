//! Closed, disposable experiment for a relation-guarded ingress transaction.
//!
//! The only exported operation creates its own database and roles, installs
//! the candidate before creating a runtime pool, exercises the chain, and
//! destroys the fixture. It never returns the marker, pool, or transaction.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use chrono::Utc;
use jid::BareJid;
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_extensions::{
    ConfiguredRoomObserver, ExtensionPayload, ObservationGeneration, PluginId,
    RoomObservationOutcome, RoomObservationResult, RoomObservationScope,
    RoomObservationSubscription, Sha256Digest, XmlElement,
};
use waddle_xmpp::ingress::{
    AliasOutcome, AliasResolution, DeliveryKey, EffectMessageIdentity, IngressEffectIntent,
    IngressOrdinal, MessageKey, NormalizedTarget, ProtocolEpoch, SemanticDigest, WireHandledCount,
};
use waddle_xmpp::mam::ArchiveOrdinal;
#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::ClaimStore;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp_core::xep0359::OriginId;
use waddle_xmpp_core::xep0359::{add_origin_id, add_stanza_id, StanzaId};
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

use crate::config::LineageConfig;
use crate::db::lineage::{self, AttestedLineage, DeploymentUuid, PgSchemaName};
use crate::db::physical_attestation::{
    attested_pool, install, AttestedPgPool, PgRoleName, PhysicalAttestationConfig,
};
use crate::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use crate::ingress_uow::{
    ArchiveDispatchObligation, ArchiveDispatchRepository, CandidateTransaction,
    CanonicalMessageRepository, CapturedRoomSource, DeliveryEffectRepository, DispatchTarget,
    EffectDescendantRepository, EffectIntentRepository, EffectReceiptRepository, IngressLineage,
    IngressUnitOfWork, IngressUowError, IngressUowTransaction, ReconcileVerdict,
    RelationGuardCapability, RelationGuardCapabilityError, RoomObservationRepository,
    SmIngressRepository, SmIngressStreamRepository,
};

const GUARD: &str =
    include_str!("../db/migrations/owner/ingress_epoch_relation_guard_candidate.sql");

#[derive(Debug, thiserror::Error)]
enum LabWriteError {
    #[error("candidate transaction preparation failed")]
    Prepare(#[source] anyhow::Error),
    #[error(transparent)]
    Operation(#[from] IngressUowError),
    #[error("candidate transaction timed out before COMMIT")]
    Timeout,
    #[error("candidate COMMIT outcome is indeterminate")]
    CommitIndeterminate,
}

/// Only the disposable fixture can construct this proof. It binds its
/// attested pool and the exact reserved ingress admission lane together.
pub(crate) struct RelationGuardLabToken {
    ingress: Database,
    capability: RelationGuardCapability,
}

impl RelationGuardLabToken {
    pub(crate) fn capability(&self) -> &RelationGuardCapability {
        &self.capability
    }

    pub(crate) fn permits(&self, database: &Database) -> bool {
        let Some(writer) = database.attested_pg_writer_capability() else {
            return false;
        };
        database.shares_ingress_lane(&self.ingress)
            && self
                .capability
                .verify_for_database(database, &writer)
                .is_ok()
    }
}

struct LabLane {
    main: Database,
    ingress: Database,
    verifier: IngressUnitOfWork,
    uow: IngressUnitOfWork,
    /// Present only when the lab installed the unregistered candidate guard
    /// and its owner marker. A lab on the registered guard has neither, so
    /// there is no candidate capability to verify.
    fixture: Option<RelationGuardLabToken>,
}

impl LabLane {
    async fn new(
        pool: AttestedPgPool,
        lineage: &AttestedLineage,
        candidate_guard: bool,
    ) -> anyhow::Result<Self> {
        let main =
            Database::from_attested_postgres_pool_with_ingress("relation-guard-lab", pool, 2, 2)
                .await
                .context("construct attested main and ingress lanes")?;
        let ingress = main.ingress_lane();
        let lineage_config = LineageConfig {
            deployment_uuid: Some(lineage.deployment_uuid),
            action: None,
        };
        let verifier = IngressUnitOfWork::open(main.clone(), lineage_config.clone())?;
        let fixture = if candidate_guard {
            let capability = verifier
                .verify_relation_guard_capability()
                .await
                .context("verify relation-guard candidate")?;
            Some(RelationGuardLabToken {
                ingress: ingress.clone(),
                capability,
            })
        } else {
            None
        };
        let uow = IngressUnitOfWork::open(ingress.clone(), lineage_config)?;
        Ok(Self {
            main,
            ingress,
            verifier,
            uow,
            fixture,
        })
    }

    fn candidate(&self) -> anyhow::Result<&RelationGuardLabToken> {
        self.fixture
            .as_ref()
            .context("lab was created without the candidate relation guard")
    }
}

struct Lab {
    resources: Option<Provisioning>,
    cluster: PgPool,
    admin: PgPool,
    owner_options: PgConnectOptions,
    runtime_options: PgConnectOptions,
    lane: LabLane,
    database_name: String,
    owner: String,
    migrator: String,
    runtime: String,
    installer: String,
    definer: String,
    lineage: AttestedLineage,
    config: PhysicalAttestationConfig,
    password: String,
    application: String,
}

/// Track candidate objects with ownership markers, including CREATE operations
/// whose acknowledgement may be lost. Never drop a same-named unrelated role.
struct Provisioning {
    cluster: PgPool,
    database: Option<(String, String)>,
    roles: Vec<(String, String)>,
}

impl Provisioning {
    async fn create_role(&mut self, name: &str, clause: &str) -> anyhow::Result<()> {
        let marker = format!("waddle-relation-lab-{}", Uuid::new_v4());
        self.roles.push((name.to_owned(), marker.clone()));
        // The marker commits atomically with CREATE ROLE. Even a lost COMMIT
        // reply can then be reconciled without dropping a pre-existing role.
        let mut connection = lock_provisioning_object(&self.cluster, name).await?;
        let mut transaction = connection.begin().await?;
        sqlx::query(&format!("CREATE ROLE {name} {clause}"))
            .execute(&mut *transaction)
            .await?;
        sqlx::query(&format!("COMMENT ON ROLE {name} IS '{marker}'"))
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        connection.close().await?;
        Ok(())
    }

    async fn create_database(&mut self, name: &str, owner: &str) -> anyhow::Result<()> {
        // CREATE DATABASE is not transactional. Record the candidate first;
        // cleanup checks its owner against our marked role before dropping it.
        self.database = Some((name.to_owned(), owner.to_owned()));
        let mut connection = lock_provisioning_object(&self.cluster, name).await?;
        sqlx::query(&format!("CREATE DATABASE {name} OWNER {owner}"))
            .execute(&mut connection)
            .await?;
        connection.close().await?;
        Ok(())
    }

    async fn cleanup(&mut self) -> anyhow::Result<()> {
        cleanup_objects(&self.cluster, &mut self.database, &mut self.roles).await
    }
}

async fn cleanup_objects(
    cluster: &PgPool,
    database: &mut Option<(String, String)>,
    roles: &mut Vec<(String, String)>,
) -> anyhow::Result<()> {
    if let Some((name, owner)) = database.as_ref() {
        let mut connection = lock_provisioning_object(cluster, name).await?;
        let owned: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_database d JOIN pg_catalog.pg_roles r ON r.oid = d.datdba WHERE d.datname = $1 AND r.rolname = $2)")
            .bind(name).bind(owner).fetch_one(&mut connection).await?;
        let owner_marked = roles.iter().find(|(role, _)| role == owner);
        if owned {
            let (_, marker) = owner_marked.context("database owner is not tracked")?;
            ensure!(
                role_has_marker(&mut connection, owner, marker).await?,
                "database owner no longer has the fixture marker"
            );
            sqlx::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
                .execute(&mut connection)
                .await?;
        }
        connection.close().await?;
        *database = None;
    }
    while let Some((name, marker)) = roles.last() {
        let mut connection = lock_provisioning_object(cluster, name).await?;
        if role_has_marker(&mut connection, name, marker).await? {
            sqlx::query(&format!("DROP ROLE {name}"))
                .execute(&mut connection)
                .await?;
        }
        connection.close().await?;
        roles.pop();
    }
    Ok(())
}

impl Drop for Provisioning {
    fn drop(&mut self) {
        if self.database.is_none() && self.roles.is_empty() {
            return;
        }
        // A cancelled fixture setup cannot await cleanup in Drop. Retain the
        // exact created-object list and finish cleanup on the current runtime.
        let cluster = self.cluster.clone();
        let mut database = self.database.take();
        let mut roles = std::mem::take(&mut self.roles);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = cleanup_objects(&cluster, &mut database, &mut roles).await;
            });
        }
    }
}

async fn lock_provisioning_object(cluster: &PgPool, name: &str) -> anyhow::Result<PgConnection> {
    let options = cluster.connect_options();
    let mut connection = PgConnection::connect_with(&options).await?;
    sqlx::query("SET lock_timeout = '5s'")
        .execute(&mut connection)
        .await?;
    sqlx::query("SET statement_timeout = '10s'")
        .execute(&mut connection)
        .await?;
    sqlx::query("SELECT pg_catalog.pg_advisory_lock(pg_catalog.hashtextextended($1, 2046))")
        .bind(name)
        .execute(&mut connection)
        .await
        .context("provisioning outcome remains indeterminate while its lock is held")?;
    Ok(connection)
}

async fn role_has_marker(
    connection: &mut PgConnection,
    name: &str,
    marker: &str,
) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = $1 AND pg_catalog.shobj_description(oid, 'pg_authid') = $2)")
        .bind(name).bind(marker).fetch_one(connection).await?)
}

#[cfg(feature = "clustering")]
mod room_archive;
#[cfg(feature = "clustering")]
pub use room_archive::verify_recorded_room_archive;

impl Lab {
    async fn create(admin_url: &str) -> anyhow::Result<Self> {
        Self::create_with_guard(admin_url, true).await
    }

    async fn create_with_guard(admin_url: &str, candidate_guard: bool) -> anyhow::Result<Self> {
        let cluster = PgPool::connect(admin_url).await?;
        let mut resources = Provisioning {
            cluster: cluster.clone(),
            database: None,
            roles: Vec::new(),
        };
        let result: anyhow::Result<Self> = async {
        let suffix = Uuid::new_v4().simple().to_string();
        let database_name = format!("relation_lab_{suffix}");
        let owner = format!("relation_owner_{suffix}");
        let migrator = format!("relation_migrator_{suffix}");
        let runtime = format!("relation_runtime_{suffix}");
        let installer = format!("relation_installer_{suffix}");
        let definer = format!("relation_definer_{suffix}");
        let application = format!("relation_writer_{suffix}");
        let password = format!("test_{}", Uuid::new_v4().simple());
        let attributes = "NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS";
        for role in [&owner, &definer] {
            resources.create_role(role, &format!("NOLOGIN {attributes}")).await?;
        }
        for role in [&migrator, &runtime, &installer] {
            resources.create_role(role, &format!("LOGIN PASSWORD '{password}' {attributes}")).await?;
        }
        for statement in [
            format!("GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
            format!("GRANT {definer} TO {installer} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
        ] {
            sqlx::query(&statement).execute(&cluster).await?;
        }
        resources.create_database(&database_name, &owner).await?;
        for statement in [
            format!("REVOKE CONNECT, TEMPORARY ON DATABASE {database_name} FROM PUBLIC"),
            format!("GRANT CONNECT ON DATABASE {database_name} TO {migrator}, {runtime}, {installer}"),
        ] {
            sqlx::query(&statement).execute(&cluster).await?;
        }

        let base = PgConnectOptions::from_str(admin_url)?;
        let admin_options = base.clone().database(&database_name);
        let owner_options = base.clone().database(&database_name).username(&migrator)
            .password(&password).options([("role", owner.as_str()), ("search_path", "guarded")]);
        let runtime_options = base.clone().database(&database_name).username(&runtime)
            .password(&password).application_name(&application).options([
                ("search_path", "guarded"),
                ("lock_timeout", "2500ms"),
                ("statement_timeout", "4500ms"),
            ]);
        let installer_options = base.database(&database_name).username(&installer)
            .password(&password).options([("search_path", "guarded")]);
        let admin = sqlx::postgres::PgPoolOptions::new().max_connections(4)
            .connect_with(admin_options).await?;
        for statement in [
            "REVOKE ALL ON SCHEMA public FROM PUBLIC".to_owned(),
            format!("CREATE SCHEMA guarded AUTHORIZATION {owner}"),
            format!("CREATE SCHEMA attestor AUTHORIZATION {definer}"),
            format!("GRANT USAGE ON SCHEMA guarded TO {runtime}, {definer}"),
            "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC".to_owned(),
            format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}, {definer}"),
        ] {
            sqlx::query(&statement).execute(&admin).await?;
        }

        let migration_url = migration_url(admin_url, &database_name, &migrator, &password, &owner)?;
        let owner_db = Database::from_config("relation-lab-owner", &DatabaseConfig::new(DatabaseDriver::Postgres, migration_url.clone())).await?;
        let lineage = lineage::enroll(&owner_db, &LineageConfig {
            deployment_uuid: Some(DeploymentUuid(Uuid::new_v4())),
            action: None,
        }).await?;
        MigrationRunner::single().run(&owner_db).await?;
        crate::ingress_uow::initialize_room_observations(&owner_db).await?;
        #[cfg(feature = "clustering")]
        if !candidate_guard {
            crate::clustering::claims::PostgresClaimStore::new(owner_db.clone()).ensure_schema().await?;
            waddle_xmpp::mam::SqlxMamStorage::open(&migration_url).await?;
        }
        drop(owner_db);

        let mut owner_connection = PgConnection::connect_with(&owner_options).await?;
        let latest: i64 = sqlx::query_scalar("SELECT max(version) FROM guarded._migrations")
            .fetch_one(&mut owner_connection).await?;
        ensure!(latest == 1034, "expected catalog through V1034, got {latest}");
        if candidate_guard {
            sqlx::raw_sql(GUARD).execute(&mut owner_connection).await?;
            sqlx::query("CREATE TABLE guarded.waddle_epoch_transition_lab_marker (version TEXT PRIMARY KEY, checksum TEXT NOT NULL, attempt_id UUID NOT NULL)")
                .execute(&mut owner_connection).await?;
            sqlx::query("INSERT INTO guarded.waddle_epoch_transition_lab_marker (version, checksum, attempt_id) VALUES ($1, $2, $3)")
                .bind("relation-guard-lab-1")
                .bind(hex::encode(Sha256::digest(GUARD.as_bytes())))
                .bind(Uuid::new_v4())
                .execute(&mut owner_connection).await?;
        }
        for statement in [
            format!("GRANT SELECT ON guarded._lineage TO {definer}"),
            format!("GRANT SELECT ON guarded.ingress_protocol_epoch, guarded.ingress_epoch_guard_manifest TO {runtime}"),
            format!("GRANT SELECT ON guarded._migrations TO {runtime}"),
            format!("GRANT SELECT, INSERT, UPDATE, DELETE ON guarded.ingress_messages, guarded.ingress_sm_streams, guarded.ingress_send_attempts, guarded.ingress_room_requests, guarded.ingress_room_occupants, guarded.ingress_direct_requests, guarded.sm_durable_sessions, guarded.account_resource_bindings TO {runtime}"),
            format!("GRANT SELECT, INSERT, DELETE ON guarded.ingress_origin_aliases, guarded.ingress_sm_refs, guarded.ingress_deliveries, guarded.sm_durable_operations, guarded.account_resource_operations TO {runtime}"),
            format!("GRANT SELECT, INSERT, UPDATE ON guarded.ingress_effect_intents TO {runtime}"),
            format!("GRANT SELECT, INSERT, UPDATE ON guarded.ingress_effect_descendants, guarded.ingress_mailbox_lanes, guarded.ingress_mailbox_entries TO {runtime}"),
            format!("GRANT SELECT, INSERT ON guarded.ingress_effect_receipts, guarded.ingress_carbon_receipts, guarded.ingress_delivery_receipts, guarded.ingress_archive_dispatch, guarded.ingress_owner_rejections, guarded.ingress_room_responses TO {runtime}"),
            format!("GRANT SELECT, INSERT, UPDATE ON extension_room_observers, extension_room_sources, extension_room_source_revisions, extension_room_observation_work, extension_room_publications TO {runtime}"),
            format!("GRANT SELECT, INSERT ON extension_room_observation_receipts TO {runtime}"),
        ] {
            sqlx::query(&statement).execute(&mut owner_connection).await?;
        }
        if candidate_guard {
            sqlx::query(&format!(
                "GRANT SELECT ON guarded.waddle_epoch_transition_lab_marker TO {runtime}"
            ))
            .execute(&mut owner_connection)
            .await?;
        }
        #[cfg(feature = "clustering")]
        if !candidate_guard {
            // The closed room-archive writer holds column-level UPDATE only,
            // so its rollback stages can revoke exactly the privilege a
            // statement needs. Narrow the table-level grants shared with the
            // candidate lab before granting the columns.
            for statement in [
                format!("GRANT UPDATE (id) ON guarded.ingress_protocol_epoch TO {runtime}"),
                format!("REVOKE UPDATE ON guarded.ingress_messages FROM {runtime}"),
                format!("GRANT UPDATE (terminal_at, retention_eligible_at) ON guarded.ingress_messages TO {runtime}"),
                format!("GRANT SELECT, UPDATE (entity_type) ON guarded.clustering_claims TO {runtime}"),
                format!("REVOKE UPDATE ON guarded.ingress_effect_intents FROM {runtime}"),
                format!("GRANT UPDATE (payload) ON guarded.ingress_effect_intents TO {runtime}"),
                format!("GRANT SELECT, INSERT ON guarded.ingress_archive_dispatch TO {runtime}"),
                format!("GRANT SELECT, INSERT ON guarded.mam_messages TO {runtime}"),
                format!("GRANT SELECT, INSERT, UPDATE ON guarded.mam_archive_sequences TO {runtime}"),
            ] {
                sqlx::query(&statement).execute(&mut owner_connection).await?;
            }
            sqlx::query(&format!("CREATE SCHEMA shadow AUTHORIZATION {owner}"))
                .execute(&mut owner_connection).await?;
            sqlx::query(&format!("GRANT USAGE ON SCHEMA shadow TO {runtime}"))
                .execute(&mut owner_connection).await?;
            for table in ["clustering_claims", "ingress_messages", "ingress_effect_intents", "ingress_effect_receipts", "ingress_archive_dispatch", "mam_messages", "mam_archive_sequences"] {
                sqlx::query(&format!("CREATE TABLE shadow.{table} (LIKE guarded.{table} INCLUDING ALL)"))
                    .execute(&mut owner_connection).await?;
                sqlx::query(&format!("GRANT SELECT, INSERT, UPDATE ON shadow.{table} TO {runtime}"))
                    .execute(&mut owner_connection).await?;
            }
        }
        drop(owner_connection);

        let config = PhysicalAttestationConfig::new(
            lineage.clone(), PgRoleName::new(installer.clone())?, PgRoleName::new(runtime.clone())?,
            PgRoleName::new(definer.clone())?, PgSchemaName("attestor".to_owned()),
        )?;
        let config = install(&installer_options, config, Uuid::new_v4()).await?;
        let attested = attested_pool(runtime_options.clone(), config.clone(), 4, Duration::from_secs(5))?;
        let lane = LabLane::new(attested, &lineage, candidate_guard)
            .await
            .context("initialize attested relation-guard lane")?;
        Ok(Self { resources: None, cluster: cluster.clone(), admin, owner_options, runtime_options, lane, database_name,
            owner, migrator, runtime, installer, definer, lineage, config, password, application })
        }.await;
        match result {
            Ok(mut lab) => {
                lab.resources = Some(resources);
                Ok(lab)
            }
            Err(error) => match resources.cleanup().await {
                Ok(()) => Err(error),
                Err(cleanup) => Err(error.context(format!("fixture cleanup failed: {cleanup}"))),
            },
        }
    }

    async fn begin(&self) -> anyhow::Result<CandidateTransaction<'_>> {
        self.lane
            .uow
            .begin_in_relation_guard_lab(self.lane.candidate()?)
            .await
            .context("relation-lab UOW begin")
    }

    async fn write(
        &self,
        key: MessageKey,
        digest: &SemanticDigest,
        intent: &IngressEffectIntent,
        descendant: Uuid,
    ) -> Result<ProtocolEpoch, LabWriteError> {
        self.write_until(
            key,
            digest,
            intent,
            descendant,
            tokio::time::Instant::now() + Duration::from_secs(8),
        )
        .await
    }

    async fn write_until(
        &self,
        key: MessageKey,
        digest: &SemanticDigest,
        intent: &IngressEffectIntent,
        descendant: Uuid,
        deadline: tokio::time::Instant,
    ) -> Result<ProtocolEpoch, LabWriteError> {
        let mut candidate = tokio::time::timeout_at(deadline, self.begin())
            .await
            .map_err(|_| LabWriteError::Timeout)?
            .map_err(LabWriteError::Prepare)?;
        let epoch = candidate.protocol_epoch();
        tokio::time::timeout_at(
            deadline,
            write_chain(&mut candidate, key, digest, intent, descendant),
        )
        .await
        .map_err(|_| LabWriteError::Timeout)??;
        // Arm the fault-injection proxy only after all domain writes and
        // immediately before this transaction's business COMMIT.
        let armed = tokio::time::timeout_at(deadline, candidate.arm_fixture_commit_fault())
            .await
            .map_err(|_| LabWriteError::Timeout)?;
        armed.map_err(LabWriteError::Operation)?;
        tokio::time::timeout_at(deadline, candidate.commit())
            .await
            .map_err(|_| LabWriteError::CommitIndeterminate)?
            .map_err(|_| LabWriteError::CommitIndeterminate)?;
        Ok(epoch)
    }

    async fn cleanup(self) -> anyhow::Result<()> {
        let Self {
            cluster,
            admin,
            lane,
            mut resources,
            ..
        } = self;
        drop(lane);
        admin.close().await;
        resources
            .as_mut()
            .context("fixture provisioning record missing")?
            .cleanup()
            .await?;
        cluster.close().await;
        Ok(())
    }
}

async fn write_chain(
    candidate: &mut IngressUowTransaction<'_>,
    key: MessageKey,
    digest: &SemanticDigest,
    intent: &IngressEffectIntent,
    descendant: Uuid,
) -> Result<(), IngressUowError> {
    let existing = CanonicalMessageRepository::lock(candidate, key).await?;
    CanonicalMessageRepository::record_message(candidate, key, digest, None).await?;
    let verdict =
        EffectIntentRepository::reconcile(candidate, key, std::slice::from_ref(intent), existing)
            .await?;
    if matches!(
        verdict,
        ReconcileVerdict::Contradiction { .. } | ReconcileVerdict::Divergent { .. }
    ) {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let receipt = crate::ingress::receipt_key(intent)?;
    EffectReceiptRepository::record_receipt(
        candidate,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await?;
    EffectDescendantRepository::attach(candidate, key, &intent.semantic_key(), descendant).await?;
    Ok(())
}

async fn write_uow_chain(
    uow: &IngressUnitOfWork,
    capability: &RelationGuardCapability,
    key: MessageKey,
    digest: &SemanticDigest,
    intent: &IngressEffectIntent,
    descendant: Uuid,
) -> Result<ProtocolEpoch, IngressUowError> {
    let mut transaction = uow.begin_with_relation_guard(capability).await?;
    let epoch = transaction.protocol_epoch();
    write_chain(&mut transaction, key, digest, intent, descendant).await?;
    transaction.commit().await?;
    Ok(epoch)
}

fn migration_url(
    url: &str,
    database: &str,
    migrator: &str,
    password: &str,
    owner: &str,
) -> anyhow::Result<String> {
    let mut parsed = url::Url::parse(url)?;
    let values: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| !matches!(key.as_ref(), "user" | "password" | "dbname" | "options"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    parsed
        .query_pairs_mut()
        .clear()
        .extend_pairs(values.iter().map(|(key, value)| (key, value)))
        .append_pair("dbname", database)
        .append_pair("user", migrator)
        .append_pair("password", password)
        .append_pair(
            "options",
            &format!("-c role={owner} -c search_path=guarded"),
        );
    Ok(parsed.to_string())
}

/// Run the candidate only in a freshly created disposable PostgreSQL database.
pub async fn verify_disposable_relation_guard_chain(admin_url: &str) -> anyhow::Result<()> {
    use futures::FutureExt;

    let mut lab = Lab::create(admin_url).await?;
    let result = std::panic::AssertUnwindSafe(exercise(&mut lab))
        .catch_unwind()
        .await
        .unwrap_or_else(|payload| {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            Err(anyhow::anyhow!("candidate fixture panicked: {message}"))
        });
    let cleanup = lab.cleanup().await;
    match (result, cleanup) {
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("fixture cleanup failed: {cleanup}")))
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn make_intent(fanout: bool) -> anyhow::Result<IngressEffectIntent> {
    Ok(IngressEffectIntent::RouteDirect {
        prepared: None,
        recipient: "candidate@example.test".parse()?,
        fanout: if fanout {
            vec!["candidate@example.test/device".parse()?]
        } else {
            vec![]
        },
        route_identity: EffectMessageIdentity::capture_ordinal(1),
    })
}

async fn counts(lab: &Lab, key: MessageKey) -> anyhow::Result<Vec<i64>> {
    let mut result = Vec::new();
    for table in [
        "ingress_messages",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_deliveries",
        "ingress_effect_descendants",
    ] {
        let sql = format!("SELECT count(*) FROM guarded.{table} WHERE message_key = $1");
        result.push(
            sqlx::query_scalar(&sql)
                .bind(key.to_storage())
                .fetch_one(&lab.admin)
                .await?,
        );
    }
    Ok(result)
}

async fn wait_for_counts(lab: &Lab, key: MessageKey, expected: &[i64]) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let no_candidate_lock: bool = sqlx::query_scalar(
                "SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_locks l JOIN pg_catalog.pg_stat_activity a ON a.pid = l.pid \
                 WHERE a.application_name = $1 AND l.relation = 'guarded.ingress_protocol_epoch'::regclass AND l.granted)"
            ).bind(&lab.application).fetch_one(&lab.admin).await?;
            if no_candidate_lock && counts(lab, key).await? == expected { return Ok(()); }
            tokio::task::yield_now().await;
        }
    }).await.context("timed out waiting for transaction rollback")?
}

async fn role_boundary(lab: &Lab) -> anyhow::Result<()> {
    let mut runtime = PgConnection::connect_with(&lab.runtime_options).await?;
    let restricted: bool = sqlx::query_scalar(
        "SELECT session_user::text = $1 AND current_user::text = $1 \
         AND NOT pg_catalog.pg_has_role(current_user, $2, 'MEMBER') \
         AND NOT pg_catalog.pg_has_role(current_user, $3, 'MEMBER') \
         AND NOT pg_catalog.pg_has_role(current_user, $2, 'SET') \
         AND NOT pg_catalog.pg_has_role(current_user, $3, 'SET') \
         AND NOT pg_catalog.pg_has_role(current_user, $4, 'MEMBER') \
         AND NOT pg_catalog.pg_has_role(current_user, $5, 'MEMBER') \
         AND NOT pg_catalog.pg_has_role(current_user, $4, 'SET') \
         AND NOT pg_catalog.pg_has_role(current_user, $5, 'SET') \
         AND NOT pg_catalog.has_database_privilege(current_user, current_database(), 'TEMP') \
         AND NOT pg_catalog.has_database_privilege(current_user, current_database(), 'CREATE') \
         AND NOT pg_catalog.has_schema_privilege(current_user, 'guarded', 'CREATE') \
         AND NOT pg_catalog.has_any_column_privilege(current_user, 'guarded.ingress_protocol_epoch', 'UPDATE') \
         AND NOT pg_catalog.has_table_privilege(current_user, 'guarded.ingress_protocol_epoch', 'INSERT,DELETE,TRUNCATE,TRIGGER') \
         AND NOT (SELECT rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls FROM pg_catalog.pg_roles WHERE rolname = current_user)"
    ).bind(&lab.runtime).bind(&lab.owner).bind(&lab.migrator).bind(&lab.installer).bind(&lab.definer).fetch_one(&mut runtime).await?;
    ensure!(
        restricted,
        "runtime role gained authority over the epoch boundary"
    );
    let owner: String = sqlx::query_scalar("SELECT pg_catalog.pg_get_userbyid(relowner) FROM pg_catalog.pg_class WHERE oid = 'guarded.ingress_protocol_epoch'::regclass")
        .fetch_one(&mut runtime).await?;
    ensure!(owner == lab.owner, "unexpected epoch owner");
    let database_owner: String = sqlx::query_scalar("SELECT pg_catalog.pg_get_userbyid(datdba) FROM pg_catalog.pg_database WHERE datname = current_database()")
        .fetch_one(&mut runtime).await?;
    let schema_owner: String = sqlx::query_scalar("SELECT pg_catalog.pg_get_userbyid(nspowner) FROM pg_catalog.pg_namespace WHERE nspname = 'guarded'")
        .fetch_one(&mut runtime).await?;
    ensure!(
        database_owner == lab.owner && schema_owner == lab.owner,
        "unexpected database or schema owner"
    );
    for table in [
        "ingress_messages",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_deliveries",
        "ingress_effect_descendants",
        "ingress_owner_rejections",
    ] {
        let guards: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_trigger WHERE tgrelid = pg_catalog.to_regclass(pg_catalog.format('guarded.%I', $1)) AND NOT tgisinternal AND tgenabled = 'A' AND ((tgname = $1 || '_epoch_guard_dml' AND tgfoid = 'guarded.waddle_ingress_epoch_guard()'::regprocedure AND tgtype = 30) OR (tgname = $1 || '_epoch_guard_truncate' AND tgfoid = 'guarded.waddle_ingress_truncate_guard()'::regprocedure AND tgtype = 34))")
            .bind(table).fetch_one(&mut runtime).await?;
        ensure!(guards == 2, "missing enabled guard on {table}");
    }
    let error = match sqlx::query(
        "SELECT epoch FROM guarded.ingress_protocol_epoch WHERE id = 1 FOR SHARE",
    )
    .fetch_one(&mut runtime)
    .await
    {
        Err(error) => error,
        Ok(_) => bail!("SELECT-only role unexpectedly row-locked epoch"),
    };
    ensure!(
        matches!(error, sqlx::Error::Database(ref e) if e.code().as_deref() == Some("42501")),
        "unexpected epoch row-lock error"
    );
    sqlx::query("BEGIN ISOLATION LEVEL READ COMMITTED")
        .execute(&mut runtime)
        .await?;
    sqlx::query("SELECT pg_catalog.set_config('waddle.protocol_epoch', '0', true), pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)")
        .execute(&mut runtime).await?;
    let error = match sqlx::query("INSERT INTO guarded.ingress_effect_receipts (message_key, kind, semantic_identity_hash) SELECT message_key, kind, semantic_identity_hash FROM guarded.ingress_effect_receipts WHERE false")
        .execute(&mut runtime).await {
        Err(error) => error,
        Ok(_) => bail!("proof unexpectedly replaced an epoch prelock"),
    };
    ensure!(
        matches!(error, sqlx::Error::Database(ref e) if e.code().as_deref() == Some("P0001") && e.message().contains("require an epoch ACCESS SHARE prelock")),
        "unexpected missing-prelock error: {error}"
    );
    sqlx::query("ROLLBACK").execute(&mut runtime).await?;
    for statement in [
        "CREATE TEMP TABLE forbidden (id integer)",
        "CREATE TABLE guarded.forbidden (id integer)",
        "ALTER TABLE guarded.ingress_messages DISABLE TRIGGER ALL",
        "UPDATE guarded.ingress_protocol_epoch SET epoch = epoch + 1 WHERE id = 1",
    ] {
        ensure!(
            sqlx::query(statement).execute(&mut runtime).await.is_err(),
            "runtime unexpectedly executed {statement}"
        );
    }
    for role in [&lab.owner, &lab.migrator, &lab.installer, &lab.definer] {
        ensure!(
            sqlx::query(&format!("SET ROLE {role}"))
                .execute(&mut runtime)
                .await
                .is_err(),
            "runtime could assume {role}"
        );
    }
    let mut candidate = lab.begin().await?;
    ensure!(
        matches!(candidate.lineage(), IngressLineage::Attested(lineage) if lineage == &lab.lineage),
        "the UoW did not retain the fixture's attested lineage"
    );
    ensure!(
        candidate.has_fixture_bounds_and_isolation().await?,
        "candidate preparation lost its READ COMMITTED epoch prelock proof or tight session timeouts"
    );
    drop(candidate);
    Ok(())
}

async fn ingress_lane_boundary(lab: &Lab) -> anyhow::Result<()> {
    ensure!(
        lab.lane.uow.begin().await.is_err(),
        "the installed-guard UoW unexpectedly bypassed its epoch row lock"
    );
    let first_held = lab.begin().await?;
    let second_held = lab.begin().await?;
    let main = lab.lane.main.guard().await?;
    tokio::time::timeout(Duration::from_secs(2), main.execute("SELECT 1", ())).await??;
    ensure!(
        tokio::time::timeout(Duration::from_millis(250), lab.begin())
            .await
            .is_err(),
        "a second UoW bypassed the occupied ingress admission lane"
    );
    drop(first_held);
    drop(second_held);
    let main_held = lab.lane.main.begin().await?;
    let main_held_second = lab.lane.main.begin().await?;
    let recovered = tokio::time::timeout(Duration::from_secs(5), lab.begin()).await??;
    drop(recovered);
    drop(main_held);
    drop(main_held_second);

    let wrong_lineage = IngressUnitOfWork::open(
        lab.lane.ingress.clone(),
        LineageConfig {
            deployment_uuid: Some(DeploymentUuid(Uuid::new_v4())),
            action: None,
        },
    )?;
    ensure!(
        matches!(
            wrong_lineage
                .begin_in_relation_guard_lab(lab.lane.candidate()?)
                .await,
            Err(IngressUowError::Lineage(_))
        ),
        "a UoW with the wrong deployment lineage was admitted"
    );
    Ok(())
}

async fn verifier_lane_boundary(lab: &Lab) -> anyhow::Result<()> {
    let lineage = LineageConfig {
        deployment_uuid: Some(lab.lineage.deployment_uuid),
        action: None,
    };

    let one_slot_pool = attested_pool(
        lab.runtime_options.clone(),
        lab.config.clone(),
        1,
        Duration::from_secs(5),
    )?;
    let one_slot = Database::from_attested_postgres_pool("one-slot-verifier", one_slot_pool);
    let one_slot_uow = IngressUnitOfWork::open(one_slot, lineage.clone())?;
    ensure!(
        matches!(
            one_slot_uow.verify_relation_guard_capability().await,
            Err(RelationGuardCapabilityError::VerifierLanesRequired)
        ),
        "a one-slot attested pool did not fail fast"
    );

    let two_slot_pool = attested_pool(
        lab.runtime_options.clone(),
        lab.config.clone(),
        2,
        Duration::from_secs(5),
    )?;
    let database = Database::from_attested_postgres_pool_with_ingress(
        "one-main-one-ingress-verifier",
        two_slot_pool,
        1,
        1,
    )
    .await?;
    let main_uow = IngressUnitOfWork::open(database.clone(), lineage.clone())?;
    let capability = main_uow.verify_relation_guard_capability().await?;
    let ingress_uow = IngressUnitOfWork::open(database.ingress_lane(), lineage)?;
    ensure!(
        matches!(
            ingress_uow.verify_relation_guard_capability().await,
            Err(RelationGuardCapabilityError::VerifierLanesRequired)
        ),
        "the verifier accepted an ingress-lane handle instead of the main handle"
    );
    let transaction = ingress_uow.begin_with_relation_guard(&capability).await?;
    ensure!(
        transaction.protocol_epoch() == ProtocolEpoch::ZERO
            && matches!(transaction.lineage(), IngressLineage::Attested(lineage) if lineage == &lab.lineage),
        "the 1+1 lane capability did not prepare the attested epoch-zero transaction"
    );
    drop(transaction);
    Ok(())
}

async fn proof_failures(
    lab: &Lab,
    digest: &SemanticDigest,
    intent: &IngressEffectIntent,
) -> anyhow::Result<()> {
    let mut owner = PgConnection::connect_with(&lab.owner_options).await?;
    sqlx::raw_sql("CREATE OR REPLACE FUNCTION guarded.lab_break_proof() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_catalog.set_config(TG_ARGV[0], TG_ARGV[1], true); RETURN NULL; END $$")
        .execute(&mut owner).await?;
    for (name, value) in [
        ("waddle.protocol_epoch", ""),
        ("waddle.protocol_epoch", "999"),
        ("waddle.protocol_epoch_xid", "0"),
    ] {
        let statement = format!(
            "CREATE TRIGGER \"000_lab_break_proof\" BEFORE INSERT ON guarded.ingress_effect_receipts FOR EACH STATEMENT EXECUTE FUNCTION guarded.lab_break_proof('{name}', '{value}')"
        );
        sqlx::query(&statement).execute(&mut owner).await?;
        let mut runtime = PgConnection::connect_with(&lab.runtime_options).await?;
        sqlx::query("BEGIN ISOLATION LEVEL READ COMMITTED")
            .execute(&mut runtime)
            .await?;
        sqlx::query("LOCK TABLE ONLY guarded.ingress_protocol_epoch IN ACCESS SHARE MODE")
            .execute(&mut runtime)
            .await?;
        sqlx::query("SELECT pg_catalog.set_config('waddle.protocol_epoch', (SELECT epoch::text FROM guarded.ingress_protocol_epoch WHERE id = 1), true), pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)").execute(&mut runtime).await?;
        let error = match sqlx::query("INSERT INTO guarded.ingress_effect_receipts (message_key, kind, semantic_identity_hash) SELECT message_key, kind, semantic_identity_hash FROM guarded.ingress_effect_receipts WHERE false")
            .execute(&mut runtime).await {
            Err(error) => error,
            Ok(_) => bail!("altered proof unexpectedly succeeded"),
        };
        ensure!(
            matches!(error, sqlx::Error::Database(ref e) if e.code().as_deref() == Some("P0001") && e.message().contains("transaction-local epoch proof")),
            "unexpected proof refusal: {error}"
        );
        sqlx::query("ROLLBACK").execute(&mut runtime).await?;
        let key = MessageKey::new();
        ensure!(
            matches!(
                lab.write(key, digest, intent, Uuid::new_v4()).await,
                Err(LabWriteError::Operation(IngressUowError::Substrate(
                    crate::ingress_substrate::IngressSubstrateError::Database { .. }
                )))
            ),
            "invalid proof did not fail the receipt seam"
        );
        wait_for_counts(lab, key, &[0, 0, 0, 0, 0]).await?;
        sqlx::query("DROP TRIGGER \"000_lab_break_proof\" ON guarded.ingress_effect_receipts")
            .execute(&mut owner)
            .await?;
    }
    sqlx::query("DROP FUNCTION guarded.lab_break_proof()")
        .execute(&mut owner)
        .await?;
    Ok(())
}

async fn shadow_and_wrong_pool(
    lab: &Lab,
    digest: &SemanticDigest,
    intent: &IngressEffectIntent,
) -> anyhow::Result<()> {
    let other = attested_pool(
        lab.runtime_options.clone(),
        lab.config.clone(),
        1,
        Duration::from_secs(5),
    )?;
    let other = Database::from_attested_postgres_pool("different-attested-pool", other);
    let capability = lab
        .lane
        .ingress
        .attested_pg_writer_capability()
        .context("lab capability")?;
    ensure!(
        other
            .verify_attested_pg_writer_capability(&capability)
            .is_err(),
        "a capability was accepted by a different pool"
    );
    let wrong_pool = IngressUnitOfWork::open(
        other.clone(),
        LineageConfig {
            deployment_uuid: Some(lab.lineage.deployment_uuid),
            action: None,
        },
    )?;
    ensure!(
        matches!(
            wrong_pool
                .begin_in_relation_guard_lab(lab.lane.candidate()?)
                .await,
            Err(IngressUowError::AttestedPoolRequired)
        ),
        "the fixture proof was accepted by a different physical pool"
    );
    ensure!(
        matches!(
            wrong_pool
                .begin_with_relation_guard(&lab.lane.candidate()?.capability)
                .await,
            Err(IngressUowError::RelationGuardCapability(
                RelationGuardCapabilityError::WrongPool
            ))
        ),
        "the production UOW accepted a capability from a different physical pool"
    );
    drop(wrong_pool);
    drop(other);

    let wrong_lane = IngressUnitOfWork::open(
        lab.lane.main.clone(),
        LineageConfig {
            deployment_uuid: Some(lab.lineage.deployment_uuid),
            action: None,
        },
    )?;
    ensure!(
        matches!(
            wrong_lane
                .begin_in_relation_guard_lab(lab.lane.candidate()?)
                .await,
            Err(IngressUowError::AttestedPoolRequired)
        ),
        "the fixture proof was accepted on the main admission lane"
    );

    let checksum = hex::encode(Sha256::digest(GUARD.as_bytes()));
    let mut owner = PgConnection::connect_with(&lab.owner_options).await?;
    sqlx::query("UPDATE guarded.waddle_epoch_transition_lab_marker SET checksum = 'tampered'")
        .execute(&mut owner)
        .await?;
    ensure!(
        lab.lane
            .verifier
            .verify_relation_guard_capability()
            .await
            .is_err(),
        "the verifier accepted a mismatched owner marker"
    );
    ensure!(
        lab.lane
            .uow
            .begin_with_relation_guard(&lab.lane.candidate()?.capability)
            .await
            .is_err(),
        "a previously issued capability survived owner-marker drift"
    );
    sqlx::query("UPDATE guarded.waddle_epoch_transition_lab_marker SET checksum = $1")
        .bind(&checksum)
        .execute(&mut owner)
        .await?;

    sqlx::query(&format!("CREATE SCHEMA shadow AUTHORIZATION {}", lab.owner))
        .execute(&lab.admin)
        .await?;
    sqlx::query(&format!("GRANT USAGE ON SCHEMA shadow TO {}", lab.runtime))
        .execute(&lab.admin)
        .await?;
    for table in [
        "ingress_messages",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_deliveries",
        "ingress_effect_descendants",
        "ingress_owner_rejections",
    ] {
        sqlx::query(&format!(
            "CREATE TABLE shadow.{table} (LIKE guarded.{table} INCLUDING ALL)"
        ))
        .execute(&lab.admin)
        .await?;
    }
    let key = MessageKey::new();
    let mut candidate = lab.begin().await?;
    candidate.use_fixture_shadow_path().await?;
    write_chain(&mut candidate, key, digest, intent, Uuid::new_v4()).await?;
    candidate.commit().await?;
    ensure!(
        counts(lab, key).await? == vec![1, 1, 1, 1, 1],
        "qualified target chain missing"
    );
    for table in [
        "ingress_messages",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_deliveries",
        "ingress_effect_descendants",
        "ingress_owner_rejections",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM shadow.{table}"))
            .fetch_one(&lab.admin)
            .await?;
        ensure!(count == 0, "candidate wrote to shadow.{table}");
    }
    Ok(())
}

async fn guarded_relation_acl_failures(lab: &Lab) -> anyhow::Result<()> {
    let cases = vec![
        (
            "GRANT DELETE ON guarded.ingress_messages TO PUBLIC".to_owned(),
            "REVOKE DELETE ON guarded.ingress_messages FROM PUBLIC".to_owned(),
            "PUBLIC DML",
        ),
        (
            format!(
                "GRANT TRUNCATE ON guarded.ingress_messages TO {}",
                lab.runtime
            ),
            format!(
                "REVOKE TRUNCATE ON guarded.ingress_messages FROM {}",
                lab.runtime
            ),
            "runtime TRUNCATE",
        ),
        (
            format!(
                "GRANT UPDATE (kind) ON guarded.ingress_effect_intents TO {}",
                lab.runtime
            ),
            format!(
                "REVOKE UPDATE (kind) ON guarded.ingress_effect_intents FROM {}",
                lab.runtime
            ),
            "runtime column UPDATE",
        ),
        (
            format!(
                "GRANT SELECT ON guarded.ingress_messages TO {} WITH GRANT OPTION",
                lab.runtime
            ),
            format!(
                "REVOKE GRANT OPTION FOR SELECT ON guarded.ingress_messages FROM {}",
                lab.runtime
            ),
            "runtime grant option",
        ),
    ];
    for (grant, revoke, reason) in cases {
        sqlx::query(&grant).execute(&lab.admin).await?;
        let rejected = lab
            .lane
            .verifier
            .verify_relation_guard_capability()
            .await
            .is_err();
        sqlx::query(&revoke).execute(&lab.admin).await?;
        ensure!(
            rejected,
            "candidate verifier accepted unexpected guarded-relation ACL: {reason}"
        );
        lab.lane.verifier.verify_relation_guard_capability().await?;
    }
    Ok(())
}

async fn prepared_descendant_api_checks(lab: &Lab) -> anyhow::Result<()> {
    let digest = SemanticDigest::from_storage(1, [3_u8; 32])?;
    let intent = make_intent(false)?;
    let message_key = MessageKey::new();
    let source = Uuid::new_v4();
    let target = Uuid::new_v4();
    lab.write(message_key, &digest, &intent, source).await?;

    let capability = lab.lane.candidate()?.capability();
    let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
    let binding = DeliveryEffectRepository::bind_effect(
        &mut transaction,
        message_key,
        &intent.semantic_key(),
    )
    .await?;
    ensure!(
        binding
            == crate::ingress_uow::EffectDeliveryBinding::Bound(DeliveryKey::effect(
                message_key,
                &intent.semantic_key(),
            )),
        "prepared effect binding returned an unexpected delivery key"
    );
    transaction.commit().await?;

    let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
    EffectDescendantRepository::lock_nowait(&mut transaction, message_key).await?;
    transaction.commit().await?;

    let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
    EffectDescendantRepository::copy(&mut transaction, source, target).await?;
    transaction.commit().await?;

    let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
    EffectDescendantRepository::settle_all(&mut transaction, source, Utc::now()).await?;
    transaction.commit().await?;

    let states: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE descendant_key = $1), \
                count(*) FILTER (WHERE descendant_key = $2), \
                count(*) FILTER (WHERE descendant_key = $1 AND settled_at IS NOT NULL), \
                count(*) FILTER (WHERE descendant_key = $2 AND settled_at IS NOT NULL) \
         FROM guarded.ingress_effect_descendants WHERE descendant_key IN ($1, $2)",
    )
    .bind(source.to_string())
    .bind(target.to_string())
    .fetch_one(&lab.admin)
    .await?;
    ensure!(
        states == (1, 1, 1, 0),
        "prepared descendant lock/copy/settle did not preserve the expected rows: {states:?}"
    );
    Ok(())
}

async fn prepared_alias_sm_archive_api_checks(lab: &Lab) -> anyhow::Result<()> {
    let capability = lab.lane.candidate()?.capability();

    let sender: BareJid = "alias-author@example.test".parse()?;
    let target = NormalizedTarget::Bare("alias-target@example.test".parse()?);
    let origin = OriginId::new("relation-guard-alias-origin");
    let digest = SemanticDigest::from_storage(1, [5_u8; 32])?;
    let alias_key = MessageKey::new();
    {
        let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
        let resolution = CanonicalMessageRepository::resolve_and_record_alias(
            &mut transaction,
            &sender,
            &target,
            &origin,
            &digest,
            || alias_key,
        )
        .await?;
        ensure!(
            matches!(resolution, AliasResolution::Aliased(AliasOutcome::Inserted(key)) if key == alias_key),
            "candidate alias repository did not insert its canonical key"
        );
        transaction.commit().await?;
    }
    {
        let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
        let resolution = CanonicalMessageRepository::resolve_and_record_alias(
            &mut transaction,
            &sender,
            &target,
            &origin,
            &digest,
            MessageKey::new,
        )
        .await?;
        ensure!(
            matches!(resolution, AliasResolution::Aliased(AliasOutcome::Existing(key)) if key == alias_key),
            "candidate alias replay did not resolve to the persisted key"
        );
        transaction.commit().await?;
    }

    let stream = SmSessionId::new(format!("relation-guard-sm-{}", Uuid::new_v4()));
    let sm_ingress_id;
    {
        let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
        sm_ingress_id = SmIngressStreamRepository::mint(&mut transaction, &stream).await?;
        SmIngressRepository::insert_sm_ref(
            &mut transaction,
            sm_ingress_id,
            IngressOrdinal::FIRST,
            WireHandledCount::new(1),
            alias_key,
        )
        .await?;
        let binding = SmIngressRepository::lookup_wire_binding(
            &mut transaction,
            sm_ingress_id,
            WireHandledCount::new(1),
        )
        .await?;
        ensure!(
            binding == Some((alias_key, IngressOrdinal::FIRST)),
            "candidate SM enrollment/ref did not retain its wire binding"
        );
        transaction.commit().await?;
    }

    let archive_intent = make_intent(true)?;
    let archive_key = MessageKey::new();
    let archive_digest = SemanticDigest::from_storage(1, [6_u8; 32])?;
    lab.write(
        archive_key,
        &archive_digest,
        &archive_intent,
        Uuid::new_v4(),
    )
    .await?;
    let (archive, resource) = match &archive_intent {
        IngressEffectIntent::RouteDirect {
            recipient, fanout, ..
        } => (
            recipient.clone(),
            fanout
                .first()
                .context("archive fixture has a resource audience")?
                .clone(),
        ),
        _ => bail!("archive fixture intent was not a direct route"),
    };
    let receipt = crate::ingress::receipt_key(&archive_intent)?;
    let ordinal = ArchiveOrdinal::from_storage(1)?;
    {
        let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
        ensure!(
            CanonicalMessageRepository::lock(&mut transaction, archive_key).await?,
            "archive fixture message is missing"
        );
        ArchiveDispatchRepository::record(
            &mut transaction,
            archive_key,
            &archive,
            ordinal,
            &[ArchiveDispatchObligation {
                receipt: receipt.clone(),
                target: DispatchTarget::Resource(resource),
            }],
        )
        .await?;
        transaction.commit().await?;
    }
    {
        let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
        let positions =
            ArchiveDispatchRepository::positions(&mut transaction, archive_key, &receipt).await?;
        ensure!(
            positions.len() == 1
                && positions[0].archive == archive
                && positions[0].ordinal == ordinal,
            "candidate archive dispatch did not persist its position"
        );
        transaction.commit().await?;
    }
    Ok(())
}

fn relation_observation_payload() -> anyhow::Result<ExtensionPayload> {
    let namespace = waddle_extensions::PayloadNamespace::try_new("urn:waddle:relation-guard:1")?;
    let element = XmlElement::try_new(namespace.clone(), "result", vec![], vec![])?;
    Ok(ExtensionPayload::try_new(namespace, element)?)
}

async fn room_observation_relation_uow_checks(lab: &Lab) -> anyhow::Result<()> {
    let room: BareJid = "room@conference.example.org".parse()?;
    let author: BareJid = "author@example.org".parse()?;
    let observer = ConfiguredRoomObserver {
        plugin: PluginId::try_new("relation-guard-observer")?,
        generation: ObservationGeneration::try_new(1)?,
        identity: Sha256Digest::try_new("a".repeat(64))?,
        scope: RoomObservationScope::Rooms(vec![room.clone()]),
        max_concurrent: 1,
    };
    let subscription = RoomObservationSubscription {
        plugin: observer.plugin.clone(),
        generation: observer.generation,
        identity: observer.identity.clone(),
        room: room.clone(),
    };
    let now = Utc::now();
    let now_ms = now.timestamp_millis();
    let capability = lab.lane.candidate()?.capability();

    {
        let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
        RoomObservationRepository::sync_configured(
            &mut transaction,
            std::slice::from_ref(&observer),
            now_ms,
        )
        .await?;
        transaction.commit().await?;
    }

    let key = MessageKey::new();
    let digest = SemanticDigest::from_storage(1, [4_u8; 32])?;
    let intent = IngressEffectIntent::RoomObserver {
        room: room.clone(),
        requester: author.clone(),
        sender: "room@conference.example.org/author".parse()?,
        plugin: observer.plugin.clone(),
        generation: observer.generation,
        identity: observer.identity.clone(),
        correction_target: None,
    };
    let intents = [intent];
    let mut message = Message::new(Some(room.clone().into()));
    message.from = Some("room@conference.example.org/author".parse()?);
    message.type_ = MessageType::Groupchat;
    message.id = Some(Id("relation-observation".to_owned()));
    message
        .bodies
        .insert(Lang::new(), "room observation candidate".to_owned());
    add_stanza_id(
        &mut message,
        &StanzaId::new("relation-observation-stanza", room.clone().into()),
    );
    add_origin_id(&mut message, "relation-observation-origin");
    {
        let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
        let existing = CanonicalMessageRepository::lock(&mut transaction, key).await?;
        CanonicalMessageRepository::record_message(&mut transaction, key, &digest, None).await?;
        let verdict =
            EffectIntentRepository::reconcile(&mut transaction, key, &intents, existing).await?;
        ensure!(
            !matches!(
                verdict,
                ReconcileVerdict::Contradiction { .. } | ReconcileVerdict::Divergent { .. }
            ),
            "room observer intent unexpectedly conflicted"
        );
        RoomObservationRepository::capture(
            &mut transaction,
            CapturedRoomSource {
                key,
                room: &room,
                message: &message,
                sender: &author,
                intents: &intents,
                observed_at: now,
                correction_target: None,
            },
        )
        .await?;
        transaction.commit().await?;
    }

    let work = {
        let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
        let work = RoomObservationRepository::claim(&mut transaction, &subscription, now_ms)
            .await?
            .context("relation-guard room observer claim")?;
        ensure!(
            RoomObservationRepository::start(&mut transaction, &work, now_ms).await?,
            "relation-guard room observer start was rejected"
        );
        transaction.commit().await?;
        work
    };

    {
        let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
        ensure!(
            RoomObservationRepository::finish(
                &mut transaction,
                &work,
                &RoomObservationOutcome::Completed(RoomObservationResult {
                    payloads: vec![relation_observation_payload()?],
                    usage: None,
                }),
                now_ms,
            )
            .await?,
            "relation-guard room observer finish was rejected"
        );
        transaction.commit().await?;
    }

    let publication = {
        let mut transaction = lab.lane.uow.begin_with_relation_guard(capability).await?;
        let publication =
            RoomObservationRepository::publication(&mut transaction, &subscription, now_ms)
                .await?
                .context("relation-guard room observer publication")?;
        ensure!(
            RoomObservationRepository::assert_publication(&mut transaction, &publication).await?,
            "relation-guard room observer publication became stale"
        );
        ensure!(
            RoomObservationRepository::mark_published(
                &mut transaction,
                &publication.id,
                now_ms.saturating_add(1),
            )
            .await?,
            "relation-guard room observer publication did not settle"
        );
        transaction.commit().await?;
        publication
    };

    let (publication_status, settled_descendants): (String, i64) = sqlx::query_as(
        "SELECT p.status, count(*) FILTER (WHERE d.settled_at IS NOT NULL) \
         FROM guarded.extension_room_publications p \
         JOIN guarded.ingress_effect_descendants d ON d.descendant_key = p.id \
         WHERE p.id = $1 GROUP BY p.status",
    )
    .bind(publication.id.to_string())
    .fetch_one(&lab.admin)
    .await?;
    ensure!(
        publication_status == "published" && settled_descendants == 1,
        "room observer publication/descendant was not settled through the relation UOW"
    );
    Ok(())
}

async fn verify_failed_provision_cleanup(lab: &Lab) -> anyhow::Result<()> {
    let suffix = Uuid::new_v4().simple().to_string();
    let role = format!("relation_cleanup_role_{suffix}");
    let database = format!("relation_cleanup_db_{suffix}");
    let mut resources = Provisioning {
        cluster: lab.cluster.clone(),
        database: None,
        roles: Vec::new(),
    };
    resources
        .create_role(
            &role,
            "NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS",
        )
        .await?;
    resources.create_database(&database, &role).await?;
    ensure!(
        sqlx::query("SELECT * FROM pg_catalog.relation_lab_intentional_missing_table")
            .execute(&lab.cluster)
            .await
            .is_err(),
        "the setup failure injection unexpectedly succeeded"
    );
    resources.cleanup().await?;
    let (role_remains, database_remains): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = $1), \
         EXISTS(SELECT 1 FROM pg_catalog.pg_database WHERE datname = $2)",
    )
    .bind(&role)
    .bind(&database)
    .fetch_one(&lab.cluster)
    .await?;
    ensure!(
        !role_remains && !database_remains,
        "failed provisioning left a role or database"
    );

    // A cleanup racing an unknown role COMMIT must wait for the creator's
    // session lock before it decides whether the marker exists.
    let role = format!("relation_commit_race_{}", Uuid::new_v4().simple());
    let marker = format!("waddle-relation-lab-{}", Uuid::new_v4());
    let mut resources = Provisioning {
        cluster: lab.cluster.clone(),
        database: None,
        roles: vec![(role.clone(), marker.clone())],
    };
    let mut creator = lock_provisioning_object(&lab.cluster, &role).await?;
    sqlx::query("BEGIN").execute(&mut creator).await?;
    sqlx::query(&format!("CREATE ROLE {role} NOLOGIN"))
        .execute(&mut creator)
        .await?;
    sqlx::query(&format!("COMMENT ON ROLE {role} IS '{marker}'"))
        .execute(&mut creator)
        .await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut creator)
        .await?;
    let release = async {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE $1 = ANY(pg_catalog.pg_blocking_pids(pid)))")
                    .bind(pid).fetch_one(&lab.cluster).await?;
                if waiting { return Ok::<(), anyhow::Error>(()); }
                tokio::task::yield_now().await;
            }
        }).await.context("cleanup did not wait for the in-flight role operation")??;
        sqlx::query("COMMIT").execute(&mut creator).await?;
        creator.close().await?;
        Ok::<(), anyhow::Error>(())
    };
    let (cleanup, released) = tokio::join!(resources.cleanup(), release);
    released?;
    cleanup?;
    let remains: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = $1)")
            .bind(&role)
            .fetch_one(&lab.cluster)
            .await?;
    ensure!(
        !remains,
        "role survived reconciliation after its delayed COMMIT"
    );
    Ok(())
}

async fn exercise(lab: &mut Lab) -> anyhow::Result<()> {
    verify_failed_provision_cleanup(lab)
        .await
        .context("failed-provision cleanup checks")?;
    role_boundary(lab).await.context("runtime role boundary")?;
    ingress_lane_boundary(lab)
        .await
        .context("ingress lane and capability checks")?;
    verifier_lane_boundary(lab)
        .await
        .context("two-lane verifier capacity checks")?;
    let digest = SemanticDigest::from_storage(1, [1_u8; 32])?;
    let intent = make_intent(false)?;
    let key = MessageKey::new();
    let descendant = Uuid::new_v4();
    ensure!(
        lab.write(key, &digest, &intent, descendant).await? == ProtocolEpoch::ZERO,
        "expected epoch zero"
    );
    ensure!(
        lab.write(key, &digest, &intent, descendant).await? == ProtocolEpoch::ZERO,
        "idempotent replay failed"
    );
    ensure!(
        counts(lab, key).await? == vec![1, 1, 1, 1, 1],
        "replay duplicated the chain"
    );
    let conflict = SemanticDigest::from_storage(1, [2_u8; 32])?;
    ensure!(
        matches!(
            lab.write(key, &conflict, &intent, descendant).await,
            Err(LabWriteError::Operation(IngressUowError::Substrate(
                crate::ingress_substrate::IngressSubstrateError::MessageContentConflict
            )))
        ),
        "digest collision did not produce the typed conflict"
    );
    ensure!(
        matches!(
            lab.write(key, &digest, &make_intent(true)?, descendant)
                .await,
            Err(LabWriteError::Operation(
                IngressUowError::EffectIntentConflict
            ))
        ),
        "effect-key payload collision did not produce the typed conflict"
    );
    let mut new_audience = make_intent(false)?;
    if let IngressEffectIntent::RouteDirect { recipient, .. } = &mut new_audience {
        *recipient = "new-audience@example.test".parse()?;
    }
    ensure!(
        matches!(
            lab.write(key, &digest, &new_audience, descendant).await,
            Err(LabWriteError::Operation(
                IngressUowError::EffectIntentConflict
            ))
        ),
        "replay added an unrelated obligation"
    );
    ensure!(
        counts(lab, key).await? == vec![1, 1, 1, 1, 1],
        "collision changed durable rows"
    );

    // A previously committed empty acceptance must not become authority for a
    // newly planned effect on replay.
    let empty_key = MessageKey::new();
    {
        let mut empty = lab.begin().await?;
        CanonicalMessageRepository::record_message(&mut empty, empty_key, &digest, None).await?;
        empty.commit().await?;
    }
    ensure!(
        matches!(
            lab.write(empty_key, &digest, &intent, Uuid::new_v4()).await,
            Err(LabWriteError::Operation(
                IngressUowError::EffectIntentConflict
            ))
        ),
        "empty acceptance gained a new replay obligation"
    );
    ensure!(
        counts(lab, empty_key).await? == vec![1, 0, 0, 0, 0],
        "empty acceptance changed on replay"
    );

    let missing = MessageKey::new();
    let mut candidate = lab.begin().await?;
    ensure!(
        matches!(
            EffectDescendantRepository::attach(
                &mut candidate,
                missing,
                &intent.semantic_key(),
                Uuid::new_v4()
            )
            .await,
            Err(IngressUowError::EffectIntentMessageMissing)
        ),
        "missing parent did not preserve its typed error"
    );
    drop(candidate);
    wait_for_counts(lab, missing, &[0, 0, 0, 0, 0]).await?;

    let rollback_key = MessageKey::new();
    let mut candidate = lab.begin().await?;
    write_chain(
        &mut candidate,
        rollback_key,
        &digest,
        &intent,
        Uuid::new_v4(),
    )
    .await?;
    drop(candidate);
    wait_for_counts(lab, rollback_key, &[0, 0, 0, 0, 0]).await?;
    proof_failures(lab, &digest, &intent)
        .await
        .context("epoch proof failure checks")?;
    shadow_and_wrong_pool(lab, &digest, &intent)
        .await
        .context("wrong-pool and search-path checks")?;
    guarded_relation_acl_failures(lab)
        .await
        .context("guarded relation ACL rejection checks")?;
    prepared_descendant_api_checks(lab)
        .await
        .context("prepared descendant repository checks")?;
    prepared_alias_sm_archive_api_checks(lab)
        .await
        .context("prepared alias, SM, and archive repository checks")?;
    room_observation_relation_uow_checks(lab)
        .await
        .context("room-observation relation UOW checks")?;
    concurrency_and_transition(lab, &digest, &intent)
        .await
        .context("concurrent writers and queued owner transition")?;
    proof_failures(lab, &digest, &intent)
        .await
        .context("post-transition epoch proof checks")?;
    lost_commit_ack(lab, &digest, &intent, false)
        .await
        .context("lost COMMIT acknowledgement")?;
    lost_commit_ack(lab, &digest, &intent, true)
        .await
        .context("expired COMMIT acknowledgement deadline")?;
    Ok(())
}

async fn wait_for_blocked(lab: &Lab, application: &str, blocker: i32) -> anyhow::Result<i32> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiter: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name = $1 \
                 AND $2 = ANY(pg_catalog.pg_blocking_pids(pid)) LIMIT 1",
            )
            .bind(application)
            .bind(blocker)
            .fetch_optional(&lab.admin)
            .await?;
            if let Some(pid) = waiter {
                return Ok(pid);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("expected PostgreSQL lock wait not observed")?
}

async fn wait_for_relation_lock_pids(lab: &Lab, count: usize) -> anyhow::Result<Vec<i32>> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pids: Vec<i32> = sqlx::query_scalar(
                "SELECT DISTINCT l.pid FROM pg_catalog.pg_locks l \
                 JOIN pg_catalog.pg_stat_activity a ON a.pid = l.pid \
                 WHERE a.application_name = $1 \
                   AND l.relation = 'guarded.ingress_protocol_epoch'::regclass \
                   AND l.locktype = 'relation' AND l.mode = 'AccessShareLock' AND l.granted \
                 ORDER BY l.pid",
            )
            .bind(&lab.application)
            .fetch_all(&lab.admin)
            .await?;
            if pids.len() >= count {
                return Ok::<_, anyhow::Error>(pids);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("independent relation-lock writers were not observed")?
}

async fn wait_for_blocker_count(
    lab: &Lab,
    application: &str,
    minimum: usize,
) -> anyhow::Result<(i32, usize)> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let observation: Option<(i32, i64)> = sqlx::query_as(
                "SELECT pid, pg_catalog.cardinality(pg_catalog.pg_blocking_pids(pid))::bigint \
                 FROM pg_catalog.pg_stat_activity WHERE application_name = $1",
            )
            .bind(application)
            .fetch_optional(&lab.admin)
            .await?;
            if let Some((pid, blockers)) = observation {
                if usize::try_from(blockers).is_ok_and(|count| count >= minimum) {
                    return Ok::<_, anyhow::Error>((pid, blockers as usize));
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("expected queued owner to wait for both writers")?
}

async fn check_relation_lock(lab: &Lab, pid: i32) -> anyhow::Result<()> {
    let (access_share, row_share): (bool, bool) = sqlx::query_as(
        "SELECT coalesce(bool_or(mode = 'AccessShareLock'), false), \
         coalesce(bool_or(mode = 'RowShareLock'), false) FROM pg_catalog.pg_locks \
         WHERE pid = $1 AND relation = 'guarded.ingress_protocol_epoch'::regclass AND granted",
    )
    .bind(pid)
    .fetch_one(&lab.admin)
    .await?;
    ensure!(
        access_share && !row_share,
        "writer did not retain the relation-only epoch lock"
    );
    Ok(())
}

async fn concurrency_and_transition(
    lab: &Lab,
    digest: &SemanticDigest,
    intent: &IngressEffectIntent,
) -> anyhow::Result<()> {
    let first = MessageKey::new();
    let second = MessageKey::new();
    let queued = MessageKey::new();
    let mut first_writer = lab.begin().await?;
    let mut second_writer = lab.begin().await?;
    ensure!(
        first_writer.protocol_epoch() == ProtocolEpoch::ZERO
            && second_writer.protocol_epoch() == ProtocolEpoch::ZERO,
        "epoch-zero writers did not prepare"
    );
    write_chain(&mut first_writer, first, digest, intent, Uuid::new_v4()).await?;
    write_chain(&mut second_writer, second, digest, intent, Uuid::new_v4()).await?;
    let writer_pids = wait_for_relation_lock_pids(lab, 2).await?;
    ensure!(
        writer_pids.len() == 2,
        "two independent writer sessions did not hold ACCESS SHARE"
    );
    for pid in writer_pids {
        check_relation_lock(lab, pid).await?;
    }

    let owner_options = lab.owner_options.clone();
    let lineage = lab.lineage.lineage_uuid.0;
    let transition_application = format!("{}_transition", lab.application);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (commit_tx, commit_rx) = tokio::sync::oneshot::channel();
    let application = transition_application.clone();
    let transition = tokio::spawn(async move {
        let mut connection =
            PgConnection::connect_with(&owner_options.application_name(&application)).await?;
        sqlx::query("BEGIN ISOLATION LEVEL READ COMMITTED")
            .execute(&mut connection)
            .await?;
        sqlx::query("LOCK TABLE ONLY guarded.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE")
            .execute(&mut connection)
            .await?;
        sqlx::query("UPDATE guarded.ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = $1 WHERE id = 1")
            .bind(lineage).execute(&mut connection).await?;
        let _ = ready_tx.send(());
        let _ = commit_rx.await;
        sqlx::query("COMMIT").execute(&mut connection).await?;
        Ok::<(), sqlx::Error>(())
    });
    let (transition_pid, transition_blockers) =
        wait_for_blocker_count(lab, &transition_application, 2).await?;
    ensure!(
        transition_blockers >= 2,
        "owner transition did not wait for both independent writers"
    );

    first_writer.commit().await?;
    let uow = lab.lane.uow.clone();
    let capability = lab.lane.candidate()?.capability.clone();
    let queued_digest = digest.clone();
    let queued_intent = intent.clone();
    let later = tokio::spawn(async move {
        write_uow_chain(
            &uow,
            &capability,
            queued,
            &queued_digest,
            &queued_intent,
            Uuid::new_v4(),
        )
        .await
    });
    let queued_pid = wait_for_blocked(lab, &lab.application, transition_pid).await?;
    let application_locks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_locks WHERE pid = $1 AND granted \
         AND relation IN ('guarded.ingress_messages'::regclass, 'guarded.ingress_effect_intents'::regclass, \
         'guarded.ingress_effect_receipts'::regclass, 'guarded.ingress_deliveries'::regclass, \
         'guarded.ingress_effect_descendants'::regclass)",
    )
    .bind(queued_pid)
    .fetch_one(&lab.admin)
    .await?;
    ensure!(
        application_locks == 0,
        "queued writer held a domain lock before the epoch barrier"
    );

    second_writer.commit().await?;
    tokio::time::timeout(Duration::from_secs(5), ready_rx).await??;
    commit_tx
        .send(())
        .map_err(|_| anyhow::anyhow!("transition task dropped"))?;
    ensure!(
        later.await?? == ProtocolEpoch::from_storage(1),
        "queued writer did not read the post-transition epoch"
    );
    transition.await??;
    ensure!(
        counts(lab, first).await? == vec![1, 1, 1, 1, 1],
        "first chain incomplete"
    );
    ensure!(
        counts(lab, second).await? == vec![1, 1, 1, 1, 1],
        "second chain incomplete"
    );
    ensure!(
        counts(lab, queued).await? == vec![1, 1, 1, 1, 1],
        "post-transition chain incomplete"
    );

    // At epoch one, block the last domain write and observe the same lock set.
    let mut blocker = PgConnection::connect_with(&lab.owner_options).await?;
    sqlx::query("BEGIN").execute(&mut blocker).await?;
    sqlx::query("LOCK TABLE ONLY guarded.ingress_effect_descendants IN SHARE MODE")
        .execute(&mut blocker)
        .await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await?;
    let key = MessageKey::new();
    let observe = async {
        let pid = wait_for_blocked(lab, &lab.application, blocker_pid).await?;
        check_relation_lock(lab, pid).await?;
        sqlx::query("COMMIT").execute(&mut blocker).await?;
        Ok::<(), anyhow::Error>(())
    };
    let (written, observed) = tokio::join!(lab.write(key, digest, intent, Uuid::new_v4()), observe);
    observed?;
    ensure!(
        written? == ProtocolEpoch::from_storage(1),
        "epoch-one chain failed"
    );

    // Cancel a chain blocked on the last table and confirm no partial commit.
    let mut blocker = PgConnection::connect_with(&lab.owner_options).await?;
    sqlx::query("BEGIN").execute(&mut blocker).await?;
    sqlx::query("LOCK TABLE ONLY guarded.ingress_effect_descendants IN SHARE MODE")
        .execute(&mut blocker)
        .await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await?;
    let cancelled = MessageKey::new();
    let observe = async {
        let pid = wait_for_blocked(lab, &lab.application, blocker_pid).await?;
        check_relation_lock(lab, pid).await
    };
    let (result, observed) = tokio::join!(
        tokio::time::timeout(
            Duration::from_millis(500),
            lab.write(cancelled, digest, intent, Uuid::new_v4())
        ),
        observe
    );
    observed?;
    ensure!(result.is_err(), "blocked chain was not cancelled");
    sqlx::query("COMMIT").execute(&mut blocker).await?;
    wait_for_counts(lab, cancelled, &[0, 0, 0, 0, 0]).await?;
    Ok(())
}

async fn lose_commit_reply(
    listener: tokio::net::TcpListener,
    target: PgConnectOptions,
    held: std::sync::Arc<tokio::sync::Notify>,
    disconnect: std::sync::Arc<tokio::sync::Notify>,
    marker: &'static [u8],
) -> std::io::Result<()> {
    use tokio::task::JoinSet;

    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (client, _) = accepted?;
                let target = target.clone();
                let held = held.clone();
                let disconnect = disconnect.clone();
                connections.spawn(async move {
                    proxy_commit_connection(client, &target, held, disconnect, marker).await
                });
            }
            result = connections.join_next(), if !connections.is_empty() => {
                match result {
                    Some(Ok(Ok(true))) => return Ok(()),
                    Some(Ok(Ok(false))) => {}
                    Some(Ok(Err(error))) => return Err(error),
                    Some(Err(error)) => return Err(std::io::Error::other(error)),
                    None => {}
                }
            }
        }
    }
}

async fn proxy_commit_connection(
    client: tokio::net::TcpStream,
    target: &PgConnectOptions,
    held: std::sync::Arc<tokio::sync::Notify>,
    disconnect: std::sync::Arc<tokio::sync::Notify>,
    marker: &'static [u8],
) -> std::io::Result<bool> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let server = super::postgres_proxy::connect_local_postgres(target).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let saw_commit = std::sync::Arc::new(AtomicBool::new(false));
    let completion_flag = saw_commit.clone();
    let upstream_flag = saw_commit.clone();
    let upstream = async move {
        const COMMIT: &[u8] = b"COMMIT\0";
        let mut bytes = [0_u8; 8192];
        let mut tail = Vec::new();
        let mut armed = false;
        loop {
            let n = client_read.read(&mut bytes).await?;
            if n == 0 {
                return Ok::<(), std::io::Error>(());
            }
            let mut scan = tail;
            scan.extend_from_slice(&bytes[..n]);
            if !armed {
                if let Some(index) = scan.windows(marker.len()).position(|part| part == marker) {
                    scan.drain(..index + marker.len());
                    armed = true;
                }
            }
            if armed && scan.windows(COMMIT.len()).any(|part| part == COMMIT) {
                upstream_flag.store(true, Ordering::SeqCst);
            }
            server_write.write_all(&bytes[..n]).await?;
            tail = scan[scan.len().saturating_sub(marker.len() - 1)..].to_vec();
        }
    };
    let downstream = async move {
        let mut bytes = [0_u8; 8192];
        loop {
            let n = server_read.read(&mut bytes).await?;
            if n == 0 {
                return Ok::<(), std::io::Error>(());
            }
            if saw_commit.load(Ordering::SeqCst) {
                held.notify_one();
                disconnect.notified().await;
                return Ok(());
            }
            client_write.write_all(&bytes[..n]).await?;
        }
    };
    tokio::select! {
        result = upstream => result.map(|_| false),
        result = downstream => result.map(|_| completion_flag.load(Ordering::SeqCst)),
    }
}

async fn lost_commit_ack(
    lab: &mut Lab,
    digest: &SemanticDigest,
    intent: &IngressEffectIntent,
    expire_deadline: bool,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let held = std::sync::Arc::new(tokio::sync::Notify::new());
    let disconnect = std::sync::Arc::new(tokio::sync::Notify::new());
    let proxy = tokio::spawn(lose_commit_reply(
        listener,
        lab.runtime_options.clone(),
        held.clone(),
        disconnect.clone(),
        b"waddle_relation_lab_business_commit",
    ));
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .database(&lab.database_name)
        .username(&lab.runtime)
        .password(&lab.password)
        .application_name(&lab.application)
        .ssl_mode(sqlx::postgres::PgSslMode::Disable)
        .options([("search_path", "guarded")]);
    let pool = attested_pool(options, lab.config.clone(), 4, Duration::from_secs(5))?;
    let lane = LabLane::new(pool, &lab.lineage, true)
        .await
        .context("initialize proxy-backed relation-guard lane")?;
    let old = std::mem::replace(&mut lab.lane, lane);
    drop(old);
    let key = MessageKey::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let write = lab.write_until(key, digest, intent, Uuid::new_v4(), deadline);
    let observe = async {
        tokio::time::timeout_at(deadline, held.notified()).await?;
        ensure!(
            counts(lab, key).await? == vec![1, 1, 1, 1, 1],
            "COMMIT was not durable before the reply was dropped"
        );
        if !expire_deadline {
            disconnect.notify_one();
        }
        Ok::<(), anyhow::Error>(())
    };
    let (result, observed) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(12), write),
        observe
    );
    observed?;
    ensure!(
        matches!(result?, Err(LabWriteError::CommitIndeterminate)),
        "lost COMMIT reply did not remain indeterminate"
    );
    if expire_deadline {
        ensure!(
            tokio::time::Instant::now() >= deadline,
            "held COMMIT reply returned before the operation deadline"
        );
    }
    disconnect.notify_one();
    proxy.await??;
    ensure!(
        counts(lab, key).await? == vec![1, 1, 1, 1, 1],
        "fresh readback did not retain the committed chain"
    );
    Ok(())
}
