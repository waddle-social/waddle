//! Implementation behind the parent module facade.

use super::authority;
use super::refresh_retention;
use super::Arc;
use super::BareJid;
use super::DbRetryClass;
use super::Error;
use super::Instant;
use super::MessageEnvelope;
use super::OriginId;
use super::StdDuration;
use super::Uuid;
#[cfg(test)]
use super::FAIL_ORPHAN_SWEEP;
use super::{
    resolve_alias, AliasResolution, CanonicalSourceBinding, DeliveryKey, IngressOrdinal,
    MessageKey, NormalizedTarget, NormalizedTargetStorage, ProtocolEpoch, SemanticDigest,
    SmIngressId, StoredAlias, WireHandledCount,
};
use super::{AtomicBool, AtomicUsize, Ordering};
use super::{Database, DatabaseDriver, DatabaseError, Row, Transaction};
use super::{DateTime, Duration, Utc};
use super::{Digest, Sha256};
use super::{SmIngressAppendKey, SmIngressReceiptKind};
use crate::db::physical_attestation::AttestedPostgresWriterCapability;

/// The time an origin-id alias remains available after its message becomes
/// terminal.  The garbage collector receives `now` and binds the derived
/// cutoff, keeping wall-clock decisions at its caller boundary.
pub const ALIAS_RETENTION: Duration = Duration::days(8);

/// Highest protocol epoch whose write semantics this binary implements:
/// the substrate's retention, locking, and proof contracts are built for
/// the epoch-1 activation this slice installs.
///
/// The GC's self-installed proof claims at most this epoch; a live epoch
/// beyond it fails closed so a future activation fences old collectors
/// instead of being claimed automatically.  Bump only in the slice that
/// teaches the substrate the new epoch's semantics.
pub fn supported_protocol_epoch() -> ProtocolEpoch {
    ProtocolEpoch::from_storage(1)
}

/// Ingress tables protected by the epoch-proof triggers from migration V1009.
///
/// Keep this list in lock-step with the migration manifest: tests query the
/// live catalog to ensure a newly-added ingress table cannot accidentally be
/// left outside the activation boundary.
pub const EPOCH_GUARDED_TABLES: [&str; 13] = [
    "ingress_messages",
    "ingress_origin_aliases",
    "ingress_sm_refs",
    "ingress_deliveries",
    "ingress_sm_streams",
    "ingress_effect_intents",
    "ingress_effect_receipts",
    "ingress_carbon_receipts",
    "ingress_delivery_receipts",
    "ingress_archive_dispatch",
    "ingress_send_attempts",
    "ingress_effect_descendants",
    "ingress_owner_rejections",
];

/// Fail-closed errors for the dark ingress substrate.
///
/// The database adapter error is intentionally not retained as a source:
/// database diagnostics can include SQL values, while an origin-id is an
/// opaque client value that must never appear in Debug or Display output.
#[derive(Debug, Error)]
pub enum IngressSubstrateError {
    #[error("ingress storage transaction timed out")]
    Timeout,
    #[error("malformed stored envelope")]
    InvalidStoredEnvelope,
    #[error("message key is already bound to a different digest or envelope")]
    MessageContentConflict,
    #[error("malformed stored stream binding or checkpoint")]
    InvalidStoredStream,
    #[error("ingress stream is missing")]
    StreamMissing,
    #[error("ingress substrate database operation failed")]
    Database { retry_class: DbRetryClass },
    #[error("ingress substrate returned a malformed stored message key")]
    InvalidStoredMessageKey,
    #[error("ingress substrate returned a malformed stored timestamp")]
    InvalidStoredTimestamp,
    #[error("ingress substrate returned a malformed semantic digest")]
    InvalidStoredDigest,
    #[error("persisted ingress alias identity is malformed or inconsistent with its hash")]
    AliasIdentityConflict,
    #[error("relayed origin alias does not prove the supplied canonical message")]
    RelayedAliasUnproven,
    #[error("canonical source principal conflicts with its immutable source binding")]
    SourceBindingConflict,
    #[error("canonical source principal is not durably asserted")]
    PrincipalAssertionFailed,
    #[error("ingress alias disappeared during concurrent resolution")]
    AliasMissingAfterConflict,
    #[error("ingress definer API returned an invalid result")]
    InvalidDefinerApiResult,
    #[error("ingress sm ordinal is already bound to a different message")]
    SmOrdinalConflict,
    #[error("ingress delivery key is already bound to a different message")]
    DeliveryKeyConflict,
    #[error("live ingress protocol epoch exceeds what this binary supports")]
    UnsupportedLiveEpoch,
    #[error("ingress schema identity changed before the epoch prelock")]
    SchemaIdentityMismatch,
    #[error("ingress epoch changed while preparing the V1009 compatibility lock")]
    EpochChangedDuringPreparation,
    #[error("the ingress relation prelock requires READ COMMITTED isolation")]
    ReadCommittedRequired,
    #[error("the pooled receipt COMMIT outcome is indeterminate")]
    ReceiptCommitIndeterminate,
}

/// Outcome of adding a child identity that requires a live message row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageWriteOutcome {
    Recorded,
    /// The identical identity pair was already durable (ambiguous-commit
    /// retry); nothing was written.
    AlreadyRecorded,
    MessageVanished,
}

/// Outcome of recording a terminal proof for a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalizeOutcome {
    Terminalized,
    AlreadyTerminal,
    MessageVanished,
}

/// Work completed by one alias garbage-collection pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AliasGcOutcome {
    pub deleted_messages: usize,
    pub deleted_orphan_proofs: usize,
    pub completed: bool,
}

/// Per-operation bounds and cooperative deadline for one alias GC run.
#[derive(Debug, Clone)]
pub struct AliasGcBudget {
    pub deadline: Instant,
    pub lock_timeout: StdDuration,
    /// Bound on each single-row statement of a candidate transaction.
    pub statement_timeout: StdDuration,
    /// Bound on the candidate scan, which walks retained history.
    pub scan_timeout: StdDuration,
    /// Committed deletions, observable by the caller even when the run is
    /// cancelled from outside before it can return its outcome.
    pub progress: AliasGcProgress,
}

/// Messages and orphan proofs committed as deleted so far by one alias GC run.
#[derive(Debug, Clone, Default)]
pub struct AliasGcProgress {
    pub(in crate::ingress_substrate) messages: Arc<AtomicUsize>,
    pub(in crate::ingress_substrate) orphan_proofs: Arc<AtomicUsize>,
    pub(in crate::ingress_substrate) orphan_cursor:
        Arc<tokio::sync::Mutex<Option<SmIngressAppendKey>>>,
    /// When the orphan sweep last COMPLETED a traversal, shared across passes.
    /// `None` until the first one, so a fresh process sweeps once promptly.
    pub(in crate::ingress_substrate) orphan_swept_at: Arc<tokio::sync::Mutex<Option<Instant>>>,
    /// A sweep is owed and has not finished yet. Shared across passes so the
    /// trigger survives a pass that ends before the traversal does: a retry
    /// carries fresh pass-local counters, so neither `deleted_messages` nor the
    /// interval can be re-derived from it.
    pub(in crate::ingress_substrate) orphan_sweep_pending: Arc<AtomicBool>,
}

impl AliasGcBudget {
    /// A new pass over the same shared traversal and sweep schedule.
    #[cfg(test)]
    pub(in crate::ingress_substrate) fn fresh_progress_pass(&self) -> Self {
        Self {
            progress: self.progress.fresh_pass(),
            ..self.clone()
        }
    }
}

impl AliasGcProgress {
    /// Start fresh accounting while retaining the traversal position across passes.
    pub(crate) fn fresh_pass(&self) -> Self {
        Self {
            orphan_cursor: self.orphan_cursor.clone(),
            orphan_swept_at: self.orphan_swept_at.clone(),
            orphan_sweep_pending: self.orphan_sweep_pending.clone(),
            ..Self::default()
        }
    }

    pub fn committed(&self) -> usize {
        self.messages.load(Ordering::Acquire)
    }

    pub fn committed_orphan_proofs(&self) -> usize {
        self.orphan_proofs.load(Ordering::Acquire)
    }

    /// Record that a sweep is owed. Survives until a traversal finishes.
    pub(in crate::ingress_substrate) fn mark_orphan_sweep_pending(&self) {
        self.orphan_sweep_pending.store(true, Ordering::Release);
    }

    pub(in crate::ingress_substrate) fn record(&self, deleted_messages: usize) {
        if deleted_messages == 0 {
            return;
        }
        self.messages.fetch_add(deleted_messages, Ordering::AcqRel);
        // Arm the sweep HERE, at the commit, not from the pass outcome. A
        // reclaimed canonical row is exactly when a cross-node append can be
        // orphaned, and a pass can still fail, be cancelled, or run out of
        // budget on a later candidate -- after this deletion is durable. Those
        // exits never reach the end of the pass, and the retry's counters are
        // fresh, so anything derived from the outcome would lose the trigger.
        self.mark_orphan_sweep_pending();
    }
}

/// PostgreSQL timeout category surfaced by alias GC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseTimeoutKind {
    Lock,
    Statement,
}

/// Typed reason an alias GC run failed.
#[derive(Debug, Error)]
pub enum AliasGcError {
    #[error("ingress alias GC database operation timed out ({kind:?})")]
    DatabaseTimeout { kind: DatabaseTimeoutKind },
    #[error(transparent)]
    Substrate(#[from] IngressSubstrateError),
}

/// Alias GC failure with all successfully committed progress retained.
#[derive(Debug, Error)]
#[error("ingress alias GC failed after deleting {deleted_messages} messages and {deleted_orphan_proofs} orphan proofs: {error}")]
pub struct AliasGcFailure {
    pub deleted_messages: usize,
    pub deleted_orphan_proofs: usize,
    #[source]
    pub error: AliasGcError,
}

/// Handle for the ingress substrate on either supported database backend.
///
/// The per-operation functions below accept a caller-owned transaction so a
/// future repository can compose ingress writes with its own atomic work.
#[derive(Clone)]
pub struct PostgresIngressSubstrate {
    pub(in crate::ingress_substrate) db: Database,
}

impl PostgresIngressSubstrate {
    /// Open the substrate against the global database.
    pub fn open(db: Database) -> Result<Self, IngressSubstrateError> {
        Ok(Self { db })
    }

    /// Start a transaction, taking SQLite's write reservation immediately.
    /// PostgreSQL begins at READ COMMITTED and locks the supported live epoch.
    ///
    /// The returned value and the write methods still accept raw transactions:
    /// callers supplying another transaction must establish these preconditions
    /// before acquiring application locks. This API does not prove that order.
    pub async fn begin(&self) -> Result<Transaction<'_>, IngressSubstrateError> {
        self.begin_inner().await
    }

    /// Start a writer using the full-identity capability retained by its
    /// physically attested pool.
    #[cfg(feature = "test-support")]
    pub(crate) async fn begin_with_attested_pg_writer(
        &self,
        capability: &AttestedPostgresWriterCapability,
    ) -> Result<Transaction<'_>, IngressSubstrateError> {
        self.db
            .verify_attested_pg_writer_capability(capability)
            .map_err(DatabaseError::from)
            .map_err(discard_database_error)?;

        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(discard_database_error)?;
        tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
            .await
            .map_err(discard_database_error)?;
        if !tighten_local_transaction_timeouts(&mut tx)
            .await
            .map_err(discard_database_error)?
        {
            return Err(IngressSubstrateError::Database {
                retry_class: DbRetryClass::NotRetryable,
            });
        }
        let epoch = prepare_attested_receipt_transaction(&mut tx, capability)
            .await?
            .protocol_epoch();
        if epoch > supported_protocol_epoch() {
            return Err(IngressSubstrateError::UnsupportedLiveEpoch);
        }
        Ok(tx)
    }

    async fn begin_inner(&self) -> Result<Transaction<'_>, IngressSubstrateError> {
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(discard_database_error)?;
        if tx.driver() == DatabaseDriver::Postgres {
            tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
                .await
                .map_err(discard_database_error)?;
            if !tighten_local_transaction_timeouts(&mut tx)
                .await
                .map_err(discard_database_error)?
            {
                return Err(IngressSubstrateError::Database {
                    retry_class: DbRetryClass::NotRetryable,
                });
            }
            let epoch = acquire_supported_epoch_lock_first(&mut tx).await?;
            tx.execute(
                "SELECT set_config('waddle.protocol_epoch', ?, true), set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)",
                crate::db_params![epoch.to_storage().to_string()],
            )
            .await
            .map_err(discard_database_error)?;
        }
        Ok(tx)
    }

    pub async fn record_message(
        &self,
        tx: &mut Transaction<'_>,
        message_key: MessageKey,
        digest: &SemanticDigest,
        envelope: Option<&MessageEnvelope>,
    ) -> Result<(), IngressSubstrateError> {
        record_message(tx, message_key, digest, envelope).await
    }

    pub async fn resolve_and_record_alias(
        &self,
        tx: &mut Transaction<'_>,
        sender: &BareJid,
        target: &NormalizedTarget,
        origin_id: &OriginId,
        digest: &SemanticDigest,
        mint: impl FnOnce() -> MessageKey,
    ) -> Result<AliasResolution, IngressSubstrateError> {
        resolve_and_record_alias(tx, sender, target, origin_id, digest, mint).await
    }

    pub async fn insert_sm_ref(
        &self,
        tx: &mut Transaction<'_>,
        sm_ingress_id: SmIngressId,
        ordinal: IngressOrdinal,
        wire_h: WireHandledCount,
        message_key: MessageKey,
    ) -> Result<MessageWriteOutcome, IngressSubstrateError> {
        insert_sm_ref(tx, sm_ingress_id, ordinal, wire_h, message_key).await
    }

    pub async fn record_delivery(
        &self,
        tx: &mut Transaction<'_>,
        delivery_key: DeliveryKey,
        message_key: MessageKey,
    ) -> Result<MessageWriteOutcome, IngressSubstrateError> {
        record_delivery(tx, delivery_key, message_key).await
    }

    pub async fn terminalize_message(
        &self,
        tx: &mut Transaction<'_>,
        message_key: MessageKey,
        proven_terminal_at: DateTime<Utc>,
    ) -> Result<TerminalizeOutcome, IngressSubstrateError> {
        terminalize_message(tx, message_key, proven_terminal_at).await
    }

    pub async fn gc_expired_aliases(
        &self,
        now: DateTime<Utc>,
        budget: AliasGcBudget,
    ) -> Result<AliasGcOutcome, AliasGcFailure> {
        gc_expired_aliases(&self.db, now, budget).await
    }
}

/// Acquire the singleton epoch row `FOR SHARE` as the first lock of a
/// substrate write transaction.
///
/// Global lock order: **epoch row, then message row, then child rows**.
/// The V1009 guard trigger requests the epoch row `FOR SHARE` during every
/// protected write; taking it up front keeps that request already granted,
/// so a write path that first locked a message row can never wait on the
/// epoch behind a queued activation while GC (epoch-first by construction)
/// waits on that same message row — the three-party deadlock cycle.
pub(crate) async fn acquire_epoch_lock_first(
    tx: &mut Transaction<'_>,
) -> Result<ProtocolEpoch, IngressSubstrateError> {
    acquire_epoch_lock_first_in_prepared_transaction(tx, None).await
}

async fn acquire_epoch_lock_first_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
) -> Result<ProtocolEpoch, IngressSubstrateError> {
    if let Some(prepared) = prepared.filter(|prepared| prepared.uses_relation_guard()) {
        return Ok(prepared.protocol_epoch());
    }
    read_live_epoch_for_share_in_prepared_transaction(tx, prepared)
        .await
        .map_err(|error| match error {
            EpochLockError::Database(error) => discard_database_error(error),
            EpochLockError::MissingRow => IngressSubstrateError::Database {
                retry_class: DbRetryClass::NotRetryable,
            },
            EpochLockError::UnsupportedLiveEpoch => IngressSubstrateError::UnsupportedLiveEpoch,
        })
}

