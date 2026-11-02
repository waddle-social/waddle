//! Global database bootstrap and dark read-only attachment boundaries.
//!
//! The owner operation retains the existing startup sequence. The runtime
//! verifier is not wired into normal startup until the remaining store DDL,
//! role topology and physical attestation gates are resolved.

use thiserror::Error;
use tracing::info;

use crate::config::LineageConfig;

use super::lineage::{self, AttestedLineage, LineageAction, LineageError, LineageUuid};
use super::{Database, DatabaseDriver, DatabaseError, MigrationRunner, Transaction};

#[derive(Debug, Error)]
pub enum OwnerBootstrapError {
    #[error("failed to bootstrap database lineage table")]
    LineageTable(#[source] DatabaseError),
    #[error("failed to enroll global database lineage")]
    Enrollment(#[source] DatabaseError),
    #[error("failed to apply global database lineage adoption")]
    Adoption(#[source] DatabaseError),
    #[error(
        "refusing to start clustering against a database with no lineage row: \
         enroll it first (WADDLE_DB_LINEAGE_ACTION=enroll on one rollout)"
    )]
    MissingClusteredLineage,
    #[error(
        "refusing to run migrations: the global database's lineage does not \
         describe this deployment (mis-pointed DSN, unadopted restore, or \
         missing WADDLE_DEPLOYMENT_UUID)"
    )]
    PreMigrationLineage(#[source] DatabaseError),
    #[error("failed to run migrations")]
    Migrations(#[source] DatabaseError),
}

#[derive(Debug, Error)]
pub enum StoreOwnerBootstrapError {
    #[error("failed to bootstrap store database lineage table")]
    LineageTable(#[source] DatabaseError),
    #[error("failed to enroll store database lineage")]
    Enrollment(#[source] DatabaseError),
    #[error("failed to apply store database lineage adoption")]
    Adoption(#[source] DatabaseError),
}

#[derive(Debug, Error)]
pub enum RuntimeAttachmentError {
    #[error("runtime attachment cannot process a lineage enrollment or adoption action")]
    OperatorActionConfigured,
    #[error("runtime attachment requires an operator-supplied PostgreSQL identity")]
    MissingExpectedPostgresIdentity,
    #[error("runtime attachment identity differs from the expected committed installation")]
    ExpectedIdentityMismatch,
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// The existing owner startup path, made explicit so a later credential split
/// can move it before restricted runtime attachment. This still runs under the
/// current startup credential and is not a role transition.
pub async fn bootstrap_global_owner(
    db: &Database,
    config: &LineageConfig,
    clustering_enabled: bool,
) -> Result<Option<LineageUuid>, OwnerBootstrapError> {
    lineage::ensure_table(db)
        .await
        .map_err(OwnerBootstrapError::LineageTable)?;

    let global_adopt_matched = match config.action.as_ref() {
        Some(LineageAction::Enroll) => {
            info!(store = "global", "enrolling database lineage");
            lineage::enroll(db, config)
                .await
                .map_err(OwnerBootstrapError::Enrollment)?;
            None
        }
        Some(LineageAction::Adopt(expected)) => {
            match lineage::adopt_if_matched(db, config, expected)
                .await
                .map_err(OwnerBootstrapError::Adoption)?
            {
                lineage::AdoptOutcome::Adopted { matched, .. } => {
                    info!(store = "global", matched = %matched, "adopted database lineage");
                    Some(matched)
                }
                lineage::AdoptOutcome::NotMatched => None,
            }
        }
        None => None,
    };

    match lineage::verify(db, config).await {
        Ok(_) => {}
        Err(DatabaseError::Lineage(LineageError::MissingRow)) => {
            if clustering_enabled && !matches!(config.action.as_ref(), Some(LineageAction::Enroll))
            {
                return Err(OwnerBootstrapError::MissingClusteredLineage);
            }
            info!("global database has no lineage row yet; proceeding to migrations un-enrolled");
        }
        Err(error) => return Err(OwnerBootstrapError::PreMigrationLineage(error)),
    }

    MigrationRunner::single()
        .run(db)
        .await
        .map_err(OwnerBootstrapError::Migrations)?;
    Ok(global_adopt_matched)
}

/// Preserve the existing per-store lineage owner action. Store schema
/// constructors currently precede this call, so this is not yet a complete
/// owner bootstrap or a runtime attachment boundary.
pub async fn bootstrap_store_owner(
    db: &Database,
    config: &LineageConfig,
) -> Result<Option<LineageUuid>, StoreOwnerBootstrapError> {
    lineage::ensure_table(db)
        .await
        .map_err(StoreOwnerBootstrapError::LineageTable)?;
    match config.action.as_ref() {
        Some(LineageAction::Enroll) => {
            lineage::enroll(db, config)
                .await
                .map_err(StoreOwnerBootstrapError::Enrollment)?;
            Ok(None)
        }
        Some(LineageAction::Adopt(expected)) => {
            match lineage::adopt_if_matched(db, config, expected)
                .await
                .map_err(StoreOwnerBootstrapError::Adoption)?
            {
                lineage::AdoptOutcome::Adopted { matched, .. } => Ok(Some(matched)),
                lineage::AdoptOutcome::NotMatched => Ok(None),
            }
        }
        None => Ok(None),
    }
}

/// Prove that this already-provisioned global database has an attested lineage
/// and the complete committed migration catalog. No schema repair, enrollment,
/// adoption or checksum backfill is attempted. `expected` must come from an
/// owner/operator handoff independent of the runtime DSN; that production
/// handoff is a later gate. PostgreSQL enforces read-only execution for the
/// whole verification transaction.
pub async fn verify_global_runtime(
    db: &Database,
    config: &LineageConfig,
    expected: &AttestedLineage,
) -> Result<AttestedLineage, RuntimeAttachmentError> {
    let (mut tx, driver, lineage_table, ledger_table) =
        begin_runtime_lineage_check(db, config, expected).await?;
    let attested =
        verify_runtime_lineage(db, &mut tx, driver, config, expected, &lineage_table).await?;
    if &attested != expected {
        return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
    }
    MigrationRunner::single()
        .verify_complete_in_transaction(&mut tx, driver, &ledger_table)
        .await?;
    tx.commit().await?;
    Ok(attested)
}

/// Verify only the committed lineage and physical identity of an already
/// provisioned durable store. The store's schema and grants are separate
/// owner/bootstrap gates; this is not wired into normal startup yet.
pub async fn verify_store_lineage_runtime(
    db: &Database,
    config: &LineageConfig,
    expected: &AttestedLineage,
) -> Result<AttestedLineage, RuntimeAttachmentError> {
    let (mut tx, driver, lineage_table, _) =
        begin_runtime_lineage_check(db, config, expected).await?;
    let attested =
        verify_runtime_lineage(db, &mut tx, driver, config, expected, &lineage_table).await?;
    if &attested != expected {
        return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
    }
    tx.commit().await?;
    Ok(attested)
}

async fn verify_runtime_lineage(
    db: &Database,
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    config: &LineageConfig,
    expected: &AttestedLineage,
    lineage_table: &str,
) -> Result<AttestedLineage, RuntimeAttachmentError> {
    if let Some(capability) = db.attested_pg_writer_capability() {
        db.verify_attested_pg_writer_capability(&capability)
            .map_err(DatabaseError::from)?;
        if &capability.attested_lineage_for_deployment(config.deployment_uuid)? != expected {
            return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
        }
        capability.attest_transaction(tx).await?;
        Ok(
            lineage::verify_qualified_with_attested_identity_in_transaction(
                tx,
                config,
                lineage_table,
                capability.identity(),
            )
            .await?,
        )
    } else {
        Ok(lineage::verify_qualified_in_transaction(tx, driver, config, lineage_table).await?)
    }
}

async fn begin_runtime_lineage_check<'a>(
    db: &'a Database,
    config: &LineageConfig,
    expected: &AttestedLineage,
) -> Result<(Transaction<'a>, DatabaseDriver, String, String), RuntimeAttachmentError> {
    if config.action.is_some() {
        return Err(RuntimeAttachmentError::OperatorActionConfigured);
    }
    let driver = db.driver();
    if driver == DatabaseDriver::Postgres && expected.postgres_identity.is_none() {
        return Err(RuntimeAttachmentError::MissingExpectedPostgresIdentity);
    }
    if config.deployment_uuid != Some(expected.deployment_uuid) {
        return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
    }
    let mut tx = db.begin().await?;
    let (lineage_table, ledger_table) = if driver == DatabaseDriver::Postgres {
        // A raw pooled BEGIN can survive a prior borrower, and SQLx's BEGIN
        // then only warns about a nested transaction. Two different
        // pre-query isolation settings make an already-used inherited
        // transaction refuse this check instead of reusing its old snapshot.
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
        let schema = &expected
            .postgres_identity
            .as_ref()
            .ok_or(RuntimeAttachmentError::MissingExpectedPostgresIdentity)?
            .schema
            .name
            .0;
        {
            let mut rows = tx.query("SELECT pg_catalog.current_schema()", ()).await?;
            let row = rows
                .next()
                .await?
                .ok_or(RuntimeAttachmentError::ExpectedIdentityMismatch)?;
            let active: Option<String> = row.get(0)?;
            if active.as_deref() != Some(schema) {
                return Err(RuntimeAttachmentError::ExpectedIdentityMismatch);
            }
        }
        // A session TEMP table can shadow `_lineage` or `_migrations`.
        // Omit pg_catalog so PostgreSQL searches it implicitly first for
        // built-ins, while the trusted schema remains current_schema().
        // pg_temp is explicitly last for relation lookup.
        let quoted_schema = format!("\"{}\"", schema.replace('"', "\"\""));
        let safe_path = format!("{quoted_schema}, pg_temp");
        tx.execute(
            "SELECT pg_catalog.set_config('search_path', ?, true)",
            crate::db_params![safe_path],
        )
        .await?;
        (
            format!("{quoted_schema}._lineage"),
            format!("{quoted_schema}._migrations"),
        )
    } else {
        ("main._lineage".to_string(), "main._migrations".to_string())
    };
    Ok((tx, driver, lineage_table, ledger_table))
}
