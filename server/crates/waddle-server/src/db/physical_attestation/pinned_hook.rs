//! Independently pinned physical checkout proof. This does not authorize a
//! later application transaction or the production runtime attachment.

use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, Postgres, Row, Transaction, TransactionManager};

use super::{
    attest_dark_catalog_pinned, discard_temporary_relations, normalize_search_path,
    quote_identifier, reject_runtime_temp_privilege, AttestationMode, AttestedPgPool,
    AttestedPostgresWriterCapability, DarkPhysicalAttestationPins, PhysicalAttestationConfig,
    PhysicalAttestationError,
};
use crate::db::read_only_timeout::{bound_postgres_session_waits, timeout_millis};
use crate::db::DarkMigrationLedgerPins;

/// Immutable operator-provisioned runtime authority. The connection being
/// checked must never be the source of these pins. This bundle binds the
/// physical, ledger, epoch and guard identities to one complete configuration;
/// the checkout currently proves only the physical attestor and lineage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedRuntimeAuthority {
    pub(super) config: PhysicalAttestationConfig,
    pub(super) attestor: DarkPhysicalAttestationPins,
    pub(super) ledger: DarkMigrationLedgerPins,
    pub(super) epoch_relation_oid: u32,
    epoch_primary_index_oid: u32,
    guard_manifest_relation_oid: u32,
}

impl PinnedRuntimeAuthority {
    pub fn new(
        config: &PhysicalAttestationConfig,
        attestor: DarkPhysicalAttestationPins,
        ledger: DarkMigrationLedgerPins,
        epoch_relation_oid: u32,
        epoch_primary_index_oid: u32,
        guard_manifest_relation_oid: u32,
    ) -> Result<Self, PhysicalAttestationError> {
        let identity = config.postgres();
        let database_name = &identity.database.name.0;
        let roles = [
            attestor.runtime_role_oid,
            attestor.installer_role_oid,
            attestor.definer_role_oid,
        ];
        // Relation and index OIDs share pg_class. OIDs in different catalogs
        // need not be unique, and the database and schema owners may coincide.
        let relations = [
            attestor.lineage_relation_oid,
            attestor.lineage_primary_index_oid,
            ledger.ledger_relation_oid,
            ledger.ledger_primary_index_oid,
            epoch_relation_oid,
            epoch_primary_index_oid,
            guard_manifest_relation_oid,
        ];
        let unique_relations = relations
            .iter()
            .enumerate()
            .all(|(index, oid)| !relations[..index].contains(oid));
        if identity.system_identifier.0 == 0
            || identity.database.oid == 0
            || identity.schema.oid == 0
            || database_name.is_empty()
            || database_name.len() > 63
            || database_name.contains('\0')
            || config.lineage.lineage_uuid.0.is_nil()
            || config.lineage.deployment_uuid.0.is_nil()
            || attestor.installation_operation_id.is_nil()
            || [
                attestor.database_owner_role_oid,
                attestor.schema_owner_role_oid,
                attestor.runtime_role_oid,
                attestor.installer_role_oid,
                attestor.definer_role_oid,
                attestor.trusted_schema_oid,
                attestor.attestor_function_oid,
            ]
            .contains(&0)
            || relations.contains(&0)
            || !unique_relations
            || roles[0] == roles[1]
            || roles[0] == roles[2]
            || roles[1] == roles[2]
            || roles.contains(&attestor.database_owner_role_oid)
            || roles.contains(&attestor.schema_owner_role_oid)
            || attestor.trusted_schema_oid == identity.schema.oid
            || ledger.schema_oid != identity.schema.oid
            || ledger.schema_owner_role_oid != attestor.schema_owner_role_oid
            || ledger.runtime_role_oid != attestor.runtime_role_oid
        {
            return Err(PhysicalAttestationError::InvalidRuntimePins);
        }
        Ok(Self {
            config: config.clone(),
            attestor,
            ledger,
            epoch_relation_oid,
            epoch_primary_index_oid,
            guard_manifest_relation_oid,
        })
    }
}

