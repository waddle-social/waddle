//! Dark MAM adapters bound to an attested PostgreSQL database.
//!
//! `DarkMamReader` provides read-only snapshots, while `DatabaseMamStorage`
//! implements `MamStorage` on the same admitted main-database transactions.
//! Neither adapter is wired into startup or protocol routing. Trusted writers,
//! owner DDL, role membership, and ACL changes must be quiescent while attached.

use crate::db::physical_attestation::{
    quote_identifier, AttestedPostgresWriterCapability, PgRoleName,
};
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};
use jid::BareJid;
use waddle_xmpp::mam::{
    ArchivedMessage, MamArchiveKind, MamQuery, MamResult, MamSchemaVerificationError,
    MamStorageError, SqlxMamStorage,
};
use waddle_xmpp_core::xep0359::StanzaId;

mod database_storage;
pub use database_storage::{DatabaseMamStorage, DatabaseMamStorageError};

/// Failure to attach or read through the dark MAM adapter.
#[derive(Debug, thiserror::Error)]
pub enum DarkMamReadError {
    #[error("MAM reader requires an attested PostgreSQL main-lane database")]
    AttestedMainDatabaseRequired,
    #[error("MAM runtime role or table privileges are unsafe")]
    UnsafePrivileges,
    #[error("could not bound MAM snapshot waits")]
    ReadBounds,
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Schema(#[from] MamSchemaVerificationError),
    #[error(transparent)]
    Mam(#[from] MamStorageError),
}

/// Read-only MAM operations on the same checked-out pool as `database`.
///
/// The owner comes from an independent trusted provisioning handoff, not from
/// the database being inspected. This type exposes no pool, URL, connection,
/// or generic SQL access. It makes no claim that other stores use this handle.
/// Trusted writers, DDL, and role/ACL changes must be quiescent for each read;
/// this is not yet suitable for activation in a running deployment.
pub struct DarkMamReader {
    database: Database,
    capability: AttestedPostgresWriterCapability,
    owner: PgRoleName,
}

impl DarkMamReader {
    /// Verify the owner-prepared archive under a bounded read-only snapshot.
    /// SQLite, unattested PostgreSQL, and non-main admission lanes fail closed.
    pub async fn attach(database: &Database, owner: PgRoleName) -> Result<Self, DarkMamReadError> {
        if database.driver() != DatabaseDriver::Postgres || !database.is_main_lane() {
            return Err(DarkMamReadError::AttestedMainDatabaseRequired);
        }
        let capability = database
            .attested_pg_writer_capability()
            .ok_or(DarkMamReadError::AttestedMainDatabaseRequired)?;
        database
            .verify_attested_pg_writer_capability(&capability)
            .map_err(DatabaseError::from)?;
        let reader = Self {
            database: database.clone(),
            capability,
            owner,
        };
        let tx = reader.verified_snapshot().await?;
        tx.rollback().await?;
        Ok(reader)
    }

    /// Query one archive without opening another pool or changing its schema.
    /// Cursor resolution, result count, and page are read in one snapshot.
    pub async fn query(
        &self,
        archive: &BareJid,
        kind: MamArchiveKind,
        query: &MamQuery,
    ) -> Result<MamResult, DarkMamReadError> {
        let mut tx = self.verified_snapshot().await?;
        let schema = &self.capability.identity().schema.name.0;
        let connection = tx
            .postgres_connection()
            .ok_or(DarkMamReadError::AttestedMainDatabaseRequired)?;
        let result = SqlxMamStorage::query_on_postgres_connection_in_schema(
            connection, schema, archive, kind, query,
        )
        .await;
        // An unobserved rollback cannot produce a successful query response.
        tx.rollback().await?;
        Ok(result?)
    }

    /// Resolve an archive-scoped stanza ID against this same verified handle.
    pub async fn lookup(&self, id: &StanzaId) -> Result<Option<ArchivedMessage>, DarkMamReadError> {
        let mut tx = self.verified_snapshot().await?;
        let schema = &self.capability.identity().schema.name.0;
        let connection = tx
            .postgres_connection()
            .ok_or(DarkMamReadError::AttestedMainDatabaseRequired)?;
        let result = SqlxMamStorage::lookup_archive_or_stanza_id_on_postgres_connection_in_schema(
            connection, schema, id,
        )
        .await;
        tx.rollback().await?;
        Ok(result?)
    }

    async fn verified_snapshot(&self) -> Result<Transaction<'_>, DarkMamReadError> {
        self.database
            .verify_attested_pg_writer_capability(&self.capability)
            .map_err(DatabaseError::from)?;
        let mut tx = self.database.begin().await?;
        // Changing isolation twice before any snapshot rejects a raw, already
        // used transaction left behind by a previous pooled borrower.
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
            (),
        )
        .await?;
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
            (),
        )
        .await?;
        crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
            .await
            .map_err(|_| DarkMamReadError::ReadBounds)?;
        let schema = &self.capability.identity().schema.name.0;
        let quoted = quote_identifier(schema);
        // SHOW/SET LOCAL above do not create the snapshot. Lock the exact
        // relations before the attestor's first SELECT pins it.
        for table in ["mam_messages", "mam_archive_sequences"] {
            tx.execute(
                &format!("LOCK TABLE {quoted}.\"{table}\" IN ACCESS SHARE MODE"),
                (),
            )
            .await?;
        }
        self.capability.attest_transaction(&mut tx).await?;
        let connection = tx
            .postgres_connection()
            .ok_or(DarkMamReadError::AttestedMainDatabaseRequired)?;
        SqlxMamStorage::verify_postgres_schema_on_connection(
            connection,
            schema,
            self.owner.as_str(),
        )
        .await?;
        verify_read_privileges(connection, schema, &self.owner).await?;
        Ok(tx)
    }
}