pub(in crate::ingress_substrate) async fn acquire_supported_epoch_lock_first(
    tx: &mut Transaction<'_>,
) -> Result<ProtocolEpoch, IngressSubstrateError> {
    acquire_supported_epoch_in_prepared_transaction(tx, None).await
}

pub(in crate::ingress_substrate) async fn acquire_supported_epoch_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
) -> Result<ProtocolEpoch, IngressSubstrateError> {
    let live = acquire_epoch_lock_first_in_prepared_transaction(tx, prepared).await?;
    if live > supported_protocol_epoch() {
        return Err(IngressSubstrateError::UnsupportedLiveEpoch);
    }
    Ok(live)
}

/// Why the singleton epoch row could not be read `FOR SHARE`.  Kept as the
/// raw driver error so callers can classify Postgres timeouts before
/// discarding the diagnostic.
pub(in crate::ingress_substrate) enum EpochLockError {
    Database(DatabaseError),
    MissingRow,
    UnsupportedLiveEpoch,
}

pub(in crate::ingress_substrate) async fn read_live_epoch_for_share(
    tx: &mut Transaction<'_>,
) -> Result<ProtocolEpoch, EpochLockError> {
    read_live_epoch_for_share_in_prepared_transaction(tx, None).await
}

async fn read_live_epoch_for_share_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
) -> Result<ProtocolEpoch, EpochLockError> {
    let qualified;
    let postgres = if prepared.is_some() {
        qualified = format!(
            "SELECT epoch FROM {} WHERE id = 1 FOR SHARE",
            prepared_ingress_relation(prepared, PreparedIngressRelation::Epoch)
        );
        qualified.as_str()
    } else {
        READ_EPOCH_POSTGRES
    };
    let mut rows = tx
        .query(
            if tx.driver() == DatabaseDriver::Postgres {
                postgres
            } else {
                READ_EPOCH_SQLITE
            },
            (),
        )
        .await
        .map_err(EpochLockError::Database)?;
    let live_epoch: i64 = rows
        .next()
        .await
        .map_err(EpochLockError::Database)?
        .ok_or(EpochLockError::MissingRow)?
        .get(0)
        .map_err(EpochLockError::Database)?;
    u32::try_from(live_epoch)
        .map(ProtocolEpoch::from_storage)
        .map_err(|_| EpochLockError::UnsupportedLiveEpoch)
}

/// Insert immutable message content, or fill an alias-created empty envelope.
/// Retries preserve existing content and reject contradictory digests/envelopes.
pub async fn record_message(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
    digest: &SemanticDigest,
    envelope: Option<&MessageEnvelope>,
) -> Result<(), IngressSubstrateError> {
    record_message_in_prepared_transaction(tx, None, message_key, digest, envelope, None).await
}

#[allow(dead_code)] // staged repository contract; activation is intentionally deferred.
pub(crate) async fn record_message_with_source(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
    digest: &SemanticDigest,
    envelope: Option<&MessageEnvelope>,
    source: &CanonicalSourceBinding,
) -> Result<(), IngressSubstrateError> {
    record_message_in_prepared_transaction(tx, None, message_key, digest, envelope, Some(source))
        .await
}

pub(crate) async fn record_message_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    message_key: MessageKey,
    digest: &SemanticDigest,
    envelope: Option<&MessageEnvelope>,
    source: Option<&CanonicalSourceBinding>,
) -> Result<(), IngressSubstrateError> {
    acquire_supported_epoch_in_prepared_transaction(tx, prepared).await?;
    record_message_after_epoch_lock(tx, prepared, message_key, digest, envelope, source)
        .await
        .map(|_| ())
}

async fn record_message_after_epoch_lock(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    message_key: MessageKey,
    digest: &SemanticDigest,
    envelope: Option<&MessageEnvelope>,
    source: Option<&CanonicalSourceBinding>,
) -> Result<MessageWriteOutcome, IngressSubstrateError> {
    if insert_message(tx, prepared, message_key, digest, envelope, source).await? {
        Ok(MessageWriteOutcome::Recorded)
    } else {
        if let Some(source) = source {
            assert_source_principal_if_recorded(tx, prepared, message_key, source).await?;
        }
        authority::complete_message_envelope(tx, prepared, message_key, digest, envelope).await?;
        Ok(MessageWriteOutcome::AlreadyRecorded)
    }
}

pub(in crate::ingress_substrate) async fn insert_message(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    message_key: MessageKey,
    digest: &SemanticDigest,
    envelope: Option<&MessageEnvelope>,
    source: Option<&CanonicalSourceBinding>,
) -> Result<bool, IngressSubstrateError> {
    let (digest_version, digest_bytes) = digest.to_storage();
    if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
        let function = format!(
            "{}.waddle_ingress_insert_message",
            quote_postgres_identifier(prepared.schema())
        );
        let sql = format!(
            "SELECT {function}(?::uuid, ?::integer, ?::bytea, ?::smallint, ?::bytea, \
             ?::text, ?::uuid, ?::numeric, ?::numeric, ?::uuid, ?::numeric)"
        );
        let principal = source.map(|source| source.principal());
        let sm_occurrence = source.and_then(CanonicalSourceBinding::sm_occurrence);
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![
                    message_key.to_storage().to_string(),
                    i32::from(digest_version),
                    digest_bytes.to_vec(),
                    envelope.map(|value| value.version().to_storage()),
                    envelope.map(authority::serialize_envelope).transpose()?,
                    principal.map(|principal| principal.bare_jid().to_string()),
                    principal.map(|principal| principal.auth_context_id().as_uuid().to_string()),
                    principal.map(|principal| principal.auth_context_version().get().to_string()),
                    principal.map(|principal| principal.auth_epoch().get().to_string()),
                    sm_occurrence.map(|(id, _)| id.to_storage().to_string()),
                    sm_occurrence.map(|(_, ordinal)| ordinal.to_storage().to_string()),
                ],
            )
            .await
            .map_err(discard_database_error)?;
        let row = rows
            .next()
            .await
            .map_err(discard_database_error)?
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        let inserted: bool = row.get(0).map_err(discard_database_error)?;
        if rows.next().await.map_err(discard_database_error)?.is_some() {
            return Err(IngressSubstrateError::InvalidDefinerApiResult);
        }
        return Ok(inserted);
    }

    let qualified;
    let postgres = if prepared.is_some() {
        qualified = format!(
            "INSERT INTO {} (message_key, digest_version, digest, envelope_version, envelope, source_binding_version, source_bare_jid, source_auth_context_id, source_auth_context_version, source_auth_epoch, source_sm_ingress_id, source_ingress_ordinal) VALUES (?::uuid, ?, ?, ?, ?, ?, ?, ?::uuid, ?::numeric, ?::numeric, ?::uuid, ?::numeric) ON CONFLICT (message_key) DO NOTHING",
            prepared_ingress_relation(prepared, PreparedIngressRelation::Messages)
        );
        qualified.as_str()
    } else {
        INSERT_MESSAGE_POSTGRES
    };
    let inserted = tx
        .execute(
            if tx.driver() == DatabaseDriver::Postgres {
                postgres
            } else {
                INSERT_MESSAGE_SQLITE
            },
            crate::db_params![
                message_key.to_storage().to_string(),
                i32::from(digest_version),
                digest_bytes.to_vec(),
                envelope.map(|value| value.version().to_storage()),
                envelope.map(authority::serialize_envelope).transpose()?,
                source.map(|_| 1_i32),
                source.map(|source| source.principal().bare_jid().to_string()),
                source.map(|source| source.principal().auth_context_id().as_uuid().to_string()),
                source.map(|source| source.principal().auth_context_version().get().to_string()),
                source.map(|source| source.principal().auth_epoch().get().to_string()),
                source.and_then(|source| source
                    .sm_occurrence()
                    .map(|(id, _)| id.to_storage().to_string())),
                source.and_then(|source| source
                    .sm_occurrence()
                    .map(|(_, ordinal)| ordinal.to_storage().to_string())),
            ],
        )
        .await
        .map_err(discard_database_error)?;
    Ok(inserted == 1)
}

async fn assert_source_principal_if_recorded(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    message_key: MessageKey,
    source: &CanonicalSourceBinding,
) -> Result<(), IngressSubstrateError> {
    let pg = format!(
        "SELECT source_binding_version::int, source_bare_jid, source_auth_context_id::text, source_auth_context_version::text, source_auth_epoch::text FROM {} WHERE message_key = ?::uuid FOR UPDATE",
        prepared_ingress_relation(prepared, PreparedIngressRelation::Messages)
    );
    let sql = if tx.driver() == DatabaseDriver::Postgres {
        pg.as_str()
    } else {
        "SELECT source_binding_version, source_bare_jid, source_auth_context_id, source_auth_context_version, source_auth_epoch FROM ingress_messages WHERE message_key = ?"
    };
    let mut rows = tx
        .query(sql, crate::db_params![message_key.to_storage().to_string()])
        .await
        .map_err(discard_database_error)?;
    let Some(row) = rows.next().await.map_err(discard_database_error)? else {
        return Err(IngressSubstrateError::RelayedAliasUnproven);
    };
    let version: Option<i32> = row.get(0).map_err(discard_database_error)?;
    if version.is_none() {
        // Legacy rows remain untouched and cannot gain source authority by replay.
        return Ok(());
    }
    let stored_bare: String = row.get(1).map_err(discard_database_error)?;
    let stored_context: String = row.get(2).map_err(discard_database_error)?;
    let stored_version: String = row.get(3).map_err(discard_database_error)?;
    let stored_epoch: String = row.get(4).map_err(discard_database_error)?;
    let principal = source.principal();
    if version != Some(1)
        || stored_bare != principal.bare_jid().to_string()
        || stored_context != principal.auth_context_id().as_uuid().to_string()
        || stored_version != principal.auth_context_version().get().to_string()
        || stored_epoch != principal.auth_epoch().get().to_string()
    {
        return Err(IngressSubstrateError::SourceBindingConflict);
    }
    Ok(())
}

/// Resolve and atomically persist one sender/target/origin-id alias.
///
/// An existing alias is read with its message digest in one joined locked
/// query.  On a concurrent first-insert loss, the unreferenced candidate is
/// removed before repeating that same query, so neither outcome observes an
/// impossible alias/digest split.
pub async fn resolve_and_record_alias(
    tx: &mut Transaction<'_>,
    sender: &BareJid,
    target: &NormalizedTarget,
    origin_id: &OriginId,
    digest: &SemanticDigest,
    mint: impl FnOnce() -> MessageKey,
) -> Result<AliasResolution, IngressSubstrateError> {
    resolve_alias_in_prepared_transaction(tx, None, sender, target, origin_id, digest, None, mint)
        .await
}

// Dark API for the staged source-binding contract; production activation is
// deliberately deferred until caller and role contracts are complete.
#[allow(dead_code)]
pub(crate) async fn resolve_and_record_alias_with_source(
    tx: &mut Transaction<'_>,
    sender: &BareJid,
    target: &NormalizedTarget,
    origin_id: &OriginId,
    digest: &SemanticDigest,
    source: &CanonicalSourceBinding,
    mint: impl FnOnce() -> MessageKey,
) -> Result<AliasResolution, IngressSubstrateError> {
    resolve_alias_in_prepared_transaction(
        tx,
        None,
        sender,
        target,
        origin_id,
        digest,
        Some(source),
        mint,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn resolve_alias_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    sender: &BareJid,
    target: &NormalizedTarget,
    origin_id: &OriginId,
    digest: &SemanticDigest,
    source: Option<&CanonicalSourceBinding>,
    mint: impl FnOnce() -> MessageKey,
) -> Result<AliasResolution, IngressSubstrateError> {
    if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
        return resolve_alias_via_definer_api(
            tx, prepared, sender, target, origin_id, digest, source, mint,
        )
        .await;
    }

    acquire_supported_epoch_in_prepared_transaction(tx, prepared).await?;
    let alias_key = AliasStorageKey::new(sender, target, origin_id);
    if let Some(stored) = locked_alias(tx, prepared, &alias_key).await? {
        if let Some(source) = source {
            assert_source_principal_if_recorded(tx, prepared, stored.key, source).await?;
        }
        return Ok(resolve_alias(true, digest, Some(&stored), mint));
    }

    let candidate = resolve_alias(true, digest, None, mint);
    let candidate_key = match candidate {
        AliasResolution::Aliased(waddle_xmpp::ingress::AliasOutcome::Inserted(key)) => key,
        _ => return Err(IngressSubstrateError::AliasMissingAfterConflict),
    };
    if !insert_message(tx, prepared, candidate_key, digest, None, source).await? {
        return Err(IngressSubstrateError::MessageContentConflict);
    }
    let qualified;
    let postgres = if prepared.is_some() {
        qualified = format!(
            "INSERT INTO {} (alias_key_hash, sender_bare_jid, target_kind, target_jid, origin_id, message_key) VALUES (?, ?, ?, ?, ?, ?::uuid) ON CONFLICT (alias_key_hash) DO NOTHING",
            prepared_ingress_relation(prepared, PreparedIngressRelation::Aliases)
        );
        qualified.as_str()
    } else {
        INSERT_ALIAS_POSTGRES
    };
    let inserted = tx
        .execute(
            if tx.driver() == DatabaseDriver::Postgres {
                postgres
            } else {
                INSERT_ALIAS_SQLITE
            },
            crate::db_params![
                alias_key.hash.to_vec(),
                alias_key.sender.to_string(),
                alias_key.target.kind(),
                alias_key.target.jid(),
                alias_key.origin_id.as_str(),
                candidate_key.to_storage().to_string(),
            ],
        )
        .await
        .map_err(discard_database_error)?;
    if inserted == 1 {
        return Ok(candidate);
    }

    let qualified;
    let postgres = if prepared.is_some() {
        qualified = format!(
            "DELETE FROM {} WHERE message_key = ?::uuid",
            prepared_ingress_relation(prepared, PreparedIngressRelation::Messages)
        );
        qualified.as_str()
    } else {
        DELETE_CANDIDATE_POSTGRES
    };
    tx.execute(
        if tx.driver() == DatabaseDriver::Postgres {
            postgres
        } else {
            DELETE_CANDIDATE_SQLITE
        },
        crate::db_params![candidate_key.to_storage().to_string()],
    )
    .await
    .map_err(discard_database_error)?;

    let stored = locked_alias(tx, prepared, &alias_key)
        .await?
        .ok_or(IngressSubstrateError::AliasMissingAfterConflict)?;
    if let Some(source) = source {
        assert_source_principal_if_recorded(tx, prepared, stored.key, source).await?;
    }
    Ok(resolve_alias(true, digest, Some(&stored), || candidate_key))
}