/// Construct a pool whose fresh and reused physical sessions each pass the
/// pinned checkout proof. It requires a direct, session-preserving PG17 primary
/// and an independently enforced privileged catalog and role mutation boundary.
/// The pool does not authorize later transactions or production attachment.
///
/// SQLx discards sessions on hook errors and on cancellation while acquiring.
/// Its client deadline cannot prove that a pre-existing server-side operation
/// has terminated, especially before the inherited transaction is rolled back
/// and server wait limits are installed. Operational fencing remains necessary.
pub fn pinned_attested_pool(
    options: PgConnectOptions,
    config: PhysicalAttestationConfig,
    pins: PinnedRuntimeAuthority,
    max_connections: u32,
    acquire_timeout: Duration,
) -> Result<AttestedPgPool, PhysicalAttestationError> {
    if options.get_username() != config.runtime_login.as_str() {
        return Err(PhysicalAttestationError::WrongConfiguredLogin);
    }
    if config != pins.config {
        return Err(PhysicalAttestationError::InvalidRuntimePins);
    }
    let on_connect = pins.clone();
    let before_acquire = pins.clone();
    let pool = PgPoolOptions::new()
        .min_connections(0)
        .max_connections(max_connections)
        .acquire_timeout(acquire_timeout)
        .after_connect(move |connection, _| {
            let authority = on_connect.clone();
            Box::pin(async move { checkout(connection, &authority).await })
        })
        .before_acquire(move |connection, _| {
            let authority = before_acquire.clone();
            Box::pin(async move {
                checkout(connection, &authority).await?;
                Ok(true)
            })
        })
        .connect_lazy_with(options);
    Ok(AttestedPgPool {
        pool,
        writer_capability: AttestedPostgresWriterCapability {
            binding: Arc::new(()),
            config,
            mode: AttestationMode::Pinned(Box::new(pins)),
        },
    })
}

async fn checkout(
    connection: &mut PgConnection,
    authority: &PinnedRuntimeAuthority,
) -> Result<(), sqlx::Error> {
    checkout_inner(connection, authority)
        .await
        .map_err(|_| sqlx::Error::Protocol("pinned physical attestation refused".to_owned()))
}

async fn checkout_inner(
    connection: &mut PgConnection,
    authority: &PinnedRuntimeAuthority,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if <Postgres as sqlx::Database>::TransactionManager::get_transaction_depth(connection) != 0 {
        return Err("SQLx transaction still active".into());
    }
    // Raw BEGIN does not update SQLx's transaction depth. End an inherited
    // transaction before observing any identity; errors discard the session.
    sqlx::query("ROLLBACK").execute(&mut *connection).await?;
    let timeouts = bound_postgres_session_waits(connection).await?;
    // A returned session can retain session-level locks independently of a
    // transaction. Releasing them before the barrier avoids self-deadlock.
    sqlx::query("SELECT pg_catalog.pg_advisory_unlock_all()")
        .execute(&mut *connection)
        .await?;
    reject_assumed_role(connection, &authority.config).await?;
    reject_runtime_temp_privilege(connection).await?;
    discard_temporary_relations(connection).await?;

    let restore_path = format!(
        "{}, pg_temp",
        quote_identifier(&authority.config.postgres().schema.name.0)
    );
    let mut tx = connection
        .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED READ ONLY")
        .await?;
    bound_local_timeouts(&mut tx).await?;
    attest_dark_catalog_pinned(
        &mut tx,
        &authority.config,
        authority.attestor,
        &restore_path,
    )
    .await?;
    // A successful rollback response is required before the session can be
    // lent. Any error or cancellation causes SQLx to discard the connection.
    tx.rollback().await?;
    normalize_search_path(connection, &authority.config).await?;
    reject_assumed_role(connection, &authority.config).await?;
    reject_runtime_temp_privilege(connection).await?;
    timeouts.restore(connection).await?;
    Ok(())
}

async fn reject_assumed_role(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
) -> Result<(), PhysicalAttestationError> {
    let row = sqlx::query("SELECT session_user::text, current_user::text")
        .fetch_one(connection)
        .await?;
    let session_user: String = row.try_get(0)?;
    let current_user: String = row.try_get(1)?;
    if session_user != config.runtime_login.as_str()
        || current_user != config.runtime_login.as_str()
    {
        return Err(PhysicalAttestationError::Mismatch {
            field: "runtime login/session role",
        });
    }
    Ok(())
}

async fn bound_local_timeouts(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for (show, set, cap) in [
        ("SHOW lock_timeout", "SET LOCAL lock_timeout = '5s'", 5_000),
        (
            "SHOW statement_timeout",
            "SET LOCAL statement_timeout = '10s'",
            10_000,
        ),
        (
            "SHOW idle_in_transaction_session_timeout",
            "SET LOCAL idle_in_transaction_session_timeout = '10s'",
            10_000,
        ),
        (
            "SHOW transaction_timeout",
            "SET LOCAL transaction_timeout = '20s'",
            20_000,
        ),
    ] {
        let value: String = sqlx::query_scalar(show).fetch_one(&mut **tx).await?;
        let millis = timeout_millis(&value).ok_or("invalid PostgreSQL timeout")?;
        if millis == 0 || millis > cap {
            sqlx::query(set).execute(&mut **tx).await?;
        }
    }
    Ok(())
}
