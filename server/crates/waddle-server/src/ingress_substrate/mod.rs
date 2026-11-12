//! Transactional ingress identity storage for PostgreSQL and SQLite.

mod authority;
pub(crate) use authority::MucArchiveContext;
// Not clustering-only: besides the clustering receivers (#1778), the websocket
// detach drain authorizes relayed append obligations with it (#1789), and that
// drain is compiled in every build.
mod canonical_sender;
pub(crate) use canonical_sender::canonical_sender_pooled;
mod maintenance;
mod owned_alias;
mod retention;
#[cfg(test)]
mod source_alias;
pub use maintenance::{
    receipt_complete_nonterminal_keys, recovery_evidence_pooled,
    unreceipted_nonterminal_candidates, RecoveryCandidate, RecoveryEvidence,
};
pub(crate) use owned_alias::{resolve_prepared_origin_in_scope, OriginCandidate};
#[cfg(test)]
pub(crate) use owned_alias::{
    AliasGate as OwnedAliasGate, SavepointPhase as OwnedAliasSavepointPhase,
    AFTER_INSERT as OWNED_ALIAS_AFTER_INSERT, AFTER_LOCK as OWNED_ALIAS_AFTER_LOCK,
    AFTER_MISS as OWNED_ALIAS_AFTER_MISS, AFTER_PROBE as OWNED_ALIAS_AFTER_PROBE,
    STALL_SAVEPOINT as OWNED_ALIAS_STALL_SAVEPOINT,
};
pub(crate) use retention::{
    invalidate_retention, invalidate_retention_in_prepared_transaction, refresh_retention,
    refresh_retention_in_prepared_transaction,
    refresh_retention_in_prepared_transaction_with_db_clock, refresh_retention_with_db_clock,
};
#[cfg(test)]
pub(crate) use source_alias::{
    classify_source_alias, SavepointPhase, SourceAliasClassification, SourceAliasError,
    SourceAliasOffer, STALL_AFTER_PROBE, STALL_SAVEPOINT_PHASE,
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
pub(crate) use authority::{
    advance_frontier_in_prepared_transaction, flush_checkpoint_in_prepared_transaction,
    load_envelope_in_prepared_transaction, load_stream_checkpoint_in_prepared_transaction,
    lookup_wire_binding_in_prepared_transaction, record_room_envelope_in_prepared_transaction,
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
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ingress::{
    resolve_alias, AliasResolution, DeliveryKey, IngressOrdinal, MessageKey, NormalizedTarget,
    NormalizedTargetStorage, ProtocolEpoch, SemanticDigest, SmIngressId, StoredAlias,
    WireHandledCount,
};
use waddle_xmpp::stream_management::{SmIngressAppendKey, SmIngressReceiptKind};
use waddle_xmpp_core::xep0359::OriginId;

use crate::db::{Database, DatabaseDriver, DatabaseError, Row, Transaction};
use crate::ingress_uow::DbRetryClass;

/// Immutable authenticated origin identity stamped with a newly-created
/// canonical message. The SM occurrence is absent for ephemeral streams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CanonicalSourceBinding {
    principal: AuthenticatedPrincipalRef,
    sm_occurrence: Option<(SmIngressId, IngressOrdinal)>,
}

impl CanonicalSourceBinding {
    pub(crate) fn new(
        principal: AuthenticatedPrincipalRef,
        sm_occurrence: Option<(SmIngressId, IngressOrdinal)>,
    ) -> Self {
        Self {
            principal,
            sm_occurrence,
        }
    }

    pub(crate) fn principal(&self) -> &AuthenticatedPrincipalRef {
        &self.principal
    }

    pub(crate) fn sm_occurrence(&self) -> Option<(SmIngressId, IngressOrdinal)> {
        self.sm_occurrence
    }
}

#[cfg(test)]
tokio::task_local! {
    /// Forces the orphan sweep's error path so the decoupling from
    /// `MaintenanceOutcome::Failed` can be asserted deterministically.
    pub(crate) static FAIL_ORPHAN_SWEEP: bool;
}
#[cfg(test)]
mod tests;

mod substrate;
mod typed_entry;
pub(crate) use substrate::acquire_epoch_lock_first;
use substrate::decode_stored_alias;
use substrate::dialect_sql;
use substrate::discard_database_error;
pub use substrate::gc_expired_aliases;
use substrate::gc_retained_child_sql;
pub use substrate::insert_sm_ref;
pub(crate) use substrate::quote_postgres_identifier;
pub use substrate::record_delivery;
pub use substrate::record_message;
#[allow(unused_imports)] // exported for dark tests and the future activation slice.
pub(crate) use substrate::record_message_with_source;
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
pub(crate) use substrate::DefinerApiCandidateContext;
pub use substrate::IngressSubstrateError;
pub use substrate::MessageWriteOutcome;
pub use substrate::PostgresIngressSubstrate;
pub use substrate::TerminalizeOutcome;
pub use substrate::ALIAS_RETENTION;
pub use substrate::EPOCH_GUARDED_TABLES;
pub(crate) use substrate::{
    assert_relayed_alias_in_prepared_transaction, insert_sm_ref_in_prepared_transaction,
    lock_message_for_child_in_prepared_transaction, prepared_ingress_relation,
    prepared_ingress_sql, record_delivery_in_prepared_transaction,
    record_message_in_prepared_transaction, resolve_alias_in_prepared_transaction,
    terminalize_message_in_prepared_transaction, PreparedIngressRelation,
};
#[cfg(test)]
pub(crate) use typed_entry::acquire_within_deadline;
pub use typed_entry::EnteredTransaction;
pub(crate) use typed_entry::{
    begin_ingress_entry, begin_unattested_raw_writer, IngressEntryAcquisition,
    IngressEntryAuthority, IngressEntryClamp, IngressEntryError, IngressEntryRequest,
    IngressEntryTransaction, PreparedAttestedReceiptWriter,
};
