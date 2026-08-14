//! Atomic PostgreSQL ingress write boundary.
//!
//! A transaction takes locks in the fixed order epoch, exact ownership claim,
//! then fenced child rows such as `sm_sessions`, room archives, and ingress
//! projections. Dropping an uncommitted [`IngressUowTransaction`] rolls it
//! back through [`crate::db::Transaction`].

mod error;
mod repositories;
mod retry;

pub use error::IngressUowError;
pub use repositories::{
    CanonicalMessageRepository, DeliveryEffectRepository, EffectIntentRepository,
    EffectIntentWriteOutcome, InboxRepository, MamArchiveRepository, PrincipalAssertion,
    PrincipalRepository, SmIngressRepository,
};
#[cfg(feature = "clustering")]
pub use repositories::{
    ClaimRepository, HandledFrontierOutcome, HandledFrontierRepository, RoomClaimFence,
    ShadowFrontierOutcome, SmClaimFence, SmIngressStreamRepository,
};
pub use retry::{run_with_retry, DbRetryClass, RetryExhausted};

use crate::{
    config::LineageConfig,
    db::{lineage, Database, DatabaseDriver, Transaction},
    ingress_substrate::{acquire_epoch_lock_first, supported_protocol_epoch},
};
#[cfg(feature = "clustering")]
use uuid::Uuid;
use waddle_xmpp::ingress::ProtocolEpoch;
#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::{CurrentNodeIdentityGuard, SharedNodeIdentity};

/// PostgreSQL-only factory for ingress transactions bound to one lineage policy.
#[derive(Clone)]
pub struct PostgresIngressUnitOfWork {
    db: Database,
    lineage: LineageConfig,
    /// The server's canonical node identity source. Bound once at
    /// construction so claim fences can only be minted against the real
    /// rotation gate, never a caller-constructed one.
    #[cfg(feature = "clustering")]
    node_identity: Option<SharedNodeIdentity>,
}

impl PostgresIngressUnitOfWork {
    /// Open against the main PostgreSQL database pool.
    ///
    /// A unit of work opened this way cannot mint claim fences; use
    /// [`Self::open_with_node_identity`] where fenced SM writes are needed.
    pub fn open(db: Database, lineage: LineageConfig) -> Result<Self, IngressUowError> {
        if db.driver() != DatabaseDriver::Postgres {
            return Err(IngressUowError::PostgresRequired);
        }
        Ok(Self {
            db,
            lineage,
            #[cfg(feature = "clustering")]
            node_identity: None,
        })
    }

    /// Open with the server's canonical [`SharedNodeIdentity`] bound, so
    /// claim fences mint under — and transactions retain — the real
    /// rotation gate.
    #[cfg(feature = "clustering")]
    pub fn open_with_node_identity(
        db: Database,
        lineage: LineageConfig,
        node_identity: SharedNodeIdentity,
    ) -> Result<Self, IngressUowError> {
        let mut uow = Self::open(db, lineage)?;
        uow.node_identity = Some(node_identity);
        Ok(uow)
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
            #[cfg(feature = "clustering")]
            identity: Uuid::new_v4(),
            #[cfg(feature = "clustering")]
            node_identity: self.node_identity.clone(),
            #[cfg(feature = "clustering")]
            authority_guards: Vec::new(),
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
    #[cfg(feature = "clustering")]
    identity: Uuid,
    /// The canonical identity source bound at [`PostgresIngressUnitOfWork`]
    /// construction; `None` when this unit of work cannot mint fences.
    #[cfg(feature = "clustering")]
    node_identity: Option<SharedNodeIdentity>,
    /// Node-authority guards minted for this transaction's claim fences.
    /// Held here — not on the independently droppable fence — so identity
    /// rotation or terminal disable cannot complete until this transaction
    /// commits or rolls back, never between a fenced write and its commit.
    #[cfg(feature = "clustering")]
    authority_guards: Vec<CurrentNodeIdentityGuard>,
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
            .map_err(IngressUowError::from)
    }

    /// Raw SQL remains confined to ingress repositories so callers cannot
    /// bypass the epoch, lineage, and claim-fencing invariants.
    fn transaction_mut(&mut self) -> &mut Transaction<'a> {
        &mut self.transaction
    }

    /// Install per-transaction PostgreSQL timeout bounds without exposing raw
    /// SQL to ingress callers.
    pub async fn set_local_timeouts(
        &mut self,
        lock_timeout_ms: u64,
        statement_timeout_ms: u64,
    ) -> Result<(), IngressUowError> {
        let mut proof = self
            .transaction
            .query(
                r#"
                SELECT
                    set_config('lock_timeout', ?, true),
                    set_config('statement_timeout', ?, true)
                "#,
                crate::db_params![
                    format!("{lock_timeout_ms}ms"),
                    format!("{statement_timeout_ms}ms"),
                ],
            )
            .await?;
        proof
            .next()
            .await?
            .ok_or(IngressUowError::EpochProofMissing)?;
        Ok(())
    }

    #[cfg(feature = "clustering")]
    fn identity(&self) -> Uuid {
        self.identity
    }

    /// The canonical node-identity source this transaction may mint claim
    /// fences against.
    #[cfg(feature = "clustering")]
    fn bound_node_identity(&self) -> Option<&SharedNodeIdentity> {
        self.node_identity.as_ref()
    }

    /// Retain a minted node-authority guard until this transaction ends.
    #[cfg(feature = "clustering")]
    fn retain_authority(&mut self, guard: CurrentNodeIdentityGuard) {
        self.authority_guards.push(guard);
    }
}

#[cfg(test)]
mod tests;
