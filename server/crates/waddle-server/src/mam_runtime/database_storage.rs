//! `MamStorage` adapter over one admitted main database transaction per
//! operation. Restricted and legacy owner attachments have separate gates.
//!
//! The attested attachment requires an independent owner handoff and checks
//! each transaction's physical capability. The legacy owner attachment uses
//! the current owner session and makes no restricted authority claim. Both
//! schema-qualify relations and avoid a standalone MAM pool.

use async_trait::async_trait;
use jid::{BareJid, Jid};
use sqlx::PgConnection;
use thiserror::Error;
use waddle_xmpp::mam::{
    ArchivedMessage, MamArchiveKind, MamQuery, MamResult, MamSchemaVerificationError, MamStorage,
    MamStorageError, SqlxMamStorage, StoreOutcome, TerminalTombstoneOutcome,
};
use waddle_xmpp::muc::RoomClaimFenceContext;
use waddle_xmpp_core::mam::ArchivedTombstone;
use waddle_xmpp_core::xep0359::OriginId;

#[cfg(feature = "clustering")]
use crate::clustering::claims::{
    attach_main_claim_occupancy, ClaimOccupancyAttachError, VerifiedMainClaimOccupancy,
};
use crate::db::lineage::PgSchemaName;
use crate::db::physical_attestation::{
    quote_identifier, validate_identifier, AttestedPostgresWriterCapability, PgRoleName,
};
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

const MAM_TABLES: [&str; 2] = ["mam_messages", "mam_archive_sequences"];
const MESSAGE_TOMBSTONE_UPDATE_COLUMNS: &str =
    "'body', 'stanza_xml', 'thread_id', 'parent_thread_id', 'reply_to_id', 'reply_to_jid', 'rich_payload'";

