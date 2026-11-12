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
pub(crate) use delivery_progress::{DeliveryPolicyDiscardReason, DeliveryProgressRepository};
mod carbon_receipts;
pub(crate) use carbon_receipts::CarbonReceiptRepository;
mod durable_more;
mod effect_descendants;
#[cfg(feature = "test-support")]
pub(crate) use effect_descendants::begin_raw_ingress_writer_with_capability;
#[cfg(feature = "test-support")]
pub(crate) use effect_descendants::begin_raw_ingress_writer_with_definer_api_candidate;
pub(crate) use effect_descendants::canonical_nowait_error;
pub(crate) use effect_descendants::{begin_raw_ingress_writer, bound_raw_transaction};
mod error;
pub use effect_descendants::{EffectDeliveryBinding, EffectDescendantRepository};
mod extension_grants;
mod room_observation;
pub(crate) use room_observation::ObserverRetentionBatch;
pub(crate) use room_observation::{
    initialize_room_observations, CapturedRoomSource, ObservationWork, RoomObservationRepository,
};
pub use room_observation::{
    prepare_dark_room_observation_schema, verify_attested_room_observation_schema,
    verify_attested_room_observation_schema_v1, RoomObservationSchemaError,
    RoomObservationSchemaEvidence, ROOM_OBSERVATION_OWNER_STATE_TABLE,
    ROOM_OBSERVATION_SCHEMA_REVISION,
};
pub use room_observation::{ObservationError, RoomPublication};
mod pending_receipts;
mod policy_discard;
pub(crate) use policy_discard::PolicyDiscardReason;
mod recovery_receipts;
pub(crate) use pending_receipts::PendingReceiptRepository;
mod archive_ordinal;
mod repositories;
mod retry;
pub(crate) use recovery_receipts::{
    PendingNotificationPreparation, PendingNotificationRecovery, RecoveryCompletion,
    RecoveryReceiptRepository,
};
mod send_attempts;
pub(crate) use send_attempts::{send_attempt_blocks_delivery, SendAttemptStatus};
#[cfg(feature = "test-support")]
pub(crate) use send_attempts::{start_fresh_postgres, start_fresh_sqlite};
pub use send_attempts::{SendAttemptRepository, SendClaim, SendLease, SendObligation};
mod projections;
pub(crate) use projections::ProjectionRepository;
mod relation_guard;
pub use relation_guard::{RelationGuardCapability, RelationGuardCapabilityError};
mod settlement;
pub(crate) use settlement::settle_recorded;
mod standalone_receipt_writer;
pub(crate) use standalone_receipt_writer::StandaloneReceiptWriter;

pub use error::IngressUowError;
pub use extension_grants::{
    ConfiguredPluginGrants, GrantAssertion, GrantAssertionFailure, GrantSync,
};
#[allow(unused_imports)] // exported for dark tests and the future activation slice.
pub(crate) use repositories::RoomOwnerDecision;
pub use repositories::{
    CanonicalMessageRepository, DeliveryEffectRepository, EffectIntentRepository,
    EffectReceiptRepository, ExtensionGrantRepository, FrontierOutcome, InboxRepository,
    MamArchiveRepository, PrincipalAssertion, PrincipalRepository, ReconcileVerdict,
    SmIngressRepository, SmIngressStreamRepository,
};
#[cfg(feature = "clustering")]
pub use repositories::{ClaimRepository, RoomClaimFence, SmClaimFence};
#[cfg(all(test, not(feature = "clustering")))]
pub(crate) use repositories::{
    ClosedRoomOwnerResultOutcome, ClosedRoomOwnerResultProposal, ClosedRoomOwnerResultSource,
};
pub(crate) use retry::is_database_timeout;
pub use retry::{run_with_retry, DbRetryClass, RetryExhausted};

