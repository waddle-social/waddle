//! Atomic PostgreSQL ingress write boundary.
//!
//! A transaction takes locks in the fixed order epoch, exact ownership claim,
//! then `sm_sessions` and ingress child rows. Dropping an uncommitted
//! [`IngressUowTransaction`] rolls it back through [`crate::db::Transaction`].

mod error;
mod repositories;

pub use error::IngressUowError;
pub use repositories::{CanonicalMessageRepository, DeliveryEffectRepository, SmIngressRepository};
#[cfg(feature = "clustering")]
pub use repositories::{
    ClaimRepository, HandledFrontierOutcome, HandledFrontierRepository, SmClaimFence,
};

use crate::{
    config::LineageConfig,
    db::{lineage, Database, DatabaseDriver, Transaction},
    ingress_substrate::{acquire_epoch_lock_first, supported_protocol_epoch},
};
use uuid::Uuid;
use waddle_xmpp::ingress::ProtocolEpoch;

/// PostgreSQL-only factory for ingress transactions bound to one lineage policy.
#[derive(Clone)]
pub struct PostgresIngressUnitOfWork {
    db: Database,
    lineage: LineageConfig,
}

impl PostgresIngressUnitOfWork {
    /// Open against the main PostgreSQL database pool.
    pub fn open(db: Database, lineage: LineageConfig) -> Result<Self, IngressUowError> {
        if db.driver() != DatabaseDriver::Postgres {
            return Err(IngressUowError::PostgresRequired);
        }
        Ok(Self { db, lineage })
    }

    /// Begin an attested, epoch-proven ingress transaction.
    ///
    /// The epoch lock is deliberately the first locking statement. It remains
    /// held until commit or drop, making the installed GUC proof describe the
    /// exact live epoch observed by this transaction.
    pub async fn begin(&self) -> Result<IngressUowTransaction<'_>, IngressUowError> {
        let mut transaction = self.db.begin().await?;
        let protocol_epoch = acquire_epoch_lock_first(&mut transaction).await?;
        let supported = supported_protocol_epoch();
        if protocol_epoch > supported {
            return Err(IngressUowError::EpochUnsupported {
                live: protocol_epoch,
                supported,
            });
        }

        let mut proof = transaction
            .query(
                r#"
                SELECT
                    set_config('waddle.protocol_epoch', ?, true),
                    set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)
                "#,
                crate::db_params![protocol_epoch.to_storage().to_string()],
            )
            .await?;
        proof
            .next()
            .await?
            .ok_or(IngressUowError::EpochProofMissing)?;
        drop(proof);

        let lineage =
            lineage::verify_in_transaction(&mut transaction, self.db.driver(), &self.lineage)
                .await
                .map_err(IngressUowError::Lineage)?;

        Ok(IngressUowTransaction {
            transaction,
            protocol_epoch,
            lineage,
            identity: Uuid::new_v4(),
        })
    }
}

/// An ingress transaction carrying the locked epoch and verified lineage.
///
/// There is intentionally no rollback method: dropping this value without
/// [`Self::commit`] rolls back the underlying database transaction.
pub struct IngressUowTransaction<'a> {
    transaction: Transaction<'a>,
    protocol_epoch: ProtocolEpoch,
    lineage: lineage::AttestedLineage,
    /// Private capability identity that binds an in-transaction claim fence
    /// to this exact transaction, not merely another transaction sharing the
    /// same pool lifetime.
    identity: Uuid,
}

impl<'a> IngressUowTransaction<'a> {
    /// The protocol epoch locked at transaction start.
    pub fn protocol_epoch(&self) -> ProtocolEpoch {
        self.protocol_epoch
    }

    /// The lineage attestation verified on this same transaction.
    pub fn lineage(&self) -> &lineage::AttestedLineage {
        &self.lineage
    }

    /// Commit all ingress and related durable writes atomically.
    pub async fn commit(self) -> Result<(), IngressUowError> {
        self.transaction
            .commit()
            .await
            .map_err(IngressUowError::Database)
    }

    /// Raw SQL remains confined to ingress repositories so callers cannot
    /// bypass the epoch, lineage, and claim-fencing invariants.
    fn transaction_mut(&mut self) -> &mut Transaction<'a> {
        &mut self.transaction
    }

    fn identity(&self) -> Uuid {
        self.identity
    }
}

#[cfg(test)]
mod tests;