/// Failure to attach `DatabaseMamStorage` to an owner-prepared archive.
#[derive(Debug, Error)]
pub enum DatabaseMamStorageError {
    #[error("DatabaseMamStorage requires an attested PostgreSQL main-lane database")]
    AttestedMainDatabaseRequired,
    #[error("owner MAM attachment requires a non-attested shared PostgreSQL main lane")]
    OwnerMainDatabaseRequired,
    #[error("the MAM or claim schema name is invalid")]
    InvalidSchema,
    #[error("the MAM runtime role or column privileges are unsafe")]
    UnsafeMamPrivileges,
    #[error("the claim schema does not match verified main claim authority")]
    UnsafeClaimPrivileges,
    #[cfg(feature = "clustering")]
    #[error(transparent)]
    ClaimAuthority(#[from] ClaimOccupancyAttachError),
    #[error("could not bound PostgreSQL waits for MAM")]
    ReadBounds,
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Schema(#[from] MamSchemaVerificationError),
}

/// MAM storage on the supplied main database without an independent pool.
///
/// [`Self::attach`] is the strict, dark attested path. The legacy owner
/// constructor is separate and cannot accept an attested handle. Every trait
/// operation opens one `Database::begin()` transaction and runs on its pinned
/// connection; only the attested path claims physical transaction authority.
pub struct DatabaseMamStorage {
    database: Database,
    authority: MamAuthority,
    mam_schema: String,
    mam_owner: PgRoleName,
    #[cfg(feature = "clustering")]
    claim_schema: String,
}

enum MamAuthority {
    Attested {
        capability: Box<AttestedPostgresWriterCapability>,
        #[cfg(feature = "clustering")]
        claim_authority: Box<VerifiedMainClaimOccupancy>,
    },
    LegacyOwner {
        #[cfg(feature = "clustering")]
        fencing_enabled: bool,
    },
}

impl DatabaseMamStorage {
    /// Inspect the partial v2 role profile and the owner-prepared MAM schema.
    /// This returns no storage handle or claim authority and is not selected by
    /// startup. Privileged mutators and archive writers must remain quiescent.
    pub async fn verify_restricted_profile_v2(
        database: &Database,
        mam_owner: &PgRoleName,
        profile: &crate::pubsub::RestrictedServingProfileV2,
    ) -> Result<(), DatabaseMamStorageError> {
        if database.driver() != DatabaseDriver::Postgres || !database.is_main_lane() {
            return Err(DatabaseMamStorageError::AttestedMainDatabaseRequired);
        }
        let capability = database
            .attested_pg_writer_capability()
            .ok_or(DatabaseMamStorageError::AttestedMainDatabaseRequired)?;
        database
            .verify_attested_pg_writer_capability(&capability)
            .map_err(DatabaseError::from)?;
        let identity = capability.identity();
        let schema = &identity.schema.name.0;
        validate_identifier(schema, "MAM schema")
            .map_err(|_| DatabaseMamStorageError::InvalidSchema)?;

        // MAM's existing full verifier requires REPEATABLE READ. Preflight
        // the named heaps in READ COMMITTED before locking them in that fresh
        // transaction; quiescent privileged mutators remain a prerequisite.
        let mut preflight = database.begin().await?;
        preflight
            .execute(
                "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
                (),
            )
            .await?;
        crate::db::read_only_timeout::bound_postgres_waits(&mut preflight)
            .await
            .map_err(|_| DatabaseMamStorageError::ReadBounds)?;
        capability.attest_transaction(&mut preflight).await?;
        let mut active = preflight
            .query(
                "SELECT n.oid::bigint=? AND n.nspname=pg_catalog.current_schema() \
             AND COALESCE((pg_catalog.current_schemas(false))[1]::text,'')=n.nspname \
             AND pg_catalog.cardinality(pg_catalog.current_schemas(false))<=2 \
             AND (pg_catalog.cardinality(pg_catalog.current_schemas(false))=1 \
               OR pg_catalog.left((pg_catalog.current_schemas(false))[2]::text,8)='pg_temp_') \
             FROM pg_catalog.pg_namespace n WHERE n.nspname=?",
                crate::db_params![i64::from(identity.schema.oid), schema],
            )
            .await?;
        if !matches!(active.next().await?, Some(row) if row.get::<bool>(0)?) {
            return Err(DatabaseMamStorageError::InvalidSchema);
        }
        drop(active);
        preflight
            .execute("SET LOCAL search_path = pg_catalog, pg_temp", ())
            .await?;
        verify_v2_mam_graph(&mut preflight, identity, capability.definer_role(), profile).await?;
        preflight.rollback().await?;

        let mut tx = database.begin().await?;
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
            (),
        )
        .await?;
        crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
            .await
            .map_err(|_| DatabaseMamStorageError::ReadBounds)?;
        let quoted = quote_identifier(schema);
        for table in MAM_TABLES {
            tx.execute(
                &format!("LOCK TABLE ONLY {quoted}.\"{table}\" IN ACCESS SHARE MODE"),
                (),
            )
            .await?;
        }
        capability.attest_transaction(&mut tx).await?;
        tx.execute("SET LOCAL search_path = pg_catalog, pg_temp", ())
            .await?;
        verify_v2_mam_graph(&mut tx, identity, capability.definer_role(), profile).await?;
        crate::pubsub::runtime_attach::verify_no_definer_writers_tx(&mut tx, schema, &MAM_TABLES)
            .await
            .map_err(|_| DatabaseMamStorageError::UnsafeMamPrivileges)?;
        let connection = tx
            .postgres_connection()
            .ok_or(DatabaseMamStorageError::AttestedMainDatabaseRequired)?;
        verify_mam_writer_privileges(
            connection,
            schema,
            mam_owner,
            capability.definer_role(),
            capability.installer_role(),
            Some(profile),
        )
        .await?;
        SqlxMamStorage::verify_postgres_schema_on_connection(
            connection,
            schema,
            mam_owner.as_str(),
        )
        .await?;
        tx.rollback().await?;
        Ok(())
    }

    /// Attach to an already owner-prepared MAM schema on an attested main
    /// PostgreSQL lane. This performs a bounded, read-only snapshot check and
    /// verifies the exact runtime grants needed by all `MamStorage` methods,
    /// including the seven tombstone-only message column updates.
    ///
    /// `mam_owner` and `claim_schema` are trusted provisioning inputs; neither
    /// is inferred from the runtime connection's `search_path`. With the
    /// clustering feature, the claim schema must name the attested main schema
    /// and pass the full claim/occupancy structural verifier. Without it,
    /// fenced writes retain the portable ordinary-store fallback. The caller
    /// must preserve owner/DDL/ACL quiescence for the attachment's lifetime.
    pub async fn attach(
        database: &Database,
        mam_owner: PgRoleName,
        claim_schema: PgSchemaName,
    ) -> Result<Self, DatabaseMamStorageError> {
        if database.driver() != DatabaseDriver::Postgres || !database.is_main_lane() {
            return Err(DatabaseMamStorageError::AttestedMainDatabaseRequired);
        }
        let capability = database
            .attested_pg_writer_capability()
            .ok_or(DatabaseMamStorageError::AttestedMainDatabaseRequired)?;
        database
            .verify_attested_pg_writer_capability(&capability)
            .map_err(DatabaseError::from)?;

        let mam_schema = capability.identity().schema.name.0.clone();
        validate_identifier(&mam_schema, "MAM schema")
            .map_err(|_| DatabaseMamStorageError::InvalidSchema)?;
        validate_identifier(&claim_schema.0, "claim schema")
            .map_err(|_| DatabaseMamStorageError::InvalidSchema)?;
        if claim_schema.0 != mam_schema {
            return Err(DatabaseMamStorageError::UnsafeClaimPrivileges);
        }
        #[cfg(feature = "clustering")]
        let claim_authority = attach_main_claim_occupancy(database.clone()).await?;

        let storage = Self {
            database: database.clone(),
            authority: MamAuthority::Attested {
                capability: Box::new(capability),
                #[cfg(feature = "clustering")]
                claim_authority: Box::new(claim_authority),
            },
            mam_schema,
            mam_owner,
            #[cfg(feature = "clustering")]
            claim_schema: claim_schema.0,
        };
        storage.verify_attachment().await?;
        Ok(storage)
    }

    /// Attach ordinary owner MAM to the sealed shared main lane. This uses
    /// the current owner credential and schema rather than an independent
    /// runtime handoff; it cannot accept an attested restricted handle.
    pub async fn attach_legacy_owner(
        database: &Database,
        clustering_enabled: bool,
        claims_live: bool,
    ) -> Result<Self, DatabaseMamStorageError> {
        if database.driver() != DatabaseDriver::Postgres
            || !database.is_main_lane()
            || database.ingress_pool_size().is_none()
            || database.uses_attested_postgres_pool()
        {
            return Err(DatabaseMamStorageError::OwnerMainDatabaseRequired);
        }
        let mut transaction = database.begin().await?;
        transaction
            .execute(
                "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
                (),
            )
            .await?;
        crate::db::read_only_timeout::bound_postgres_waits(&mut transaction)
            .await
            .map_err(|_| DatabaseMamStorageError::ReadBounds)?;
        let mut identity = transaction
            .query(
                "SELECT pg_catalog.current_schema()::text, CURRENT_USER::text",
                (),
            )
            .await?;
        let row = identity
            .next()
            .await?
            .ok_or(DatabaseMamStorageError::OwnerMainDatabaseRequired)?;
        let mam_schema = row.get::<String>(0)?;
        let mam_owner = PgRoleName::new(row.get::<String>(1)?)
            .map_err(|_| DatabaseMamStorageError::InvalidSchema)?;
        validate_identifier(&mam_schema, "MAM schema")
            .map_err(|_| DatabaseMamStorageError::InvalidSchema)?;
        #[cfg(feature = "clustering")]
        if clustering_enabled && claims_live {
            let mut claims = transaction
                .query(
                    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
                     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                     JOIN pg_catalog.pg_roles r ON r.oid = c.relowner \
                     WHERE n.nspname = ? AND c.relname = 'clustering_claims' \
                     AND c.relkind = 'r' AND c.relpersistence = 'p' AND r.rolname = ?)",
                    crate::db_params![mam_schema.clone(), mam_owner.as_str().to_owned()],
                )
                .await?;
            let present = claims
                .next()
                .await?
                .ok_or(DatabaseMamStorageError::UnsafeClaimPrivileges)?
                .get::<bool>(0)?;
            if !present {
                return Err(DatabaseMamStorageError::UnsafeClaimPrivileges);
            }
        }
        transaction.rollback().await?;
        let storage = Self {
            database: database.clone(),
            authority: MamAuthority::LegacyOwner {
                #[cfg(feature = "clustering")]
                fencing_enabled: clustering_enabled && claims_live,
            },
            mam_schema: mam_schema.clone(),
            mam_owner,
            #[cfg(feature = "clustering")]
            claim_schema: mam_schema,
        };
        #[cfg(not(feature = "clustering"))]
        let _ = (clustering_enabled, claims_live);
        storage.verify_attachment().await?;
        Ok(storage)
    }

