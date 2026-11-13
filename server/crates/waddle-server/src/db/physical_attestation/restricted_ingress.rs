//! Dark, read-only observation of the restricted definer profile.
//!
//! This is deliberately distinct from the direct-DML relation-guard profile.
//! No observation is a serving capability. Independently quiescent trusted
//! catalog/ACL/role mutation is required throughout inspection: PostgreSQL's
//! privilege lookup caches do not constitute a catalog-generation barrier.
//! A trusted AX-first mutation protocol and lexical, pool-bound transaction
//! contract must be reviewed before any selector can consume this profile.
//! The separately installed physical-identity attestor is an independent trust
//! prerequisite: this observer neither verifies its body nor fences its installer.
//! The legacy pool hook runs before the transaction fence; those earlier calls
//! still require independently quiescent catalog and role mutation.

use std::time::Duration;

use sqlx::{Connection, PgConnection, Row};
use thiserror::Error;

use super::{
    attest_connection, quote_identifier, AttestationMode, AttestedPgPool, PgRoleName,
    PhysicalAttestationError, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};

mod catalog;
mod catalog_preflight;
mod guarded_receipt;
mod manifest;
mod receipt_authority;
mod receipt_catalog;
pub(crate) use guarded_receipt::verify_guarded_receipt_profile;
pub use guarded_receipt::GuardedReceiptOperationPins;
pub use receipt_authority::{
    ReceiptAuthorityError, ReceiptCommit, ReceiptOperation, ReceiptOperationAuthority,
    ReceiptOperationPins, ReceiptReadback,
};
#[cfg(feature = "test-support")]
mod receipt_transaction;
#[cfg(feature = "test-support")]
pub use receipt_transaction::{
    RestrictedEffectReceipt, RestrictedReceiptPilot, RestrictedReceiptReadback,
    RestrictedReceiptWrite, RestrictedTerminalization, RestrictedTerminalizationWrite,
};
#[cfg(feature = "test-support")]
mod transaction_probe;
#[cfg(feature = "test-support")]
pub use transaction_probe::{RestrictedTransactionProbe, RestrictedTransactionProbeObservation};

/// A diagnostic receipt only; it cannot select or prepare ingress transactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestrictedIngressProfileObservation {
    pub manifest_version: u32,
    pub schema_oid: u32,
    pub epoch_relation_oid: u32,
    pub runtime_role_oid: u32,
    pub api_owner_oid: u32,
}

#[derive(Debug, Error)]
pub enum RestrictedIngressProfileError {
    #[error("restricted ingress profile differs at {0}")]
    Mismatch(&'static str),
    #[error("restricted ingress profile inspection timed out")]
    Timeout,
    #[error(transparent)]
    Physical(#[from] PhysicalAttestationError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

const MANIFEST_VERSION: u32 = 3;

type Result<T> = std::result::Result<T, RestrictedIngressProfileError>;

impl AttestedPgPool {
    /// Observe one exact PG17 definer profile using this pool's physical
    /// identity and runtime login. The independently trusted epoch relation
    /// OID must be supplied; a name alone cannot authorize a replacement.
    /// Each call leases a physically attested connection (including reconnect).
    /// No grants, DDL, role changes, epoch changes or application calls occur.
    /// The result is stale immediately after inspection and grants no authority.
    pub async fn inspect_restricted_ingress_profile(
        &self,
        api_owner: &PgRoleName,
        epoch_relation_oid: u32,
    ) -> Result<RestrictedIngressProfileObservation> {
        if matches!(&self.writer_capability.mode, AttestationMode::Pinned(_)) {
            return Err(PhysicalAttestationError::UnconvertedPinnedTransaction.into());
        }
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut connection = self.acquire().await?;
            let config = &self.writer_capability.config;
            let mut tx = connection
                .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED READ ONLY")
                .await?;
            // Try rather than wait: a contended transaction must roll back
            // without retaining an older snapshot while the mutator proceeds.
            let acquired: bool =
                sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)")
                    .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                    .fetch_one(&mut *tx)
                    .await?;
            if !acquired {
                tx.rollback().await?;
                return Err(RestrictedIngressProfileError::Mismatch(
                    "catalog mutation fence",
                ));
            }
            sqlx::query("SET LOCAL statement_timeout = '5s'")
                .execute(&mut *tx)
                .await?;
            sqlx::query("SET LOCAL lock_timeout = '3s'")
                .execute(&mut *tx)
                .await?;
            attest_connection(&mut tx, config).await?;
            let result = inspect_profile_on_connection(
                &mut tx,
                &config.postgres().schema.name.0,
                config.postgres().schema.oid,
                config.runtime_login.as_str(),
                api_owner.as_str(),
                epoch_relation_oid,
            )
            .await?;
            tx.rollback().await?;
            Ok(result)
        })
        .await
        .map_err(|_| RestrictedIngressProfileError::Timeout)?
    }
}