/// Resolve an origin alias through the dark fixed SECURITY DEFINER API.
///
/// The first call is a read-only miss probe so the application-owned key mint
/// remains lazy. A second call with that key performs the insert and rechecks
/// the alias under the function's canonical parent lock, all inside the same
/// caller-owned transaction.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn resolve_alias_via_definer_api(
    tx: &mut Transaction<'_>,
    prepared: &PreparedAttestedReceiptWriter,
    sender: &BareJid,
    target: &NormalizedTarget,
    origin_id: &OriginId,
    digest: &SemanticDigest,
    source: Option<&CanonicalSourceBinding>,
    mint: impl FnOnce() -> MessageKey,
) -> Result<AliasResolution, IngressSubstrateError> {
    if tx.driver() != DatabaseDriver::Postgres {
        return Err(IngressSubstrateError::SchemaIdentityMismatch);
    }

    let probe = call_definer_alias_api(
        tx, prepared, sender, target, origin_id, digest, source, None,
    )
    .await?;
    let result = match probe {
        DefinerAliasResult::Missing => {
            call_definer_alias_api(
                tx,
                prepared,
                sender,
                target,
                origin_id,
                digest,
                source,
                Some(mint()),
            )
            .await?
        }
        result => result,
    };

    match result {
        DefinerAliasResult::Inserted(key) => Ok(AliasResolution::Aliased(
            waddle_xmpp::ingress::AliasOutcome::Inserted(key),
        )),
        DefinerAliasResult::Existing(stored) => {
            Ok(resolve_alias(true, digest, Some(&stored), MessageKey::new))
        }
        DefinerAliasResult::Missing => Err(IngressSubstrateError::AliasMissingAfterConflict),
    }
}

enum DefinerAliasResult {
    Missing,
    Inserted(MessageKey),
    Existing(StoredAlias),
}

#[allow(clippy::too_many_arguments)]
async fn call_definer_alias_api(
    tx: &mut Transaction<'_>,
    prepared: &PreparedAttestedReceiptWriter,
    sender: &BareJid,
    target: &NormalizedTarget,
    origin_id: &OriginId,
    digest: &SemanticDigest,
    source: Option<&CanonicalSourceBinding>,
    candidate: Option<MessageKey>,
) -> Result<DefinerAliasResult, IngressSubstrateError> {
    let function = format!(
        "{}.waddle_ingress_resolve_origin",
        quote_postgres_identifier(prepared.schema())
    );
    let sql = format!(
        "SELECT canonical_message_key::text, outcome, stored_digest_version, stored_digest \
         FROM {function}(?::uuid, ?::integer, ?::bytea, ?::bytea, ?::text, ?::integer, \
         ?::text, ?::text, ?::text, ?::uuid, ?::numeric, ?::numeric, ?::uuid, ?::numeric)"
    );
    let alias = AliasStorageKey::new(sender, target, origin_id);
    let target = &alias.target;
    let principal = source.map(|source| source.principal());
    let sm_occurrence = source.and_then(CanonicalSourceBinding::sm_occurrence);
    let (digest_version, digest_bytes) = digest.to_storage();
    let mut rows = tx
        .query(
            &sql,
            crate::db_params![
                candidate.map(|key| key.to_storage().to_string()),
                i32::from(digest_version),
                digest_bytes.to_vec(),
                alias.hash.to_vec(),
                sender.to_string(),
                target.kind(),
                target.jid().to_owned(),
                origin_id.as_str().to_owned(),
                principal.map(|principal| principal.bare_jid().to_string()),
                principal.map(|principal| principal.auth_context_id().as_uuid().to_string()),
                principal.map(|principal| principal.auth_context_version().get().to_string()),
                principal.map(|principal| principal.auth_epoch().get().to_string()),
                sm_occurrence.map(|(id, _)| id.to_storage().to_string()),
                sm_occurrence.map(|(_, ordinal)| ordinal.to_storage().to_string()),
            ],
        )
        .await
        .map_err(discard_database_error)?;
    let row = rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::AliasMissingAfterConflict)?;
    let key_text: Option<String> = row.get(0).map_err(discard_database_error)?;
    let outcome: String = row.get(1).map_err(discard_database_error)?;
    let stored_version: Option<i32> = row.get(2).map_err(discard_database_error)?;
    let stored_bytes: Option<Vec<u8>> = row.get(3).map_err(discard_database_error)?;
    if rows.next().await.map_err(discard_database_error)?.is_some() {
        return Err(IngressSubstrateError::InvalidDefinerApiResult);
    }
    let key = key_text
        .map(|key| {
            key.parse::<Uuid>()
                .map(MessageKey::from_storage)
                .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey)
        })
        .transpose()?;

    match outcome.as_str() {
        "missing"
            if key.is_none()
                && stored_version.is_none()
                && stored_bytes.is_none()
                && candidate.is_none() =>
        {
            Ok(DefinerAliasResult::Missing)
        }
        "inserted" => {
            let key = key.ok_or(IngressSubstrateError::InvalidStoredMessageKey)?;
            if Some(key) != candidate {
                return Err(IngressSubstrateError::InvalidDefinerApiResult);
            }
            if stored_version != Some(i32::from(digest_version))
                || stored_bytes.as_deref() != Some(digest_bytes.as_slice())
            {
                return Err(IngressSubstrateError::InvalidDefinerApiResult);
            }
            Ok(DefinerAliasResult::Inserted(key))
        }
        "existing" | "digest_conflict" => {
            let key = key.ok_or(IngressSubstrateError::InvalidStoredMessageKey)?;
            let version = stored_version
                .and_then(|value| u8::try_from(value).ok())
                .ok_or(IngressSubstrateError::InvalidStoredDigest)?;
            let bytes: [u8; 32] = stored_bytes
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or(IngressSubstrateError::InvalidStoredDigest)?;
            let stored = SemanticDigest::from_storage(version, bytes)
                .map_err(|_| IngressSubstrateError::InvalidStoredDigest)?;
            if (outcome == "existing") != (stored == *digest) {
                return Err(IngressSubstrateError::InvalidStoredDigest);
            }
            Ok(DefinerAliasResult::Existing(StoredAlias {
                key,
                digest: stored,
            }))
        }
        "message_key_conflict"
            if key.is_none() && stored_version.is_none() && stored_bytes.is_none() =>
        {
            Err(IngressSubstrateError::MessageContentConflict)
        }
        "alias_identity_conflict" if key.is_some() => {
            Err(IngressSubstrateError::AliasIdentityConflict)
        }
        "source_binding_conflict" if key.is_some() => {
            Err(IngressSubstrateError::SourceBindingConflict)
        }
        "principal_assertion_failed"
            if key.is_none() && stored_version.is_none() && stored_bytes.is_none() =>
        {
            Err(IngressSubstrateError::PrincipalAssertionFailed)
        }
        _ => Err(IngressSubstrateError::InvalidDefinerApiResult),
    }
}

/// Prove an existing relayed origin against its exact canonical parent.
///
/// The caller has already acquired the epoch and room-claim fences. PostgreSQL
/// retains row locks on the supplied parent, then the alias, until transaction
/// end. NOWAIT on the alias avoids waiting in a reverse lock order. SQLite uses
/// the caller's BEGIN IMMEDIATE writer serialization. Neither path discovers or
/// creates another parent.
pub(crate) async fn assert_relayed_alias_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    message_key: MessageKey,
    sender: &BareJid,
    target: &NormalizedTarget,
    origin_id: &OriginId,
    digest: &SemanticDigest,
) -> Result<(), IngressSubstrateError> {
    if !lock_message_for_child_in_prepared_transaction(tx, prepared, message_key).await? {
        return Err(IngressSubstrateError::RelayedAliasUnproven);
    }

    let alias_key = AliasStorageKey::new(sender, target, origin_id);
    let postgres = format!(
        "SELECT m.message_key::text, m.digest_version, m.digest, a.sender_bare_jid, a.target_kind, a.target_jid, a.origin_id FROM {} a JOIN {} m USING (message_key) WHERE a.alias_key_hash = ? AND a.message_key = ?::uuid FOR UPDATE OF a NOWAIT",
        prepared_ingress_relation(prepared, PreparedIngressRelation::Aliases),
        prepared_ingress_relation(prepared, PreparedIngressRelation::Messages),
    );
    const SQLITE: &str = "SELECT m.message_key, m.digest_version, m.digest, a.sender_bare_jid, a.target_kind, a.target_jid, a.origin_id FROM ingress_origin_aliases a JOIN ingress_messages m USING (message_key) WHERE a.alias_key_hash = ? AND a.message_key = ?";
    let mut rows = tx
        .query(
            if tx.driver() == DatabaseDriver::Postgres {
                &postgres
            } else {
                SQLITE
            },
            crate::db_params![
                alias_key.hash.to_vec(),
                message_key.to_storage().to_string()
            ],
        )
        .await
        .map_err(relayed_alias_lock_error)?;
    let row = rows
        .next()
        .await
        .map_err(relayed_alias_lock_error)?
        .ok_or(IngressSubstrateError::RelayedAliasUnproven)?;
    validate_stored_alias_identity(&row, &alias_key)?;
    let stored = decode_stored_alias(&row)?;
    if stored.key != message_key || &stored.digest != digest {
        return Err(IngressSubstrateError::RelayedAliasUnproven);
    }
    Ok(())
}

fn relayed_alias_lock_error(error: DatabaseError) -> IngressSubstrateError {
    if let DatabaseError::Internal(sqlx::Error::Database(database)) = &error {
        if database.code().as_deref() == Some("55P03") {
            return IngressSubstrateError::Database {
                retry_class: DbRetryClass::CanonicalLockContention,
            };
        }
    }
    discard_database_error(error)
}

/// Attach a stream-management ordinal to a live message.
///
/// Idempotent for ambiguous-commit retries: replaying the identical
/// `(SmIngressId, ordinal) -> MessageKey` reference reports
/// [`MessageWriteOutcome::AlreadyRecorded`], while the same ordinal bound
/// to a DIFFERENT message is the typed [`IngressSubstrateError::SmOrdinalConflict`].
pub async fn insert_sm_ref(
    tx: &mut Transaction<'_>,
    sm_ingress_id: SmIngressId,
    ordinal: IngressOrdinal,
    wire_h: WireHandledCount,
    message_key: MessageKey,
) -> Result<MessageWriteOutcome, IngressSubstrateError> {
    insert_sm_ref_in_prepared_transaction(tx, None, sm_ingress_id, ordinal, wire_h, message_key)
        .await
}

pub(crate) async fn insert_sm_ref_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    sm_ingress_id: SmIngressId,
    ordinal: IngressOrdinal,
    wire_h: WireHandledCount,
    message_key: MessageKey,
) -> Result<MessageWriteOutcome, IngressSubstrateError> {
    acquire_supported_epoch_in_prepared_transaction(tx, prepared).await?;
    if !lock_message_for_child_in_prepared_transaction(tx, prepared, message_key).await? {
        return Ok(MessageWriteOutcome::MessageVanished);
    }
    let generation = authority::wire_generation(tx, prepared, sm_ingress_id, wire_h).await?;
    if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
        let function = format!(
            "{}.waddle_ingress_insert_sm_ref",
            quote_postgres_identifier(prepared.schema())
        );
        let mut rows = tx
            .query(
                &format!("SELECT {function}(?::uuid, ?::numeric, ?::bigint, ?::bigint, ?::uuid)"),
                crate::db_params![
                    sm_ingress_id.to_storage().to_string(),
                    ordinal.to_storage().to_string(),
                    i64::from(wire_h.to_storage()),
                    generation,
                    message_key.to_storage().to_string(),
                ],
            )
            .await
            .map_err(discard_database_error)?;
        let row = rows
            .next()
            .await
            .map_err(discard_database_error)?
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        let inserted: bool = row.get(0).map_err(discard_database_error)?;
        if rows.next().await.map_err(discard_database_error)?.is_some() {
            return Err(IngressSubstrateError::InvalidDefinerApiResult);
        }
        if inserted {
            super::retention::invalidate_retention_in_prepared_transaction(
                tx,
                Some(prepared),
                message_key,
            )
            .await?;
            return Ok(MessageWriteOutcome::Recorded);
        }
        let read = prepared_ingress_sql(
            tx.driver(),
            Some(prepared),
            PreparedIngressRelation::SmRefs,
            READ_SM_REF_POSTGRES,
            READ_SM_REF_SQLITE,
        );
        let existing = stored_child_message_key(
            tx,
            &read,
            crate::db_params![
                sm_ingress_id.to_storage().to_string(),
                ordinal.to_storage().to_string(),
            ],
        )
        .await?;
        if existing == Some(message_key)
            && authority::lookup_wire_binding_in_prepared_transaction(
                tx,
                Some(prepared),
                sm_ingress_id,
                wire_h,
            )
            .await?
                == Some((message_key, ordinal))
        {
            return Ok(MessageWriteOutcome::AlreadyRecorded);
        }
        return Err(IngressSubstrateError::SmOrdinalConflict);
    }

    let insert = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::SmRefs,
        INSERT_SM_REF_POSTGRES,
        INSERT_SM_REF_SQLITE,
    );
    let inserted = tx
        .execute(
            &insert,
            crate::db_params![
                sm_ingress_id.to_storage().to_string(),
                ordinal.to_storage().to_string(),
                i64::from(wire_h.to_storage()),
                generation,
                message_key.to_storage().to_string(),
            ],
        )
        .await
        .map_err(discard_database_error)?;
    if inserted == 1 {
        super::retention::invalidate_retention_in_prepared_transaction(tx, prepared, message_key)
            .await?;
        return Ok(MessageWriteOutcome::Recorded);
    }
    let read = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::SmRefs,
        READ_SM_REF_POSTGRES,
        READ_SM_REF_SQLITE,
    );
    let existing = stored_child_message_key(
        tx,
        &read,
        crate::db_params![
            sm_ingress_id.to_storage().to_string(),
            ordinal.to_storage().to_string(),
        ],
    )
    .await?;
    if existing == Some(message_key)
        && authority::lookup_wire_binding_in_prepared_transaction(
            tx,
            prepared,
            sm_ingress_id,
            wire_h,
        )
        .await?
            == Some((message_key, ordinal))
    {
        Ok(MessageWriteOutcome::AlreadyRecorded)
    } else {
        Err(IngressSubstrateError::SmOrdinalConflict)
    }
}

/// Record a delivery/effect identity for a live message.
///
/// Idempotent for ambiguous-commit retries, mirroring [`insert_sm_ref`]:
/// the identical `DeliveryKey -> MessageKey` pair reports
/// [`MessageWriteOutcome::AlreadyRecorded`]; the same key bound to a
/// different message is [`IngressSubstrateError::DeliveryKeyConflict`].
pub async fn record_delivery(
    tx: &mut Transaction<'_>,
    delivery_key: DeliveryKey,
    message_key: MessageKey,
) -> Result<MessageWriteOutcome, IngressSubstrateError> {
    record_delivery_in_prepared_transaction(tx, None, delivery_key, message_key).await
}

pub(crate) async fn record_delivery_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    delivery_key: DeliveryKey,
    message_key: MessageKey,
) -> Result<MessageWriteOutcome, IngressSubstrateError> {
    acquire_supported_epoch_in_prepared_transaction(tx, prepared).await?;
    record_delivery_after_epoch_lock(tx, prepared, delivery_key, message_key).await
}