    fn mam_schema(&self) -> &str {
        &self.mam_schema
    }

    #[cfg(feature = "clustering")]
    fn claim_schema(&self) -> &str {
        &self.claim_schema
    }

    async fn verify_attachment(&self) -> Result<(), DatabaseMamStorageError> {
        let mut transaction = self.begin_operation(false, false).await?;
        let verification = {
            let connection = transaction
                .postgres_connection()
                .ok_or(DatabaseMamStorageError::AttestedMainDatabaseRequired)?;
            SqlxMamStorage::verify_postgres_schema_on_connection(
                connection,
                self.mam_schema(),
                self.mam_owner.as_str(),
            )
            .await
        };
        // The verifier is a snapshot-only check. Never return an adapter if
        // the snapshot could not be closed successfully.
        transaction.rollback().await?;
        verification?;
        Ok(())
    }

    async fn begin_operation(
        &self,
        write: bool,
        check_claim_schema: bool,
    ) -> Result<Transaction<'_>, DatabaseMamStorageError> {
        match &self.authority {
            MamAuthority::Attested { capability, .. } => self
                .database
                .verify_attested_pg_writer_capability(capability)
                .map_err(DatabaseError::from)?,
            MamAuthority::LegacyOwner { .. } => {
                if self.database.uses_attested_postgres_pool() {
                    return Err(DatabaseMamStorageError::OwnerMainDatabaseRequired);
                }
            }
        }
        let mut transaction = self.database.begin().await?;
        let mode = if write { "READ WRITE" } else { "READ ONLY" };
        let second_isolation = if write {
            "READ COMMITTED"
        } else {
            "REPEATABLE READ"
        };
        transaction
            .execute(
                &format!("SET TRANSACTION ISOLATION LEVEL READ COMMITTED {mode}"),
                (),
            )
            .await?;
        transaction
            .execute(
                &format!("SET TRANSACTION ISOLATION LEVEL {second_isolation} {mode}"),
                (),
            )
            .await?;
        crate::db::read_only_timeout::bound_postgres_waits(&mut transaction)
            .await
            .map_err(|_| DatabaseMamStorageError::ReadBounds)?;