async fn inspect_profile_on_connection(
    connection: &mut PgConnection,
    schema: &str,
    schema_oid: u32,
    runtime: &str,
    api_owner: &str,
    epoch_oid: u32,
) -> Result<RestrictedIngressProfileObservation> {
    inspect_profile_with_receipt_journal(
        connection,
        schema,
        schema_oid,
        runtime,
        api_owner,
        epoch_oid,
        ProfileMode::Base,
    )
    .await
}

#[derive(Clone, Copy)]
enum ProfileMode {
    Base,
    ReceiptJournal,
    GuardedReceipt,
}

async fn inspect_profile_with_receipt_journal(
    connection: &mut PgConnection,
    schema: &str,
    schema_oid: u32,
    runtime: &str,
    api_owner: &str,
    epoch_oid: u32,
    mode: ProfileMode,
) -> Result<RestrictedIngressProfileObservation> {
    let receipt_journal = !matches!(mode, ProfileMode::Base);
    let guarded_receipt = matches!(mode, ProfileMode::GuardedReceipt);
    let row = sqlx::query(
        "SELECT pg_catalog.current_setting('server_version_num')::integer, \
         pg_catalog.current_setting('search_path'), session_user::text, current_user::text, \
         pg_catalog.current_setting('session_replication_role'), \
         pg_catalog.current_setting('transaction_isolation')",
    )
    .fetch_one(&mut *connection)
    .await?;
    let version: i32 = row.try_get(0)?;
    if !(170_000..180_000).contains(&version) {
        return Err(RestrictedIngressProfileError::Mismatch("PostgreSQL major"));
    }
    if row.try_get::<String, _>(1)? != format!("{}, pg_temp", quote_identifier(schema))
        || row.try_get::<String, _>(2)? != runtime
        || row.try_get::<String, _>(3)? != runtime
        || row.try_get::<String, _>(4)? != "origin"
        || row.try_get::<String, _>(5)? != "read committed"
    {
        return Err(RestrictedIngressProfileError::Mismatch(
            "runtime session/search path",
        ));
    }
    // Qualify all references and pin name/type resolution for catalog casts.
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await?;
    let manifest = manifest::load()?;
    catalog_preflight::bound(connection, schema).await?;
    let before = catalog::verify_roles_and_tables(
        connection, schema, schema_oid, runtime, api_owner, epoch_oid,
    )
    .await?;
    catalog_preflight::verify(connection, schema, &manifest).await?;
    // A name-only LOCK on a view or inheritance parent can reach objects the
    // runtime should never touch. First establish this is the expected heap;
    // then, after any wait, use a fresh RC snapshot to recheck all identities
    // and catalog safety before reading the manifest or deparsing expressions.
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_epoch_guard_manifest IN ACCESS SHARE MODE",
        quote_identifier(schema),
    ))
    .execute(&mut *connection)
    .await?;
    let after = catalog::verify_roles_and_tables(
        connection, schema, schema_oid, runtime, api_owner, epoch_oid,
    )
    .await?;
    if before != after {
        return Err(RestrictedIngressProfileError::Mismatch(
            "post-lock catalog identities",
        ));
    }
    catalog_preflight::verify(connection, schema, &manifest).await?;
    manifest::verify_functions_and_triggers(
        connection,
        schema,
        runtime,
        api_owner,
        receipt_journal,
        guarded_receipt,
        &manifest,
    )
    .await?;
    catalog::verify_manifest_rows(connection, schema).await?;
    manifest::verify_structure(connection, schema, &manifest).await?;
    restore_application_path(connection, schema, runtime).await?;
    Ok(RestrictedIngressProfileObservation {
        manifest_version: MANIFEST_VERSION,
        schema_oid,
        epoch_relation_oid: epoch_oid,
        runtime_role_oid: after.runtime_oid,
        api_owner_oid: after.api_oid,
    })
}

// A catalog scan must not leave pg_catalog as the application's active schema.
// The fixed rollback probe and public observer share this complete verifier.
async fn restore_application_path(
    connection: &mut PgConnection,
    schema: &str,
    runtime: &str,
) -> Result<()> {
    let path = format!("{}, pg_temp", quote_identifier(schema));
    sqlx::query("SELECT pg_catalog.set_config('search_path', $1, true)")
        .bind(&path)
        .execute(&mut *connection)
        .await?;
    let row = sqlx::query(
        "SELECT pg_catalog.current_setting('search_path'), pg_catalog.current_schema()::text, \
         session_user::text, current_user::text",
    )
    .fetch_one(connection)
    .await?;
    if row.try_get::<String, _>(0)? != path
        || row.try_get::<Option<String>, _>(1)?.as_deref() != Some(schema)
        || row.try_get::<String, _>(2)? != runtime
        || row.try_get::<String, _>(3)? != runtime
    {
        return Err(RestrictedIngressProfileError::Mismatch(
            "restored application session",
        ));
    }
    Ok(())
}