async fn record_delivery_after_epoch_lock(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    delivery_key: DeliveryKey,
    message_key: MessageKey,
) -> Result<MessageWriteOutcome, IngressSubstrateError> {
    if !lock_message_for_child_in_prepared_transaction(tx, prepared, message_key).await? {
        return Ok(MessageWriteOutcome::MessageVanished);
    }
    if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
        let function = format!(
            "{}.waddle_ingress_record_delivery",
            quote_postgres_identifier(prepared.schema())
        );
        let mut rows = tx
            .query(
                &format!("SELECT {function}(?::uuid, ?::uuid)"),
                crate::db_params![
                    delivery_key.to_storage().to_string(),
                    message_key.to_storage().to_string(),
                ],
            )
            .await
            .map_err(discard_database_error)?;
        let row = rows
            .next()
            .await
            .map_err(discard_database_error)?
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        let inserted: bool = row.get(0).map_err(discard_database_error)?;
        if rows.next().await.map_err(discard_database_error)?.is_some() {
            return Err(IngressSubstrateError::InvalidDefinerApiResult);
        }
        if inserted {
            return Ok(MessageWriteOutcome::Recorded);
        }
        let function = format!(
            "{}.waddle_ingress_lookup_delivery",
            quote_postgres_identifier(prepared.schema())
        );
        let mut rows = tx
            .query(
                &format!("SELECT {function}(?::uuid)::text"),
                crate::db_params![delivery_key.to_storage().to_string()],
            )
            .await
            .map_err(discard_database_error)?;
        let row = rows
            .next()
            .await
            .map_err(discard_database_error)?
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        let existing_text: Option<String> = row.get(0).map_err(discard_database_error)?;
        if rows.next().await.map_err(discard_database_error)?.is_some() {
            return Err(IngressSubstrateError::InvalidDefinerApiResult);
        }
        let existing = existing_text
            .map(|key| {
                key.parse::<Uuid>()
                    .map(MessageKey::from_storage)
                    .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey)
            })
            .transpose()?;
        return if existing == Some(message_key) {
            Ok(MessageWriteOutcome::AlreadyRecorded)
        } else {
            Err(IngressSubstrateError::DeliveryKeyConflict)
        };
    }

    let insert = prepared_ingress_sql(
        tx.driver(), prepared, PreparedIngressRelation::Deliveries,
        ("INSERT INTO ", " (delivery_key, message_key) VALUES (?::uuid, ?::uuid) ON CONFLICT (delivery_key) DO NOTHING"),
        INSERT_DELIVERY_SQLITE,
    );
    let inserted = tx
        .execute(
            &insert,
            crate::db_params![
                delivery_key.to_storage().to_string(),
                message_key.to_storage().to_string(),
            ],
        )
        .await
        .map_err(discard_database_error)?;
    if inserted == 1 {
        return Ok(MessageWriteOutcome::Recorded);
    }
    let read = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::Deliveries,
        (
            "SELECT message_key::text FROM ",
            " WHERE delivery_key = ?::uuid",
        ),
        READ_DELIVERY_SQLITE,
    );
    let existing = stored_child_message_key(
        tx,
        &read,
        crate::db_params![delivery_key.to_storage().to_string()],
    )
    .await?;
    if existing == Some(message_key) {
        Ok(MessageWriteOutcome::AlreadyRecorded)
    } else {
        Err(IngressSubstrateError::DeliveryKeyConflict)
    }
}

/// Record a terminal proof once; later calls preserve the first proof time.
pub async fn terminalize_message(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
    proven_terminal_at: DateTime<Utc>,
) -> Result<TerminalizeOutcome, IngressSubstrateError> {
    terminalize_message_in_prepared_transaction(tx, None, message_key, proven_terminal_at).await
}

pub(crate) async fn terminalize_message_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    message_key: MessageKey,
    proven_terminal_at: DateTime<Utc>,
) -> Result<TerminalizeOutcome, IngressSubstrateError> {
    acquire_supported_epoch_in_prepared_transaction(tx, prepared).await?;
    if !lock_message_for_child_in_prepared_transaction(tx, prepared, message_key).await? {
        return Ok(TerminalizeOutcome::MessageVanished);
    }
    let sql = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::Messages,
        (
            "UPDATE ",
            " SET terminal_at = ?::timestamptz WHERE message_key = ?::uuid AND terminal_at IS NULL",
        ),
        TERMINALIZE_MESSAGE_SQLITE,
    );
    let changed = tx
        .execute(
            &sql,
            crate::db_params![
                proven_terminal_at.to_rfc3339(),
                message_key.to_storage().to_string(),
            ],
        )
        .await
        .map_err(discard_database_error)?;
    if changed == 1 {
        super::retention::refresh_retention_in_prepared_transaction(
            tx,
            prepared,
            message_key,
            proven_terminal_at,
        )
        .await?;
        return Ok(TerminalizeOutcome::Terminalized);
    }
    if message_exists_in_prepared_transaction(tx, prepared, message_key).await? {
        Ok(TerminalizeOutcome::AlreadyTerminal)
    } else {
        Ok(TerminalizeOutcome::MessageVanished)
    }
}

/// Garbage collect terminal messages whose alias retention has elapsed.
///
/// Each candidate is locked before checking children.  That lock interlocks
/// with child writes and alias resolution, whose first statement on a live
/// message is `FOR UPDATE`.
pub async fn gc_expired_aliases(
    db: &Database,
    now: DateTime<Utc>,
    budget: AliasGcBudget,
) -> Result<AliasGcOutcome, AliasGcFailure> {
    // Committed deletions arm the sweep inside `AliasGcProgress::record`, at the
    // commit itself, so `?` here cannot discard the trigger even though the
    // failure carries committed progress.
    let mut outcome = gc_canonical_candidates(db, now, &budget).await?;
    if !outcome.completed {
        return Ok(outcome);
    }
    // A cross-node ledger writer can insert after its canonical parent was
    // reclaimed: authorization and append are separate transactions. Sweep
    // independently to reclaim those late proofs. Cursor-page the examined
    // rows: a filter-limited scan of healthy proofs can repeatedly hit the scan
    // timeout without ever advancing to an orphan later in retained history.
    //
    // Only sweep when it can plausibly find something. An orphan appears when a
    // canonical row is reclaimed while a cross-node append for it is still in
    // flight, so a pass that reclaimed nothing has created no new ones; the
    // interval still catches those in-flight races and anything predating this
    // code. Sweeping on every pass would add a transaction to every maintenance
    // cycle for work that is almost always absent, and the ledger is not
    // load-bearing -- nothing waits on an orphan being collected.
    if !should_sweep_orphans(&budget).await {
        return Ok(outcome);
    }
    //
    // Drain as many windows as the cooperative budget allows rather than one per
    // pass. A mid-traversal pass reports `Partial`, and the coordinator answers
    // that with its doubling failure backoff (to 30s) while consuming coalesced
    // triggers, so one window per pass would traverse at ~256 rows / 30s. Message
    // keys are time-ordered UUIDv7s, so a sustained append rate above that would
    // extend the ordered tail faster than the cursor advances: the sweep would
    // never wrap, and a late proof for an older key would stay orphaned
    // indefinitely.
    loop {
        if Instant::now() >= budget.deadline {
            outcome.completed = false;
            return Ok(outcome);
        }
        // Never fail the pass from here. Canonical collection has already
        // succeeded, and the maintenance pass that wraps this one maps a failed
        // GC phase to `MaintenanceOutcome::Failed`, which also governs the
        // recovery phase's accounting and backoff. This sweep is opportunistic
        // cleanup of rows nothing is waiting on, so a scan timeout or a
        // transient database error reports the pass incomplete -- it retries --
        // rather than dragging unrelated recovery work down with it.
        let step = match gc_orphan_proof_batch(db, &budget).await {
            Ok(step) => step,
            Err(error) => {
                tracing::warn!(%error, "ingress orphan proof sweep failed; pass stays incomplete");
                waddle_xmpp::counter_add!(
                    "waddle.ingress.gc.orphan_sweep_failed",
                    "{sweep}",
                    "Orphan append-proof sweeps that failed without failing their \
                     maintenance pass.",
                    1,
                );
                outcome.completed = false;
                return Ok(outcome);
            }
        };
        let deleted = match step {
            OrphanSweepStep::Advanced { deleted }
            | OrphanSweepStep::Finished { deleted }
            | OrphanSweepStep::BudgetExpired { deleted } => deleted,
        };
        budget
            .progress
            .orphan_proofs
            .fetch_add(deleted, Ordering::AcqRel);
        outcome.deleted_orphan_proofs += deleted;
        match step {
            OrphanSweepStep::Advanced { .. } => {}
            OrphanSweepStep::Finished { .. } => {
                budget
                    .progress
                    .orphan_sweep_pending
                    .store(false, Ordering::Release);
                *budget.progress.orphan_swept_at.lock().await = Some(Instant::now());
                return Ok(outcome);
            }
            OrphanSweepStep::BudgetExpired { .. } => {
                outcome.completed = false;
                return Ok(outcome);
            }
        }
    }
}

/// Outcome of examining one bounded orphan-sweep window.
///
/// The cursor alone cannot express this: it is `None` both when a short window
/// ended the traversal and when the budget expired before the first window
/// advanced it, and treating the second as completion would report a pass that
/// examined nothing as complete.
#[derive(Debug)]
pub(in crate::ingress_substrate) enum OrphanSweepStep {
    /// A full window was examined; the cursor advanced past it.
    Advanced { deleted: usize },
    /// A short window ended the traversal; the cursor reset to the start.
    Finished { deleted: usize },
    /// The cooperative budget expired with the traversal unfinished.
    BudgetExpired { deleted: usize },
}

/// How long the sweep may go unrun when canonical collection is reclaiming
/// nothing. Orphans are rare and nothing waits on them, so this trades
/// reclamation latency for keeping ordinary maintenance passes free of an extra
/// transaction.
pub(in crate::ingress_substrate) const ORPHAN_SWEEP_INTERVAL: StdDuration =
    StdDuration::from_secs(300);

/// Whether this pass should examine the append ledger at all.
///
/// The answer is deliberately sticky. A pass that is admitted but ends before
/// the traversal finishes leaves the sweep owed, and the retry cannot re-derive
/// that from its own fresh counters, so the flag -- not this pass's state --
/// decides.
pub(in crate::ingress_substrate) async fn should_sweep_orphans(budget: &AliasGcBudget) -> bool {
    if budget.progress.orphan_sweep_pending.load(Ordering::Acquire) {
        return true;
    }
    // Mid-traversal always continues: leaving a cursor parked would defeat the
    // paging that keeps each window bounded.
    if budget.progress.orphan_cursor.lock().await.is_some() {
        budget.progress.mark_orphan_sweep_pending();
        return true;
    }
    let swept_at = budget.progress.orphan_swept_at.lock().await;
    let due = swept_at.is_none_or(|at| at.elapsed() >= ORPHAN_SWEEP_INTERVAL);
    if due {
        // Arm, do not consume: the interval restarts when a traversal FINISHES,
        // so a pass that dies early does not silently buy another five minutes.
        budget.progress.mark_orphan_sweep_pending();
    }
    due
}

pub(in crate::ingress_substrate) async fn gc_orphan_proof_batch(
    db: &Database,
    budget: &AliasGcBudget,
) -> Result<OrphanSweepStep, AliasGcError> {
    #[cfg(test)]
    if FAIL_ORPHAN_SWEEP.try_with(|fail| *fail).unwrap_or(false) {
        return Err(AliasGcError::Substrate(IngressSubstrateError::Database {
            retry_class: DbRetryClass::SerializationFailure,
        }));
    }
    // Serialize windows sharing a traversal so a slower pass cannot rewind it.
    let mut cursor = budget.progress.orphan_cursor.lock().await;
    let mut tx = db.begin_immediate().await.map_err(gc_error_from_database)?;
    // READ COMMITTED is required so the orphan scan starts from a snapshot
    // taken after the epoch lock below. This transaction only deletes
    // `sm_ingress_appends` (which is not in V1009's guarded relation set),
    // but it reads guarded canonical state and must not race activation.
    if tx.driver() == DatabaseDriver::Postgres {
        tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
            .await
            .map_err(gc_error_from_database)?;
    }
    install_gc_timeouts(&mut tx, budget, budget.statement_timeout).await?;
    if Instant::now() >= budget.deadline {
        return Ok(OrphanSweepStep::BudgetExpired { deleted: 0 });
    }
    // The share lock and transaction-local proof are held through commit.
    // Activation therefore cannot change the live epoch between the
    // canonical-parent scan and deletion of its orphan proof.
    let live_epoch = acquire_gc_epoch_lock_first(&mut tx, 0)
        .await
        .map_err(|failure| failure.error)?;
    if live_epoch > supported_protocol_epoch() {
        return Err(AliasGcError::Substrate(
            IngressSubstrateError::UnsupportedLiveEpoch,
        ));
    }
    if tx.driver() == DatabaseDriver::Postgres {
        let mut proof = tx
            .query(
                r#"
                SELECT
                    set_config('waddle.protocol_epoch', ?, true),
                    set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)
                "#,
                crate::db_params![live_epoch.to_storage().to_string()],
            )
            .await
            .map_err(gc_error_from_database)?;
        proof
            .next()
            .await
            .map_err(gc_error_from_database)?
            .ok_or(AliasGcError::Substrate(IngressSubstrateError::Database {
                retry_class: DbRetryClass::NotRetryable,
            }))?;
        drop(proof);
    }
    if Instant::now() >= budget.deadline {
        return Ok(OrphanSweepStep::BudgetExpired { deleted: 0 });
    }
    install_gc_timeouts(&mut tx, budget, budget.scan_timeout).await?;
    let mut rows = if let Some(after) = cursor.as_ref() {
        tx.query(
            dialect_sql(
                tx.driver(),
                GC_ORPHAN_WINDOW_AFTER_POSTGRES,
                GC_ORPHAN_WINDOW_AFTER_SQLITE,
            ),
            crate::db_params![
                after.message_key.to_storage().to_string(),
                i64::from(after.kind.to_storage()),
                after.semantic_identity_hash.to_vec(),
                after.resource.to_string(),
                GC_BATCH_LIMIT as i64,
            ],
        )
        .await
    } else {
        tx.query(
            dialect_sql(
                tx.driver(),
                GC_ORPHAN_WINDOW_POSTGRES,
                GC_ORPHAN_WINDOW_SQLITE,
            ),
            crate::db_params![GC_BATCH_LIMIT as i64],
        )
        .await
    }
    .map_err(gc_error_from_database)?;
    let mut window = Vec::new();
    while let Some(row) = rows.next().await.map_err(gc_error_from_database)? {
        let message_key: String = row.get(0).map_err(gc_error_from_database)?;
        let kind: i64 = row.get(1).map_err(gc_error_from_database)?;
        let hash: Vec<u8> = row.get(2).map_err(gc_error_from_database)?;
        let resource: String = row.get(3).map_err(gc_error_from_database)?;
        let key = SmIngressAppendKey {
            message_key: message_key
                .parse::<Uuid>()
                .map(MessageKey::from_storage)
                .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey)?,
            kind: SmIngressReceiptKind::from_storage(
                i32::try_from(kind).map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)?,
            ),
            semantic_identity_hash: hash
                .try_into()
                .map_err(|_| IngressSubstrateError::InvalidStoredDigest)?,
            resource: resource
                .parse()
                .map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)?,
        };
        let orphan: bool = row.get(4).map_err(gc_error_from_database)?;
        window.push((key, orphan));
    }
    drop(rows);
    install_gc_timeouts(&mut tx, budget, budget.statement_timeout).await?;
    let mut deleted = 0;
    let mut finished_window = true;
    for (key, orphan) in &window {
        if Instant::now() >= budget.deadline {
            // Keep committed deletions useful even when the cooperative budget
            // cannot drain a whole window. Retry the remaining window next pass.
            finished_window = false;
            break;
        }
        if !orphan {
            continue;
        }
        let affected = tx
            .execute(
                dialect_sql(
                    tx.driver(),
                    GC_DELETE_ORPHAN_PROOFS_POSTGRES,
                    GC_DELETE_ORPHAN_PROOFS_SQLITE,
                ),
                crate::db_params![
                    key.message_key.to_storage().to_string(),
                    i64::from(key.kind.to_storage()),
                    key.semantic_identity_hash.to_vec(),
                    key.resource.to_string(),
                ],
            )
            .await
            .map_err(gc_error_from_database)?;
        deleted += usize::try_from(affected).map_err(|_| {
            AliasGcError::Substrate(IngressSubstrateError::Database {
                retry_class: DbRetryClass::NotRetryable,
            })
        })?;
    }
    tx.commit().await.map_err(gc_error_from_database)?;
    // Cursor advancement and the caller's accounting immediately follow commit
    // without an intervening suspension point.
    if !finished_window {
        return Ok(OrphanSweepStep::BudgetExpired { deleted });
    }
    if window.len() == GC_BATCH_LIMIT {
        *cursor = window.last().map(|(key, _)| key.clone());
        return Ok(OrphanSweepStep::Advanced { deleted });
    }
    *cursor = None;
    Ok(OrphanSweepStep::Finished { deleted })
}