        #[cfg(feature = "clustering")]
        if check_claim_schema {
            match &self.authority {
                MamAuthority::Attested {
                    claim_authority, ..
                } if !claim_authority.belongs_to_main_pool(&self.database) => {
                    return Err(DatabaseMamStorageError::UnsafeClaimPrivileges);
                }
                MamAuthority::LegacyOwner {
                    fencing_enabled: false,
                } => return Err(DatabaseMamStorageError::UnsafeClaimPrivileges),
                _ => {}
            }
        }
        #[cfg(not(feature = "clustering"))]
        let _ = check_claim_schema;
        #[cfg(feature = "clustering")]
        if check_claim_schema {
            let claim_schema = quote_identifier(self.claim_schema());
            // Room writes take claim locks before archive locks. Preserve that
            // ordering here so a fenced MAM write cannot invert the room
            // mutation lock order.
            transaction
                .execute(
                    &format!(
                        "LOCK TABLE {claim_schema}.\"clustering_claims\" IN ACCESS SHARE MODE"
                    ),
                    (),
                )
                .await?;
        }
        let mam_schema = quote_identifier(self.mam_schema());
        for table in MAM_TABLES {
            transaction
                .execute(
                    &format!("LOCK TABLE {mam_schema}.\"{table}\" IN ACCESS SHARE MODE"),
                    (),
                )
                .await?;
        }

