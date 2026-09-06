//! Transactional ingress identity storage for PostgreSQL and SQLite.

mod authority;
#[cfg(test)]
mod authority_tests;
pub use authority::{
    advance_frontier, flush_checkpoint, load_envelope, load_stream_checkpoint, lookup_wire_binding,
    receipts_complete, record_receipt, record_receipt_pooled, EffectReceiptKind, EnvelopeVersion,
    FrontierOutcome, MessageEnvelope,
};

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use jid::BareJid;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::Instant;
use uuid::Uuid;
use waddle_xmpp::ingress::{
    resolve_alias, AliasResolution, DeliveryKey, IngressOrdinal, MessageKey, NormalizedTarget,
    NormalizedTargetStorage, ProtocolEpoch, SemanticDigest, SmIngressId, StoredAlias,
    WireHandledCount,
};
use waddle_xmpp_core::xep0359::OriginId;

use crate::db::{Database, DatabaseDriver, DatabaseError, Row, Transaction};
use crate::ingress_uow::DbRetryClass;

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
pub const EPOCH_GUARDED_TABLES: [&str; 7] = [
    "ingress_messages",
    "ingress_origin_aliases",
    "ingress_sm_refs",
    "ingress_deliveries",
    "ingress_sm_streams",
    "ingress_effect_intents",
    "ingress_effect_receipts",
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
    #[error("ingress substrate returned a malformed semantic digest")]
    InvalidStoredDigest,
    #[error("ingress alias disappeared during concurrent resolution")]
    AliasMissingAfterConflict,
    #[error("ingress sm ordinal is already bound to a different message")]
    SmOrdinalConflict,
    #[error("ingress delivery key is already bound to a different message")]
    DeliveryKeyConflict,
    #[error("live ingress protocol epoch exceeds what this binary supports")]
    UnsupportedLiveEpoch,
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

/// Messages committed as deleted so far by one alias GC run.
#[derive(Debug, Clone, Default)]
pub struct AliasGcProgress(Arc<AtomicUsize>);

impl AliasGcProgress {
    pub fn committed(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }

    fn record(&self, deleted_messages: usize) {
        self.0.fetch_add(deleted_messages, Ordering::AcqRel);
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
#[error("ingress alias GC failed after deleting {deleted_messages} messages: {error}")]
pub struct AliasGcFailure {
    pub deleted_messages: usize,
    #[source]
    pub error: AliasGcError,
}

/// Handle for the ingress substrate on either supported database backend.
///
/// The per-operation functions below accept a caller-owned transaction so a
/// future repository can compose ingress writes with its own atomic work.
#[derive(Clone)]
pub struct PostgresIngressSubstrate {
    db: Database,
}

impl PostgresIngressSubstrate {
    /// Open the substrate against the global database.
    pub fn open(db: Database) -> Result<Self, IngressSubstrateError> {
        Ok(Self { db })
    }

    /// Start a transaction, taking SQLite's write reservation immediately.
    pub async fn begin(&self) -> Result<Transaction<'_>, IngressSubstrateError> {
        self.db
            .begin_immediate()
            .await
            .map_err(discard_database_error)
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
    read_live_epoch_for_share(tx)
        .await
        .map_err(|error| match error {
            EpochLockError::Database(error) => discard_database_error(error),
            EpochLockError::MissingRow => IngressSubstrateError::Database {
                retry_class: DbRetryClass::NotRetryable,
            },
            EpochLockError::UnsupportedLiveEpoch => IngressSubstrateError::UnsupportedLiveEpoch,
        })
}

/// Why the singleton epoch row could not be read `FOR SHARE`.  Kept as the
/// raw driver error so callers can classify Postgres timeouts before
/// discarding the diagnostic.
enum EpochLockError {
    Database(DatabaseError),
    MissingRow,
    UnsupportedLiveEpoch,
}

async fn read_live_epoch_for_share(
    tx: &mut Transaction<'_>,
) -> Result<ProtocolEpoch, EpochLockError> {
    let mut rows = tx
        .query(
            dialect_sql(tx.driver(), READ_EPOCH_POSTGRES, READ_EPOCH_SQLITE),
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
    acquire_epoch_lock_first(tx).await?;
    if !insert_message(tx, message_key, digest, envelope).await? {
        authority::complete_message_envelope(tx, message_key, digest, envelope).await?;
    }
    Ok(())
}

async fn insert_message(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
    digest: &SemanticDigest,
    envelope: Option<&MessageEnvelope>,
) -> Result<bool, IngressSubstrateError> {
    let (digest_version, digest_bytes) = digest.to_storage();
    let inserted = tx
        .execute(
            dialect_sql(tx.driver(), INSERT_MESSAGE_POSTGRES, INSERT_MESSAGE_SQLITE),
            crate::db_params![
                message_key.to_storage().to_string(),
                i32::from(digest_version),
                digest_bytes.to_vec(),
                envelope.map(|value| value.version().to_storage()),
                envelope.map(authority::serialize_envelope).transpose()?,
            ],
        )
        .await
        .map_err(discard_database_error)?;
    Ok(inserted == 1)
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
    let _ = acquire_epoch_lock_first(tx).await?;
    let alias_key = AliasStorageKey::new(sender, target, origin_id);
    if let Some(stored) = locked_alias(tx, &alias_key).await? {
        return Ok(resolve_alias(true, digest, Some(&stored), mint));
    }

    let candidate = resolve_alias(true, digest, None, mint);
    let candidate_key = match candidate {
        AliasResolution::Aliased(waddle_xmpp::ingress::AliasOutcome::Inserted(key)) => key,
        _ => return Err(IngressSubstrateError::AliasMissingAfterConflict),
    };
    if !insert_message(tx, candidate_key, digest, None).await? {
        return Err(IngressSubstrateError::MessageContentConflict);
    }
    let inserted = tx
        .execute(
            dialect_sql(tx.driver(), INSERT_ALIAS_POSTGRES, INSERT_ALIAS_SQLITE),
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

    tx.execute(
        dialect_sql(
            tx.driver(),
            DELETE_CANDIDATE_POSTGRES,
            DELETE_CANDIDATE_SQLITE,
        ),
        crate::db_params![candidate_key.to_storage().to_string()],
    )
    .await
    .map_err(discard_database_error)?;

    let stored = locked_alias(tx, &alias_key)
        .await?
        .ok_or(IngressSubstrateError::AliasMissingAfterConflict)?;
    Ok(resolve_alias(true, digest, Some(&stored), || candidate_key))
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
    let _ = acquire_epoch_lock_first(tx).await?;
    if !lock_message_for_child(tx, message_key).await? {
        return Ok(MessageWriteOutcome::MessageVanished);
    }
    let inserted = tx
        .execute(
            dialect_sql(tx.driver(), INSERT_SM_REF_POSTGRES, INSERT_SM_REF_SQLITE),
            crate::db_params![
                sm_ingress_id.to_storage().to_string(),
                ordinal.to_storage().to_string(),
                i64::from(wire_h.to_storage()),
                message_key.to_storage().to_string(),
            ],
        )
        .await
        .map_err(discard_database_error)?;
    if inserted == 1 {
        return Ok(MessageWriteOutcome::Recorded);
    }
    let existing = stored_child_message_key(
        tx,
        dialect_sql(tx.driver(), READ_SM_REF_POSTGRES, READ_SM_REF_SQLITE),
        crate::db_params![
            sm_ingress_id.to_storage().to_string(),
            ordinal.to_storage().to_string(),
        ],
    )
    .await?;
    if existing == Some(message_key)
        && lookup_wire_binding(tx, sm_ingress_id, wire_h).await? == Some((message_key, ordinal))
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
    let _ = acquire_epoch_lock_first(tx).await?;
    if !lock_message_for_child(tx, message_key).await? {
        return Ok(MessageWriteOutcome::MessageVanished);
    }
    let inserted = tx
        .execute(
            dialect_sql(
                tx.driver(),
                INSERT_DELIVERY_POSTGRES,
                INSERT_DELIVERY_SQLITE,
            ),
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
    let existing = stored_child_message_key(
        tx,
        dialect_sql(tx.driver(), READ_DELIVERY_POSTGRES, READ_DELIVERY_SQLITE),
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
    let _ = acquire_epoch_lock_first(tx).await?;
    if !lock_message_for_child(tx, message_key).await? {
        return Ok(TerminalizeOutcome::MessageVanished);
    }
    let changed = tx
        .execute(
            dialect_sql(
                tx.driver(),
                TERMINALIZE_MESSAGE_POSTGRES,
                TERMINALIZE_MESSAGE_SQLITE,
            ),
            crate::db_params![
                proven_terminal_at.to_rfc3339(),
                message_key.to_storage().to_string(),
            ],
        )
        .await
        .map_err(discard_database_error)?;
    if changed == 1 {
        return Ok(TerminalizeOutcome::Terminalized);
    }
    if message_exists(tx, message_key).await? {
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
    let cutoff = (now - ALIAS_RETENTION).to_rfc3339();
    let mut deleted_messages = 0usize;

    loop {
        if Instant::now() >= budget.deadline {
            return Ok(AliasGcOutcome {
                deleted_messages,
                completed: false,
            });
        }
        // Bounded batches of rows with work left to do: expired rows kept
        // alive by refs/deliveries stop matching once their aliases are gone,
        // so retained history does not grow the scan or the candidate vector.
        let candidates = expired_candidates(db, &cutoff, &budget)
            .await
            .map_err(|error| gc_failure(deleted_messages, error))?;
        if candidates.is_empty() {
            return Ok(AliasGcOutcome {
                deleted_messages,
                completed: true,
            });
        }
        let batch_len = candidates.len();
        let batch = match gc_candidate_batch(db, &cutoff, candidates, &budget).await {
            Ok(batch) => batch,
            Err(mut failure) => {
                failure.deleted_messages += deleted_messages;
                return Err(failure);
            }
        };
        deleted_messages += batch.deleted_messages;
        if batch.deadline_reached {
            return Ok(AliasGcOutcome {
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
                deleted_messages,
                completed: batch_len < GC_BATCH_LIMIT && batch.skipped_locked == 0,
            });
        }
    }
}

/// Work done by one candidate batch.
struct BatchOutcome {
    deleted_messages: usize,
    /// Candidates locked and collected (their aliases are gone).
    processed: usize,
    /// Candidates held by another writer, left eligible for a later run.
    skipped_locked: usize,
    /// The cooperative deadline stopped the batch early.
    deadline_reached: bool,
}

/// Upper bound on messages examined per GC scan.
const GC_BATCH_LIMIT: usize = 256;

async fn gc_candidate_batch(
    db: &Database,
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
            CandidateLock::Eligible => processed += 1,
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
        // Alias retention has elapsed: the aliases go unconditionally, even
        // when live refs/deliveries keep the message row itself alive —
        // otherwise a reused sender/target/origin-id would keep resolving
        // against the stale message indefinitely.
        tx.execute(
            dialect_sql(tx.driver(), DELETE_ALIASES_POSTGRES, DELETE_ALIASES_SQLITE),
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await
        .map_err(|error| gc_database_failure(deleted_messages, error))?;
        let deleted = tx
            .execute(
                dialect_sql(
                    tx.driver(),
                    GC_DELETE_MESSAGE_POSTGRES,
                    GC_DELETE_MESSAGE_SQLITE,
                ),
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await
            .map_err(|error| gc_database_failure(deleted_messages, error))?;
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
    }

    Ok(BatchOutcome {
        deleted_messages,
        processed,
        skipped_locked,
        deadline_reached: false,
    })
}

async fn install_gc_timeouts(
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

/// Typed alias identity held until the driver binding edge: the sender and
/// origin-id stay borrowed as their protocol types; textual forms are read
/// only while hashing and inside `db_params!`.
struct AliasStorageKey<'a> {
    hash: [u8; 32],
    sender: &'a BareJid,
    target: NormalizedTargetStorage,
    origin_id: &'a OriginId,
}

impl<'a> AliasStorageKey<'a> {
    fn new(sender: &'a BareJid, target: &NormalizedTarget, origin_id: &'a OriginId) -> Self {
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

async fn locked_alias(
    tx: &mut Transaction<'_>,
    key: &AliasStorageKey<'_>,
) -> Result<Option<StoredAlias>, IngressSubstrateError> {
    let mut rows = tx
        .query(
            dialect_sql(tx.driver(), LOCK_ALIAS_POSTGRES, LOCK_ALIAS_SQLITE),
            crate::db_params![key.hash.to_vec()],
        )
        .await
        .map_err(discard_database_error)?;
    match rows.next().await.map_err(discard_database_error)? {
        Some(row) => decode_stored_alias(&row).map(Some),
        None => Ok(None),
    }
}

fn decode_stored_alias(row: &Row) -> Result<StoredAlias, IngressSubstrateError> {
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
async fn stored_child_message_key(
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

async fn lock_message_for_child(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
) -> Result<bool, IngressSubstrateError> {
    let mut rows = tx
        .query(
            dialect_sql(tx.driver(), LOCK_MESSAGE_POSTGRES, LOCK_MESSAGE_SQLITE),
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    Ok(rows.next().await.map_err(discard_database_error)?.is_some())
}

async fn message_exists(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
) -> Result<bool, IngressSubstrateError> {
    let mut rows = tx
        .query(
            dialect_sql(tx.driver(), MESSAGE_EXISTS_POSTGRES, MESSAGE_EXISTS_SQLITE),
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    Ok(rows.next().await.map_err(discard_database_error)?.is_some())
}

/// Scan the next batch of candidates under the same statement bound as the
/// per-candidate transactions, so a slow scan is classified as a statement
/// timeout instead of being cancelled from outside.
async fn expired_candidates(
    db: &Database,
    cutoff: &str,
    budget: &AliasGcBudget,
) -> Result<Vec<MessageKey>, AliasGcError> {
    let mut tx = db.begin().await.map_err(gc_error_from_database)?;
    install_gc_timeouts(&mut tx, budget, budget.scan_timeout).await?;
    let mut rows = tx
        .query(
            dialect_sql(tx.driver(), GC_CANDIDATES_POSTGRES, GC_CANDIDATES_SQLITE),
            crate::db_params![cutoff, GC_BATCH_LIMIT as i64],
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

/// Why a candidate was not collected in this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkipReason {
    /// Another writer holds the row; it stays eligible for the next run.
    LockedElsewhere,
    /// The row is gone, no longer terminal, or not yet past retention.
    NotEligible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateLock {
    Eligible,
    Skipped(SkipReason),
}

/// Lock one candidate row and report whether its retention has elapsed.
///
/// A row held by another writer — a concurrent GC run or an in-flight child
/// write — is skipped rather than waited on: the child write keeps the
/// message alive anyway, and a skipped candidate stays eligible for the next
/// run.  Only the epoch row is ever waited on, bounded by `lock_timeout`.
async fn lock_eligible_terminal_message(
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

fn discard_database_error(error: DatabaseError) -> IngressSubstrateError {
    if crate::ingress_uow::is_database_timeout(&error) {
        return IngressSubstrateError::Timeout;
    }
    IngressSubstrateError::Database {
        retry_class: DbRetryClass::from_database_error(&error),
    }
}

fn gc_failure(deleted_messages: usize, error: AliasGcError) -> AliasGcFailure {
    AliasGcFailure {
        deleted_messages,
        error,
    }
}

fn gc_database_failure(deleted_messages: usize, error: DatabaseError) -> AliasGcFailure {
    gc_failure(deleted_messages, gc_error_from_database(error))
}

fn gc_error_from_database(error: DatabaseError) -> AliasGcError {
    match database_timeout_kind(&error) {
        Some(kind) => AliasGcError::DatabaseTimeout { kind },
        None => AliasGcError::Substrate(discard_database_error(error)),
    }
}

fn database_timeout_kind(error: &DatabaseError) -> Option<DatabaseTimeoutKind> {
    let DatabaseError::Internal(sqlx::Error::Database(database_error)) = error else {
        return None;
    };
    match database_error.code().as_deref() {
        Some("55P03") => Some(DatabaseTimeoutKind::Lock),
        Some("57014") => Some(DatabaseTimeoutKind::Statement),
        _ => None,
    }
}

async fn acquire_gc_epoch_lock_first(
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

fn dialect_sql(
    driver: DatabaseDriver,
    postgres: &'static str,
    sqlite: &'static str,
) -> &'static str {
    match driver {
        DatabaseDriver::Postgres => postgres,
        DatabaseDriver::Sqlite => sqlite,
    }
}
const READ_EPOCH_POSTGRES: &str =
    r#"SELECT epoch FROM ingress_protocol_epoch WHERE id = 1 FOR SHARE"#;
const READ_EPOCH_SQLITE: &str = r#"SELECT epoch FROM ingress_protocol_epoch WHERE id = 1 "#;
const INSERT_MESSAGE_POSTGRES: &str = r#"
        INSERT INTO ingress_messages (message_key, digest_version, digest, envelope_version, envelope)
        VALUES (?::uuid, ?, ?, ?, ?)
        ON CONFLICT (message_key) DO NOTHING
        "#;
const INSERT_MESSAGE_SQLITE: &str = r#"
        INSERT INTO ingress_messages (message_key, digest_version, digest, envelope_version, envelope)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT (message_key) DO NOTHING
        "#;
const INSERT_ALIAS_POSTGRES: &str = r#"
            INSERT INTO ingress_origin_aliases
                (alias_key_hash, sender_bare_jid, target_kind, target_jid, origin_id, message_key)
            VALUES (?, ?, ?, ?, ?, ?::uuid)
            ON CONFLICT (alias_key_hash) DO NOTHING
            "#;
const INSERT_ALIAS_SQLITE: &str = r#"
            INSERT INTO ingress_origin_aliases
                (alias_key_hash, sender_bare_jid, target_kind, target_jid, origin_id, message_key)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT (alias_key_hash) DO NOTHING
            "#;
const DELETE_CANDIDATE_POSTGRES: &str =
    r#"DELETE FROM ingress_messages WHERE message_key = ?::uuid"#;
const DELETE_CANDIDATE_SQLITE: &str = r#"DELETE FROM ingress_messages WHERE message_key = ?"#;
const INSERT_SM_REF_POSTGRES: &str = r#"
            INSERT INTO ingress_sm_refs (sm_ingress_id, ingress_ordinal, wire_h, message_key)
            VALUES (?::uuid, ?::numeric, ?, ?::uuid)
            ON CONFLICT (sm_ingress_id, ingress_ordinal) DO NOTHING
            "#;
const INSERT_SM_REF_SQLITE: &str = r#"
            INSERT INTO ingress_sm_refs (sm_ingress_id, ingress_ordinal, wire_h, message_key)
            VALUES (?, ?, ?, ?)
            ON CONFLICT (sm_ingress_id, ingress_ordinal) DO NOTHING
            "#;
const READ_SM_REF_POSTGRES: &str = r#"
        SELECT message_key::text FROM ingress_sm_refs
        WHERE sm_ingress_id = ?::uuid AND ingress_ordinal = ?::numeric
        "#;
const READ_SM_REF_SQLITE: &str = r#"
        SELECT message_key FROM ingress_sm_refs
        WHERE sm_ingress_id = ? AND ingress_ordinal = ?
        "#;
const INSERT_DELIVERY_POSTGRES: &str = r#"
            INSERT INTO ingress_deliveries (delivery_key, message_key)
            VALUES (?::uuid, ?::uuid)
            ON CONFLICT (delivery_key) DO NOTHING
            "#;
const INSERT_DELIVERY_SQLITE: &str = r#"
            INSERT INTO ingress_deliveries (delivery_key, message_key)
            VALUES (?, ?)
            ON CONFLICT (delivery_key) DO NOTHING
            "#;
const READ_DELIVERY_POSTGRES: &str =
    r#"SELECT message_key::text FROM ingress_deliveries WHERE delivery_key = ?::uuid"#;
const READ_DELIVERY_SQLITE: &str =
    r#"SELECT message_key FROM ingress_deliveries WHERE delivery_key = ?"#;
const TERMINALIZE_MESSAGE_POSTGRES: &str = r#"
            UPDATE ingress_messages
            SET terminal_at = ?::timestamptz
            WHERE message_key = ?::uuid AND terminal_at IS NULL
            "#;
const TERMINALIZE_MESSAGE_SQLITE: &str = r#"
            UPDATE ingress_messages
            SET terminal_at = ?
            WHERE message_key = ? AND terminal_at IS NULL
            "#;
const DELETE_ALIASES_POSTGRES: &str =
    r#"DELETE FROM ingress_origin_aliases WHERE message_key = ?::uuid"#;
const DELETE_ALIASES_SQLITE: &str = r#"DELETE FROM ingress_origin_aliases WHERE message_key = ?"#;
const GC_DELETE_MESSAGE_POSTGRES: &str = r#"
                DELETE FROM ingress_messages m
                WHERE m.message_key = ?::uuid
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_origin_aliases a WHERE a.message_key = m.message_key
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = m.message_key
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_deliveries d WHERE d.message_key = m.message_key
                  )
                "#;
const GC_DELETE_MESSAGE_SQLITE: &str = r#"
                DELETE FROM ingress_messages AS m
                WHERE m.message_key = ?
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_origin_aliases a WHERE a.message_key = m.message_key
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = m.message_key
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_deliveries d WHERE d.message_key = m.message_key
                  )
                "#;
const LOCK_ALIAS_POSTGRES: &str = r#"
            SELECT m.message_key::text, m.digest_version, m.digest
            FROM ingress_origin_aliases a
            JOIN ingress_messages m USING (message_key)
            WHERE a.alias_key_hash = ?
            FOR UPDATE OF m
            "#;
const LOCK_ALIAS_SQLITE: &str = r#"
            SELECT m.message_key, m.digest_version, m.digest
            FROM ingress_origin_aliases a
            JOIN ingress_messages m USING (message_key)
            WHERE a.alias_key_hash = ?

            "#;
const LOCK_MESSAGE_POSTGRES: &str =
    r#"SELECT 1 FROM ingress_messages WHERE message_key = ?::uuid FOR UPDATE"#;
const LOCK_MESSAGE_SQLITE: &str = r#"SELECT 1 FROM ingress_messages WHERE message_key = ? "#;
const MESSAGE_EXISTS_POSTGRES: &str =
    r#"SELECT 1 FROM ingress_messages WHERE message_key = ?::uuid"#;
const MESSAGE_EXISTS_SQLITE: &str = r#"SELECT 1 FROM ingress_messages WHERE message_key = ?"#;
const GC_CANDIDATES_POSTGRES: &str = r#"
            SELECT m.message_key::text
            FROM ingress_messages m
            WHERE m.terminal_at IS NOT NULL AND m.terminal_at <= ?::timestamptz
              AND (
                  EXISTS (
                      SELECT 1 FROM ingress_origin_aliases a WHERE a.message_key = m.message_key
                  )
                  OR (
                      NOT EXISTS (
                          SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = m.message_key
                      )
                      AND NOT EXISTS (
                          SELECT 1 FROM ingress_deliveries d WHERE d.message_key = m.message_key
                      )
                  )
              )
            ORDER BY m.terminal_at, m.message_key
            LIMIT ?
            "#;
const GC_CANDIDATES_SQLITE: &str = r#"
            SELECT m.message_key
            FROM ingress_messages m
            WHERE m.terminal_at IS NOT NULL AND m.terminal_at <= ?
              AND (
                  EXISTS (
                      SELECT 1 FROM ingress_origin_aliases a WHERE a.message_key = m.message_key
                  )
                  OR (
                      NOT EXISTS (
                          SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = m.message_key
                      )
                      AND NOT EXISTS (
                          SELECT 1 FROM ingress_deliveries d WHERE d.message_key = m.message_key
                      )
                  )
              )
            ORDER BY m.terminal_at, m.message_key
            LIMIT ?
            "#;
const GC_LOCK_CANDIDATE_POSTGRES: &str = r#"
            SELECT terminal_at <= ?::timestamptz
            FROM ingress_messages
            WHERE message_key = ?::uuid AND terminal_at IS NOT NULL
            FOR UPDATE SKIP LOCKED
            "#;
const GC_LOCK_CANDIDATE_SQLITE: &str = r#"
            SELECT terminal_at <= ?
            FROM ingress_messages
            WHERE message_key = ? AND terminal_at IS NOT NULL

            "#;
const GC_CHECK_CANDIDATE_POSTGRES: &str = r#"
                    SELECT 1
                    FROM ingress_messages
                    WHERE message_key = ?::uuid
                      AND terminal_at IS NOT NULL
                      AND terminal_at <= ?::timestamptz
                    "#;
const GC_CHECK_CANDIDATE_SQLITE: &str = r#"
                    SELECT 1
                    FROM ingress_messages
                    WHERE message_key = ?
                      AND terminal_at IS NOT NULL
                      AND terminal_at <= ?
                    "#;
#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use sqlx::Connection;
    use tokio::sync::Barrier;
    use waddle_xmpp::ingress::{AliasOutcome, AliasResolution};

    use super::*;
    use crate::db::{DatabaseConfig, MigrationRunner};

    fn gc_budget() -> AliasGcBudget {
        AliasGcBudget {
            deadline: Instant::now() + StdDuration::from_secs(30),
            lock_timeout: StdDuration::from_secs(5),
            statement_timeout: StdDuration::from_secs(10),
            scan_timeout: StdDuration::from_secs(10),
            progress: AliasGcProgress::default(),
        }
    }

    #[test]
    fn storage_codecs_round_trip_full_range_values_and_uuid_keys() {
        let ordinal = IngressOrdinal::from_storage(u64::MAX)
            .expect("u64::MAX is a valid persisted ingress ordinal");
        assert_eq!(ordinal.to_storage(), u64::MAX);
        assert!(IngressOrdinal::from_storage(0).is_err());

        let uuid =
            Uuid::parse_str("018e68e7-6a5f-7d4d-a0bc-64dc70a9ce10").expect("fixture UUID is valid");
        assert_eq!(MessageKey::from_storage(uuid).to_storage(), uuid);
        assert_eq!(DeliveryKey::from_storage(uuid).to_storage(), uuid);
        assert_eq!(SmIngressId::from_storage(uuid).to_storage(), uuid);

        for target in [
            NormalizedTarget::Absent,
            NormalizedTarget::Bare(
                "romeo@example.com"
                    .parse()
                    .expect("fixture is a valid bare JID"),
            ),
            NormalizedTarget::Full(
                "romeo@example.com/phone"
                    .parse()
                    .expect("fixture is a valid full JID"),
            ),
        ] {
            let storage = target.to_storage();
            assert_eq!(
                NormalizedTarget::from_storage(storage.kind(), storage.jid()),
                Ok(target),
                "target codec must round trip every variant"
            );
        }
    }

    #[derive(Debug)]
    struct FakePgError(&'static str);

    impl std::fmt::Display for FakePgError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "fake pg error {}", self.0)
        }
    }

    impl std::error::Error for FakePgError {}

    impl sqlx::error::DatabaseError for FakePgError {
        fn message(&self) -> &str {
            "fake pg error"
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.0))
        }

        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }

        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
    }

    fn fake_database_error(code: &'static str) -> DatabaseError {
        DatabaseError::Internal(sqlx::Error::Database(Box::new(FakePgError(code))))
    }

    #[test]
    fn alias_gc_maps_postgres_timeout_sqlstates_to_typed_kinds() {
        assert_eq!(
            database_timeout_kind(&fake_database_error("55P03")),
            Some(DatabaseTimeoutKind::Lock)
        );
        assert_eq!(
            database_timeout_kind(&fake_database_error("57014")),
            Some(DatabaseTimeoutKind::Statement)
        );
        assert_eq!(database_timeout_kind(&fake_database_error("42P01")), None);
    }

    #[tokio::test]
    async fn alias_key_is_unique_and_resolves_existing_or_conflict_from_joined_digest() {
        let Some(fixture) = Fixture::open("alias_key").await else {
            return;
        };
        let sender = sender();
        let target = target();
        let origin = OriginId::new("opaque-client-origin");
        let first_digest = digest(7);
        let first_key = MessageKey::new();

        let mut tx = fixture.store.begin().await.expect("begin alias insert");
        let first = fixture
            .store
            .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &first_digest, || {
                first_key
            })
            .await
            .expect("insert alias");
        tx.commit().await.expect("commit alias insert");
        assert_eq!(first, inserted(first_key));

        let mut tx = fixture.store.begin().await.expect("begin existing read");
        let resolved = fixture
            .store
            .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &first_digest, || {
                MessageKey::new()
            })
            .await
            .expect("resolve existing alias");
        tx.commit().await.expect("commit existing read");
        assert_eq!(resolved, existing(first_key));

        let mut tx = fixture.store.begin().await.expect("begin conflict read");
        let conflict = fixture
            .store
            .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &digest(8), || {
                MessageKey::new()
            })
            .await
            .expect("resolve conflicting alias");
        tx.commit().await.expect("commit conflict read");
        assert!(matches!(
            conflict,
            AliasResolution::Aliased(AliasOutcome::Conflict(ref value))
                if value.existing == first_key && value.stored == first_digest && value.offered == digest(8)
        ));
        assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
        assert_eq!(fixture.count("ingress_messages").await, 1);
        fixture.close().await;
    }

    #[tokio::test]
    async fn concurrent_first_alias_insert_same_digest_leaves_one_message_and_existing_result() {
        let Some(fixture) = Fixture::open("alias_race_same").await else {
            return;
        };
        let sender = sender();
        let target = target();
        let origin = OriginId::new("race-same-digest");
        let start = Arc::new(Barrier::new(2));
        let first = race_alias(
            fixture.store.clone(),
            sender.clone(),
            target.clone(),
            origin.clone(),
            digest(9),
            Arc::clone(&start),
        );
        let second = race_alias(
            fixture.store.clone(),
            sender,
            target,
            origin,
            digest(9),
            start,
        );
        let (first, second) = tokio::join!(first, second);
        let outcomes = [
            first.expect("first race result"),
            second.expect("second race result"),
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    AliasResolution::Aliased(AliasOutcome::Inserted(_))
                ))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    AliasResolution::Aliased(AliasOutcome::Existing(_))
                ))
                .count(),
            1
        );
        assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
        assert_eq!(fixture.count("ingress_messages").await, 1);
        fixture.close().await;
    }

    #[tokio::test]
    async fn concurrent_first_alias_insert_different_digest_leaves_one_message_and_conflict_result()
    {
        let Some(fixture) = Fixture::open("alias_race_conflict").await else {
            return;
        };
        let sender = sender();
        let target = target();
        let origin = OriginId::new("race-different-digest");
        let start = Arc::new(Barrier::new(2));
        let first = race_alias(
            fixture.store.clone(),
            sender.clone(),
            target.clone(),
            origin.clone(),
            digest(10),
            Arc::clone(&start),
        );
        let second = race_alias(
            fixture.store.clone(),
            sender,
            target,
            origin,
            digest(11),
            start,
        );
        let (first, second) = tokio::join!(first, second);
        let outcomes = [
            first.expect("first race result"),
            second.expect("second race result"),
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    AliasResolution::Aliased(AliasOutcome::Inserted(_))
                ))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    AliasResolution::Aliased(AliasOutcome::Conflict(_))
                ))
                .count(),
            1
        );
        assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
        assert_eq!(fixture.count("ingress_messages").await, 1);
        fixture.close().await;
    }

    #[tokio::test]
    async fn terminalize_keeps_the_first_proven_terminal_time() {
        let Some(fixture) = Fixture::open("terminalize").await else {
            return;
        };
        let key = fixture.record_message().await;
        let first_time = timestamp(1);
        let second_time = timestamp(2);
        let mut tx = fixture
            .store
            .begin()
            .await
            .expect("begin first terminalize");
        assert_eq!(
            fixture
                .store
                .terminalize_message(&mut tx, key, first_time)
                .await
                .expect("first terminalize"),
            TerminalizeOutcome::Terminalized
        );
        tx.commit().await.expect("commit first terminalize");
        let mut tx = fixture
            .store
            .begin()
            .await
            .expect("begin repeated terminalize");
        assert_eq!(
            fixture
                .store
                .terminalize_message(&mut tx, key, second_time)
                .await
                .expect("repeat terminalize"),
            TerminalizeOutcome::AlreadyTerminal
        );
        tx.commit().await.expect("commit repeated terminalize");
        assert!(fixture.terminal_is(key, first_time).await);
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_deletes_aliasless_terminal_messages_and_makes_child_writes_vanish() {
        let Some(fixture) = Fixture::open("gc_aliasless").await else {
            return;
        };
        let key = fixture.record_message().await;
        let terminal_at = timestamp(3);
        fixture.terminalize(key, terminal_at).await;
        let result = fixture
            .store
            .gc_expired_aliases(terminal_at + ALIAS_RETENTION, gc_budget())
            .await
            .expect("garbage collect terminal message");
        assert_eq!(result.deleted_messages, 1);
        let mut tx = fixture
            .store
            .begin()
            .await
            .expect("begin missing child insert");
        assert_eq!(
            fixture
                .store
                .insert_sm_ref(
                    &mut tx,
                    SmIngressId::new(),
                    IngressOrdinal::FIRST,
                    WireHandledCount::from_storage(1),
                    key
                )
                .await
                .expect("record vanished child"),
            MessageWriteOutcome::MessageVanished
        );
        tx.commit().await.expect("commit vanished child result");
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_respects_the_exact_terminal_retention_cutoff() {
        let Some(fixture) = Fixture::open("gc_cutoff").await else {
            return;
        };
        let key = fixture.record_message().await;
        let terminal_at = timestamp(4);
        fixture.terminalize(key, terminal_at).await;
        assert_eq!(
            fixture
                .store
                .gc_expired_aliases(
                    terminal_at + ALIAS_RETENTION - Duration::microseconds(1),
                    gc_budget(),
                )
                .await
                .expect("collect before exact cutoff")
                .deleted_messages,
            0
        );
        assert_eq!(
            fixture
                .store
                .gc_expired_aliases(terminal_at + ALIAS_RETENTION, gc_budget())
                .await
                .expect("collect at exact cutoff")
                .deleted_messages,
            1
        );
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_budget_stops_without_work_then_preserves_partial_progress() {
        let Some(fixture) = Fixture::open("gc_budget_progress").await else {
            return;
        };
        let terminal_at = timestamp(10);
        let total = GC_BATCH_LIMIT + 2;
        fixture.record_terminal_messages(total, terminal_at).await;
        let now = terminal_at + ALIAS_RETENTION + Duration::days(1);

        let no_work = fixture
            .store
            .gc_expired_aliases(
                now,
                AliasGcBudget {
                    deadline: tokio::time::Instant::now() - StdDuration::from_millis(1),
                    ..gc_budget()
                },
            )
            .await
            .expect("an expired budget is a cooperative stop");
        assert_eq!(
            no_work,
            AliasGcOutcome {
                deleted_messages: 0,
                completed: false,
            }
        );

        fixture
            .db
            .execute(
                "CREATE FUNCTION waddle_test_slow_gc_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.05); RETURN OLD; END $$",
            )
            .await
            .expect("create deterministic GC pacing function");
        fixture
            .db
            .execute(
                "CREATE TRIGGER waddle_test_slow_gc_delete BEFORE DELETE ON ingress_messages FOR EACH ROW EXECUTE FUNCTION waddle_test_slow_gc_delete()",
            )
            .await
            .expect("install deterministic GC pacing trigger");
        let partial = fixture
            .store
            .gc_expired_aliases(
                now,
                AliasGcBudget {
                    deadline: tokio::time::Instant::now() + StdDuration::from_secs(1),
                    ..gc_budget()
                },
            )
            .await
            .expect("budgeted GC returns committed progress");
        assert!(!partial.completed);
        assert!(partial.deleted_messages > 0);
        assert!(partial.deleted_messages < total);
        let remaining = usize::try_from(fixture.count("ingress_messages").await)
            .expect("remaining count fits usize");
        assert_eq!(partial.deleted_messages + remaining, total);

        fixture
            .db
            .execute("DROP TRIGGER waddle_test_slow_gc_delete ON ingress_messages")
            .await
            .expect("remove deterministic GC pacing trigger");
        fixture
            .db
            .execute("DROP FUNCTION waddle_test_slow_gc_delete()")
            .await
            .expect("remove deterministic GC pacing function");
        let completed = fixture
            .store
            .gc_expired_aliases(now, gc_budget())
            .await
            .expect("finish remaining GC backlog");
        assert!(completed.completed);
        assert_eq!(completed.deleted_messages, remaining);
        assert_eq!(partial.deleted_messages + completed.deleted_messages, total);
        assert_eq!(fixture.count("ingress_messages").await, 0);
        assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_exact_full_batch_completes_after_empty_rescan() {
        let Some(fixture) = Fixture::open("gc_exact_batch").await else {
            return;
        };
        let terminal_at = timestamp(11);
        fixture
            .record_terminal_messages(GC_BATCH_LIMIT, terminal_at)
            .await;
        let outcome = fixture
            .store
            .gc_expired_aliases(
                terminal_at + ALIAS_RETENTION + Duration::days(1),
                gc_budget(),
            )
            .await
            .expect("collect exact full batch and confirm empty rescan");
        assert_eq!(
            outcome,
            AliasGcOutcome {
                deleted_messages: GC_BATCH_LIMIT,
                completed: true,
            }
        );
        assert_eq!(fixture.count("ingress_messages").await, 0);
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_skips_a_candidate_locked_by_another_writer_and_keeps_going() {
        let Some(fixture) = Fixture::open("gc_lock_progress").await else {
            return;
        };
        let terminal_at = timestamp(12);
        let keys = fixture.record_terminal_messages(2, terminal_at).await;
        let mut blocker = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open candidate blocker");
        let mut blocker_tx = blocker.begin().await.expect("begin candidate blocker");
        sqlx::query("SELECT 1 FROM ingress_messages WHERE message_key = $1 FOR UPDATE")
            .bind(keys[1].to_storage())
            .execute(&mut *blocker_tx)
            .await
            .expect("lock second GC candidate");

        let outcome = fixture
            .store
            .gc_expired_aliases(
                terminal_at + ALIAS_RETENTION + Duration::days(1),
                AliasGcBudget {
                    lock_timeout: StdDuration::from_millis(100),
                    ..gc_budget()
                },
            )
            .await
            .expect("a locked candidate is skipped, not waited on");
        assert_eq!(
            outcome,
            AliasGcOutcome {
                deleted_messages: 1,
                completed: false,
            },
            "a skipped candidate is still eligible, so the run is not complete"
        );
        assert_eq!(fixture.count("ingress_messages").await, 1);
        blocker_tx.rollback().await.expect("release candidate lock");
        drop(blocker);

        let outcome = fixture
            .store
            .gc_expired_aliases(
                terminal_at + ALIAS_RETENTION + Duration::days(1),
                gc_budget(),
            )
            .await
            .expect("the skipped candidate is reclaimed by the next run");
        assert_eq!(
            outcome,
            AliasGcOutcome {
                deleted_messages: 1,
                completed: true,
            }
        );
        assert_eq!(fixture.count("ingress_messages").await, 0);
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_keeps_draining_later_batches_after_skipping_a_locked_candidate() {
        let Some(fixture) = Fixture::open("gc_skip_then_drain").await else {
            return;
        };
        let terminal_at = timestamp(17);
        let total = GC_BATCH_LIMIT + 8;
        let keys = fixture.record_terminal_messages(total, terminal_at).await;
        let mut blocker = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open candidate blocker");
        let mut blocker_tx = blocker.begin().await.expect("begin candidate blocker");
        sqlx::query("SELECT 1 FROM ingress_messages WHERE message_key = $1 FOR UPDATE")
            .bind(keys[0].to_storage())
            .execute(&mut *blocker_tx)
            .await
            .expect("lock the oldest GC candidate");

        let outcome = fixture
            .store
            .gc_expired_aliases(
                terminal_at + ALIAS_RETENTION + Duration::days(1),
                gc_budget(),
            )
            .await
            .expect("a locked head candidate must not stop the run");
        assert_eq!(
            outcome,
            AliasGcOutcome {
                deleted_messages: total - 1,
                completed: false,
            },
            "every unlocked row across both batches is reclaimed; the locked one keeps the run partial"
        );
        assert_eq!(fixture.count("ingress_messages").await, 1);
        blocker_tx.rollback().await.expect("release candidate lock");
        drop(blocker);
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_epoch_lock_timeout_has_zero_progress() {
        let Some(fixture) = Fixture::open("gc_epoch_lock_timeout").await else {
            return;
        };
        let terminal_at = timestamp(13);
        fixture.record_terminal_messages(1, terminal_at).await;
        let mut blocker = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open epoch blocker");
        let mut blocker_tx = blocker.begin().await.expect("begin epoch blocker");
        sqlx::query("SELECT 1 FROM ingress_protocol_epoch WHERE id = 1 FOR UPDATE")
            .execute(&mut *blocker_tx)
            .await
            .expect("lock protocol epoch row");

        // Production-relative bounds: the statement timer covers the lock
        // wait, so the lock bound must be the one that fires first.
        let failure = fixture
            .store
            .gc_expired_aliases(
                terminal_at + ALIAS_RETENTION + Duration::days(1),
                AliasGcBudget {
                    lock_timeout: StdDuration::from_millis(100),
                    statement_timeout: StdDuration::from_millis(250),
                    ..gc_budget()
                },
            )
            .await
            .expect_err("epoch lock must time out");
        assert_eq!(failure.deleted_messages, 0);
        assert!(matches!(
            failure.error,
            AliasGcError::DatabaseTimeout {
                kind: DatabaseTimeoutKind::Lock
            }
        ));
        assert_eq!(fixture.count("ingress_messages").await, 1);
        blocker_tx.rollback().await.expect("release epoch lock");
        drop(blocker);
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_non_timeout_failure_after_progress_preserves_committed_count() {
        let Some(fixture) = Fixture::open("gc_failure_progress").await else {
            return;
        };
        let terminal_at = timestamp(14);
        let keys = fixture.record_terminal_messages(2, terminal_at).await;
        fixture
            .db
            .execute(&format!(
                "CREATE FUNCTION waddle_test_fail_gc_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF OLD.message_key = '{}'::uuid THEN RAISE EXCEPTION 'forced GC failure' USING ERRCODE = 'P0001'; END IF; RETURN OLD; END $$",
                keys[1].to_storage()
            ))
            .await
            .expect("create deterministic GC failure function");
        fixture
            .db
            .execute(
                "CREATE TRIGGER waddle_test_fail_gc_delete AFTER DELETE ON ingress_messages FOR EACH ROW EXECUTE FUNCTION waddle_test_fail_gc_delete()",
            )
            .await
            .expect("install deterministic GC failure trigger");

        let failure = fixture
            .store
            .gc_expired_aliases(
                terminal_at + ALIAS_RETENTION + Duration::days(1),
                gc_budget(),
            )
            .await
            .expect_err("second candidate trigger must fail");
        assert_eq!(failure.deleted_messages, 1);
        assert!(matches!(failure.error, AliasGcError::Substrate(_)));
        assert_eq!(fixture.count("ingress_messages").await, 1);
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_progress_handle_reports_committed_deletions_after_external_cancellation() {
        let Some(fixture) = Fixture::open("gc_progress_cancel").await else {
            return;
        };
        let terminal_at = timestamp(16);
        let total = 8usize;
        fixture.record_terminal_messages(total, terminal_at).await;
        fixture
            .db
            .execute(
                "CREATE FUNCTION waddle_test_slow_gc_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.05); RETURN OLD; END $$",
            )
            .await
            .expect("create deterministic GC pacing function");
        fixture
            .db
            .execute(
                "CREATE TRIGGER waddle_test_slow_gc_delete BEFORE DELETE ON ingress_messages FOR EACH ROW EXECUTE FUNCTION waddle_test_slow_gc_delete()",
            )
            .await
            .expect("install deterministic GC pacing trigger");
        let progress = AliasGcProgress::default();
        let cancelled = tokio::time::timeout(
            StdDuration::from_millis(150),
            fixture.store.gc_expired_aliases(
                terminal_at + ALIAS_RETENTION + Duration::days(1),
                AliasGcBudget {
                    progress: progress.clone(),
                    ..gc_budget()
                },
            ),
        )
        .await;
        assert!(
            cancelled.is_err(),
            "the paced run must be cancelled mid-way"
        );
        let remaining = usize::try_from(fixture.count("ingress_messages").await)
            .expect("remaining count fits usize");
        assert!(remaining > 0 && remaining < total);
        assert_eq!(
            progress.committed(),
            total - remaining,
            "the progress handle must match the rows actually committed"
        );
        fixture.close().await;
    }

    #[tokio::test]
    async fn concurrent_gc_runs_never_double_count_deletions() {
        let Some(fixture) = Fixture::open("gc_concurrent").await else {
            return;
        };
        let terminal_at = timestamp(15);
        let total = 32usize;
        fixture.record_terminal_messages(total, terminal_at).await;
        let barrier = Arc::new(Barrier::new(3));
        let now = terminal_at + ALIAS_RETENTION + Duration::days(1);
        let spawn_gc = |store: PostgresIngressSubstrate, barrier: Arc<Barrier>| {
            tokio::spawn(async move {
                barrier.wait().await;
                store.gc_expired_aliases(now, gc_budget()).await
            })
        };
        let first = spawn_gc(fixture.store.clone(), barrier.clone());
        let second = spawn_gc(fixture.store.clone(), barrier.clone());
        barrier.wait().await;
        let results = [
            first.await.expect("join first GC"),
            second.await.expect("join second GC"),
        ];
        let mut deleted = 0usize;
        for result in results {
            let outcome = result.expect("concurrent GC runs skip each other's locked rows");
            deleted += outcome.deleted_messages;
        }
        assert_eq!(deleted, total);
        assert_eq!(fixture.count("ingress_messages").await, 0);
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_expires_aliases_but_preserves_messages_with_live_sm_refs_or_deliveries() {
        let Some(fixture) = Fixture::open("gc_live_children").await else {
            return;
        };
        // Create the message through the alias path so the retention pass
        // has an expired alias to remove.
        let key = MessageKey::new();
        let mut tx = fixture.store.begin().await.expect("begin alias insert");
        fixture
            .store
            .resolve_and_record_alias(
                &mut tx,
                &sender(),
                &target(),
                &OriginId::new("expired-with-live-children"),
                &digest(5),
                || key,
            )
            .await
            .expect("insert alias");
        tx.commit().await.expect("commit alias insert");

        let terminal_at = timestamp(5);
        let mut tx = fixture.store.begin().await.expect("begin child writes");
        assert_eq!(
            fixture
                .store
                .insert_sm_ref(
                    &mut tx,
                    SmIngressId::new(),
                    IngressOrdinal::FIRST,
                    WireHandledCount::from_storage(1),
                    key
                )
                .await
                .expect("insert sm ref"),
            MessageWriteOutcome::Recorded
        );
        assert_eq!(
            fixture
                .store
                .record_delivery(&mut tx, DeliveryKey::new(), key)
                .await
                .expect("record delivery"),
            MessageWriteOutcome::Recorded
        );
        tx.commit().await.expect("commit child writes");
        fixture.terminalize(key, terminal_at).await;
        assert_eq!(
            fixture
                .store
                .gc_expired_aliases(terminal_at + ALIAS_RETENTION, gc_budget())
                .await
                .expect("collect terminal message with children")
                .deleted_messages,
            0
        );
        // The expired alias is gone even though live children keep the
        // message row itself alive.
        assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
        assert_eq!(fixture.count("ingress_messages").await, 1);
        assert_eq!(fixture.count("ingress_sm_refs").await, 1);
        assert_eq!(fixture.count("ingress_deliveries").await, 1);
        // Alias-less retained rows drop out of the candidate scan: a second
        // pass finds no work and touches nothing.
        assert_eq!(
            fixture
                .store
                .gc_expired_aliases(terminal_at + ALIAS_RETENTION, gc_budget())
                .await
                .expect("re-run GC over retained history")
                .deleted_messages,
            0
        );
        assert_eq!(fixture.count("ingress_messages").await, 1);
        fixture.close().await;
    }

    #[tokio::test]
    async fn sm_refs_are_keyed_by_sm_ingress_identity_not_transport_stream() {
        let Some(fixture) = Fixture::open("sm_ref_identity").await else {
            return;
        };
        let key = fixture.record_message().await;
        let sm_id = SmIngressId::new();
        let mut tx = fixture.store.begin().await.expect("begin first ref");
        assert_eq!(
            fixture
                .store
                .insert_sm_ref(
                    &mut tx,
                    sm_id,
                    IngressOrdinal::FIRST,
                    WireHandledCount::from_storage(1),
                    key
                )
                .await
                .expect("record the first handled-stanza reference"),
            MessageWriteOutcome::Recorded
        );
        tx.commit().await.expect("commit first ref");

        // An XEP-0198 resume continues the SAME SM ingress identity on a new
        // transport stream: replaying the identical durable reference is an
        // idempotent AlreadyRecorded, so an ambiguous-commit retry can finish
        // its unit of work.
        let mut tx = fixture.store.begin().await.expect("begin replayed ref");
        assert_eq!(
            fixture
                .store
                .insert_sm_ref(
                    &mut tx,
                    sm_id,
                    IngressOrdinal::FIRST,
                    WireHandledCount::from_storage(1),
                    key
                )
                .await
                .expect("identical replay is idempotent"),
            MessageWriteOutcome::AlreadyRecorded
        );
        tx.commit().await.expect("commit replayed ref");

        // The same ordinal bound to a DIFFERENT message is a typed identity
        // conflict, distinguishable from outages and from AlreadyRecorded.
        let other = fixture.record_message().await;
        let mut tx = fixture.store.begin().await.expect("begin conflicting ref");
        assert!(
            matches!(
                fixture
                    .store
                    .insert_sm_ref(
                        &mut tx,
                        sm_id,
                        IngressOrdinal::FIRST,
                        WireHandledCount::from_storage(1),
                        other
                    )
                    .await,
                Err(IngressSubstrateError::SmOrdinalConflict)
            ),
            "a different message under the same (SmIngressId, ordinal) must conflict"
        );
        drop(tx);
        assert_eq!(fixture.count("ingress_sm_refs").await, 1);

        // record_delivery mirrors the same idempotency contract.
        let delivery = DeliveryKey::new();
        let mut tx = fixture.store.begin().await.expect("begin delivery");
        assert_eq!(
            fixture
                .store
                .record_delivery(&mut tx, delivery, key)
                .await
                .expect("record delivery"),
            MessageWriteOutcome::Recorded
        );
        assert_eq!(
            fixture
                .store
                .record_delivery(&mut tx, delivery, key)
                .await
                .expect("identical delivery replay is idempotent"),
            MessageWriteOutcome::AlreadyRecorded
        );
        assert!(matches!(
            fixture
                .store
                .record_delivery(&mut tx, delivery, other)
                .await,
            Err(IngressSubstrateError::DeliveryKeyConflict)
        ));
        drop(tx);
        fixture.close().await;
    }

    #[tokio::test]
    async fn epoch_advances_at_most_once_per_transaction_and_future_epochs_fence_gc() {
        let Some(fixture) = Fixture::open("epoch_single_advance").await else {
            return;
        };
        let key = fixture.record_message().await;
        let terminal_at = timestamp(8);
        fixture.terminalize(key, terminal_at).await;

        // A duplicated activation statement inside ONE transaction must not
        // commit a two-epoch jump no deployment ever observed.
        let mut conn = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open activation connection");
        let mut tx = conn.begin().await.expect("begin double-advance tx");
        sqlx::query(
            "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
             lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
        )
        .execute(&mut *tx)
        .await
        .expect("first advance in the transaction");
        assert!(
            sqlx::query(
                "UPDATE ingress_protocol_epoch SET epoch = 2, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
            )
            .execute(&mut *tx)
            .await
            .is_err(),
            "a second epoch advance in the same transaction must be rejected"
        );
        drop(tx);

        // Advance to epoch 2 through two sanctioned transactions; the live
        // epoch now exceeds what this binary supports, so its GC fails
        // closed instead of auto-claiming the future epoch.
        for statement in [
            "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
             lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
            "UPDATE ingress_protocol_epoch SET epoch = 2, activated_at = now(), \
             lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
        ] {
            sqlx::query(statement)
                .execute(&mut conn)
                .await
                .expect("sanctioned single-step advance");
        }
        assert!(matches!(
            fixture
                .store
                .gc_expired_aliases(terminal_at + ALIAS_RETENTION, gc_budget())
                .await,
            Err(AliasGcFailure {
                deleted_messages: 0,
                error: AliasGcError::Substrate(IngressSubstrateError::UnsupportedLiveEpoch),
            })
        ));
        drop(conn);
        fixture.close().await;
    }

    #[tokio::test]
    async fn alias_insert_succeeds_at_combined_maximum_key_lengths() {
        let Some(fixture) = Fixture::open("alias_max_len").await else {
            return;
        };
        // Maximum-length localparts/domains (1023 bytes each per RFC 7622)
        // with a maximum-length origin-id: the fixed-width hash key must
        // absorb what a composite B-tree tuple could not.
        let long_domain = |label: &str| {
            std::iter::repeat_n(label.repeat(62), 4)
                .collect::<Vec<_>>()
                .join(".")
        };
        let long_sender: BareJid = format!("{}@{}", "a".repeat(1023), long_domain("b"))
            .parse()
            .expect("long sender JID is valid");
        let long_target: BareJid = format!("{}@{}", "c".repeat(1023), long_domain("d"))
            .parse()
            .expect("long target JID is valid");
        let origin = OriginId::new("o".repeat(1024));
        let key = MessageKey::new();
        let mut tx = fixture.store.begin().await.expect("begin max-length alias");
        assert_eq!(
            fixture
                .store
                .resolve_and_record_alias(
                    &mut tx,
                    &long_sender,
                    &NormalizedTarget::Bare(long_target),
                    &origin,
                    &digest(6),
                    || key,
                )
                .await
                .expect("maximum-length alias key inserts"),
            inserted(key)
        );
        tx.commit().await.expect("commit max-length alias");
        fixture.close().await;
    }

    #[tokio::test]
    async fn epoch_one_rejects_unproven_writes_and_accepts_transaction_bound_proof() {
        let Some(fixture) = Fixture::open("epoch_proof").await else {
            return;
        };
        let key = fixture.record_message().await;
        fixture
            .db
            .execute(
                "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
            )
            .await
            .expect("activate epoch one");

        for statement in [
            format!(
                "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
                Uuid::new_v4(),
                key.to_storage()
            ),
            format!(
                "UPDATE ingress_messages SET terminal_at = now() WHERE message_key = '{}'",
                key.to_storage()
            ),
            format!(
                "DELETE FROM ingress_messages WHERE message_key = '{}'",
                key.to_storage()
            ),
            "TRUNCATE ingress_deliveries".to_string(),
        ] {
            assert!(
                fixture.db.execute(&statement).await.is_err(),
                "epoch-one protected operation must require proof: {statement}"
            );
        }

        let mut tx = fixture
            .store
            .begin()
            .await
            .expect("begin proof transaction");
        tx.execute("SET LOCAL waddle.protocol_epoch = '1'", ())
            .await
            .expect("set epoch proof");
        tx.execute(
            "SELECT set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)",
            (),
        )
        .await
        .expect("set xid proof");
        fixture
            .store
            .record_delivery(&mut tx, DeliveryKey::new(), key)
            .await
            .expect("proof authorizes protected write");
        tx.commit().await.expect("commit proof transaction");

        // A proof with a wrong epoch or no xid is intentionally incomplete.
        let mut wrong_epoch = fixture
            .store
            .begin()
            .await
            .expect("begin wrong epoch proof");
        wrong_epoch
            .execute("SET LOCAL waddle.protocol_epoch = '0'", ())
            .await
            .expect("set wrong epoch proof");
        assert!(fixture
            .store
            .record_delivery(&mut wrong_epoch, DeliveryKey::new(), key)
            .await
            .is_err());
        drop(wrong_epoch); // dropping an uncommitted transaction rolls it back
        fixture.close().await;
    }

    #[tokio::test]
    async fn epoch_and_manifest_tables_enforce_their_singleton_and_append_only_rules() {
        let Some(fixture) = Fixture::open("epoch_invariants").await else {
            return;
        };
        for statement in [
            "UPDATE ingress_protocol_epoch SET epoch = 2, activated_at = now(), lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
            "UPDATE ingress_protocol_epoch SET epoch = 1 WHERE id = 1",
            "DELETE FROM ingress_protocol_epoch WHERE id = 1",
            "TRUNCATE ingress_protocol_epoch",
            "INSERT INTO ingress_protocol_epoch (id, epoch) VALUES (2, 0)",
            "UPDATE ingress_epoch_guard_manifest SET table_name = 'bad' WHERE table_name = 'ingress_messages'",
            "DELETE FROM ingress_epoch_guard_manifest WHERE table_name = 'ingress_messages'",
            "TRUNCATE ingress_epoch_guard_manifest",
            // TRUNCATE on protected tables is rejected at EVERY epoch by the
            // lock-free truncate guard (its trigger must never request the
            // epoch row: TRUNCATE's ACCESS EXCLUSIVE is taken first, which
            // would invert the epoch-first lock order).
            "TRUNCATE ingress_messages",
            "TRUNCATE ingress_sm_refs",
        ] {
            assert!(fixture.db.execute(statement).await.is_err(), "must reject: {statement}");
        }
        let mut tx = fixture.store.begin().await.expect("begin manifest probe");
        tx.execute(
            "INSERT INTO ingress_epoch_guard_manifest (table_name) VALUES ('ingress_future_probe')",
            (),
        )
        .await
        .expect("manifest permits future enrollment");
        drop(tx); // probe must leave the append-only manifest unchanged
        fixture.close().await;
    }

    #[tokio::test]
    async fn epoch_guard_manifest_matches_rust_and_live_trigger_catalog() {
        let Some(fixture) = Fixture::open("guard_manifest").await else {
            return;
        };
        let conn = fixture.db.guard().await.expect("guard manifest database");
        let mut rows = conn
            .query(
                "SELECT table_name FROM ingress_epoch_guard_manifest ORDER BY table_name",
                (),
            )
            .await
            .expect("read guard manifest");
        let mut manifest = Vec::new();
        while let Some(row) = rows.next().await.expect("read manifest row") {
            manifest.push(row.get::<String>(0).expect("decode manifest table"));
        }
        let mut expected = EPOCH_GUARDED_TABLES.map(str::to_owned).to_vec();
        expected.sort();
        assert_eq!(manifest, expected, "migration manifest and Rust list agree");

        // Every live ingress_* table must be enrolled: an unlisted table
        // would silently sit outside the activation boundary.
        let mut live_rows = conn
            .query(
                "SELECT c.relname FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = current_schema() AND c.relkind = 'r' \
                   AND c.relname LIKE 'ingress_%' ORDER BY c.relname",
                (),
            )
            .await
            .expect("enumerate live ingress tables");
        let mut live = Vec::new();
        while let Some(row) = live_rows.next().await.expect("read live table row") {
            live.push(row.get::<String>(0).expect("decode live table name"));
        }
        let mut expected_live = expected.clone();
        expected_live.push("ingress_epoch_guard_manifest".to_string());
        expected_live.push("ingress_protocol_epoch".to_string());
        expected_live.sort();
        assert_eq!(
            live, expected_live,
            "every live ingress_* table is either guarded or the epoch/manifest table"
        );

        // Trigger shape is pinned, not just presence: BEFORE statement-level
        // I/U/D (tgtype 30) plus BEFORE statement-level TRUNCATE (tgtype 34),
        // both ENABLE ALWAYS and bound to the guard function.
        for table in EPOCH_GUARDED_TABLES {
            let mut trigger_rows = conn
                .query(
                    "SELECT tg.tgname, tg.tgtype::int, tg.tgenabled::text, p.proname \
                     FROM pg_trigger tg \
                     JOIN pg_class c ON c.oid = tg.tgrelid \
                     JOIN pg_namespace n ON n.oid = c.relnamespace \
                     JOIN pg_proc p ON p.oid = tg.tgfoid \
                     WHERE n.nspname = current_schema() AND c.relname = ? \
                       AND NOT tg.tgisinternal ORDER BY tg.tgname",
                    crate::db_params![table],
                )
                .await
                .expect("read guard triggers");
            let mut triggers = Vec::new();
            while let Some(row) = trigger_rows.next().await.expect("read trigger row") {
                triggers.push((
                    row.get::<String>(0).expect("decode trigger name"),
                    row.get::<i64>(1).expect("decode trigger type"),
                    row.get::<String>(2).expect("decode trigger mode"),
                    row.get::<String>(3).expect("decode trigger function"),
                ));
            }
            assert!(
                triggers.contains(&(
                    format!("{table}_epoch_guard_dml"),
                    30,
                    "A".to_string(),
                    "waddle_ingress_epoch_guard".to_string(),
                )),
                "{table} must carry the BEFORE-statement I/U/D guard: {triggers:?}"
            );
            assert!(
                triggers.contains(&(
                    format!("{table}_epoch_guard_truncate"),
                    34,
                    "A".to_string(),
                    "waddle_ingress_truncate_guard".to_string(),
                )),
                "{table} must carry the lock-free BEFORE-statement TRUNCATE reject: {triggers:?}"
            );
        }
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_installs_its_own_epoch_proof_and_collects_at_epoch_one() {
        let Some(fixture) = Fixture::open("gc_epoch_one").await else {
            return;
        };
        let key = fixture.record_message().await;
        let terminal_at = timestamp(7);
        fixture.terminalize(key, terminal_at).await;
        fixture
            .db
            .execute(
                "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
            )
            .await
            .expect("activate epoch one");
        let outcome = fixture
            .store
            .gc_expired_aliases(terminal_at + ALIAS_RETENTION, gc_budget())
            .await
            .expect("GC proves its own transactions at epoch one");
        assert_eq!(
            outcome.deleted_messages, 1,
            "GC collects normally after activation"
        );
        fixture.close().await;
    }

    #[tokio::test]
    async fn guard_uses_its_table_schema_not_the_callers_search_path() {
        let Some(fixture) = Fixture::open("hostile_search_path").await else {
            return;
        };
        let key = fixture.record_message().await;
        // Independent short name: suffixing the fixture schema would exceed
        // PostgreSQL's 63-byte identifier cap and silently truncate into a
        // collision with the fixture schema itself.
        let hostile = format!("waddle_test_hostile_{}", Uuid::new_v4().simple());
        let mut conn = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open one hostile-search-path connection");
        sqlx::query(&format!("CREATE SCHEMA {hostile}"))
            .execute(&mut conn)
            .await
            .expect("create hostile schema");
        sqlx::query(&format!(
            "CREATE TABLE {hostile}.ingress_protocol_epoch (id INTEGER PRIMARY KEY, epoch BIGINT NOT NULL)"
        ))
        .execute(&mut conn)
        .await
        .expect("create hostile epoch shadow");
        sqlx::query(&format!(
            "INSERT INTO {hostile}.ingress_protocol_epoch (id, epoch) VALUES (1, 1)"
        ))
        .execute(&mut conn)
        .await
        .expect("seed hostile epoch shadow");
        sqlx::query(&format!("SET search_path = {hostile}, {}", fixture.schema))
            .execute(&mut conn)
            .await
            .expect("place hostile schema first");
        sqlx::query(&format!(
            "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
            Uuid::new_v4(),
            key.to_storage()
        ))
        .execute(&mut conn)
        .await
        .expect("epoch-zero guard must ignore hostile epoch-one shadow");
        drop(conn);
        fixture.close().await;
    }

    /// Poll `pg_stat_activity` until a backend is blocked on a heavyweight
    /// lock while running a query containing `fragment`.  The fragment is a
    /// bound parameter, so this poll's own query text never matches itself.
    async fn wait_for_lock_waiter(admin: &sqlx::PgPool, fragment: &str) {
        for _ in 0..400 {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM pg_stat_activity \
                 WHERE wait_event_type = 'Lock' AND query LIKE $1",
            )
            .bind(format!("%{fragment}%"))
            .fetch_one(admin)
            .await
            .expect("poll pg_stat_activity for a lock waiter");
            if waiting > 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("no blocked backend appeared for query fragment {fragment:?}");
    }

    #[tokio::test]
    async fn session_level_guc_and_stale_xid_proofs_do_not_authorize_writes() {
        let Some(fixture) = Fixture::open("epoch_proof_matrix").await else {
            return;
        };
        let key = fixture.record_message().await;
        fixture
            .db
            .execute(
                "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
            )
            .await
            .expect("activate epoch one");

        let mut conn = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open one dedicated connection");
        let insert = format!(
            "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
            Uuid::new_v4(),
            key.to_storage()
        );

        // Session-level SET of BOTH GUCs: the xid captured now belongs to
        // this transaction, so it cannot prove any later transaction.
        sqlx::query("SET waddle.protocol_epoch = '1'")
            .execute(&mut conn)
            .await
            .expect("session-level epoch setting");
        let stale_xid: String = sqlx::query_scalar(
            "SELECT set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, false)",
        )
        .fetch_one(&mut conn)
        .await
        .expect("session-level xid setting");
        assert!(
            sqlx::query(&insert).execute(&mut conn).await.is_err(),
            "a session-retained proof must not authorize a later transaction"
        );

        // Missing xid half of the proof.
        let mut tx = conn.begin().await.expect("begin missing-xid transaction");
        sqlx::query("SET LOCAL waddle.protocol_epoch = '1'")
            .execute(&mut *tx)
            .await
            .expect("set epoch half only");
        assert!(
            sqlx::query(&insert).execute(&mut *tx).await.is_err(),
            "the epoch GUC alone must not authorize a write"
        );
        drop(tx);

        // Stale xid replayed as a literal from an earlier transaction.
        let mut tx = conn.begin().await.expect("begin stale-xid transaction");
        sqlx::query("SET LOCAL waddle.protocol_epoch = '1'")
            .execute(&mut *tx)
            .await
            .expect("set epoch for stale proof");
        sqlx::query("SELECT set_config('waddle.protocol_epoch_xid', $1, true)")
            .bind(&stale_xid)
            .execute(&mut *tx)
            .await
            .expect("replay stale xid literal");
        assert!(
            sqlx::query(&insert).execute(&mut *tx).await.is_err(),
            "a stale xid proof must not authorize a write"
        );
        drop(tx);

        // A correct transaction-local proof works — and does not survive
        // into the next transaction on the SAME pooled connection.
        let mut tx = conn.begin().await.expect("begin proven transaction");
        sqlx::query("SET LOCAL waddle.protocol_epoch = '1'")
            .execute(&mut *tx)
            .await
            .expect("set local epoch");
        sqlx::query(
            "SELECT set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)",
        )
        .execute(&mut *tx)
        .await
        .expect("set local xid");
        sqlx::query(&insert)
            .execute(&mut *tx)
            .await
            .expect("transaction-bound proof authorizes the write");
        tx.commit().await.expect("commit proven transaction");
        assert!(
            sqlx::query(&format!(
                "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
                Uuid::new_v4(),
                key.to_storage()
            ))
            .execute(&mut conn)
            .await
            .is_err(),
            "SET LOCAL must not be retained past commit on the same connection"
        );
        drop(conn);
        fixture.close().await;
    }

    #[tokio::test]
    async fn epoch_activation_waits_behind_in_flight_epoch_zero_writes() {
        let Some(fixture) = Fixture::open("activation_race").await else {
            return;
        };
        let key = fixture.record_message().await;

        // Transaction A: an epoch-0 protected write whose statement trigger
        // took FOR SHARE on the epoch row, held until commit.
        let mut writer = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open epoch-zero writer connection");
        let mut tx = writer.begin().await.expect("begin epoch-zero write");
        sqlx::query(&format!(
            "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
            Uuid::new_v4(),
            key.to_storage()
        ))
        .execute(&mut *tx)
        .await
        .expect("epoch-zero write starts before activation");

        // Concurrent activation must block behind the in-flight write.
        let flip_url = fixture.schema_url.clone();
        let flip = tokio::spawn(async move {
            let mut conn = sqlx::PgConnection::connect(&flip_url)
                .await
                .expect("open activation connection");
            sqlx::query(
                "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
            )
            .execute(&mut conn)
            .await
            .expect("activation update completes after the writer commits");
        });
        wait_for_lock_waiter(&fixture.admin, "UPDATE ingress_protocol_epoch").await;
        assert!(!flip.is_finished(), "activation must still be blocked");
        tx.commit().await.expect("commit the epoch-zero write");
        flip.await.expect("join activation task");

        // First post-activation write without a proof is rejected.
        assert!(
            sqlx::query(&format!(
                "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
                Uuid::new_v4(),
                key.to_storage()
            ))
            .execute(&mut writer)
            .await
            .is_err(),
            "the first post-activation write requires the transaction proof"
        );
        drop(writer);
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_skips_a_message_whose_row_is_locked_by_an_in_flight_child_insert() {
        let Some(fixture) = Fixture::open("gc_ref_race").await else {
            return;
        };
        let key = fixture.record_message().await;
        let terminal_at = timestamp(6);
        fixture.terminalize(key, terminal_at).await;

        let mut tx = fixture.store.begin().await.expect("begin child insert");
        assert_eq!(
            fixture
                .store
                .insert_sm_ref(
                    &mut tx,
                    SmIngressId::new(),
                    IngressOrdinal::FIRST,
                    WireHandledCount::from_storage(1),
                    key
                )
                .await
                .expect("insert sm ref before GC"),
            MessageWriteOutcome::Recorded
        );

        let outcome = fixture
            .store
            .gc_expired_aliases(
                terminal_at + ALIAS_RETENTION,
                AliasGcBudget {
                    lock_timeout: StdDuration::from_millis(50),
                    ..gc_budget()
                },
            )
            .await
            .expect("a row held by an in-flight child write is skipped");
        assert_eq!(
            outcome,
            AliasGcOutcome {
                deleted_messages: 0,
                completed: false,
            }
        );
        tx.commit()
            .await
            .expect("commit child insert after GC skipped the row");
        assert_eq!(fixture.count("ingress_messages").await, 1);
        assert_eq!(fixture.count("ingress_sm_refs").await, 1);
        fixture.close().await;
    }

    #[tokio::test]
    async fn child_insert_blocked_behind_gc_deletion_observes_message_vanished() {
        let Some(fixture) = Fixture::open("ref_gc_race").await else {
            return;
        };
        let key = fixture.record_message().await;

        // Simulate GC's deletion transaction: FOR UPDATE on the message row,
        // held while a child insert arrives.
        let mut gc_conn = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open GC connection");
        let mut gc_tx = gc_conn.begin().await.expect("begin GC transaction");
        sqlx::query(&format!(
            "SELECT 1 FROM ingress_messages WHERE message_key = '{}' FOR UPDATE",
            key.to_storage()
        ))
        .execute(&mut *gc_tx)
        .await
        .expect("GC locks the message row");

        let insert_store = fixture.store.clone();
        let insert = tokio::spawn(async move {
            let mut tx = insert_store.begin().await?;
            let outcome = insert_store
                .insert_sm_ref(
                    &mut tx,
                    SmIngressId::new(),
                    IngressOrdinal::FIRST,
                    WireHandledCount::from_storage(1),
                    key,
                )
                .await?;
            tx.commit()
                .await
                .map_err(|_| IngressSubstrateError::Database {
                    retry_class: DbRetryClass::NotRetryable,
                })?;
            Ok::<_, IngressSubstrateError>(outcome)
        });
        wait_for_lock_waiter(&fixture.admin, "FOR UPDATE").await;
        assert!(!insert.is_finished(), "child insert must wait behind GC");
        sqlx::query(&format!(
            "DELETE FROM ingress_messages WHERE message_key = '{}'",
            key.to_storage()
        ))
        .execute(&mut *gc_tx)
        .await
        .expect("GC deletes the childless message");
        gc_tx.commit().await.expect("commit GC deletion");
        assert_eq!(
            insert
                .await
                .expect("join child insert task")
                .expect("child insert completes after GC commits"),
            MessageWriteOutcome::MessageVanished,
            "a child insert serialized behind GC observes the deletion"
        );
        drop(gc_conn);
        fixture.close().await;
    }

    #[tokio::test]
    async fn restricted_role_cannot_disable_or_replace_the_epoch_guard() {
        let Some(fixture) = Fixture::open("restricted_role").await else {
            return;
        };
        let role = format!("waddle_test_dml_{}", Uuid::new_v4().simple());
        let mut conn = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open role-test connection");
        sqlx::query(&format!("CREATE ROLE {role} NOLOGIN"))
            .execute(&mut conn)
            .await
            .expect("create restricted DML role");
        sqlx::query(&format!(
            "GRANT USAGE ON SCHEMA {} TO {role}",
            fixture.schema
        ))
        .execute(&mut conn)
        .await
        .expect("grant schema usage");
        for table in EPOCH_GUARDED_TABLES {
            sqlx::query(&format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.{table} TO {role}",
                fixture.schema
            ))
            .execute(&mut conn)
            .await
            .expect("grant DML on protected table");
        }
        sqlx::query(&format!(
            "GRANT SELECT ON {}.ingress_protocol_epoch TO {role}",
            fixture.schema
        ))
        .execute(&mut conn)
        .await
        .expect("grant epoch read");

        sqlx::query(&format!("SET ROLE {role}"))
            .execute(&mut conn)
            .await
            .expect("assume restricted role");
        for statement in [
            "ALTER TABLE ingress_messages DISABLE TRIGGER ingress_messages_epoch_guard_dml"
                .to_string(),
            "CREATE OR REPLACE FUNCTION waddle_ingress_epoch_guard() RETURNS trigger \
             LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$"
                .to_string(),
            "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
             lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1"
                .to_string(),
            "INSERT INTO ingress_epoch_guard_manifest (table_name) VALUES ('rogue')".to_string(),
            "DROP TRIGGER ingress_messages_epoch_guard_dml ON ingress_messages".to_string(),
        ] {
            assert!(
                sqlx::query(&statement).execute(&mut conn).await.is_err(),
                "the restricted role must not bypass the guard: {statement}"
            );
        }
        sqlx::query("RESET ROLE")
            .execute(&mut conn)
            .await
            .expect("reset role");
        sqlx::query(&format!("DROP OWNED BY {role}"))
            .execute(&mut conn)
            .await
            .expect("drop role grants");
        sqlx::query(&format!("DROP ROLE {role}"))
            .execute(&mut conn)
            .await
            .expect("drop restricted role");
        drop(conn);
        fixture.close().await;
    }

    async fn race_alias(
        store: PostgresIngressSubstrate,
        sender: BareJid,
        target: NormalizedTarget,
        origin: OriginId,
        digest: SemanticDigest,
        start: Arc<Barrier>,
    ) -> Result<AliasResolution, IngressSubstrateError> {
        let mut tx = store.begin().await?;
        start.wait().await;
        let result = store
            .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &digest, MessageKey::new)
            .await?;
        tx.commit().await.map_err(discard_database_error)?;
        Ok(result)
    }

    fn inserted(key: MessageKey) -> AliasResolution {
        AliasResolution::Aliased(AliasOutcome::Inserted(key))
    }

    fn existing(key: MessageKey) -> AliasResolution {
        AliasResolution::Aliased(AliasOutcome::Existing(key))
    }

    fn digest(byte: u8) -> SemanticDigest {
        SemanticDigest::from_storage(1, [byte; 32]).expect("valid semantic digest fixture")
    }

    fn sender() -> BareJid {
        "romeo@example.com"
            .parse()
            .expect("fixture is a valid bare JID")
    }

    fn target() -> NormalizedTarget {
        NormalizedTarget::Full(
            "juliet@example.com/laptop"
                .parse()
                .expect("fixture is a valid full JID"),
        )
    }

    fn timestamp(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, second)
            .single()
            .expect("fixture timestamp is valid")
    }

    struct Fixture {
        store: PostgresIngressSubstrate,
        db: Database,
        admin: sqlx::PgPool,
        schema: String,
        schema_url: String,
    }

    impl Fixture {
        async fn open(test_name: &str) -> Option<Self> {
            let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
                eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (ingress substrate)");
                return None;
            };
            let schema = format!(
                "waddle_test_ingress_{test_name}_{}",
                Uuid::new_v4().simple()
            );
            let admin = sqlx::PgPool::connect(&database_url)
                .await
                .expect("connect postgres admin pool");
            sqlx::query(&format!("CREATE SCHEMA {schema}"))
                .execute(&admin)
                .await
                .expect("create isolated postgres schema");
            let schema_url = postgres_url_with_search_path(&database_url, &schema);
            let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, schema_url.clone());
            config.pool_size = 10;
            let db = Database::from_config("ingress-substrate-test", &config)
                .await
                .expect("open isolated postgres database");
            MigrationRunner::single()
                .run(&db)
                .await
                .expect("apply migrations to isolated schema");
            let store =
                PostgresIngressSubstrate::open(db.clone()).expect("open Postgres substrate");
            Some(Self {
                store,
                db,
                admin,
                schema,
                schema_url,
            })
        }

        async fn close(self) {
            drop(self.store);
            drop(self.db);
            sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
                .execute(&self.admin)
                .await
                .expect("drop isolated postgres schema");
        }

        async fn record_message(&self) -> MessageKey {
            let key = MessageKey::new();
            let mut tx = self.store.begin().await.expect("begin message insert");
            self.store
                .record_message(&mut tx, key, &digest(1), None)
                .await
                .expect("record message");
            tx.commit().await.expect("commit message insert");
            key
        }

        async fn terminalize(&self, key: MessageKey, terminal_at: DateTime<Utc>) {
            let mut tx = self.store.begin().await.expect("begin terminalize");
            assert_eq!(
                self.store
                    .terminalize_message(&mut tx, key, terminal_at)
                    .await
                    .expect("terminalize message"),
                TerminalizeOutcome::Terminalized
            );
            tx.commit().await.expect("commit terminalize");
        }

        async fn record_terminal_messages(
            &self,
            count: usize,
            first_terminal_at: DateTime<Utc>,
        ) -> Vec<MessageKey> {
            let mut tx = self.store.begin().await.expect("begin message seed");
            let mut keys = Vec::with_capacity(count);
            for index in 0..count {
                let key = MessageKey::new();
                self.store
                    .record_message(&mut tx, key, &digest(1), None)
                    .await
                    .expect("record seeded message");
                assert_eq!(
                    self.store
                        .terminalize_message(
                            &mut tx,
                            key,
                            first_terminal_at + Duration::milliseconds(index as i64),
                        )
                        .await
                        .expect("terminalize seeded message"),
                    TerminalizeOutcome::Terminalized
                );
                keys.push(key);
            }
            tx.commit().await.expect("commit message seed");
            keys
        }

        async fn count(&self, table: &str) -> i64 {
            let conn = self.db.guard().await.expect("database guard");
            let mut rows = conn
                .query(&format!("SELECT COUNT(*) FROM {table}"), ())
                .await
                .expect("count ingress rows");
            let row = rows
                .next()
                .await
                .expect("read count row")
                .expect("count row exists");
            row.get(0).expect("decode count")
        }

        async fn terminal_is(&self, key: MessageKey, expected: DateTime<Utc>) -> bool {
            let conn = self.db.guard().await.expect("database guard");
            let mut rows = conn
                .query(
                    "SELECT terminal_at = ?::timestamptz FROM ingress_messages WHERE message_key = ?::uuid",
                    crate::db_params![expected.to_rfc3339(), key.to_storage().to_string()],
                )
                .await
                .expect("read terminal time");
            let row = rows
                .next()
                .await
                .expect("read terminal row")
                .expect("message row exists");
            row.get(0).expect("decode terminal comparison")
        }
    }

    fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
        let mut url = url::Url::parse(database_url).expect("parse postgres URL");
        let retained: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
            .append_pair("options", &format!("-c search_path={schema}"));
        url.to_string()
    }
}