pub(in crate::ingress_substrate) async fn gc_canonical_candidates(
    db: &Database,
    now: DateTime<Utc>,
    budget: &AliasGcBudget,
) -> Result<AliasGcOutcome, AliasGcFailure> {
    let cutoff = (now - ALIAS_RETENTION).to_rfc3339();
    let mut deleted_messages = 0usize;

    loop {
        if Instant::now() >= budget.deadline {
            return Ok(AliasGcOutcome {
                deleted_orphan_proofs: 0,
                deleted_messages,
                completed: false,
            });
        }
        // Bounded batches exclude live SM/descendant custody. Ready legacy
        // rows are adopted under the same canonical lock as collection.
        let candidates = expired_candidates(db, &cutoff, budget)
            .await
            .map_err(|error| gc_failure(deleted_messages, error))?;
        if candidates.is_empty() {
            return Ok(AliasGcOutcome {
                deleted_orphan_proofs: 0,
                deleted_messages,
                completed: true,
            });
        }
        let batch_len = candidates.len();
        let batch = match gc_candidate_batch(db, now, &cutoff, candidates, budget).await {
            Ok(batch) => batch,
            Err(mut failure) => {
                failure.deleted_messages += deleted_messages;
                return Err(failure);
            }
        };
        deleted_messages += batch.deleted_messages;
        if batch.deadline_reached {
            return Ok(AliasGcOutcome {
                deleted_orphan_proofs: 0,
                deleted_messages,
                completed: false,
            });
        }
        // A candidate held by another writer stays eligible and reappears at
        // the head of every rescan, so the last batch's skips decide whether
        // the backlog is drained; a batch that processed nothing would only
        // rescan the same locked rows.
        if batch_len < GC_BATCH_LIMIT || batch.processed == 0 {
            return Ok(AliasGcOutcome {
                deleted_orphan_proofs: 0,
                deleted_messages,
                completed: batch_len < GC_BATCH_LIMIT && batch.skipped_locked == 0,
            });
        }
    }
}

/// Work done by one candidate batch.
pub(in crate::ingress_substrate) struct BatchOutcome {
    pub(in crate::ingress_substrate) deleted_messages: usize,
    /// Candidates locked and collected (their aliases are gone).
    pub(in crate::ingress_substrate) processed: usize,
    /// Candidates held by another writer, left eligible for a later run.
    pub(in crate::ingress_substrate) skipped_locked: usize,
    /// The cooperative deadline stopped the batch early.
    pub(in crate::ingress_substrate) deadline_reached: bool,
}

/// Upper bound on messages examined per GC scan.
pub(in crate::ingress_substrate) const GC_BATCH_LIMIT: usize = 256;

pub(in crate::ingress_substrate) async fn gc_candidate_batch(
    db: &Database,
    now: DateTime<Utc>,
    cutoff: &str,
    candidates: Vec<MessageKey>,
    budget: &AliasGcBudget,
) -> Result<BatchOutcome, AliasGcFailure> {
    let mut deleted_messages = 0usize;
    let mut processed = 0usize;
    let mut skipped_locked = 0usize;
    for message_key in candidates {
        if Instant::now() >= budget.deadline {
            return Ok(BatchOutcome {
                deleted_messages,
                processed,
                skipped_locked,
                deadline_reached: true,
            });
        }
        let mut tx = db
            .begin_immediate()
            .await
            .map_err(|error| gc_database_failure(deleted_messages, error))?;
        // First statement: Postgres's default READ COMMITTED mode is required
        // so every candidate lock/recheck sees the latest committed children.
        if tx.driver() == DatabaseDriver::Postgres {
            tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
                .await
                .map_err(|error| gc_database_failure(deleted_messages, error))?;
        }
        install_gc_timeouts(&mut tx, budget, budget.statement_timeout)
            .await
            .map_err(|error| gc_failure(deleted_messages, error))?;
        // GC is an epoch-aware writer: install the transaction-bound proof
        // for the live epoch so its deletes pass the V1009 guard at
        // epoch >= 1 (a no-op at epoch 0).  The FOR SHARE epoch read holds
        // to commit, so a concurrent activation waits and the proof cannot
        // go stale mid-transaction.  The proof only claims epochs this
        // binary was built for: a future activation past
        // supported_protocol_epoch() must fence this GC, not be auto-claimed.
        let live_epoch = acquire_gc_epoch_lock_first(&mut tx, deleted_messages).await?;
        if live_epoch > supported_protocol_epoch() {
            return Err(gc_failure(
                deleted_messages,
                IngressSubstrateError::UnsupportedLiveEpoch.into(),
            ));
        }
        if tx.driver() == DatabaseDriver::Postgres {
            let mut proof = tx
                .query(
                    r#"
                SELECT
                    set_config('waddle.protocol_epoch', ?, true),
                    set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)
                "#,
                    crate::db_params![live_epoch.to_storage().to_string()],
                )
                .await
                .map_err(|error| gc_database_failure(deleted_messages, error))?;
            proof
                .next()
                .await
                .map_err(|error| gc_database_failure(deleted_messages, error))?
                .ok_or_else(|| {
                    gc_failure(
                        deleted_messages,
                        IngressSubstrateError::Database {
                            retry_class: DbRetryClass::NotRetryable,
                        }
                        .into(),
                    )
                })?;
            drop(proof);
        }
        match lock_eligible_terminal_message(&mut tx, message_key, cutoff)
            .await
            .map_err(|error| gc_failure(deleted_messages, error))?
        {
            CandidateLock::Eligible => {}
            CandidateLock::Skipped(reason) => {
                if reason == SkipReason::LockedElsewhere {
                    skipped_locked += 1;
                }
                tx.commit()
                    .await
                    .map_err(|error| gc_database_failure(deleted_messages, error))?;
                continue;
            }
        }
        // Inspect the immutable mapping only after locking the source. A stale
        // candidate that has become a child must never be reclaimed alone.
        let pair = rejection_pair(&mut tx, message_key)
            .await
            .map_err(|error| gc_failure(deleted_messages, error))?;
        let child = match pair {
            RejectionPair::Retained => {
                tx.commit()
                    .await
                    .map_err(|error| gc_database_failure(deleted_messages, error))?;
                continue;
            }
            RejectionPair::Standalone => None,
            RejectionPair::Source(child) => {
                match lock_eligible_terminal_message(&mut tx, child, cutoff)
                    .await
                    .map_err(|error| gc_failure(deleted_messages, error))?
                {
                    CandidateLock::Eligible => Some(child),
                    CandidateLock::Skipped(reason) => {
                        if reason == SkipReason::LockedElsewhere {
                            skipped_locked += 1;
                        }
                        tx.commit()
                            .await
                            .map_err(|error| gc_database_failure(deleted_messages, error))?;
                        continue;
                    }
                }
            }
        };
        // No chain is reclaimed as a pair. Mapping insertion takes the source
        // lock, so these reads remain stable until the transaction completes.
        let standalone_blocked = child.is_none()
            && owner_result_retains_source(&mut tx, message_key)
                .await
                .map_err(|error| gc_database_failure(deleted_messages, error))?;
        let child_blocked = if let Some(child) = child {
            owner_result_retains_source(&mut tx, child)
                .await
                .map_err(|error| gc_database_failure(deleted_messages, error))?
        } else {
            false
        };
        if standalone_blocked || child_blocked {
            tx.commit()
                .await
                .map_err(|error| gc_database_failure(deleted_messages, error))?;
            continue;
        }
        let members = [Some(message_key), child];
        let mut ready = true;
        for member in members.into_iter().flatten() {
            // Recompute custody and both retention clocks while both canonical
            // locks are held. NULL clocks are adopted for a full new tail.
            refresh_retention(&mut tx, member, now)
                .await
                .map_err(|error| gc_failure(deleted_messages, error.into()))?;
            let complete =
                crate::ingress_uow::EffectReceiptRepository::receipts_complete_on_transaction(
                    &mut tx, member,
                )
                .await
                .map_err(|error| gc_failure(deleted_messages, error.into()))?;
            let expired = gc_message_expired(&mut tx, member, cutoff)
                .await
                .map_err(|error| gc_database_failure(deleted_messages, error))?;
            ready &= complete && expired;
        }
        if !ready {
            tx.commit()
                .await
                .map_err(|error| gc_database_failure(deleted_messages, error))?;
            processed += 1;
            continue;
        }
        // The mapping cascades only inside this transaction. Delete the child
        // first, then its source, or roll back every change to both rows.
        let mut deleted = 0u64;
        let mut lost_eligibility = false;
        for member in [child, Some(message_key)].into_iter().flatten() {
            let count = delete_gc_message(&mut tx, member)
                .await
                .map_err(|error| gc_database_failure(deleted_messages, error))?;
            if count != 1 {
                lost_eligibility = true;
                break;
            }
            deleted += count;
        }
        if lost_eligibility {
            tx.rollback()
                .await
                .map_err(|error| gc_database_failure(deleted_messages, error))?;
            continue;
        }
        tx.commit()
            .await
            .map_err(|error| gc_database_failure(deleted_messages, error))?;
        let deleted = usize::try_from(deleted).map_err(|_| {
            gc_failure(
                deleted_messages,
                IngressSubstrateError::Database {
                    retry_class: DbRetryClass::NotRetryable,
                }
                .into(),
            )
        })?;
        budget.progress.record(deleted);
        deleted_messages += deleted;
        processed += 1;
    }

    Ok(BatchOutcome {
        deleted_messages,
        processed,
        skipped_locked,
        deadline_reached: false,
    })
}

async fn gc_message_expired(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
    cutoff: &str,
) -> Result<bool, DatabaseError> {
    let sql = dialect_sql(tx.driver(),
        "SELECT retention_eligible_at IS NOT NULL AND retention_eligible_at <= ?::timestamptz FROM ingress_messages WHERE message_key = ?::uuid AND terminal_at IS NOT NULL",
        "SELECT retention_eligible_at IS NOT NULL AND retention_eligible_at <= ? FROM ingress_messages WHERE message_key = ? AND terminal_at IS NOT NULL");
    let mut rows = tx
        .query(
            sql,
            crate::db_params![cutoff, message_key.to_storage().to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => row.get(0),
        None => Ok(false),
    }
}

async fn delete_gc_message(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
) -> Result<u64, DatabaseError> {
    tx.execute(
        dialect_sql(tx.driver(), DELETE_ALIASES_POSTGRES, DELETE_ALIASES_SQLITE),
        crate::db_params![message_key.to_storage().to_string()],
    )
    .await?;
    tx.execute(
        dialect_sql(
            tx.driver(),
            GC_DELETE_DELIVERIES_POSTGRES,
            GC_DELETE_DELIVERIES_SQLITE,
        ),
        crate::db_params![message_key.to_storage().to_string()],
    )
    .await?;
    let sql =
        gc_retained_child_sql(tx, GC_DELETE_MESSAGE_POSTGRES, GC_DELETE_MESSAGE_SQLITE).await?;
    let deleted = tx
        .execute(
            &sql,
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await?;
    if deleted == 1 {
        tx.execute(
            dialect_sql(
                tx.driver(),
                GC_DELETE_INGRESS_APPENDS_POSTGRES,
                GC_DELETE_INGRESS_APPENDS_SQLITE,
            ),
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await?;
    }
    Ok(deleted)
}

/// Prepare a writer from the physical attestor's complete identity.
///
/// The schema-qualified epoch relation lock is the first SQL operation after
/// transaction isolation and timeout setup. Only then does the writer run
/// the trusted physical attestor on this same transaction connection, before
/// reading the epoch or taking application locks. This path never derives
/// identity from the active search path or DSN.
pub(crate) async fn prepare_attested_receipt_transaction(
    tx: &mut Transaction<'_>,
    capability: &AttestedPostgresWriterCapability,
) -> Result<PreparedAttestedReceiptWriter, IngressSubstrateError> {
    if tx.driver() != DatabaseDriver::Postgres {
        return Err(IngressSubstrateError::Database {
            retry_class: DbRetryClass::NotRetryable,
        });
    }

    let expected = &capability.identity().schema;
    let relation = format!(
        "{}.ingress_protocol_epoch",
        quote_postgres_identifier(&expected.name.0)
    );
    tx.execute(
        &format!("LOCK TABLE ONLY {relation} IN ACCESS SHARE MODE"),
        (),
    )
    .await
    .map_err(discard_database_error)?;

    capability
        .attest_transaction(tx)
        .await
        .map_err(discard_database_error)?;

    let mut schema_rows = tx
        .query(
            "SELECT oid::text FROM pg_catalog.pg_namespace WHERE nspname = ?",
            crate::db_params![expected.name.0.clone()],
        )
        .await
        .map_err(discard_database_error)?;
    let actual_schema_oid = schema_rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::SchemaIdentityMismatch)?
        .get::<String>(0)
        .map_err(discard_database_error)?
        .parse::<u32>()
        .map_err(|_| IngressSubstrateError::SchemaIdentityMismatch)?;
    drop(schema_rows);
    if actual_schema_oid != expected.oid {
        return Err(IngressSubstrateError::SchemaIdentityMismatch);
    }

    let mut isolation_rows = tx
        .query("SELECT current_setting('transaction_isolation')", ())
        .await
        .map_err(discard_database_error)?;
    let isolation = isolation_rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::ReadCommittedRequired)?
        .get::<String>(0)
        .map_err(discard_database_error)?;
    if isolation != "read committed" {
        return Err(IngressSubstrateError::ReadCommittedRequired);
    }
    drop(isolation_rows);

    // PostgreSQL searches the temporary schema before ordinary schemas unless
    // it is explicitly ordered. Keep the attested schema first and pg_temp
    // last for the remainder of this transaction; receipt SQL is also
    // explicitly schema-qualified at its repository boundary.
    let search_path = format!("{}, pg_temp", quote_postgres_identifier(&expected.name.0));
    let mut search_path_rows = tx
        .query(
            "SELECT pg_catalog.set_config('search_path', ?, true)",
            crate::db_params![search_path],
        )
        .await
        .map_err(discard_database_error)?;
    search_path_rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::Database {
            retry_class: DbRetryClass::NotRetryable,
        })?;
    drop(search_path_rows);

    let fresh_epoch = read_epoch_in_relation(tx, &relation, false).await?;
    let mut proof = tx
        .query(
            "SELECT set_config('waddle.protocol_epoch', ?, true), set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)",
            crate::db_params![fresh_epoch.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    proof
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::Database {
            retry_class: DbRetryClass::NotRetryable,
        })?;
    drop(proof);

    let locked_epoch = read_epoch_in_relation(tx, &relation, true).await?;
    if locked_epoch != fresh_epoch {
        return Err(IngressSubstrateError::EpochChangedDuringPreparation);
    }
    Ok(PreparedAttestedReceiptWriter {
        protocol_epoch: locked_epoch,
        schema: expected.name.0.clone(),
        relation_guard: false,
        definer_api_candidate: false,
    })
}

/// Evidence that the transaction was prepared on the attested PostgreSQL
/// connection before application locks or writes. The type stays private to
/// the crate and is stored inside the owning UoW transaction.
pub(crate) struct PreparedAttestedReceiptWriter {
    protocol_epoch: ProtocolEpoch,
    schema: String,
    relation_guard: bool,
    definer_api_candidate: bool,
}

impl PreparedAttestedReceiptWriter {
    /// Construct only after the same-transaction relation capability has
    /// verified its owner marker and installed the epoch proof.
    pub(crate) fn for_relation_guard(
        capability: &AttestedPostgresWriterCapability,
        protocol_epoch: ProtocolEpoch,
    ) -> Self {
        Self {
            protocol_epoch,
            schema: capability.identity().schema.name.0.clone(),
            relation_guard: true,
            definer_api_candidate: false,
        }
    }

    pub(crate) fn protocol_epoch(&self) -> ProtocolEpoch {
        self.protocol_epoch
    }

    pub(crate) fn schema(&self) -> &str {
        &self.schema
    }

    pub(crate) fn uses_relation_guard(&self) -> bool {
        self.relation_guard
    }

    pub(crate) fn uses_definer_api_candidate(&self) -> bool {
        self.definer_api_candidate
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn enable_definer_api_candidate_for_test(&mut self) {
        self.definer_api_candidate = true;
    }
}

/// Lexical authority for calling only the unregistered definer API while a
/// restricted-login candidate is under test. The context borrows the
/// prepared proof from the same transaction and carries only its attested
/// schema; it cannot acquire another connection or enable the candidate.
pub(crate) struct DefinerApiCandidateContext<'a> {
    prepared: &'a PreparedAttestedReceiptWriter,
}

impl<'a> DefinerApiCandidateContext<'a> {
    pub(crate) fn from_prepared(prepared: &'a PreparedAttestedReceiptWriter) -> Option<Self> {
        prepared
            .uses_definer_api_candidate()
            .then_some(Self { prepared })
    }

    pub(crate) fn schema(&self) -> &str {
        self.prepared.schema()
    }

    pub(crate) fn prepared(&self) -> &'a PreparedAttestedReceiptWriter {
        self.prepared
    }

    pub(crate) fn qualified_function(&self, function: &'static str) -> String {
        format!("{}.{}", quote_postgres_identifier(self.schema()), function)
    }
}

