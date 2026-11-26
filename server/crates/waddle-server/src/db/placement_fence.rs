//! Transactional placement-lease fencing helper composing with `waddle-server`'s
//! `EnteredTransaction` / `IngressUowTransaction` (`FOR SHARE` on PostgreSQL,
//! active lease row verification on SQLite).

use waddle_broker_core::placement::FenceGeneration;
use waddle_broker_core::store::{TenantQueryScope, TenantStoreError};

use crate::db::{DatabaseDriver, Transaction};

/// Verify inside an already-open `waddle-server` transaction (e.g., an
/// `EnteredTransaction` or `IngressUowTransaction`) that `scope` still holds
/// the active placement lease at `scope.placement_generation`.
///
/// On PostgreSQL this acquires a `FOR SHARE` row lock on `tenant_placement_leases`
/// so concurrent `transfer` / `release` statements block until this transaction
/// commits or rolls back, and any write after a committed transfer/release sees
/// 0 rows and fails closed.
pub async fn assert_transaction_placement_lease(
    tx: &mut Transaction<'_>,
    scope: &TenantQueryScope,
) -> Result<(), TenantStoreError> {
    let gen_i64 = i64::try_from(scope.placement_generation.get())
        .map_err(|_| TenantStoreError::Backend("placement generation overflow".to_owned()))?;

    let sql = match tx.driver() {
        DatabaseDriver::Postgres => {
            "SELECT 1 FROM tenant_placement_leases
             WHERE tenant_id = ?
               AND holder_spiffe_id = ?
               AND holder_incarnation_pubkey = ?
               AND generation = ?
               AND transfer_state IN ('active', 'draining')
               AND expires_at > now()
             FOR SHARE"
        }
        DatabaseDriver::Sqlite => {
            "SELECT 1 FROM tenant_placement_leases
             WHERE tenant_id = ?
               AND holder_spiffe_id = ?
               AND holder_incarnation_pubkey = ?
               AND generation = ?
               AND transfer_state IN ('active', 'draining')
               AND expires_at_ms > CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)"
        }
    };

    let mut rows = tx
        .query(
            sql,
            crate::db_params![
                scope.tenant_id.as_str().to_owned(),
                scope.holder.spiffe_id.as_str().to_owned(),
                scope.holder.incarnation_pubkey.as_bytes().to_vec(),
                gen_i64
            ],
        )
        .await
        .map_err(|err| TenantStoreError::Backend(format!("placement fence query failed: {err}")))?;

    if rows
        .next()
        .await
        .map_err(|err| TenantStoreError::Backend(format!("placement fence row failed: {err}")))?
        .is_some()
    {
        Ok(())
    } else {
        Err(TenantStoreError::StalePlacementGeneration {
            tenant_id: scope.tenant_id.clone(),
            expected: scope.placement_generation,
            actual: FenceGeneration::ZERO,
        })
    }
}

/// Optional placement-fence check: no-op when `scope` is `None` (legacy
/// single-tenant mode), strict [`assert_transaction_placement_lease`] when `Some`.
pub async fn check_optional_placement_fence(
    tx: &mut Transaction<'_>,
    scope: Option<&TenantQueryScope>,
) -> Result<(), TenantStoreError> {
    match scope {
        Some(scope) => assert_transaction_placement_lease(tx, scope).await,
        None => Ok(()),
    }
}