pub(crate) async fn candidate_boolean(
    transaction: &mut Transaction<'_>,
    query: &str,
    params: impl crate::db::IntoParams,
) -> Result<bool, IngressUowError> {
    let mut rows = transaction.query(query, params).await?;
    let row = rows
        .next()
        .await?
        .ok_or(crate::ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
    let result: bool = row.get(0)?;
    if rows.next().await?.is_some() {
        return Err(
            crate::ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult.into(),
        );
    }
    Ok(result)
}

pub(super) const DEFINER_API_PAGE_SIZE: i64 = 256;

use std::sync::Arc;
use std::time::Duration;

use crate::config::LineageConfig;
use crate::db::physical_attestation::AttestedPostgresWriterCapability;
use crate::db::{lineage, Database, DatabaseDriver, Transaction};
use crate::ingress_substrate::{
    begin_ingress_entry, set_local_transaction_timeouts, IngressEntryAcquisition,
    IngressEntryAuthority, IngressEntryClamp, IngressEntryRequest, IngressEntryTransaction,
    PreparedAttestedReceiptWriter,
};
#[cfg(feature = "clustering")]
use uuid::Uuid;
use waddle_xmpp::ingress::ProtocolEpoch;
#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::{CurrentNodeIdentityGuard, NodeIdentity, SharedNodeIdentity};

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
    observations: std::sync::Arc<std::sync::atomic::AtomicBool>,
    db: Arc<Database>,
    standalone_receipt_writer: StandaloneReceiptWriter,
    lineage: LineageConfig,
    /// Optional full-identity writer capability retained by the exact
    /// physically attested pool.
    attested_pg_writer: Option<AttestedPostgresWriterCapability>,
    /// The server's canonical node identity source. Bound once at
    /// construction so claim fences can only be minted against the real
    /// rotation gate, never a caller-constructed one.
    fencing: IngressFencing,
}

/// A private witness that the repository owns the complete transaction.
pub(crate) struct OwnedAliasScope(());

/// The epoch guard an ingress transaction is opened under.
#[derive(Clone, Copy)]
enum EpochPreparation<'g> {
    InstalledGuard,
    RelationGuard(&'g RelationGuardCapability),
}

impl IngressUnitOfWork {
    /// Bind a dark custody candidate to this exact attested pool and full
    /// configuration, never to a caller-selected schema name alone.
    #[cfg(all(feature = "test-support", feature = "clustering"))]
    pub(crate) fn custody_candidate_schema(
        &self,
        config: &crate::db::physical_attestation::PhysicalAttestationConfig,
    ) -> Result<String, IngressUowError> {
        let capability = self
            .attested_pg_writer
            .as_ref()
            .ok_or(IngressUowError::AttestedPoolRequired)?;
        self.db
            .verify_attested_pg_writer_capability(capability)
            .map_err(crate::db::DatabaseError::from)?;
        if !capability.matches_config(config) {
            return Err(IngressUowError::AttestedPoolRequired);
        }
        Ok(capability.identity().schema.name.0.clone())
    }

    pub(crate) fn database(&self) -> &Database {
        &self.db
    }

    pub(crate) fn standalone_receipt_writer(&self) -> &StandaloneReceiptWriter {
        &self.standalone_receipt_writer
    }

    pub(crate) fn enable_room_observations(&self) {
        self.observations
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Open against the supplied database admission lane in single-node mode.
    ///
    /// A full `Database` handle retains its main lane even when it also owns
    /// a reserved ingress lane. Only the attested `IngressAuthority` assembly
    /// path accepts that reserved lane; direct UoWs continue to use whichever
    /// admission class their supplied handle represents.
    ///
    /// A unit of work opened this way cannot mint claim fences; use
    /// [`Self::open_with_node_identity`] where fenced SM writes are needed.
    pub fn open(db: Database, lineage: LineageConfig) -> Result<Self, IngressUowError> {
        let attested_pg_writer = db.attested_pg_writer_capability();
        let db = Arc::new(db);
        let standalone_receipt_writer = StandaloneReceiptWriter::from_shared(Arc::clone(&db));
        Ok(Self {
            observations: Default::default(),
            db,
            standalone_receipt_writer,
            lineage,
            attested_pg_writer,
            fencing: IngressFencing::SingleNode,
        })
    }

    /// Enable the full-identity writer path using the capability retained
    /// by this unit of work's exact physically attested PostgreSQL pool.
    #[cfg(feature = "test-support")]
    pub(crate) fn with_attested_pg_writer(mut self) -> Result<Self, IngressUowError> {
        if self.db.driver() != DatabaseDriver::Postgres {
            return Err(IngressUowError::PostgresRequired);
        }
        self.attested_pg_writer = Some(
            self.db
                .attested_pg_writer_capability()
                .ok_or(IngressUowError::AttestedPoolRequired)?,
        );
        Ok(self)
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
        self.begin_inner(None, EpochPreparation::InstalledGuard)
            .await
    }

    /// Verify the dark candidate guard on this UOW's exact attested database
    /// handle. Call this on its main lane, which must retain a reserved ingress
    /// lane on the same pool. This does not enable the relation path for
    /// ordinary startup. Verification uses one lease from each lane so the
    /// owner locks precede the RR snapshot without self-deadlocking a one-slot
    /// admission lane.
    pub async fn verify_relation_guard_capability(
        &self,
    ) -> Result<RelationGuardCapability, RelationGuardCapabilityError> {
        RelationGuardCapability::verify(&self.db, &self.lineage).await
    }

    /// Explicitly begin with the verifier-issued candidate relation guard.
    /// `begin()` remains on the registered V1009 row-lock protocol.
    pub async fn begin_with_relation_guard(
        &self,
        capability: &RelationGuardCapability,
    ) -> Result<IngressUowTransaction<'_>, IngressUowError> {
        let writer = self
            .attested_pg_writer
            .as_ref()
            .ok_or(IngressUowError::AttestedPoolRequired)?;
        capability.verify_for_database(&self.db, writer)?;
        self.begin_inner(
            Some((Duration::from_secs(3), Duration::from_secs(5))),
            EpochPreparation::RelationGuard(capability),
        )
        .await
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
        self.begin_inner(Some((lock, statement)), EpochPreparation::InstalledGuard)
            .await
    }

