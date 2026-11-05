//! Opt-in capability for the dark candidate relation-lock writer path.

use std::time::Duration;

use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::config::LineageConfig;
use crate::db::physical_attestation::{
    AttestedPostgresWriterCapability, PgRoleName, PhysicalAttestationError,
};
use crate::db::{
    self, DarkMigrationOwnerConfig, DarkMigrationOwnerError, Database, DatabaseDriver,
    DatabaseError, Transaction,
};
use crate::ingress_substrate::quote_postgres_identifier;

const OWNER_MARKER_TABLE: &str = "waddle_epoch_transition_lab_marker";
const OWNER_MARKER_VERSION: &str = "relation-guard-lab-1";
const VERIFY_DEADLINE: Duration = Duration::from_secs(15);
const LOCK_TIMEOUT: &str = "3s";
const STATEMENT_TIMEOUT: &str = "5s";
const CANDIDATE_GUARD_SQL: &str =
    include_str!("../db/migrations/owner/ingress_epoch_relation_guard_candidate.sql");

/// Read-only verification errors for the explicitly selected candidate path.
#[derive(Debug, Error)]
pub enum RelationGuardCapabilityError {
    #[error("relation-guard capability requires PostgreSQL")]
    PostgresRequired,
    #[error("relation-guard capability requires this database's attested pool")]
    AttestedPoolRequired,
    #[error("relation-guard verification requires a main lane with a reserved ingress lane and two pool slots")]
    VerifierLanesRequired,
    #[error("relation-guard capability does not belong to this attested pool")]
    WrongPool,
    #[error("relation-guard candidate, ACLs, or owner marker did not match")]
    VerificationFailed,
    #[error("relation-guard capability verification timed out")]
    Timeout,
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Catalog(#[from] DarkMigrationOwnerError),
    #[error(transparent)]
    Role(#[from] PhysicalAttestationError),
}

/// Opaque, verifier-issued permission to explicitly select relation-lock
/// preparation on the ordinary ingress UOW. It is bound to one physically
/// attested PostgreSQL pool, runtime role, schema, and owner marker receipt.
#[derive(Clone)]
pub struct RelationGuardCapability {
    writer: AttestedPostgresWriterCapability,
    owner_role: PgRoleName,
    schema_name: String,
    schema_oid: u32,
    marker_attempt: Uuid,
    marker_checksum: String,
}

impl RelationGuardCapability {
    pub(crate) async fn verify(
        database: &Database,
        lineage: &LineageConfig,
    ) -> Result<Self, RelationGuardCapabilityError> {
        tokio::time::timeout(VERIFY_DEADLINE, Self::verify_inner(database, lineage))
            .await
            .map_err(|_| RelationGuardCapabilityError::Timeout)?
    }

    async fn verify_inner(
        database: &Database,
        lineage: &LineageConfig,
    ) -> Result<Self, RelationGuardCapabilityError> {
        if database.driver() != DatabaseDriver::Postgres {
            return Err(RelationGuardCapabilityError::PostgresRequired);
        }
        let writer = database
            .attested_pg_writer_capability()
            .ok_or(RelationGuardCapabilityError::AttestedPoolRequired)?;
        database
            .verify_attested_pg_writer_capability(&writer)
            .map_err(|_| RelationGuardCapabilityError::WrongPool)?;
        let snapshot_database = database
            .relation_guard_snapshot_lane()
            .ok_or(RelationGuardCapabilityError::VerifierLanesRequired)?;
        let expected_lineage = writer.attested_lineage_for_deployment(lineage.deployment_uuid)?;
        let identity = writer.identity();
        let schema = identity.schema.name.0.clone();
        let schema_oid = identity.schema.oid;
        let runtime_role = writer.runtime_role().clone();
        let quoted_schema = quote_postgres_identifier(&schema);
        let marker_relation = format!("{quoted_schema}.{OWNER_MARKER_TABLE}");
        let epoch_relation = format!("{quoted_schema}.ingress_protocol_epoch");

        // Retain migration locks and relation barriers on a main-lane lease,
        // then obtain the RR snapshot on the reserved ingress lane. A single
        // RR transaction cannot safely acquire both: its first advisory-lock
        // SELECT fixes the snapshot before any lock wait, so it could inspect
        // pre-commit catalog/marker state after an owner transition commits.
        let mut lock_transaction = database.begin().await?;
        lock_transaction
            .execute(
                "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
                (),
            )
            .await?;
        lock_transaction
            .execute(&format!("SET LOCAL lock_timeout = '{LOCK_TIMEOUT}'"), ())
            .await?;
        lock_transaction
            .execute(
                &format!("SET LOCAL statement_timeout = '{STATEMENT_TIMEOUT}'"),
                (),
            )
            .await?;

        // Match the owner transition's lock order before taking the epoch
        // barrier and fixing the repeatable-read catalog/marker snapshot.
        for key in [
            crate::db::lineage::LINEAGE_ADVISORY_LOCK_KEY,
            crate::db::MIGRATION_LEDGER_ADVISORY_LOCK_KEY,
        ] {
            let mut rows = lock_transaction
                .query(
                    "SELECT pg_catalog.pg_advisory_xact_lock(?)",
                    crate::db_params![key],
                )
                .await?;
            if rows.next().await?.is_none() {
                return Err(RelationGuardCapabilityError::VerificationFailed);
            }
        }
        lock_transaction
            .execute(
                &format!("LOCK TABLE ONLY {epoch_relation} IN ACCESS SHARE MODE"),
                (),
            )
            .await?;
        // The runtime has SELECT on the marker only. The migration advisory
        // locks above serialize owner preparation; ACCESS SHARE also matches
        // the existing owner-transition observer's runtime lock requirement.
        lock_transaction
            .execute(
                &format!("LOCK TABLE ONLY {marker_relation} IN ACCESS SHARE MODE"),
                (),
            )
            .await?;

        let mut transaction = snapshot_database.begin().await?;
        transaction
            .execute(
                "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
                (),
            )
            .await?;
        transaction
            .execute(&format!("SET LOCAL lock_timeout = '{LOCK_TIMEOUT}'"), ())
            .await?;
        transaction
            .execute(
                &format!("SET LOCAL statement_timeout = '{STATEMENT_TIMEOUT}'"),
                (),
            )
            .await?;
        transaction
            .execute(
                &format!("LOCK TABLE ONLY {epoch_relation} IN ACCESS SHARE MODE"),
                (),
            )
            .await?;
        transaction
            .execute(
                &format!("LOCK TABLE ONLY {marker_relation} IN ACCESS SHARE MODE"),
                (),
            )
            .await?;

        writer.attest_transaction(&mut transaction).await?;
        let (owner_role, observed_schema_oid, current_schema): (String, String, Option<String>) = {
            let mut rows = transaction
                .query(
                    "SELECT owner.rolname::text, n.oid::text, pg_catalog.current_schema()::text \
                     FROM pg_catalog.pg_namespace n \
                     JOIN pg_catalog.pg_roles owner ON owner.oid = n.nspowner \
                     WHERE n.nspname = ?",
                    crate::db_params![schema.clone()],
                )
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or(RelationGuardCapabilityError::VerificationFailed)?;
            (row.get(0)?, row.get(1)?, row.get(2)?)
        };
        if observed_schema_oid.parse::<u32>().ok() != Some(schema_oid)
            || current_schema.as_deref() != Some(schema.as_str())
        {
            return Err(RelationGuardCapabilityError::VerificationFailed);
        }
        let owner_role = PgRoleName::new(owner_role)?;
        let config =
            DarkMigrationOwnerConfig::new(expected_lineage, owner_role.clone(), runtime_role)?;
        db::verify_relation_guard_candidate_in_transaction(
            &mut transaction,
            &config,
            &schema,
            &quoted_schema,
        )
        .await?;
        let marker_checksum = hex::encode(Sha256::digest(CANDIDATE_GUARD_SQL.as_bytes()));
        let marker_attempt = verify_owner_marker(
            &mut transaction,
            &owner_role,
            writer.runtime_role(),
            &schema,
            &marker_checksum,
        )
        .await?;
        transaction.commit().await?;
        lock_transaction.commit().await?;

        Ok(Self {
            writer,
            owner_role,
            schema_name: schema,
            schema_oid,
            marker_attempt,
            marker_checksum,
        })
    }

    pub(crate) fn verify_for_database(
        &self,
        database: &Database,
        writer: &AttestedPostgresWriterCapability,
    ) -> Result<(), RelationGuardCapabilityError> {
        database
            .verify_attested_pg_writer_capability(&self.writer)
            .map_err(|_| RelationGuardCapabilityError::WrongPool)?;
        if !self.writer.belongs_to(writer)
            || self.writer.identity().schema.name.0 != self.schema_name
            || self.writer.identity().schema.oid != self.schema_oid
        {
            return Err(RelationGuardCapabilityError::WrongPool);
        }
        Ok(())
    }

    pub(crate) async fn prepare_transaction(
        &self,
        transaction: &mut Transaction<'_>,
    ) -> Result<crate::ingress_substrate::PreparedAttestedReceiptWriter, DatabaseError> {
        let quoted_schema = quote_postgres_identifier(&self.schema_name);
        transaction
            .execute(
                &format!(
                    "LOCK TABLE ONLY {quoted_schema}.ingress_protocol_epoch IN ACCESS SHARE MODE"
                ),
                (),
            )
            .await?;
        transaction
            .execute(
                &format!(
                    "LOCK TABLE ONLY {quoted_schema}.{OWNER_MARKER_TABLE} IN ACCESS SHARE MODE"
                ),
                (),
            )
            .await?;
        self.writer.attest_transaction(transaction).await?;
        verify_owner_marker(
            transaction,
            &self.owner_role,
            self.writer.runtime_role(),
            &self.schema_name,
            &self.marker_checksum,
        )
        .await
        .map_err(|_| DatabaseError::QueryFailed("relation-guard marker changed".to_owned()))
        .and_then(|attempt| {
            if attempt == self.marker_attempt {
                Ok(attempt)
            } else {
                Err(DatabaseError::QueryFailed(
                    "relation-guard marker changed".to_owned(),
                ))
            }
        })?;

        let mut search_path = transaction
            .query(
                "SELECT pg_catalog.set_config('search_path', ?, true)",
                crate::db_params![format!("{quoted_schema}, pg_temp")],
            )
            .await?;
        if search_path.next().await?.is_none() {
            return Err(DatabaseError::QueryFailed(
                "relation-guard search path was not installed".to_owned(),
            ));
        }
        drop(search_path);

        let connection = transaction.postgres_connection().ok_or_else(|| {
            DatabaseError::QueryFailed("relation-guard requires PostgreSQL".to_owned())
        })?;
        let epoch = crate::db::physical_attestation::read_epoch_after_prelock(
            connection,
            &self.schema_name,
            self.schema_oid,
        )
        .await
        .map_err(|_| DatabaseError::QueryFailed("relation-guard epoch proof failed".to_owned()))?;
        Ok(
            crate::ingress_substrate::PreparedAttestedReceiptWriter::for_relation_guard(
                &self.writer,
                epoch,
            ),
        )
    }
}

async fn verify_owner_marker(
    transaction: &mut Transaction<'_>,
    owner: &PgRoleName,
    runtime: &PgRoleName,
    schema: &str,
    checksum: &str,
) -> Result<Uuid, RelationGuardCapabilityError> {
    let relation = format!(
        "{}.{}",
        quote_postgres_identifier(schema),
        OWNER_MARKER_TABLE
    );
    let mut shape = transaction
        .query(
            r#"SELECT c.relkind = 'r' AND c.relpersistence = 'p' AND c.relowner = owning_role.oid
                  AND NOT c.relispartition AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity
                  AND NOT c.relhasrules
                  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid)
                  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i
                                  WHERE i.inhrelid = c.oid OR i.inhparent = c.oid)
                  AND (SELECT count(*) FROM pg_catalog.pg_attribute a
                       WHERE a.attrelid = c.oid AND a.attnum > 0) = 3
                  AND (SELECT count(*) FROM pg_catalog.pg_attribute a
                       WHERE a.attrelid = c.oid AND NOT a.attisdropped AND a.attnotnull AND NOT a.atthasdef
                         AND a.attgenerated = '' AND a.attidentity = ''
                         AND ((a.attnum = 1 AND a.attname = 'version'
                               AND a.atttypid = 'pg_catalog.text'::regtype)
                           OR (a.attnum = 2 AND a.attname = 'checksum'
                               AND a.atttypid = 'pg_catalog.text'::regtype)
                           OR (a.attnum = 3 AND a.attname = 'attempt_id'
                               AND a.atttypid = 'pg_catalog.uuid'::regtype))) = 3
                  AND (SELECT count(*) FROM pg_catalog.pg_constraint x WHERE x.conrelid = c.oid) = 1
                  AND (SELECT count(*) FROM pg_catalog.pg_index i WHERE i.indrelid = c.oid) = 1
                  AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x
                              JOIN pg_catalog.pg_index i ON i.indexrelid = x.conindid
                              WHERE x.conrelid = c.oid AND x.contype = 'p' AND x.convalidated
                                AND NOT x.condeferrable AND NOT x.condeferred
                                AND x.conkey = ARRAY[1]::smallint[] AND i.indisvalid
                                AND i.indisready AND i.indisunique AND i.indisprimary
                                AND i.indnkeyatts = 1 AND i.indnatts = 1
                                AND i.indexprs IS NULL AND i.indpred IS NULL)
                  AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
                                      coalesce(c.relacl, pg_catalog.acldefault('r', c.relowner))
                                  ) a WHERE a.grantee NOT IN (owning_role.oid, runtime.oid)
                                       OR (a.grantee = runtime.oid
                                           AND (a.privilege_type <> 'SELECT' OR a.is_grantable)))
                  AND EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
                                  coalesce(c.relacl, pg_catalog.acldefault('r', c.relowner))
                              ) a WHERE a.grantee = runtime.oid AND a.privilege_type = 'SELECT'
                                       AND NOT a.is_grantable)
                  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute col
                                  CROSS JOIN LATERAL pg_catalog.aclexplode(col.attacl) a
                                  WHERE col.attrelid = c.oid AND a.grantee <> owning_role.oid)
                  AND NOT owning_role.rolcanlogin AND NOT owning_role.rolsuper AND NOT owning_role.rolcreaterole
                  AND NOT owning_role.rolcreatedb AND NOT owning_role.rolreplication AND NOT owning_role.rolbypassrls
               FROM pg_catalog.pg_class c
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
               JOIN pg_catalog.pg_roles owning_role ON owning_role.rolname = ?
               JOIN pg_catalog.pg_roles runtime ON runtime.rolname = ?
               WHERE n.nspname = ? AND c.relname = ?"#,
            crate::db_params![owner.as_str(), runtime.as_str(), schema, OWNER_MARKER_TABLE],
        )
        .await?;
    let valid = match shape.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    drop(shape);
    if !valid {
        return Err(RelationGuardCapabilityError::VerificationFailed);
    }

    let mut marker = transaction
        .query(
            &format!(
                "SELECT count(*) = 1 AND bool_and(\
                   pg_catalog.convert_to(version, 'UTF8') = pg_catalog.convert_to(?, 'UTF8') AND \
                   pg_catalog.convert_to(checksum, 'UTF8') = pg_catalog.convert_to(?, 'UTF8')), \
                 min(attempt_id::text) FROM {relation}"
            ),
            crate::db_params![OWNER_MARKER_VERSION, checksum],
        )
        .await?;
    let row = marker
        .next()
        .await?
        .ok_or(RelationGuardCapabilityError::VerificationFailed)?;
    if !row.get::<bool>(0)? {
        return Err(RelationGuardCapabilityError::VerificationFailed);
    }
    let attempt = row
        .get::<Option<String>>(1)?
        .and_then(|value| Uuid::parse_str(&value).ok())
        .ok_or(RelationGuardCapabilityError::VerificationFailed)?;
    Ok(attempt)
}