/// The relations selected by prepared ingress UoW statements.
#[derive(Clone, Copy)]
pub(crate) enum PreparedIngressRelation {
    Epoch,
    Messages,
    Aliases,
    SmRefs,
    SmStreams,
    Sessions,
    Effects,
    Deliveries,
    DeliveryReceipts,
    CarbonReceipts,
    EffectReceipts,
    EffectDescendants,
    #[allow(dead_code)] // used by staged relayed-source authority methods.
    OwnerRejections,
    SendAttempts,
    SmAppends,
    PendingDelivery,
    #[cfg(feature = "clustering")]
    Claims,
    InboxEntries,
    ArchiveDispatch,
}

/// Only a prepared transaction can supply a schema; callers cannot pass an
/// untrusted schema string through the ingress repository boundary.
pub(crate) fn prepared_ingress_relation(
    prepared: Option<&PreparedAttestedReceiptWriter>,
    relation: PreparedIngressRelation,
) -> String {
    let name = match relation {
        PreparedIngressRelation::Epoch => "ingress_protocol_epoch",
        PreparedIngressRelation::Messages => "ingress_messages",
        PreparedIngressRelation::Aliases => "ingress_origin_aliases",
        PreparedIngressRelation::SmRefs => "ingress_sm_refs",
        PreparedIngressRelation::SmStreams => "ingress_sm_streams",
        PreparedIngressRelation::Sessions => "sessions",
        PreparedIngressRelation::Effects => "ingress_effect_intents",
        PreparedIngressRelation::Deliveries => "ingress_deliveries",
        PreparedIngressRelation::DeliveryReceipts => "ingress_delivery_receipts",
        PreparedIngressRelation::CarbonReceipts => "ingress_carbon_receipts",
        PreparedIngressRelation::EffectReceipts => "ingress_effect_receipts",
        PreparedIngressRelation::EffectDescendants => "ingress_effect_descendants",
        PreparedIngressRelation::OwnerRejections => "ingress_owner_rejections",
        PreparedIngressRelation::SendAttempts => "ingress_send_attempts",
        PreparedIngressRelation::SmAppends => "sm_ingress_appends",
        PreparedIngressRelation::PendingDelivery => "pending_delivery",
        #[cfg(feature = "clustering")]
        PreparedIngressRelation::Claims => "clustering_claims",
        PreparedIngressRelation::InboxEntries => "inbox_entries",
        PreparedIngressRelation::ArchiveDispatch => "ingress_archive_dispatch",
    };
    match prepared {
        Some(prepared) => format!("{}.{name}", quote_postgres_identifier(prepared.schema())),
        None => name.to_owned(),
    }
}

/// Build PostgreSQL from fixed text on either side of an allowlisted relation.
/// Raw transactions select its bare name; SQLite uses its original statement.
pub(crate) fn prepared_ingress_sql(
    driver: DatabaseDriver,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    relation: PreparedIngressRelation,
    postgres: (&'static str, &'static str),
    sqlite: &'static str,
) -> std::borrow::Cow<'static, str> {
    match driver {
        DatabaseDriver::Postgres => std::borrow::Cow::Owned(format!(
            "{}{}{}",
            postgres.0,
            prepared_ingress_relation(prepared, relation),
            postgres.1
        )),
        DatabaseDriver::Sqlite => std::borrow::Cow::Borrowed(sqlite),
    }
}

async fn read_epoch_in_relation(
    tx: &mut Transaction<'_>,
    relation: &str,
    for_share: bool,
) -> Result<ProtocolEpoch, IngressSubstrateError> {
    let lock_clause = if for_share { " FOR SHARE" } else { "" };
    let mut rows = tx
        .query(
            &format!("SELECT epoch FROM {relation} WHERE id = 1{lock_clause}"),
            (),
        )
        .await
        .map_err(discard_database_error)?;
    let epoch = rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::Database {
            retry_class: DbRetryClass::NotRetryable,
        })?
        .get::<i64>(0)
        .map_err(discard_database_error)?;
    u32::try_from(epoch)
        .map(ProtocolEpoch::from_storage)
        .map_err(|_| IngressSubstrateError::UnsupportedLiveEpoch)
}

pub(crate) fn quote_postgres_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

pub(in crate::ingress_substrate) async fn install_gc_timeouts(
    tx: &mut Transaction<'_>,
    budget: &AliasGcBudget,
    statement_timeout: StdDuration,
) -> Result<(), AliasGcError> {
    if set_local_transaction_timeouts(tx, budget.lock_timeout, statement_timeout)
        .await
        .map_err(gc_error_from_database)?
    {
        Ok(())
    } else {
        Err(AliasGcError::Substrate(IngressSubstrateError::Database {
            retry_class: DbRetryClass::NotRetryable,
        }))
    }
}

/// Install transaction-local PostgreSQL `lock_timeout` / `statement_timeout`
/// bounds (`SET LOCAL` semantics: they revert at commit or rollback).
/// Returns whether the driver confirmed the settings; `false` is a
/// defensive signal for a missing `set_config` row.
pub(crate) async fn set_local_transaction_timeouts(
    tx: &mut Transaction<'_>,
    lock_timeout: StdDuration,
    statement_timeout: StdDuration,
) -> Result<bool, DatabaseError> {
    if tx.driver() == DatabaseDriver::Sqlite {
        return Ok(true);
    }
    let mut proof = tx
        .query(
            r#"
            SELECT
                set_config('lock_timeout', ?, true),
                set_config('statement_timeout', ?, true)
            "#,
            crate::db_params![
                format!("{}ms", lock_timeout.as_millis()),
                format!("{}ms", statement_timeout.as_millis()),
            ],
        )
        .await?;
    Ok(proof.next().await?.is_some())
}

/// Bound raw writer waits without relaxing a tighter transaction setting.
pub(crate) async fn tighten_local_transaction_timeouts(
    tx: &mut Transaction<'_>,
) -> Result<bool, DatabaseError> {
    if tx.driver() == DatabaseDriver::Sqlite {
        return Ok(true);
    }
    let mut rows = tx
        .query(
            r#"
            SELECT
              set_config('lock_timeout', CASE
                WHEN current_setting('lock_timeout')::interval = interval '0 ms'
                  OR current_setting('lock_timeout')::interval > interval '100 ms'
                THEN '100ms' ELSE current_setting('lock_timeout') END, true),
              set_config('statement_timeout', CASE
                WHEN current_setting('statement_timeout')::interval = interval '0 ms'
                  OR current_setting('statement_timeout')::interval > interval '250 ms'
                THEN '250ms' ELSE current_setting('statement_timeout') END, true)
            "#,
            (),
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Typed alias identity held until the driver binding edge: the sender and
/// origin-id stay borrowed as their protocol types; textual forms are read
/// only while hashing and inside `db_params!`.
pub(in crate::ingress_substrate) struct AliasStorageKey<'a> {
    pub(in crate::ingress_substrate) hash: [u8; 32],
    pub(in crate::ingress_substrate) sender: &'a BareJid,
    pub(in crate::ingress_substrate) target: NormalizedTargetStorage,
    pub(in crate::ingress_substrate) origin_id: &'a OriginId,
}

impl<'a> AliasStorageKey<'a> {
    pub(in crate::ingress_substrate) fn new(
        sender: &'a BareJid,
        target: &NormalizedTarget,
        origin_id: &'a OriginId,
    ) -> Self {
        let target = target.to_storage();
        // Uniqueness rides a fixed-width SHA-256 of the length-prefixed
        // canonical encoding: a composite B-tree key over the raw columns
        // can exceed PostgreSQL's index-row limit at maximum JID and
        // origin-id lengths.  Length prefixes keep the encoding injective.
        let sender_text = sender.to_string();
        let mut hasher = Sha256::new();
        hasher.update(b"waddle:ingress-alias-key:v1\0");
        for part in [sender_text.as_str(), target.jid(), origin_id.as_str()] {
            hasher.update(u32::try_from(part.len()).unwrap_or(u32::MAX).to_be_bytes());
            hasher.update(part.as_bytes());
        }
        hasher.update(target.kind().to_be_bytes());
        Self {
            hash: hasher.finalize().into(),
            sender,
            target,
            origin_id,
        }
    }
}

pub(in crate::ingress_substrate) async fn locked_alias(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    key: &AliasStorageKey<'_>,
) -> Result<Option<StoredAlias>, IngressSubstrateError> {
    let qualified;
    let postgres = if prepared.is_some() {
        qualified = format!(
            "SELECT m.message_key::text, m.digest_version, m.digest, a.sender_bare_jid, a.target_kind, a.target_jid, a.origin_id FROM {} a JOIN {} m USING (message_key) WHERE a.alias_key_hash = ? FOR UPDATE OF m",
            prepared_ingress_relation(prepared, PreparedIngressRelation::Aliases),
            prepared_ingress_relation(prepared, PreparedIngressRelation::Messages),
        );
        qualified.as_str()
    } else {
        LOCK_ALIAS_POSTGRES
    };
    let mut rows = tx
        .query(
            if tx.driver() == DatabaseDriver::Postgres {
                postgres
            } else {
                LOCK_ALIAS_SQLITE
            },
            crate::db_params![key.hash.to_vec()],
        )
        .await
        .map_err(discard_database_error)?;
    match rows.next().await.map_err(discard_database_error)? {
        Some(row) => {
            validate_stored_alias_identity(&row, key)?;
            decode_stored_alias(&row).map(Some)
        }
        None => Ok(None),
    }
}

pub(in crate::ingress_substrate) fn validate_stored_alias_identity(
    row: &Row,
    key: &AliasStorageKey<'_>,
) -> Result<(), IngressSubstrateError> {
    validate_stored_alias_identity_at(row, key, 3)
}

pub(in crate::ingress_substrate) fn validate_stored_alias_identity_at(
    row: &Row,
    key: &AliasStorageKey<'_>,
    offset: usize,
) -> Result<(), IngressSubstrateError> {
    let sender_text: String = row.get(offset).map_err(discard_database_error)?;
    let target_kind: i64 = row.get(offset + 1).map_err(discard_database_error)?;
    let target_jid: String = row.get(offset + 2).map_err(discard_database_error)?;
    let origin_text: String = row.get(offset + 3).map_err(discard_database_error)?;

    let sender = sender_text
        .parse::<BareJid>()
        .map_err(|_| IngressSubstrateError::AliasIdentityConflict)?;
    let target_kind =
        i32::try_from(target_kind).map_err(|_| IngressSubstrateError::AliasIdentityConflict)?;
    let target = NormalizedTarget::from_storage(target_kind, &target_jid)
        .map_err(|_| IngressSubstrateError::AliasIdentityConflict)?;
    let origin = OriginId::new(origin_text.clone());

    if sender != *key.sender
        || sender_text != key.sender.to_string()
        || target.to_storage() != key.target
        || target_kind != key.target.kind()
        || target_jid != key.target.jid()
        || origin != *key.origin_id
        || origin_text != key.origin_id.as_str()
    {
        return Err(IngressSubstrateError::AliasIdentityConflict);
    }
    Ok(())
}

pub(in crate::ingress_substrate) fn decode_stored_alias(
    row: &Row,
) -> Result<StoredAlias, IngressSubstrateError> {
    let key_text: String = row.get(0).map_err(discard_database_error)?;
    let digest_version: i64 = row.get(1).map_err(discard_database_error)?;
    let digest: Vec<u8> = row.get(2).map_err(discard_database_error)?;
    let key = key_text
        .parse::<Uuid>()
        .map(MessageKey::from_storage)
        .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey)?;
    let digest_version =
        u8::try_from(digest_version).map_err(|_| IngressSubstrateError::InvalidStoredDigest)?;
    let digest: [u8; 32] = digest
        .try_into()
        .map_err(|_| IngressSubstrateError::InvalidStoredDigest)?;
    let digest = SemanticDigest::from_storage(digest_version, digest)
        .map_err(|_| IngressSubstrateError::InvalidStoredDigest)?;
    Ok(StoredAlias { key, digest })
}

/// Read the message key a child row (sm ref / delivery) is bound to.
pub(in crate::ingress_substrate) async fn stored_child_message_key(
    tx: &mut Transaction<'_>,
    sql: &str,
    params: impl crate::db::IntoParams,
) -> Result<Option<MessageKey>, IngressSubstrateError> {
    let mut rows = tx
        .query(sql, params)
        .await
        .map_err(discard_database_error)?;
    let Some(row) = rows.next().await.map_err(discard_database_error)? else {
        return Ok(None);
    };
    let key_text: String = row.get(0).map_err(discard_database_error)?;
    key_text
        .parse::<Uuid>()
        .map(MessageKey::from_storage)
        .map(Some)
        .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey)
}

pub(crate) async fn lock_message_for_child_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    message_key: MessageKey,
) -> Result<bool, IngressSubstrateError> {
    if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
        let function = format!(
            "{}.waddle_ingress_lock_message",
            quote_postgres_identifier(prepared.schema())
        );
        let mut rows = tx
            .query(
                &format!("SELECT {function}(?::uuid)"),
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await
            .map_err(discard_database_error)?;
        let row = rows
            .next()
            .await
            .map_err(discard_database_error)?
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        let found: bool = row.get(0).map_err(discard_database_error)?;
        if rows.next().await.map_err(discard_database_error)?.is_some() {
            return Err(IngressSubstrateError::InvalidDefinerApiResult);
        }
        return Ok(found);
    }

    let qualified;
    let postgres = if prepared.is_some() {
        qualified = format!(
            "SELECT 1 FROM {} WHERE message_key = ?::uuid FOR UPDATE",
            prepared_ingress_relation(prepared, PreparedIngressRelation::Messages)
        );
        qualified.as_str()
    } else {
        LOCK_MESSAGE_POSTGRES
    };
    let mut rows = tx
        .query(
            if tx.driver() == DatabaseDriver::Postgres {
                postgres
            } else {
                LOCK_MESSAGE_SQLITE
            },
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    Ok(rows.next().await.map_err(discard_database_error)?.is_some())
}

#[cfg(test)]
pub(in crate::ingress_substrate) async fn message_exists(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
) -> Result<bool, IngressSubstrateError> {
    message_exists_in_prepared_transaction(tx, None, message_key).await
}

async fn message_exists_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    message_key: MessageKey,
) -> Result<bool, IngressSubstrateError> {
    let sql = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::Messages,
        ("SELECT 1 FROM ", " WHERE message_key = ?::uuid"),
        MESSAGE_EXISTS_SQLITE,
    );
    let mut rows = tx
        .query(
            &sql,
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    Ok(rows.next().await.map_err(discard_database_error)?.is_some())
}