        if let MamAuthority::Attested { capability, .. } = &self.authority {
            capability.attest_transaction(&mut transaction).await?;
            let connection = transaction
                .postgres_connection()
                .ok_or(DatabaseMamStorageError::AttestedMainDatabaseRequired)?;
            verify_mam_writer_privileges(
                connection,
                self.mam_schema(),
                &self.mam_owner,
                capability.definer_role(),
                capability.installer_role(),
                None,
            )
            .await?;
        }
        Ok(transaction)
    }

    async fn finish<T>(
        &self,
        transaction: Transaction<'_>,
        operation: Result<T, MamStorageError>,
    ) -> Result<T, MamStorageError> {
        match operation {
            Ok(value) => {
                transaction
                    .commit()
                    .await
                    .map_err(|error| MamStorageError::Database(error.to_string()))?;
                Ok(value)
            }
            Err(error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|rollback| MamStorageError::Database(rollback.to_string()))?;
                Err(error)
            }
        }
    }
}

async fn verify_v2_mam_graph(
    tx: &mut Transaction<'_>,
    identity: &crate::db::lineage::PgIdentity,
    definer: &PgRoleName,
    profile: &crate::pubsub::RestrictedServingProfileV2,
) -> Result<(), DatabaseMamStorageError> {
    crate::pubsub::runtime_attach::verify_restricted_v2_graph_tx(
        tx,
        identity,
        definer.as_str(),
        profile,
    )
    .await
    .map_err(|_| DatabaseMamStorageError::UnsafeMamPrivileges)?;
    for (table, oid) in MAM_TABLES.into_iter().zip(profile.mam_relation_oids) {
        crate::pubsub::runtime_attach::verify_v2_relation_tx(tx, identity, table, oid)
            .await
            .map_err(|_| DatabaseMamStorageError::UnsafeMamPrivileges)?;
    }
    Ok(())
}

macro_rules! run_operation {
    ($storage:expr, $write:expr, $check_claim:expr, $connection:ident => $body:block) => {{
        let mut transaction = $storage
            .begin_operation($write, $check_claim)
            .await
            .map_err(|error| MamStorageError::Database(error.to_string()))?;
        let operation = {
            let $connection = transaction.postgres_connection().ok_or_else(|| {
                MamStorageError::Database(
                    "MAM operation lost its pinned PostgreSQL connection".to_owned(),
                )
            })?;
            $body
        };
        $storage.finish(transaction, operation).await
    }};
}