    /// Fresh source rediscovery must see parents committed after an earlier
    /// read, even when the pool/server defaults to repeatable-read isolation.
    pub(crate) async fn begin_read_committed_with_timeouts(
        &self,
        lock: Duration,
        statement: Duration,
    ) -> Result<IngressUowTransaction<'_>, IngressUowError> {
        self.begin_inner(Some((lock, statement)), EpochPreparation::InstalledGuard)
            .await
    }

    /// The fixture's sealed marker binds this experiment to its attested
    /// reserved ingress lane. The installed-guard entrypoints remain unchanged.
    #[cfg(feature = "test-support")]
    pub(crate) async fn begin_in_relation_guard_lab<'a>(
        &'a self,
        fixture: &'a crate::test_support::relation_guard_lab::RelationGuardLabToken,
    ) -> Result<CandidateTransaction<'a>, IngressUowError> {
        if !fixture.permits(&self.db) || self.attested_pg_writer.is_none() {
            return Err(IngressUowError::AttestedPoolRequired);
        }
        let transaction = tokio::time::timeout(
            Duration::from_secs(5),
            self.begin_with_relation_guard(fixture.capability()),
        )
        .await
        .map_err(|_| IngressUowError::Timeout)??;
        Ok(CandidateTransaction { transaction })
    }

    /// Open through the typed ingress entry, then attest lineage.
    ///
    /// `bounds` is `(lock, statement)`: the lock bound is also the deadline
    /// for acquiring the transaction. The installed guard applies both as
    /// transaction timeouts; the relation guard keeps its own fixed clamp.
    async fn begin_inner(
        &self,
        bounds: Option<(Duration, Duration)>,
        preparation: EpochPreparation<'_>,
    ) -> Result<IngressUowTransaction<'_>, IngressUowError> {
        let (authority, explicit_timeouts, clamp) = match (preparation, &self.attested_pg_writer) {
            (EpochPreparation::InstalledGuard, None) => (
                IngressEntryAuthority::Unattested,
                bounds,
                IngressEntryClamp::Writer,
            ),
            (EpochPreparation::InstalledGuard, Some(capability)) => (
                IngressEntryAuthority::InstalledGuard(capability),
                bounds,
                IngressEntryClamp::Writer,
            ),
            (EpochPreparation::RelationGuard(guard), Some(_)) => (
                IngressEntryAuthority::RelationGuard(guard),
                None,
                IngressEntryClamp::RelationGuard,
            ),
            (EpochPreparation::RelationGuard(_), None) => {
                return Err(IngressUowError::AttestedPoolRequired)
            }
        };
        let mut entry = begin_ingress_entry(
            &self.db,
            IngressEntryRequest {
                authority,
                acquisition: IngressEntryAcquisition::WriterDefault,
                acquisition_deadline: bounds.map(|(lock, _)| lock),
                explicit_timeouts,
                clamp,
            },
        )
        .await?;

        // A private in-memory SQLite database is ephemeral by the server's own
        // lineage classification (nothing survives the process, so there is
        // no lineage row to attest); every durable database must attest.
        let lineage = if self.db.is_in_memory_sqlite() {
            IngressLineage::Ephemeral
        } else {
            let verified = match &self.attested_pg_writer {
                Some(capability) => capability
                    .attested_lineage_for_deployment(self.lineage.deployment_uuid)
                    .map_err(IngressUowError::Lineage)?,
                None => lineage::verify_in_transaction(
                    entry.transaction_mut(),
                    self.db.driver(),
                    &self.lineage,
                )
                .await
                .map_err(IngressUowError::Lineage)?,
            };
            IngressLineage::Attested(verified)
        };

        Ok(IngressUowTransaction {
            observations: self.observations.load(std::sync::atomic::Ordering::Acquire),
            entry,
            lineage,
            #[cfg(feature = "clustering")]
            identity: Uuid::new_v4(),
            fencing: self.fencing.clone(),
            #[cfg(feature = "clustering")]
            authority_guards: Vec::new(),
        })
    }
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
    observations: bool,
    /// The transaction as the typed ingress entry returned it: the proven
    /// epoch and, on an attested pool, the receipt-writer evidence captured
    /// while only the epoch relation prelock was held. Receipt operations
    /// must never re-prelock after an application row lock has been acquired.
    /// Only the entry constructs this value, so no unit-of-work transaction
    /// exists around a transaction that bypassed it.
    entry: IngressEntryTransaction<'a>,
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
}