/// Scan the next batch of candidates under the same statement bound as the
/// per-candidate transactions, so a slow scan is classified as a statement
/// timeout instead of being cancelled from outside.
pub(in crate::ingress_substrate) async fn expired_candidates(
    db: &Database,
    cutoff: &str,
    budget: &AliasGcBudget,
) -> Result<Vec<MessageKey>, AliasGcError> {
    let mut tx = db.begin().await.map_err(gc_error_from_database)?;
    install_gc_timeouts(&mut tx, budget, budget.scan_timeout).await?;
    let candidate_sql =
        gc_retained_child_sql(&mut tx, GC_CANDIDATES_POSTGRES, GC_CANDIDATES_SQLITE)
            .await
            .map_err(gc_error_from_database)?;
    let mut rows = tx
        .query(
            &candidate_sql,
            crate::db_params![cutoff, cutoff, GC_BATCH_LIMIT as i64],
        )
        .await
        .map_err(gc_error_from_database)?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next().await.map_err(gc_error_from_database)? {
        let message_key: String = row.get(0).map_err(gc_error_from_database)?;
        let message_key = message_key
            .parse::<Uuid>()
            .map(MessageKey::from_storage)
            .map_err(|_| AliasGcError::Substrate(IngressSubstrateError::InvalidStoredMessageKey))?;
        candidates.push(message_key);
    }
    drop(rows);
    tx.commit().await.map_err(gc_error_from_database)?;
    Ok(candidates)
}

/// Pending enqueue may terminalize ingress before its offline copy is sent.
/// Retain its ordering authority just like a live SM reference. The pending
/// store owns its schema and is absent in minimal installations; inspect the
/// catalog before mentioning it in SQL. All dedup evidence remains protected,
/// and the candidate scan omits protected rows, so a
/// full batch of pending copies cannot starve collectable later messages.
pub(in crate::ingress_substrate) async fn gc_retained_child_sql(
    tx: &mut Transaction<'_>,
    postgres: &'static str,
    sqlite: &'static str,
) -> Result<String, DatabaseError> {
    let base = dialect_sql(tx.driver(), postgres, sqlite);
    let mut rows = tx
        .query(
            dialect_sql(
                tx.driver(),
                "SELECT 1 WHERE to_regclass('pending_delivery') IS NOT NULL",
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'pending_delivery'",
            ),
            (),
        )
        .await?;
    let has_pending = rows.next().await?.is_some();
    if !has_pending {
        return Ok(base.to_owned());
    }
    let mut sql = base.to_owned();
    for alias in ["m", "child"] {
        sql = sql.replace(
            &format!("SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = {alias}.message_key"),
            &format!("SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = {alias}.message_key UNION ALL SELECT 1 FROM ingress_archive_dispatch dispatch JOIN pending_delivery pending ON pending.row_id = dispatch.pending_row_id WHERE dispatch.message_key = {alias}.message_key AND dispatch.pending_row_id <> ''"),
        );
    }
    Ok(sql)
}

/// A mapped child is never an independent GC candidate. A source and its
/// child can only be reclaimed together after the rejected decision is latched.
#[derive(Debug)]
enum RejectionPair {
    Standalone,
    Source(MessageKey),
    Retained,
}

async fn rejection_pair(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
) -> Result<RejectionPair, AliasGcError> {
    let sql = dialect_sql(
        tx.driver(),
        "SELECT child.child_message_key::text, m.owner_decision::int, EXISTS (SELECT 1 FROM ingress_owner_rejections parent WHERE parent.child_message_key = m.message_key) FROM ingress_messages m LEFT JOIN ingress_owner_rejections child ON child.source_message_key = m.message_key WHERE m.message_key = ?::uuid",
        "SELECT child.child_message_key, m.owner_decision, EXISTS (SELECT 1 FROM ingress_owner_rejections parent WHERE parent.child_message_key = m.message_key) FROM ingress_messages m LEFT JOIN ingress_owner_rejections child ON child.source_message_key = m.message_key WHERE m.message_key = ?",
    );
    let mut rows = tx
        .query(sql, crate::db_params![message_key.to_storage().to_string()])
        .await
        .map_err(gc_error_from_database)?;
    let Some(row) = rows.next().await.map_err(gc_error_from_database)? else {
        return Ok(RejectionPair::Retained);
    };
    let child: Option<String> = row.get(0).map_err(gc_error_from_database)?;
    let decision: Option<i32> = row.get(1).map_err(gc_error_from_database)?;
    let is_child: bool = row.get(2).map_err(gc_error_from_database)?;
    if is_child {
        return Ok(RejectionPair::Retained);
    }
    match (child, decision) {
        (Some(child), Some(2)) => {
            let child = child.parse::<Uuid>().map_err(|_| {
                AliasGcError::Substrate(IngressSubstrateError::InvalidStoredMessageKey)
            })?;
            Ok(RejectionPair::Source(MessageKey::from_storage(child)))
        }
        (None, Some(2)) | (Some(_), _) => Ok(RejectionPair::Retained),
        (None, _) => Ok(RejectionPair::Standalone),
    }
}

async fn owner_result_retains_source(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
) -> Result<bool, DatabaseError> {
    let sql = dialect_sql(
        tx.driver(),
        "SELECT 1 FROM ingress_messages m WHERE m.message_key = ?::uuid AND (EXISTS (SELECT 1 FROM ingress_owner_rejections owner_child WHERE owner_child.source_message_key = m.message_key) OR m.owner_decision = 2 OR (m.source_binding_version IS NOT NULL AND (m.owner_decision IS NULL OR m.owner_decision = 0) AND EXISTS (SELECT 1 FROM ingress_effect_intents owner_intent WHERE owner_intent.message_key = m.message_key AND owner_intent.kind = 12)))",
        "SELECT 1 FROM ingress_messages m WHERE m.message_key = ? AND (EXISTS (SELECT 1 FROM ingress_owner_rejections owner_child WHERE owner_child.source_message_key = m.message_key) OR m.owner_decision = 2 OR (m.source_binding_version IS NOT NULL AND (m.owner_decision IS NULL OR m.owner_decision = 0) AND EXISTS (SELECT 1 FROM ingress_effect_intents owner_intent WHERE owner_intent.message_key = m.message_key AND owner_intent.kind = 12)))",
    );
    let mut rows = tx
        .query(sql, crate::db_params![message_key.to_storage().to_string()])
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Why a candidate was not collected in this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ingress_substrate) enum SkipReason {
    /// Another writer holds the row; it stays eligible for the next run.
    LockedElsewhere,
    /// The row is gone, no longer terminal, or not yet past retention.
    NotEligible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ingress_substrate) enum CandidateLock {
    Eligible,
    Skipped(SkipReason),
}

/// Lock one candidate row and report whether its retention has elapsed.
///
/// A row held by another writer — a concurrent GC run or an in-flight child
/// write — is skipped rather than waited on: the child write keeps the
/// message alive anyway, and a skipped candidate stays eligible for the next
/// run.  Only the epoch row is ever waited on, bounded by `lock_timeout`.
pub(in crate::ingress_substrate) async fn lock_eligible_terminal_message(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
    cutoff: &str,
) -> Result<CandidateLock, AliasGcError> {
    let mut rows = tx
        .query(
            dialect_sql(
                tx.driver(),
                GC_LOCK_CANDIDATE_POSTGRES,
                GC_LOCK_CANDIDATE_SQLITE,
            ),
            crate::db_params![cutoff, message_key.to_storage().to_string()],
        )
        .await
        .map_err(gc_error_from_database)?;
    let locked = rows.next().await.map_err(gc_error_from_database)?;
    drop(rows);
    match locked {
        Some(row) => {
            let expired: bool = row.get(0).map_err(gc_error_from_database)?;
            Ok(if expired {
                CandidateLock::Eligible
            } else {
                CandidateLock::Skipped(SkipReason::NotEligible)
            })
        }
        None => {
            // No row under SKIP LOCKED means either "held elsewhere" or
            // "gone / not eligible"; tell them apart without locking.
            let mut rows = tx
                .query(
                    dialect_sql(
                        tx.driver(),
                        GC_CHECK_CANDIDATE_POSTGRES,
                        GC_CHECK_CANDIDATE_SQLITE,
                    ),
                    crate::db_params![message_key.to_storage().to_string(), cutoff],
                )
                .await
                .map_err(gc_error_from_database)?;
            let still_eligible = rows.next().await.map_err(gc_error_from_database)?.is_some();
            Ok(CandidateLock::Skipped(if still_eligible {
                SkipReason::LockedElsewhere
            } else {
                SkipReason::NotEligible
            }))
        }
    }
}

pub(in crate::ingress_substrate) fn discard_database_error(
    error: DatabaseError,
) -> IngressSubstrateError {
    if crate::ingress_uow::is_database_timeout(&error) {
        return IngressSubstrateError::Timeout;
    }
    IngressSubstrateError::Database {
        retry_class: DbRetryClass::from_database_error(&error),
    }
}

pub(in crate::ingress_substrate) fn gc_failure(
    deleted_messages: usize,
    error: AliasGcError,
) -> AliasGcFailure {
    AliasGcFailure {
        deleted_orphan_proofs: 0,
        deleted_messages,
        error,
    }
}

pub(in crate::ingress_substrate) fn gc_database_failure(
    deleted_messages: usize,
    error: DatabaseError,
) -> AliasGcFailure {
    gc_failure(deleted_messages, gc_error_from_database(error))
}

pub(in crate::ingress_substrate) fn gc_error_from_database(error: DatabaseError) -> AliasGcError {
    match database_timeout_kind(&error) {
        Some(kind) => AliasGcError::DatabaseTimeout { kind },
        None => AliasGcError::Substrate(discard_database_error(error)),
    }
}

pub(in crate::ingress_substrate) fn database_timeout_kind(
    error: &DatabaseError,
) -> Option<DatabaseTimeoutKind> {
    let DatabaseError::Internal(sqlx::Error::Database(database_error)) = error else {
        return None;
    };
    match database_error.code().as_deref() {
        Some("55P03") => Some(DatabaseTimeoutKind::Lock),
        Some("57014") => Some(DatabaseTimeoutKind::Statement),
        _ => None,
    }
}

pub(in crate::ingress_substrate) async fn acquire_gc_epoch_lock_first(
    tx: &mut Transaction<'_>,
    deleted_messages: usize,
) -> Result<ProtocolEpoch, AliasGcFailure> {
    read_live_epoch_for_share(tx).await.map_err(|error| {
        let error = match error {
            EpochLockError::Database(error) => gc_error_from_database(error),
            EpochLockError::MissingRow => {
                AliasGcError::Substrate(IngressSubstrateError::Database {
                    retry_class: DbRetryClass::NotRetryable,
                })
            }
            EpochLockError::UnsupportedLiveEpoch => {
                AliasGcError::Substrate(IngressSubstrateError::UnsupportedLiveEpoch)
            }
        };
        gc_failure(deleted_messages, error)
    })
}

pub(in crate::ingress_substrate) fn dialect_sql(
    driver: DatabaseDriver,
    postgres: &'static str,
    sqlite: &'static str,
) -> &'static str {
    match driver {
        DatabaseDriver::Postgres => postgres,
        DatabaseDriver::Sqlite => sqlite,
    }
}
pub(in crate::ingress_substrate) const READ_EPOCH_POSTGRES: &str =
    r#"SELECT epoch FROM ingress_protocol_epoch WHERE id = 1 FOR SHARE"#;
pub(in crate::ingress_substrate) const READ_EPOCH_SQLITE: &str =
    r#"SELECT epoch FROM ingress_protocol_epoch WHERE id = 1 "#;
pub(in crate::ingress_substrate) const INSERT_MESSAGE_POSTGRES: &str = r#"
        INSERT INTO ingress_messages (message_key, digest_version, digest, envelope_version, envelope, source_binding_version, source_bare_jid, source_auth_context_id, source_auth_context_version, source_auth_epoch, source_sm_ingress_id, source_ingress_ordinal)
        VALUES (?::uuid, ?, ?, ?, ?, ?, ?, ?::uuid, ?::numeric, ?::numeric, ?::uuid, ?::numeric)
        ON CONFLICT (message_key) DO NOTHING
        "#;
pub(in crate::ingress_substrate) const INSERT_MESSAGE_SQLITE: &str = r#"
        INSERT INTO ingress_messages (message_key, digest_version, digest, envelope_version, envelope, source_binding_version, source_bare_jid, source_auth_context_id, source_auth_context_version, source_auth_epoch, source_sm_ingress_id, source_ingress_ordinal)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (message_key) DO NOTHING
        "#;
pub(in crate::ingress_substrate) const INSERT_ALIAS_POSTGRES: &str = r#"
            INSERT INTO ingress_origin_aliases
                (alias_key_hash, sender_bare_jid, target_kind, target_jid, origin_id, message_key)
            VALUES (?, ?, ?, ?, ?, ?::uuid)
            ON CONFLICT (alias_key_hash) DO NOTHING
            "#;
pub(in crate::ingress_substrate) const INSERT_ALIAS_SQLITE: &str = r#"
            INSERT INTO ingress_origin_aliases
                (alias_key_hash, sender_bare_jid, target_kind, target_jid, origin_id, message_key)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT (alias_key_hash) DO NOTHING
            "#;
pub(in crate::ingress_substrate) const DELETE_CANDIDATE_POSTGRES: &str =
    r#"DELETE FROM ingress_messages WHERE message_key = ?::uuid"#;
pub(in crate::ingress_substrate) const DELETE_CANDIDATE_SQLITE: &str =
    r#"DELETE FROM ingress_messages WHERE message_key = ?"#;
pub(in crate::ingress_substrate) const INSERT_SM_REF_POSTGRES: (&str, &str) = (
    r#"
            INSERT INTO "#,
    r#" (sm_ingress_id, ingress_ordinal, wire_h, wire_generation, message_key)
            VALUES (?::uuid, ?::numeric, ?, ?, ?::uuid)
            ON CONFLICT (sm_ingress_id, ingress_ordinal) DO NOTHING
            "#,
);
pub(in crate::ingress_substrate) const INSERT_SM_REF_SQLITE: &str = r#"
            INSERT INTO ingress_sm_refs (sm_ingress_id, ingress_ordinal, wire_h, wire_generation, message_key)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT (sm_ingress_id, ingress_ordinal) DO NOTHING
            "#;
pub(in crate::ingress_substrate) const READ_SM_REF_POSTGRES: (&str, &str) = (
    r#"
        SELECT message_key::text FROM "#,
    r#"
        WHERE sm_ingress_id = ?::uuid AND ingress_ordinal = ?::numeric
        "#,
);
pub(in crate::ingress_substrate) const READ_SM_REF_SQLITE: &str = r#"
        SELECT message_key FROM ingress_sm_refs
        WHERE sm_ingress_id = ? AND ingress_ordinal = ?
        "#;