#[async_trait]
impl MamStorage for DatabaseMamStorage {
    async fn store_message(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<StoreOutcome, MamStorageError> {
        run_operation!(self, true, false, connection => {
            SqlxMamStorage::store_message_on_postgres_connection_in_schema(
                connection,
                self.mam_schema(),
                archive_jid,
                message,
            )
            .await
        })
    }

    async fn store_message_fenced(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
        fence: &RoomClaimFenceContext,
    ) -> Result<StoreOutcome, MamStorageError> {
        #[cfg(feature = "clustering")]
        {
            if matches!(
                &self.authority,
                MamAuthority::LegacyOwner {
                    fencing_enabled: false
                }
            ) {
                return self.store_message(archive_jid, message).await;
            }
            run_operation!(self, true, true, connection => {
                SqlxMamStorage::store_message_fenced_on_postgres_connection_in_schema(
                    connection,
                    self.mam_schema(),
                    self.claim_schema(),
                    archive_jid,
                    message,
                    fence,
                )
                .await
            })
        }
        #[cfg(not(feature = "clustering"))]
        {
            let _ = fence;
            self.store_message(archive_jid, message).await
        }
    }

    async fn query_messages(
        &self,
        archive_jid: &BareJid,
        archive_kind: MamArchiveKind,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError> {
        run_operation!(self, false, false, connection => {
            SqlxMamStorage::query_on_postgres_connection_in_schema(
                connection,
                self.mam_schema(),
                archive_jid,
                archive_kind,
                query,
            )
            .await
        })
    }

    async fn get_message(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        run_operation!(self, false, false, connection => {
            SqlxMamStorage::get_message_on_postgres_connection_in_schema(
                connection,
                self.mam_schema(),
                archive_id,
            )
            .await
        })
    }

    async fn replace_with_tombstone(
        &self,
        archive_id: &str,
        tombstone: ArchivedTombstone,
    ) -> Result<bool, MamStorageError> {
        run_operation!(self, true, false, connection => {
            SqlxMamStorage::replace_with_tombstone_on_postgres_connection_in_schema(
                connection,
                self.mam_schema(),
                archive_id,
                tombstone,
            )
            .await
        })
    }

    async fn replace_with_terminal_tombstone(
        &self,
        archive_id: &str,
        tombstone: ArchivedTombstone,
    ) -> Result<TerminalTombstoneOutcome, MamStorageError> {
        run_operation!(self, true, false, connection => {
            SqlxMamStorage::replace_with_terminal_tombstone_on_postgres_connection_in_schema(
                connection,
                self.mam_schema(),
                archive_id,
                tombstone,
            )
            .await
        })
    }

    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        run_operation!(self, false, false, connection => {
            SqlxMamStorage::get_message_by_stanza_id_on_postgres_connection_in_schema(
                connection,
                self.mam_schema(),
                archive_jid,
                stanza_id,
            )
            .await
        })
    }

    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        run_operation!(self, false, false, connection => {
            SqlxMamStorage::get_message_by_message_id_on_postgres_connection_in_schema(
                connection,
                self.mam_schema(),
                archive_jid,
                message_id,
            )
            .await
        })
    }

    async fn get_message_by_sender_and_origin_id(
        &self,
        archive_jid: &BareJid,
        archive_kind: MamArchiveKind,
        sender: &Jid,
        origin_id: &OriginId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        run_operation!(self, false, false, connection => {
            SqlxMamStorage::get_message_by_sender_and_origin_id_on_postgres_connection_in_schema(
                connection,
                self.mam_schema(),
                archive_jid,
                archive_kind,
                sender,
                origin_id,
            )
            .await
        })
    }

    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        run_operation!(self, false, false, connection => {
            SqlxMamStorage::get_message_by_archive_or_stanza_id_on_postgres_connection_in_schema(
                connection,
                self.mam_schema(),
                archive_jid,
                stanza_id,
            )
            .await
        })
    }

    async fn count_messages(&self, archive_jid: &BareJid) -> Result<u32, MamStorageError> {
        run_operation!(self, false, false, connection => {
            SqlxMamStorage::count_messages_on_postgres_connection_in_schema(
                connection,
                self.mam_schema(),
                archive_jid,
            )
            .await
        })
    }
}

