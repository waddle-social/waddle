//! Atomic dialect-aware ingress write boundary.
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
    EffectIntentWriteOutcome, EffectReceiptRepository, FrontierOutcome, InboxRepository,
    MamArchiveRepository, PrincipalAssertion, PrincipalRepository, SmIngressRepository,
    SmIngressStreamRepository,
};
#[cfg(feature = "clustering")]
pub use repositories::{
    ClaimRepository, HandledFrontierOutcome, HandledFrontierRepository, RoomClaimFence,
    SmClaimFence,
};
pub use retry::{run_with_retry, DbRetryClass, RetryExhausted};

use std::time::Duration;

use crate::{
    config::LineageConfig,
    db::{lineage, Database, DatabaseDriver, Transaction},
    ingress_substrate::{
        acquire_epoch_lock_first, set_local_transaction_timeouts, supported_protocol_epoch,
    },
};
#[cfg(feature = "clustering")]
use uuid::Uuid;
use waddle_xmpp::ingress::ProtocolEpoch;
#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::{CurrentNodeIdentityGuard, SharedNodeIdentity};

/// Ownership authority available to an ingress unit of work.
#[derive(Clone)]
pub enum IngressFencing {
    #[cfg(feature = "clustering")]
    Clustered(SharedNodeIdentity),
    SingleNode,
}

/// Dialect-aware factory for ingress transactions bound to one lineage policy.
#[derive(Clone)]
pub struct IngressUnitOfWork {
    db: Database,
    lineage: LineageConfig,
    /// The server's canonical node identity source. Bound once at
    /// construction so claim fences can only be minted against the real
    /// rotation gate, never a caller-constructed one.
    fencing: IngressFencing,
}

impl IngressUnitOfWork {
    /// Open against the main database pool in single-node mode.
    ///
    /// A unit of work opened this way cannot mint claim fences; use
    /// [`Self::open_with_node_identity`] where fenced SM writes are needed.
    pub fn open(db: Database, lineage: LineageConfig) -> Result<Self, IngressUowError> {
        Ok(Self {
            db,
            lineage,
            fencing: IngressFencing::SingleNode,
        })
    }

    /// Ownership fencing configured for transactions opened by this factory.
    pub fn fencing(&self) -> &IngressFencing {
        &self.fencing
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
        if uow.db.driver() != DatabaseDriver::Postgres {
            return Err(IngressUowError::ClusteredFencingRequiresPostgres);
        }
        uow.fencing = IngressFencing::Clustered(node_identity);
        Ok(uow)
    }

    /// Begin an attested, epoch-proven ingress transaction.
    ///
    /// The epoch lock is deliberately the first locking statement. It remains
    /// held until commit or drop, making the installed GUC proof describe the
    /// exact live epoch observed by this transaction.
    pub async fn begin(&self) -> Result<IngressUowTransaction<'_>, IngressUowError> {
        let mut transaction = match self.db.driver() {
            DatabaseDriver::Sqlite => self.db.begin_immediate().await?,
            DatabaseDriver::Postgres => self.db.begin().await?,
        };
        let protocol_epoch = acquire_epoch_lock_first(&mut transaction).await?;
        let supported = supported_protocol_epoch();
        if protocol_epoch > supported {
            return Err(IngressUowError::EpochUnsupported {
                live: protocol_epoch,
                supported,
            });
        }

        if self.db.driver() == DatabaseDriver::Postgres {
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
        }

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
            fencing: self.fencing.clone(),
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
    /// The canonical identity source bound at [`IngressUnitOfWork`]
    /// construction; single-node transactions cannot mint claim fences.
    fencing: IngressFencing,
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

    /// The fencing mode bound to this transaction.
    pub fn fencing(&self) -> &IngressFencing {
        &self.fencing
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
        if self.transaction.driver() == DatabaseDriver::Sqlite {
            return Ok(());
        }
        if set_local_transaction_timeouts(
            &mut self.transaction,
            Duration::from_millis(lock_timeout_ms),
            Duration::from_millis(statement_timeout_ms),
        )
        .await?
        {
            Ok(())
        } else {
            Err(IngressUowError::EpochProofMissing)
        }
    }

    #[cfg(feature = "clustering")]
    fn identity(&self) -> Uuid {
        self.identity
    }

    /// The canonical node-identity source this transaction may mint claim
    /// fences against.
    #[cfg(feature = "clustering")]
    fn bound_node_identity(&self) -> Option<&SharedNodeIdentity> {
        match &self.fencing {
            IngressFencing::Clustered(identity) => Some(identity),
            IngressFencing::SingleNode => None,
        }
    }

    /// Retain a minted node-authority guard until this transaction ends.
    #[cfg(feature = "clustering")]
    fn retain_authority(&mut self, guard: CurrentNodeIdentityGuard) {
        self.authority_guards.push(guard);
    }
}

#[cfg(test)]
mod tests;