pub(in crate::ingress_substrate) const INSERT_DELIVERY_SQLITE: &str = r#"
            INSERT INTO ingress_deliveries (delivery_key, message_key)
            VALUES (?, ?)
            ON CONFLICT (delivery_key) DO NOTHING
            "#;
pub(in crate::ingress_substrate) const READ_DELIVERY_SQLITE: &str =
    r#"SELECT message_key FROM ingress_deliveries WHERE delivery_key = ?"#;
pub(in crate::ingress_substrate) const TERMINALIZE_MESSAGE_SQLITE: &str = r#"
            UPDATE ingress_messages
            SET terminal_at = ?
            WHERE message_key = ? AND terminal_at IS NULL
            "#;
pub(in crate::ingress_substrate) const DELETE_ALIASES_POSTGRES: &str =
    r#"DELETE FROM ingress_origin_aliases WHERE message_key = ?::uuid"#;
pub(in crate::ingress_substrate) const DELETE_ALIASES_SQLITE: &str =
    r#"DELETE FROM ingress_origin_aliases WHERE message_key = ?"#;
pub(in crate::ingress_substrate) const GC_DELETE_DELIVERIES_POSTGRES: &str =
    r#"DELETE FROM ingress_deliveries WHERE message_key = ?::uuid"#;
// Pending custody owns its replay payload independently of both the session
// and canonical ingress retention. Only positively discharged custody can be
// reclaimed with its canonical row or by the orphan sweep (#1760).
pub(in crate::ingress_substrate) const GC_DELETE_INGRESS_APPENDS_POSTGRES: &str =
    r#"DELETE FROM sm_ingress_appends WHERE message_key = ? AND disposition <> 0"#;
pub(in crate::ingress_substrate) const GC_DELETE_INGRESS_APPENDS_SQLITE: &str =
    r#"DELETE FROM sm_ingress_appends WHERE message_key = ? AND disposition <> 0"#;
pub(in crate::ingress_substrate) const GC_ORPHAN_WINDOW_POSTGRES: &str = r#"
    SELECT a.message_key, a.receipt_kind, a.semantic_identity_hash, a.resource,
        NOT EXISTS (
            SELECT 1 FROM ingress_messages m WHERE m.message_key = a.message_key::uuid
        ) AS orphan
    FROM sm_ingress_appends a
    WHERE a.disposition <> 0
    ORDER BY a.message_key, a.receipt_kind, a.semantic_identity_hash, a.resource
    LIMIT ?
"#;
pub(in crate::ingress_substrate) const GC_ORPHAN_WINDOW_SQLITE: &str = r#"
    SELECT a.message_key, a.receipt_kind, a.semantic_identity_hash, a.resource,
        NOT EXISTS (
            SELECT 1 FROM ingress_messages m WHERE m.message_key = a.message_key
        ) AS orphan
    FROM sm_ingress_appends a
    WHERE a.disposition <> 0
    ORDER BY a.message_key, a.receipt_kind, a.semantic_identity_hash, a.resource
    LIMIT ?
"#;
pub(in crate::ingress_substrate) const GC_ORPHAN_WINDOW_AFTER_POSTGRES: &str = r#"
    SELECT a.message_key, a.receipt_kind, a.semantic_identity_hash, a.resource,
        NOT EXISTS (
            SELECT 1 FROM ingress_messages m WHERE m.message_key = a.message_key::uuid
        ) AS orphan
    FROM sm_ingress_appends a
    WHERE a.disposition <> 0 AND (a.message_key, a.receipt_kind, a.semantic_identity_hash, a.resource) > (?, ?, ?, ?)
    ORDER BY a.message_key, a.receipt_kind, a.semantic_identity_hash, a.resource
    LIMIT ?
"#;
pub(in crate::ingress_substrate) const GC_ORPHAN_WINDOW_AFTER_SQLITE: &str = r#"
    SELECT a.message_key, a.receipt_kind, a.semantic_identity_hash, a.resource,
        NOT EXISTS (
            SELECT 1 FROM ingress_messages m WHERE m.message_key = a.message_key
        ) AS orphan
    FROM sm_ingress_appends a
    WHERE a.disposition <> 0 AND (a.message_key, a.receipt_kind, a.semantic_identity_hash, a.resource) > (?, ?, ?, ?)
    ORDER BY a.message_key, a.receipt_kind, a.semantic_identity_hash, a.resource
    LIMIT ?
"#;
pub(in crate::ingress_substrate) const GC_DELETE_ORPHAN_PROOFS_POSTGRES: &str = r#"
    DELETE FROM sm_ingress_appends
    WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ?
      AND disposition <> 0
      AND NOT EXISTS (
          SELECT 1 FROM ingress_messages m WHERE m.message_key = sm_ingress_appends.message_key::uuid
      )
"#;
pub(in crate::ingress_substrate) const GC_DELETE_ORPHAN_PROOFS_SQLITE: &str = r#"
    DELETE FROM sm_ingress_appends
    WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ?
      AND disposition <> 0
      AND NOT EXISTS (
          SELECT 1 FROM ingress_messages m WHERE m.message_key = sm_ingress_appends.message_key
      )
"#;
pub(in crate::ingress_substrate) const GC_DELETE_DELIVERIES_SQLITE: &str =
    r#"DELETE FROM ingress_deliveries WHERE message_key = ?"#;
pub(in crate::ingress_substrate) const GC_DELETE_MESSAGE_POSTGRES: &str = r#"
                DELETE FROM ingress_messages m
                WHERE m.message_key = ?::uuid
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_origin_aliases a WHERE a.message_key = m.message_key
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = m.message_key
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_owner_rejections owner_child WHERE owner_child.source_message_key = m.message_key
                  )
                  AND NOT (m.source_binding_version IS NOT NULL AND (m.owner_decision IS NULL OR m.owner_decision = 0)
                      AND EXISTS (SELECT 1 FROM ingress_effect_intents owner_intent WHERE owner_intent.message_key = m.message_key AND owner_intent.kind = 12))
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_deliveries d WHERE d.message_key = m.message_key
                  )
                "#;
pub(in crate::ingress_substrate) const GC_DELETE_MESSAGE_SQLITE: &str = r#"
                DELETE FROM ingress_messages AS m
                WHERE m.message_key = ?
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_origin_aliases a WHERE a.message_key = m.message_key
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = m.message_key
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_owner_rejections owner_child WHERE owner_child.source_message_key = m.message_key
                  )
                  AND NOT (m.source_binding_version IS NOT NULL AND (m.owner_decision IS NULL OR m.owner_decision = 0)
                      AND EXISTS (SELECT 1 FROM ingress_effect_intents owner_intent WHERE owner_intent.message_key = m.message_key AND owner_intent.kind = 12))
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_deliveries d WHERE d.message_key = m.message_key
                  )
                "#;
pub(in crate::ingress_substrate) const LOCK_ALIAS_POSTGRES: &str = r#"
            SELECT m.message_key::text, m.digest_version, m.digest,
                   a.sender_bare_jid, a.target_kind, a.target_jid, a.origin_id
            FROM ingress_origin_aliases a
            JOIN ingress_messages m USING (message_key)
            WHERE a.alias_key_hash = ?
            FOR UPDATE OF m
            "#;
pub(in crate::ingress_substrate) const LOCK_ALIAS_SQLITE: &str = r#"
            SELECT m.message_key, m.digest_version, m.digest,
                   a.sender_bare_jid, a.target_kind, a.target_jid, a.origin_id
            FROM ingress_origin_aliases a
            JOIN ingress_messages m USING (message_key)
            WHERE a.alias_key_hash = ?

            "#;
pub(in crate::ingress_substrate) const LOCK_MESSAGE_POSTGRES: &str =
    r#"SELECT 1 FROM ingress_messages WHERE message_key = ?::uuid FOR UPDATE"#;
pub(in crate::ingress_substrate) const LOCK_MESSAGE_SQLITE: &str =
    r#"SELECT 1 FROM ingress_messages WHERE message_key = ? "#;
pub(in crate::ingress_substrate) const MESSAGE_EXISTS_SQLITE: &str =
    r#"SELECT 1 FROM ingress_messages WHERE message_key = ?"#;
// Eligibility is shared with the CNPG query; see the retention predicate in
// docs/operations/ingress-authority.md and gc_monitoring_predicate_matches_collector.
pub(in crate::ingress_substrate) const GC_CANDIDATES_POSTGRES: &str = r#"
            SELECT m.message_key::text
            FROM ingress_messages m
            WHERE m.terminal_at IS NOT NULL
              AND (m.retention_eligible_at IS NULL OR m.retention_eligible_at <= ?::timestamptz)
              AND NOT EXISTS (
                  SELECT 1 FROM ingress_effect_intents i WHERE i.message_key = m.message_key
                    AND NOT EXISTS (
                        SELECT 1 FROM ingress_effect_receipts r
                        WHERE r.message_key = i.message_key AND r.kind = i.kind
                          AND r.semantic_identity_hash = i.semantic_identity_hash
                    )
              )
              AND NOT EXISTS (
                  SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = m.message_key
              )
              AND NOT EXISTS (
                  SELECT 1 FROM ingress_owner_rejections parent WHERE parent.child_message_key = m.message_key
              )
              AND (m.owner_decision IS NULL OR m.owner_decision <> 2 OR EXISTS (
                  SELECT 1 FROM ingress_owner_rejections owner_child WHERE owner_child.source_message_key = m.message_key
              ))
              AND NOT EXISTS (
                  SELECT 1 FROM ingress_owner_rejections pair WHERE pair.source_message_key = m.message_key
                    AND (m.owner_decision IS NULL OR m.owner_decision <> 2 OR NOT EXISTS (
                      SELECT 1 FROM ingress_messages child WHERE child.message_key = pair.child_message_key
                        AND child.terminal_at IS NOT NULL
                        AND (child.retention_eligible_at IS NULL OR child.retention_eligible_at <= ?::timestamptz)
                        AND NOT EXISTS (SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = child.message_key)
                        AND NOT EXISTS (SELECT 1 FROM ingress_effect_descendants d WHERE d.message_key = child.message_key AND d.settled_at IS NULL)
                        AND NOT EXISTS (SELECT 1 FROM ingress_owner_rejections nested WHERE nested.source_message_key = child.message_key)
                        AND (child.owner_decision IS NULL OR child.owner_decision <> 2)
                        AND NOT (child.source_binding_version IS NOT NULL AND (child.owner_decision IS NULL OR child.owner_decision = 0)
                          AND EXISTS (SELECT 1 FROM ingress_effect_intents owner_intent WHERE owner_intent.message_key = child.message_key AND owner_intent.kind = 12))
                        AND NOT EXISTS (SELECT 1 FROM ingress_effect_intents i WHERE i.message_key = child.message_key AND NOT EXISTS (
                          SELECT 1 FROM ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash))
                    ))
              )
              AND NOT (m.source_binding_version IS NOT NULL AND (m.owner_decision IS NULL OR m.owner_decision = 0)
                  AND EXISTS (SELECT 1 FROM ingress_effect_intents owner_intent WHERE owner_intent.message_key = m.message_key AND owner_intent.kind = 12))
              AND NOT EXISTS (
                  SELECT 1 FROM ingress_effect_descendants d WHERE d.message_key = m.message_key AND d.settled_at IS NULL
              )
            ORDER BY m.retention_eligible_at, m.message_key
            LIMIT ?
            "#;
pub(in crate::ingress_substrate) const GC_CANDIDATES_SQLITE: &str = r#"
            SELECT m.message_key
            FROM ingress_messages m
            WHERE m.terminal_at IS NOT NULL
              AND (m.retention_eligible_at IS NULL OR m.retention_eligible_at <= ?)
              AND NOT EXISTS (
                  SELECT 1 FROM ingress_effect_intents i WHERE i.message_key = m.message_key
                    AND NOT EXISTS (
                        SELECT 1 FROM ingress_effect_receipts r
                        WHERE r.message_key = i.message_key AND r.kind = i.kind
                          AND r.semantic_identity_hash = i.semantic_identity_hash
                    )
              )
              AND NOT EXISTS (
                  SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = m.message_key
              )
              AND NOT EXISTS (
                  SELECT 1 FROM ingress_owner_rejections parent WHERE parent.child_message_key = m.message_key
              )
              AND (m.owner_decision IS NULL OR m.owner_decision <> 2 OR EXISTS (
                  SELECT 1 FROM ingress_owner_rejections owner_child WHERE owner_child.source_message_key = m.message_key
              ))
              AND NOT EXISTS (
                  SELECT 1 FROM ingress_owner_rejections pair WHERE pair.source_message_key = m.message_key
                    AND (m.owner_decision IS NULL OR m.owner_decision <> 2 OR NOT EXISTS (
                      SELECT 1 FROM ingress_messages child WHERE child.message_key = pair.child_message_key
                        AND child.terminal_at IS NOT NULL
                        AND (child.retention_eligible_at IS NULL OR child.retention_eligible_at <= ?)
                        AND NOT EXISTS (SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = child.message_key)
                        AND NOT EXISTS (SELECT 1 FROM ingress_effect_descendants d WHERE d.message_key = child.message_key AND d.settled_at IS NULL)
                        AND NOT EXISTS (SELECT 1 FROM ingress_owner_rejections nested WHERE nested.source_message_key = child.message_key)
                        AND (child.owner_decision IS NULL OR child.owner_decision <> 2)
                        AND NOT (child.source_binding_version IS NOT NULL AND (child.owner_decision IS NULL OR child.owner_decision = 0)
                          AND EXISTS (SELECT 1 FROM ingress_effect_intents owner_intent WHERE owner_intent.message_key = child.message_key AND owner_intent.kind = 12))
                        AND NOT EXISTS (SELECT 1 FROM ingress_effect_intents i WHERE i.message_key = child.message_key AND NOT EXISTS (
                          SELECT 1 FROM ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash))
                    ))
              )
              AND NOT (m.source_binding_version IS NOT NULL AND (m.owner_decision IS NULL OR m.owner_decision = 0)
                  AND EXISTS (SELECT 1 FROM ingress_effect_intents owner_intent WHERE owner_intent.message_key = m.message_key AND owner_intent.kind = 12))
              AND NOT EXISTS (
                  SELECT 1 FROM ingress_effect_descendants d WHERE d.message_key = m.message_key AND d.settled_at IS NULL
              )
            ORDER BY m.retention_eligible_at, m.message_key
            LIMIT ?
            "#;
pub(in crate::ingress_substrate) const GC_LOCK_CANDIDATE_POSTGRES: &str = r#"
            SELECT retention_eligible_at IS NULL OR retention_eligible_at <= ?::timestamptz
            FROM ingress_messages
            WHERE message_key = ?::uuid AND terminal_at IS NOT NULL
            FOR UPDATE SKIP LOCKED
            "#;
pub(in crate::ingress_substrate) const GC_LOCK_CANDIDATE_SQLITE: &str = r#"
            SELECT retention_eligible_at IS NULL OR retention_eligible_at <= ?
            FROM ingress_messages
            WHERE message_key = ? AND terminal_at IS NOT NULL

            "#;
pub(in crate::ingress_substrate) const GC_CHECK_CANDIDATE_POSTGRES: &str = r#"
                    SELECT 1
                    FROM ingress_messages
                    WHERE message_key = ?::uuid
                      AND terminal_at IS NOT NULL
                      AND (retention_eligible_at IS NULL OR retention_eligible_at <= ?::timestamptz)
                    "#;
pub(in crate::ingress_substrate) const GC_CHECK_CANDIDATE_SQLITE: &str = r#"
                    SELECT 1
                    FROM ingress_messages
                    WHERE message_key = ?
                      AND terminal_at IS NOT NULL
                      AND (retention_eligible_at IS NULL OR retention_eligible_at <= ?)
                    "#;