/// Test wrapper used to inject commit acknowledgement faults after the normal
/// UOW/repository chain has completed.
#[cfg(feature = "test-support")]
pub(crate) struct CandidateTransaction<'a> {
    transaction: IngressUowTransaction<'a>,
}

#[cfg(feature = "test-support")]
impl CandidateTransaction<'_> {
    pub(crate) async fn has_fixture_bounds_and_isolation(
        &mut self,
    ) -> Result<bool, IngressUowError> {
        let mut rows = self
            .transaction
            .transaction_mut()
            .query(
                "SELECT pg_catalog.current_setting('lock_timeout')::interval = interval '2500 ms' \
             AND pg_catalog.current_setting('statement_timeout')::interval = interval '4500 ms' \
             AND pg_catalog.current_setting('transaction_isolation') = 'read committed' \
             AND coalesce(pg_catalog.current_setting('waddle.protocol_epoch', true) = \
                 (SELECT epoch::text FROM guarded.ingress_protocol_epoch WHERE id = 1), false) \
             AND coalesce(pg_catalog.current_setting('waddle.protocol_epoch_xid', true) = \
                 pg_catalog.pg_current_xact_id()::text, false) \
             AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype = 'relation' \
                 AND relation = 'guarded.ingress_protocol_epoch'::regclass \
                 AND pid = pg_catalog.pg_backend_pid() AND mode = 'AccessShareLock' AND granted) \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype = 'relation' \
                 AND relation = 'guarded.ingress_protocol_epoch'::regclass \
                 AND pid = pg_catalog.pg_backend_pid() AND mode = 'RowShareLock' AND granted)",
                (),
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or(IngressUowError::TransactionBoundsUnproven)?;
        Ok(row.get(0)?)
    }

    pub(crate) async fn use_fixture_shadow_path(&mut self) -> Result<(), IngressUowError> {
        self.transaction
            .transaction_mut()
            .execute("SET LOCAL search_path = shadow, guarded", ())
            .await?;
        Ok(())
    }

    pub(crate) async fn arm_fixture_commit_fault(&mut self) -> Result<(), IngressUowError> {
        self.transaction
            .transaction_mut()
            .execute("SELECT 1 /* waddle_relation_lab_business_commit */", ())
            .await?;
        Ok(())
    }

    pub(crate) async fn commit(self) -> Result<(), IngressUowError> {
        self.transaction.commit().await
    }
}

#[cfg(feature = "test-support")]
impl<'a> std::ops::Deref for CandidateTransaction<'a> {
    type Target = IngressUowTransaction<'a>;

    fn deref(&self) -> &Self::Target {
        &self.transaction
    }
}

#[cfg(feature = "test-support")]
impl std::ops::DerefMut for CandidateTransaction<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.transaction
    }
}

impl<'a> IngressUowTransaction<'a> {
    /// The protocol epoch locked at transaction start.
    pub fn protocol_epoch(&self) -> ProtocolEpoch {
        self.entry.protocol_epoch()
    }

    /// The fencing mode bound to this transaction.
    pub fn fencing(&self) -> &IngressFencing {
        &self.fencing
    }

    /// The lineage attestation verified on this same transaction.
    pub fn lineage(&self) -> &IngressLineage {
        &self.lineage
    }

    pub(crate) fn receipt_schema(&self) -> Option<&str> {
        self.entry
            .prepared()
            .map(PreparedAttestedReceiptWriter::schema)
    }

    pub(crate) fn uses_definer_api_candidate(&self) -> bool {
        self.entry
            .prepared()
            .is_some_and(PreparedAttestedReceiptWriter::uses_definer_api_candidate)
    }