async fn verify_read_privileges(
    connection: &mut sqlx::PgConnection,
    schema: &str,
    owner: &PgRoleName,
) -> Result<(), DarkMamReadError> {
    let valid: bool = sqlx::query_scalar(
        r#"SELECT COALESCE((
             SELECT r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreaterole AND NOT r.rolcreatedb
                AND NOT r.rolreplication AND NOT r.rolbypassrls AND r.oid <> o.oid
                AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                                WHERE m.member = r.oid OR m.roleid = r.oid)
                AND n.nspowner = o.oid
                AND pg_catalog.has_schema_privilege(r.oid, n.oid, 'USAGE')
                AND NOT pg_catalog.has_schema_privilege(r.oid, n.oid, 'CREATE')
                AND NOT EXISTS (
                    SELECT 1 FROM pg_catalog.aclexplode(
                        COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))) a
                    WHERE a.grantee <> o.oid
                      AND (a.privilege_type <> 'USAGE' OR a.is_grantable))
                AND NOT pg_catalog.has_database_privilege(r.rolname, pg_catalog.current_database(), 'CREATE,TEMP')
                AND NOT pg_catalog.has_parameter_privilege(r.oid, 'session_replication_role', 'SET')
                AND pg_catalog.current_setting('session_replication_role') = 'origin'
                AND (SELECT COUNT(*) FROM pg_catalog.pg_class c
                     WHERE c.relnamespace = n.oid
                       AND c.relname IN ('mam_messages', 'mam_archive_sequences')
                       AND c.relowner = o.oid
                       AND pg_catalog.has_table_privilege(r.oid, c.oid, 'SELECT')) = 2
                AND NOT EXISTS (
                    SELECT 1 FROM pg_catalog.pg_class c
                    CROSS JOIN LATERAL pg_catalog.aclexplode(
                        COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) a
                    WHERE c.relnamespace = n.oid
                      AND c.relname IN ('mam_messages', 'mam_archive_sequences')
                      AND a.grantee <> o.oid
                      AND (a.grantee <> r.oid OR a.is_grantable
                           OR a.privilege_type NOT IN ('SELECT', 'INSERT')))
                AND NOT EXISTS (
                    SELECT 1 FROM pg_catalog.pg_class c
                    JOIN pg_catalog.pg_attribute col ON col.attrelid = c.oid
                    CROSS JOIN LATERAL pg_catalog.aclexplode(col.attacl) a
                    WHERE c.relnamespace = n.oid
                      AND c.relname IN ('mam_messages', 'mam_archive_sequences')
                      AND col.attnum > 0 AND NOT col.attisdropped
                      AND a.grantee <> o.oid
                      AND (a.grantee <> r.oid OR a.is_grantable
                           OR c.relname <> 'mam_archive_sequences'
                           OR col.attname <> 'next_seq' OR a.privilege_type <> 'UPDATE'))
             FROM pg_catalog.pg_roles r
             JOIN pg_catalog.pg_roles o ON o.rolname = $2
             JOIN pg_catalog.pg_namespace n ON n.nspname = $1
             WHERE r.rolname = current_user
           ), false)"#,
    )
    .bind(schema)
    .bind(owner.as_str())
    .fetch_one(connection)
    .await
    .map_err(DatabaseError::from)?;
    if valid {
        Ok(())
    } else {
        Err(DarkMamReadError::UnsafePrivileges)
    }
}
