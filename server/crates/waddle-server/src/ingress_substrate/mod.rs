//! Transactional ingress identity storage for PostgreSQL and SQLite.

mod authority;
pub(crate) use authority::MucArchiveContext;
// Not clustering-only: besides the clustering receivers (#1778), the websocket
// detach drain authorizes relayed append obligations with it (#1789), and that
// drain is compiled in every build.
mod canonical_sender;
pub(crate) use canonical_sender::canonical_sender_pooled;
mod maintenance;
mod retention;
pub use maintenance::{
    receipt_complete_nonterminal_keys, recovery_evidence_pooled,
    unreceipted_nonterminal_candidates, RecoveryCandidate, RecoveryEvidence,
};
pub(crate) use retention::{
    invalidate_retention, refresh_retention, refresh_retention_with_db_clock,
};
#[cfg(test)]
mod authority_tests;
pub use authority::{
    advance_frontier, flush_checkpoint, load_envelope, load_stream_checkpoint, lookup_wire_binding,
    receipts_complete, record_receipt, record_receipt_pooled, record_room_canonical_envelope,
    EffectReceiptKind, EnvelopeVersion, FrontierOutcome, MessageEnvelope,
};

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
use waddle_xmpp::stream_management::{SmIngressAppendKey, SmIngressReceiptKind};
use waddle_xmpp_core::xep0359::OriginId;

use crate::db::{Database, DatabaseDriver, DatabaseError, Row, Transaction};
use crate::ingress_uow::DbRetryClass;

#[cfg(test)]
tokio::task_local! {
    /// Forces the orphan sweep's error path so the decoupling from
    /// `MaintenanceOutcome::Failed` can be asserted deterministically.
    pub(crate) static FAIL_ORPHAN_SWEEP: bool;
}
#[cfg(test)]
mod tests;

mod substrate;
pub(crate) use substrate::acquire_epoch_lock_first;
use substrate::acquire_gc_epoch_lock_first;
use substrate::database_timeout_kind;
use substrate::decode_stored_alias;
use substrate::dialect_sql;
use substrate::discard_database_error;
use substrate::expired_candidates;
use substrate::gc_candidate_batch;
use substrate::gc_canonical_candidates;
use substrate::gc_database_failure;
use substrate::gc_error_from_database;
pub use substrate::gc_expired_aliases;
use substrate::gc_failure;
use substrate::gc_orphan_proof_batch;
use substrate::gc_retained_child_sql;
use substrate::insert_message;
pub use substrate::insert_sm_ref;
use substrate::install_gc_timeouts;
use substrate::lock_eligible_terminal_message;
use substrate::lock_message_for_child;
use substrate::locked_alias;
use substrate::message_exists;
use substrate::read_live_epoch_for_share;
pub use substrate::record_delivery;
pub use substrate::record_message;
pub use substrate::resolve_and_record_alias;
pub(crate) use substrate::set_local_transaction_timeouts;
use substrate::should_sweep_orphans;
use substrate::stored_child_message_key;
pub use substrate::supported_protocol_epoch;
pub use substrate::terminalize_message;
pub use substrate::AliasGcBudget;
pub use substrate::AliasGcError;
pub use substrate::AliasGcFailure;
pub use substrate::AliasGcOutcome;
pub use substrate::AliasGcProgress;
use substrate::AliasStorageKey;
use substrate::BatchOutcome;
use substrate::CandidateLock;
pub use substrate::DatabaseTimeoutKind;
use substrate::EpochLockError;
pub use substrate::IngressSubstrateError;
pub use substrate::MessageWriteOutcome;
use substrate::OrphanSweepStep;
pub use substrate::PostgresIngressSubstrate;
use substrate::SkipReason;
pub use substrate::TerminalizeOutcome;
pub use substrate::ALIAS_RETENTION;
use substrate::DELETE_ALIASES_POSTGRES;
use substrate::DELETE_ALIASES_SQLITE;
use substrate::DELETE_CANDIDATE_POSTGRES;
use substrate::DELETE_CANDIDATE_SQLITE;
pub use substrate::EPOCH_GUARDED_TABLES;
use substrate::GC_BATCH_LIMIT;
use substrate::GC_CANDIDATES_POSTGRES;
use substrate::GC_CANDIDATES_SQLITE;
use substrate::GC_CHECK_CANDIDATE_POSTGRES;
use substrate::GC_CHECK_CANDIDATE_SQLITE;
use substrate::GC_DELETE_DELIVERIES_POSTGRES;
use substrate::GC_DELETE_DELIVERIES_SQLITE;
use substrate::GC_DELETE_INGRESS_APPENDS_POSTGRES;
use substrate::GC_DELETE_INGRESS_APPENDS_SQLITE;
use substrate::GC_DELETE_MESSAGE_POSTGRES;
use substrate::GC_DELETE_MESSAGE_SQLITE;
use substrate::GC_DELETE_ORPHAN_PROOFS_POSTGRES;
use substrate::GC_DELETE_ORPHAN_PROOFS_SQLITE;
use substrate::GC_LOCK_CANDIDATE_POSTGRES;
use substrate::GC_LOCK_CANDIDATE_SQLITE;
use substrate::GC_ORPHAN_WINDOW_AFTER_POSTGRES;
use substrate::GC_ORPHAN_WINDOW_AFTER_SQLITE;
use substrate::GC_ORPHAN_WINDOW_POSTGRES;
use substrate::GC_ORPHAN_WINDOW_SQLITE;
use substrate::INSERT_ALIAS_POSTGRES;
use substrate::INSERT_ALIAS_SQLITE;
use substrate::INSERT_DELIVERY_POSTGRES;
use substrate::INSERT_DELIVERY_SQLITE;
use substrate::INSERT_MESSAGE_POSTGRES;
use substrate::INSERT_MESSAGE_SQLITE;
use substrate::INSERT_SM_REF_POSTGRES;
use substrate::INSERT_SM_REF_SQLITE;
use substrate::LOCK_ALIAS_POSTGRES;
use substrate::LOCK_ALIAS_SQLITE;
use substrate::LOCK_MESSAGE_POSTGRES;
use substrate::LOCK_MESSAGE_SQLITE;
use substrate::MESSAGE_EXISTS_POSTGRES;
use substrate::MESSAGE_EXISTS_SQLITE;
use substrate::ORPHAN_SWEEP_INTERVAL;
use substrate::READ_DELIVERY_POSTGRES;
use substrate::READ_DELIVERY_SQLITE;
use substrate::READ_EPOCH_POSTGRES;
use substrate::READ_EPOCH_SQLITE;
use substrate::READ_SM_REF_POSTGRES;
use substrate::READ_SM_REF_SQLITE;
use substrate::TERMINALIZE_MESSAGE_POSTGRES;
use substrate::TERMINALIZE_MESSAGE_SQLITE;
