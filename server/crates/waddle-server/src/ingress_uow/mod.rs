//! Atomic dialect-aware ingress write boundary.
//!
//! A transaction takes locks in the fixed order epoch, exact ownership claim,
//! then fenced child rows such as `sm_sessions`, room archives, and ingress
//! projections. Dropping an uncommitted [`IngressUowTransaction`] rolls it
//! back through [`crate::db::Transaction`].

mod archive_dispatch;
mod delivery_progress;
pub(crate) use archive_dispatch::{
    ArchiveDispatchObligation, ArchiveDispatchRepository, DispatchReadiness, DispatchTarget,
};
pub(crate) use delivery_progress::DeliveryProgressRepository;
mod carbon_receipts;
pub(crate) use carbon_receipts::CarbonReceiptRepository;
mod durable_more;
mod error;
mod extension_grants;
mod judgment_outbox;
pub(crate) use judgment_outbox::MessageJudgmentOutboxRepository;
mod pending_receipts;
mod recovery_receipts;
pub(crate) use pending_receipts::PendingReceiptRepository;
mod archive_ordinal;
mod repositories;
mod retry;
pub(crate) use recovery_receipts::{RecoveryCompletion, RecoveryReceiptRepository};
mod settlement;
pub(crate) use settlement::settle_recorded;

pub use error::IngressUowError;
pub use extension_grants::{
    ConfiguredPluginGrants, GrantAssertion, GrantAssertionFailure, GrantSync,
};
pub use repositories::{
    CanonicalMessageRepository, DeliveryEffectRepository, EffectIntentRepository,
    EffectReceiptRepository, ExtensionGrantRepository, FrontierOutcome, InboxRepository,
    MamArchiveRepository, PrincipalAssertion, PrincipalRepository, ReconcileVerdict,
    SmIngressRepository, SmIngressStreamRepository,
};
#[cfg(feature = "clustering")]
pub use repositories::{ClaimRepository, RoomClaimFence, SmClaimFence};
pub(crate) use retry::is_database_timeout;
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
    /// Operator control for the `is_question`/safety community-enrichment
    /// judgment outbox (#1831 Phase 2). `false` (the default from every
    /// [`Self::open`]/[`Self::open_with_node_identity`] call) means every
    /// [`IngressUowTransaction`] this factory opens reports
    /// [`IngressUowTransaction::judgment_outbox_enabled`] as `false`, so
    /// `ingress::durable::apply_durable` never enqueues a row — this
    /// measurement-only feature stays fully inert unless a caller
    /// deliberately opts in via [`Self::with_judgment_outbox_enabled`].
    judgment_outbox_enabled: bool,
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
            judgment_outbox_enabled: false,
        })
    }

    /// Ownership fencing configured for transactions opened by this factory.
    pub fn fencing(&self) -> &IngressFencing {
        &self.fencing
    }

    /// Opt this factory's transactions into enqueueing a
    /// `message_judgment_outbox` row alongside every freshly archived
    /// direct/groupchat message (#1831 Phase 2). Additive and narrow by
    /// design: every existing `open`/`open_with_node_identity` call site
    /// keeps building a disabled unit of work unless it explicitly chains
    /// this or [`Self::set_judgment_outbox_enabled`].
    pub fn with_judgment_outbox_enabled(mut self, enabled: bool) -> Self {
        self.set_judgment_outbox_enabled(enabled);
        self
    }

    /// In-place form of [`Self::with_judgment_outbox_enabled`]. Production
    /// wiring uses this from
    /// [`crate::ingress::IngressAuthority::with_judgment_outbox_enabled`],
    /// which cannot move `self.uow` out of `self` — `IngressAuthority`
    /// implements `Drop`, and Rust forbids partially moving a field out of
    /// any type that does, even to immediately move it back in.
    pub fn set_judgment_outbox_enabled(&mut self, enabled: bool) {
        self.judgment_outbox_enabled = enabled;
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
        self.begin_inner(None).await
    }

    /// Bound even the initial epoch lock wait before taking any row locks.
    /// Transaction acquisition, including pool checkout on both backends and
    /// SQLite's `BEGIN IMMEDIATE`, uses the lock bound. PostgreSQL's local
    /// timeouts apply only after checkout; acquisition expiry is also typed.
    pub async fn begin_with_timeouts(
        &self,
        lock: Duration,
        statement: Duration,
    ) -> Result<IngressUowTransaction<'_>, IngressUowError> {
        self.begin_inner(Some((lock, statement))).await
    }

    async fn begin_inner(
        &self,
        bounds: Option<(Duration, Duration)>,
    ) -> Result<IngressUowTransaction<'_>, IngressUowError> {
        let acquisition = async {
            match self.db.driver() {
                DatabaseDriver::Sqlite => self.db.begin_immediate().await,
                DatabaseDriver::Postgres => self.db.begin().await,
            }
        };
        let mut transaction = match bounds {
            Some((lock, _)) => acquire_transaction_with_timeout(lock, acquisition).await?,
            None => acquisition.await?,
        };
        if let Some((lock, statement)) = bounds {
            if !set_local_transaction_timeouts(&mut transaction, lock, statement).await? {
                return Err(IngressUowError::TransactionBoundsUnproven);
            }
        }
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

        // A private in-memory SQLite database is ephemeral by the server's own
        // lineage classification (nothing survives the process, so there is
        // no lineage row to attest); every durable database must attest.
        let lineage = if self.db.is_in_memory_sqlite() {
            IngressLineage::Ephemeral
        } else {
            IngressLineage::Attested(
                lineage::verify_in_transaction(&mut transaction, self.db.driver(), &self.lineage)
                    .await
                    .map_err(IngressUowError::Lineage)?,
            )
        };

        Ok(IngressUowTransaction {
            transaction,
            protocol_epoch,
            lineage,
            #[cfg(feature = "clustering")]
            identity: Uuid::new_v4(),
            fencing: self.fencing.clone(),
            #[cfg(feature = "clustering")]
            authority_guards: Vec::new(),
            judgment_outbox_enabled: self.judgment_outbox_enabled,
        })
    }
}