async fn verify_mam_writer_privileges(
    connection: &mut PgConnection,
    schema: &str,
    owner: &PgRoleName,
    definer: &PgRoleName,
    installer: &PgRoleName,
    profile: Option<&crate::pubsub::RestrictedServingProfileV2>,
) -> Result<(), DatabaseMamStorageError> {
    let query = format!(
        r#"WITH runtime AS (
             SELECT oid, rolname, rolcanlogin, rolsuper, rolcreaterole, rolcreatedb,
                    rolreplication, rolbypassrls
             FROM pg_catalog.pg_roles WHERE rolname = current_user
           ), owner AS (
             SELECT oid, rolname FROM pg_catalog.pg_roles WHERE rolname = $2
           ), definer AS (
             SELECT oid, rolcanlogin, rolsuper, rolcreaterole, rolcreatedb,
                    rolreplication, rolbypassrls
             FROM pg_catalog.pg_roles WHERE rolname = $3
           ), installer AS (
             SELECT oid FROM pg_catalog.pg_roles WHERE rolname = $4
           ), target AS (
             SELECT oid, nspname, nspowner, nspacl FROM pg_catalog.pg_namespace WHERE nspname = $1
           ), relations AS (
             SELECT c.*, n.oid AS namespace_oid, n.nspowner
             FROM pg_catalog.pg_class c
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relname IN ('mam_messages', 'mam_archive_sequences')
           )
           SELECT
             (SELECT count(*) FROM relations) = 2
             AND COALESCE((
               SELECT session_user::text = r.rolname AND current_user::text = r.rolname
                 AND r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreaterole
                 AND NOT r.rolcreatedb AND NOT r.rolreplication AND NOT r.rolbypassrls
                 AND r.oid <> d.datdba AND r.oid <> o.oid
                 AND o.rolname = $2
                 AND f.oid <> r.oid AND f.oid <> o.oid
                 AND NOT f.rolcanlogin AND NOT f.rolsuper AND NOT f.rolcreaterole
                 AND NOT f.rolcreatedb AND NOT f.rolreplication AND NOT f.rolbypassrls
                 AND EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                             WHERE m.member = i.oid AND m.roleid = f.oid
                               AND m.set_option AND NOT m.inherit_option AND NOT m.admin_option)
                 AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                                 WHERE m.member IN (f.oid, r.oid)
                                    OR (m.member = i.oid AND m.roleid <> f.oid)
                                    OR m.roleid IN (i.oid, r.oid)
                                    OR (m.roleid = f.oid AND m.member <> i.oid))
                 AND pg_catalog.has_schema_privilege(r.oid, n.oid, 'USAGE')
                 AND NOT pg_catalog.has_schema_privilege(r.oid, n.oid, 'CREATE')
                 AND NOT pg_catalog.has_schema_privilege(f.oid, n.oid, 'CREATE')
                 AND NOT pg_catalog.has_database_privilege(r.oid, d.oid, 'CREATE,TEMP')
                 AND NOT pg_catalog.has_database_privilege(f.oid, d.oid, 'CREATE,TEMP')
                 AND NOT pg_catalog.has_parameter_privilege(r.oid, 'session_replication_role', 'SET')
                 AND pg_catalog.current_setting('session_replication_role') = 'origin'
                 AND NOT EXISTS (
                   SELECT 1 FROM pg_catalog.aclexplode(
                     COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))) a
                   WHERE a.grantee <> n.nspowner
                     AND (a.grantee NOT IN (r.oid, f.oid,
                              COALESCE($5::bigint::oid,r.oid), COALESCE($6::bigint::oid,r.oid),
                              CASE WHEN $5::bigint IS NULL THEN r.oid ELSE pg_catalog.to_regrole('pg_monitor') END)
                          OR a.is_grantable OR a.privilege_type <> 'USAGE'))
               FROM runtime r CROSS JOIN owner o CROSS JOIN definer f
               CROSS JOIN installer i CROSS JOIN target n
               JOIN pg_catalog.pg_database d ON d.datname = pg_catalog.current_database()
             ), false)
             AND COALESCE((
               SELECT pg_catalog.bool_and(
                 c.relkind = 'r' AND c.relpersistence = 'p'
                 AND c.relowner = c.nspowner
                 AND c.relowner = o.oid
                 AND NOT c.relispartition AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity
                 AND c.relam = (SELECT oid FROM pg_catalog.pg_am WHERE amname = 'heap')
                 AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t
                                 WHERE t.tgrelid = c.oid AND NOT t.tgisinternal)
                 AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw WHERE rw.ev_class = c.oid)
                 AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i
                                 WHERE i.inhrelid = c.oid OR i.inhparent = c.oid)
                 AND pg_catalog.has_table_privilege(r.oid, c.oid, 'SELECT')
                 AND pg_catalog.has_table_privilege(r.oid, c.oid, 'INSERT')
                 AND NOT pg_catalog.has_table_privilege(r.oid, c.oid,
                       'UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER')
                 AND CASE c.relname
                   WHEN 'mam_messages' THEN
                     (SELECT count(*) = 7 FROM pg_catalog.pg_attribute a
                      WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
                        AND a.attname IN ({MESSAGE_TOMBSTONE_UPDATE_COLUMNS})
                        AND pg_catalog.has_column_privilege(r.oid, c.oid, a.attnum, 'UPDATE'))
                     AND NOT EXISTS (
                       SELECT 1 FROM pg_catalog.pg_attribute a
                       WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
                         AND pg_catalog.has_column_privilege(r.oid, c.oid, a.attnum, 'UPDATE')
                         AND a.attname NOT IN ({MESSAGE_TOMBSTONE_UPDATE_COLUMNS}))
                   WHEN 'mam_archive_sequences' THEN
                     pg_catalog.has_column_privilege(r.oid, c.oid, 'next_seq', 'UPDATE')
                     AND NOT EXISTS (
                       SELECT 1 FROM pg_catalog.pg_attribute a
                       WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
                         AND pg_catalog.has_column_privilege(r.oid, c.oid, a.attnum, 'UPDATE')
                         AND a.attname <> 'next_seq')
                   ELSE false
                 END
                 AND NOT pg_catalog.has_table_privilege(r.oid, c.oid,
                       'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION, UPDATE WITH GRANT OPTION, DELETE WITH GRANT OPTION, TRUNCATE WITH GRANT OPTION, REFERENCES WITH GRANT OPTION, TRIGGER WITH GRANT OPTION')
                 AND NOT pg_catalog.has_any_column_privilege(r.oid, c.oid,
                       'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION, UPDATE WITH GRANT OPTION, REFERENCES WITH GRANT OPTION')
                 AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000
                       THEN NOT pg_catalog.has_table_privilege(r.oid, c.oid, 'MAINTAIN') ELSE true END
               )
               FROM relations c CROSS JOIN runtime r CROSS JOIN owner o
             ), false)
             AND NOT EXISTS (
               SELECT 1 FROM relations c CROSS JOIN runtime r
               CROSS JOIN LATERAL pg_catalog.aclexplode(
                 COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) a
               WHERE a.grantee <> c.relowner
                 AND (a.grantee <> r.oid OR a.is_grantable
                   OR a.privilege_type NOT IN ('SELECT', 'INSERT')))
             AND NOT EXISTS (
               SELECT 1 FROM relations c CROSS JOIN runtime r
               JOIN pg_catalog.pg_attribute col ON col.attrelid = c.oid
               CROSS JOIN LATERAL pg_catalog.aclexplode(
                 COALESCE(col.attacl, pg_catalog.acldefault('r', c.relowner))) a
               WHERE col.attnum > 0 AND NOT col.attisdropped
                 AND a.grantee <> c.relowner
                 AND (a.grantee <> r.oid OR a.is_grantable OR a.privilege_type <> 'UPDATE'
                   OR (c.relname = 'mam_messages'
                       AND col.attname NOT IN ({MESSAGE_TOMBSTONE_UPDATE_COLUMNS}))
                   OR (c.relname = 'mam_archive_sequences' AND col.attname <> 'next_seq')))
             AND NOT EXISTS (
               SELECT 1 FROM pg_catalog.aclexplode(
                 COALESCE((SELECT nspacl FROM target),
                          pg_catalog.acldefault('n', (SELECT nspowner FROM target)))
               ) a CROSS JOIN runtime r CROSS JOIN definer f CROSS JOIN target n
               WHERE a.grantee <> n.nspowner AND a.grantee NOT IN (r.oid, f.oid,
                   COALESCE($5::bigint::oid,r.oid), COALESCE($6::bigint::oid,r.oid),
                   CASE WHEN $5::bigint IS NULL THEN r.oid ELSE pg_catalog.to_regrole('pg_monitor') END))
           "#,
    );
    let safe: bool = sqlx::query_scalar(&query)
        .bind(schema)
        .bind(owner.as_str())
        .bind(definer.as_str())
        .bind(installer.as_str())
        .bind(profile.map(|p| i64::from(p.v1.api_oid)))
        .bind(profile.map(|p| i64::from(p.v1.helper_oid)))
        .fetch_one(connection)
        .await
        .map_err(|error| DatabaseMamStorageError::Database(DatabaseError::from(error)))?;
    if safe {
        Ok(())
    } else {
        Err(DatabaseMamStorageError::UnsafeMamPrivileges)
    }
}
