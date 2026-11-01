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
#[cfg(feature = "test-support")]
pub(crate) use authority::with_pooled_receipt_commit_ack_lost;
pub use authority::{
    advance_frontier, flush_checkpoint, load_envelope, load_stream_checkpoint, lookup_wire_binding,
    receipts_complete, record_receipt, record_receipt_pooled, record_room_canonical_envelope,
    EffectReceiptKind, EnvelopeVersion, FrontierOutcome, MessageEnvelope,
};
pub(crate) use authority::{receipts_complete_in_schema, record_receipt_in_prepared_transaction};

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
use substrate::decode_stored_alias;
use substrate::dialect_sql;
use substrate::discard_database_error;
pub use substrate::gc_expired_aliases;
use substrate::gc_retained_child_sql;
pub use substrate::insert_sm_ref;
use substrate::lock_message_for_child;
#[cfg(feature = "test-support")]
pub(crate) use substrate::prepare_attested_receipt_transaction;
pub(crate) use substrate::quote_postgres_identifier;
pub use substrate::record_delivery;
pub use substrate::record_message;
pub use substrate::resolve_and_record_alias;
pub(crate) use substrate::set_local_transaction_timeouts;
pub use substrate::supported_protocol_epoch;
pub use substrate::terminalize_message;
pub(crate) use substrate::tighten_local_transaction_timeouts;
pub use substrate::AliasGcBudget;
pub use substrate::AliasGcError;
pub use substrate::AliasGcFailure;
pub use substrate::AliasGcOutcome;
pub use substrate::AliasGcProgress;
pub use substrate::DatabaseTimeoutKind;
pub use substrate::IngressSubstrateError;
pub use substrate::MessageWriteOutcome;
pub use substrate::PostgresIngressSubstrate;
#[cfg(feature = "test-support")]
pub(crate) use substrate::PreparedAttestedReceiptWriter;
pub use substrate::TerminalizeOutcome;
pub use substrate::ALIAS_RETENTION;
pub use substrate::EPOCH_GUARDED_TABLES;