    /// Select the unregistered function-owner API only in its disposable
    /// candidate fixture. This flag belongs to this already-prepared UOW and
    /// cannot create a second connection or transaction.
    #[cfg(feature = "test-support")]
    pub fn enable_definer_api_candidate_for_test(&mut self) -> Result<(), IngressUowError> {
        if self.entry.transaction_mut().driver() != DatabaseDriver::Postgres {
            return Err(IngressUowError::PostgresRequired);
        }
        let prepared = self
            .entry
            .prepared_mut()
            .filter(|prepared| prepared.uses_relation_guard())
            .ok_or(IngressUowError::AttestedPoolRequired)?;
        prepared.enable_definer_api_candidate_for_test();
        Ok(())
    }

    /// Test-only source-alias fixture access; not a runtime repository API.
    #[cfg(test)]
    pub(crate) fn source_alias_parts_for_fixture(
        &mut self,
    ) -> (&mut Transaction<'a>, Option<&PreparedAttestedReceiptWriter>) {
        self.prepared_parts()
    }

    /// Keep the prepared identity and its transaction together when ingress
    /// repositories choose their schema-qualified PostgreSQL statements.
    fn prepared_parts(&mut self) -> (&mut Transaction<'a>, Option<&PreparedAttestedReceiptWriter>) {
        self.entry.parts_mut()
    }

    /// The same pair with the transaction as the typed handle that
    /// scheduler stores require for descendant locks and writes.
    fn entered_parts(
        &mut self,
    ) -> (
        &mut crate::ingress_substrate::EnteredTransaction<'a>,
        Option<&PreparedAttestedReceiptWriter>,
    ) {
        self.entry.entered_parts_mut()
    }

    pub(crate) fn room_observers_enabled(&self) -> bool {
        self.observations
    }

    /// Commit all ingress and related durable writes atomically.
    pub async fn commit(self) -> Result<(), IngressUowError> {
        self.entry.commit().await.map_err(IngressUowError::from)
    }

    /// Raw SQL remains confined to ingress repositories so callers cannot
    /// bypass the epoch, lineage, and claim-fencing invariants.
    pub(crate) fn transaction_mut(&mut self) -> &mut Transaction<'a> {
        self.entry.transaction_mut()
    }

    /// The transaction as the typed handle scheduler stores require when a
    /// unit of work attaches their descendants in its own transaction.
    #[cfg(feature = "test-support")]
    pub(crate) fn entered_transaction_mut(
        &mut self,
    ) -> &mut crate::ingress_substrate::EnteredTransaction<'a> {
        self.entry.entered_parts_mut().0
    }

    /// Install per-transaction PostgreSQL timeout bounds without exposing raw
    /// SQL to ingress callers.
    pub async fn set_local_timeouts(
        &mut self,
        lock_timeout_ms: u64,
        statement_timeout_ms: u64,
    ) -> Result<(), IngressUowError> {
        if self.entry.transaction_mut().driver() == DatabaseDriver::Sqlite {
            return Ok(());
        }
        if set_local_transaction_timeouts(
            self.entry.transaction_mut(),
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

    #[cfg(feature = "clustering")]
    fn retained_authority(&self) -> Option<&CurrentNodeIdentityGuard> {
        let identity = self.bound_node_identity()?;
        self.authority_guards
            .iter()
            .find(|guard| identity.owns_guard(guard))
    }

    /// Reuse authority already retained by this transaction. Acquiring another
    /// read guard behind a queued rotation would deadlock against that guard.
    /// A newly acquired guard is returned to the caller so a failed claim
    /// assertion does not retain it until the transaction ends.
    #[cfg(feature = "clustering")]
    async fn acquire_authority(
        &mut self,
        expected: &NodeIdentity,
    ) -> Result<Option<CurrentNodeIdentityGuard>, IngressUowError> {
        let identity = self
            .bound_node_identity()
            .cloned()
            .ok_or(IngressUowError::NodeIdentityUnbound)?;
        if let Some(authority) = self.retained_authority() {
            return if authority.identity() == expected {
                Ok(None)
            } else {
                Err(IngressUowError::ClaimFenceMissing)
            };
        }
        identity
            .guard_if_current(expected)
            .await
            .map(Some)
            .ok_or(IngressUowError::ClaimFenceMissing)
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
mod lock_timeout_tests;

#[cfg(all(test, feature = "clustering"))]
mod archive_lock_order_tests;

#[cfg(test)]
mod canonical_qualified_tests;

#[cfg(test)]
mod raw_writer_tests;
