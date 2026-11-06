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
//! Epoch AS does not cover that attestor's pre-transaction pool-hook calls.

use std::time::Duration;

use sqlx::{Connection, PgConnection, Row};
use thiserror::Error;

use super::{
    attest_connection, quote_identifier, AttestedPgPool, PgRoleName, PhysicalAttestationError,
};

mod catalog;
mod manifest;

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
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut connection = self.acquire().await?;
            let config = &self.writer_capability.config;
            let mut tx = connection.begin().await?;
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
                .execute(&mut *tx)
                .await?;
            sqlx::query("SET LOCAL statement_timeout = '5s'")
                .execute(&mut *tx)
                .await?;
            sqlx::query("SET LOCAL lock_timeout = '3s'")
                .execute(&mut *tx)
                .await?;
            attest_connection(&mut tx, config).await?;
            let result = inspect_catalog(
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

async fn inspect_catalog(
    connection: &mut PgConnection,
    schema: &str,
    schema_oid: u32,
    runtime: &str,
    api_owner: &str,
    epoch_oid: u32,
) -> Result<RestrictedIngressProfileObservation> {
    let row = sqlx::query(
        "SELECT pg_catalog.current_setting('server_version_num')::integer, \
         pg_catalog.current_setting('search_path'), session_user::text, current_user::text, \
         pg_catalog.current_setting('session_replication_role')",
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
    {
        return Err(RestrictedIngressProfileError::Mismatch(
            "runtime session/search path",
        ));
    }
    // Qualify all references and pin name/type resolution for catalog casts.
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await?;
    let (runtime_oid, api_oid) = catalog::verify_roles_and_tables(
        connection, schema, schema_oid, runtime, api_owner, epoch_oid,
    )
    .await?;
    manifest::verify(connection, schema, runtime, api_owner).await?;
    Ok(RestrictedIngressProfileObservation {
        manifest_version: 1,
        schema_oid,
        epoch_relation_oid: epoch_oid,
        runtime_role_oid: runtime_oid,
        api_owner_oid: api_oid,
    })
}
