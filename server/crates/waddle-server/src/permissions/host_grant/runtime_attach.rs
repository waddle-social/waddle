//! Verification of the runtime role's capabilities on the host-grant
//! relations.
//!
//! The canonical store relies on what its role cannot do: the runtime role
//! cannot enroll a scope, bind a room or change the authority incarnation
//! (it only reads those relations), and it cannot delete or truncate any
//! host-grant row or rewrite the operation journal. The owner phase grants
//! exactly that set from its grant matrix and refuses a runtime role that
//! already holds more. This check reads the same profile back from the
//! catalog on a runtime handle; later privilege changes by the owner are not
//! observed.
//!
//! A replica runs the check when it starts and keeps the result as a
//! [`HostGrantAttachment`]. An acceptance transaction admits a managed room
//! only on a replica that holds one, so a replica whose runtime role does
//! not match the profile treats every enrolled scope as unavailable.
//!
//! The attachment also binds the replica to the authority as it found it:
//! the incarnation, the restore epoch, which must be the replica's
//! configured one, and the start of the PostgreSQL server
//! ([`AuthorityBinding`]). Every acceptance compares them with what it
//! reads, so a restore transition, or a restore or restart of the database
//! under a running replica, stops that replica admitting managed rooms.

use super::error::HostGrantError;
use super::issuance::ServedAccountDomains;
use super::restore::{AuthorityBinding, RestoreEpoch, RestoreFenceRefusal};
use super::store::HostGrantStore;
use crate::db::read_only_timeout::{bound_postgres_waits, ReadOnlyTimeoutError};
use crate::db::{runtime_store_acl, Database, DatabaseDriver, Transaction};

/// What a replica established about its host-grant store when it started:
/// its runtime role holds exactly the store's privileges. It also carries
/// the account domains the replica serves, which admission honours
/// privileged grants for.
///
/// The value exists only after the check passed, and an acceptance
/// transaction admits a managed room only when its unit of work holds one.
#[derive(Clone, Debug)]
pub struct HostGrantAttachment {
    served: ServedAccountDomains,
    binding: Option<AuthorityBinding>,
}

impl HostGrantAttachment {
    /// Verify the role of `store`'s handle against the runtime profile, and
    /// bind the attachment to the authority it reads: the restore epoch the
    /// authority records must be `configured`, the replica's own.
    pub async fn verify(
        store: &HostGrantStore,
        served: ServedAccountDomains,
        configured: RestoreEpoch,
    ) -> Result<Self, HostGrantError> {
        store.verify_runtime_privileges().await?;
        let binding = bind(store, configured).await?;
        Ok(Self {
            served,
            binding: Some(binding),
        })
    }

    /// An attachment without the privilege check and without a binding, for
    /// suites whose database login owns the schema it tests.
    #[cfg(feature = "test-support")]
    pub fn unverified_for_test(served: ServedAccountDomains) -> Self {
        Self {
            served,
            binding: None,
        }
    }

    /// An attachment without the privilege check, bound as
    /// [`Self::verify`] binds one, for suites whose database login owns the
    /// schema it tests.
    #[cfg(feature = "test-support")]
    pub async fn bound_for_test(
        store: &HostGrantStore,
        served: ServedAccountDomains,
        configured: RestoreEpoch,
    ) -> Result<Self, HostGrantError> {
        let binding = bind(store, configured).await?;
        Ok(Self {
            served,
            binding: Some(binding),
        })
    }

    /// The account domains this replica serves.
    pub fn served(&self) -> &ServedAccountDomains {
        &self.served
    }

    /// The authority this replica verified, which every acceptance compares
    /// with what it reads.
    pub fn binding(&self) -> Option<&AuthorityBinding> {
        self.binding.as_ref()
    }
}

/// Read the authority's binding and refuse one established under another
/// restore epoch than the replica's configured one.
async fn bind(
    store: &HostGrantStore,
    configured: RestoreEpoch,
) -> Result<AuthorityBinding, HostGrantError> {
    let binding = store.authority_binding().await?;
    if binding.restore_epoch != configured {
        return Err(RestoreFenceRefusal::EpochMismatch {
            configured,
            stored: binding.restore_epoch,
        }
        .into());
    }
    Ok(binding)
}

/// Every host-grant relation, locked before the catalog is read.
const RELATIONS: [&str; 7] = [
    "host_grant_authority",
    "host_grant_scope_lifecycles",
    "host_grant_scopes",
    "host_grant_rooms",
    "host_grants",
    "host_grant_operations",
    "host_grant_operation_changes",
];

fn quoted(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

async fn begin_bounded_snapshot(db: &Database) -> Result<Transaction<'_>, HostGrantError> {
    let mut tx = db.begin().await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
        (),
    )
    .await?;
    bound_postgres_waits(&mut tx)
        .await
        .map_err(|error| match error {
            ReadOnlyTimeoutError::InvalidSetting { setting } => {
                HostGrantError::InvalidTimeoutSetting { setting }
            }
            ReadOnlyTimeoutError::Database(error) => HostGrantError::Database(error),
        })?;
    Ok(tx)
}

/// Check the role and its privileges on the locked relations.
async fn verify_relations(tx: &mut Transaction<'_>, schema: &str) -> Result<(), HostGrantError> {
    if let Some(component) =
        runtime_store_acl::unexpected_host_grant_executable_schema(tx, schema).await?
    {
        return Err(HostGrantError::UnexpectedExecutableSchema { component });
    }
    if let Some(component) = runtime_store_acl::verify_host_grants(tx, schema).await? {
        return Err(HostGrantError::UnsafeRuntimePrivileges { component });
    }
    Ok(())
}

/// Lock every relation before the first snapshot-forming statement, so the
/// owner cannot commit a change between the snapshot and the lock. Without a
/// schema the names resolve through the handle's search path.
async fn lock_relations(
    tx: &mut Transaction<'_>,
    schema: Option<&str>,
) -> Result<(), HostGrantError> {
    for relation in RELATIONS {
        let name = match schema {
            Some(schema) => format!("{}.{}", quoted(schema), quoted(relation)),
            None => quoted(relation),
        };
        tx.execute(&format!("LOCK TABLE {name} IN ACCESS SHARE MODE"), ())
            .await?;
    }
    Ok(())
}

/// Verify the handle's role against the runtime profile, in the schema its
/// statements resolve to.
pub(super) async fn verify_current_role(db: &Database) -> Result<(), HostGrantError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(HostGrantError::PostgresRequired);
    }
    let mut tx = begin_bounded_snapshot(db).await?;
    lock_relations(&mut tx, None).await?;
    let mut rows = tx
        .query(
            "SELECT n.nspname::text FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.oid = pg_catalog.to_regclass('host_grant_scopes')",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(HostGrantError::UnsafeRuntimePrivileges {
            component: "host_grant_scopes",
        });
    };
    let schema: String = row.get(0)?;
    drop(rows);
    verify_relations(&mut tx, &schema).await?;
    tx.rollback().await?;
    Ok(())
}