async fn acquire_transaction_with_timeout<'a>(
    lock: Duration,
    acquisition: impl std::future::Future<Output = Result<Transaction<'a>, crate::db::DatabaseError>>,
) -> Result<Transaction<'a>, IngressUowError> {
    Ok(tokio::time::timeout(lock, acquisition)
        .await
        .map_err(|_| IngressUowError::Timeout)??)
}

/// The lineage attestation an ingress transaction runs under.
#[derive(Debug, Clone)]
pub enum IngressLineage {
    /// A durable database whose lineage row verified against this deployment.
    Attested(lineage::AttestedLineage),
    /// A private in-memory SQLite database: nothing outlives the process and
    /// no lineage row exists to attest.
    Ephemeral,
}

/// An ingress transaction carrying the locked epoch and verified lineage.
///
/// There is intentionally no rollback method: dropping this value without
/// [`Self::commit`] rolls back the underlying database transaction.
pub struct IngressUowTransaction<'a> {
    transaction: Transaction<'a>,
    protocol_epoch: ProtocolEpoch,
    lineage: IngressLineage,
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
    /// Snapshot of [`IngressUnitOfWork::with_judgment_outbox_enabled`] taken
    /// when this transaction opened. See
    /// [`Self::judgment_outbox_enabled`].
    judgment_outbox_enabled: bool,
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
    pub fn lineage(&self) -> &IngressLineage {
        &self.lineage
    }

    /// Whether `ingress::durable::apply_durable` may enqueue a
    /// `message_judgment_outbox` row inside this same transaction (#1831
    /// Phase 2). `false` unless the owning [`IngressUnitOfWork`] was built
    /// with [`IngressUnitOfWork::with_judgment_outbox_enabled`].
    pub(crate) fn judgment_outbox_enabled(&self) -> bool {
        self.judgment_outbox_enabled
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

#[cfg(test)]
mod judgment_outbox_tests;

#[cfg(test)]
mod lock_timeout_tests;
