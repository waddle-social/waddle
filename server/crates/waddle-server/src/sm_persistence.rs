//! libSQL/Postgres-backed XEP-0198 stream-management persistence
//! (issue #209 slice (d) phase 2).
//!
//! `waddle_xmpp::stream_management::persistence::SmPersistenceStorage`
//! defines the typed contract; this module provides the production
//! database backend. Mirrors the `crate::pending_delivery` module's
//! storage shape: `Database::open` opens SQLite or Postgres; per-stream
//! locks make multi-statement quota / append paths atomic across both
//! drivers.

use crate::db::Rows;
#[cfg(feature = "clustering")]
use crate::sm_persistence_fenced::PostgresFencedSmPersistence;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jid::FullJid;
use tracing::{debug, info, instrument};
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    IngressCustodyDisposition, KeyedSnapshotOutcome, PersistedDetachedAppend,
    PersistedIngressAppend, PersistedSession, PersistedUnackedStanza, PromotionAppendOutcome,
    SmAckWindow, SmClaimFence, SmPersistenceError, SmPersistenceStorage,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::presence::Show;

use crate::db::{Database, DatabaseConfig, DatabaseDriver, IntoParams};

mod atomic_store;
/// `pub(crate)` (rather than private, the norm for this module's
/// siblings) so ADR-0017 Phase 3 Slice 4's `PostgresFencedSmPersistence`
/// (`crate::sm_persistence_fenced`, a sibling top-level module — not a
/// submodule of this one, per the "not a decorator" file-layout rule) can
/// reuse the same wire encode/decode logic against the byte-identical
/// `sm_sessions`/`sm_unacked` schema, instead of forking a second copy
/// that could silently drift from this one.
pub(crate) mod codec;
mod incremental;
pub(crate) mod ingress_append;
mod joined_sessions;
pub(crate) mod promotion_inventory;
/// Unregistered #2244 recipient/source interlock exercise. No production
/// writer or startup path selects this candidate.
#[cfg(feature = "test-support")]
pub mod recipient_route_interlock_candidate;
/// Transactional revision participation, not selected by any serving path.
pub mod revisioned;
mod runtime_attach;
mod schema;
/// Unselected revision authority for the recipient-route snapshot laboratory.
#[cfg(feature = "test-support")]
pub mod snapshot_revision_candidate;
pub use runtime_attach::SmRuntimeAttachError;

/// Verify the owner-prepared stream management relations and the runtime's
/// write privileges on them in one read-only snapshot of the attested main
/// pool. Shared by the portable and the claim-fenced stores, which use the
/// same relations.
#[cfg(feature = "clustering")]
pub(crate) async fn verify_runtime_postgres_schema(
    db: &crate::db::Database,
) -> Result<(), SmRuntimeAttachError> {
    runtime_attach::verify(db, true).await
}
/// Verify the durable session authority's owner-prepared relations, the
/// portable relations it also writes, and the runtime's privileges on both.
pub(crate) async fn verify_durable_runtime_postgres_schema(
    db: &crate::db::Database,
) -> Result<(), SmRuntimeAttachError> {
    runtime_attach::verify_durable(db).await
}
pub use schema::bootstrap_schema_owner as bootstrap_portable_sm_owner_schema;

use codec::{
    decode_session, decode_session_principal, decode_unacked, decode_unacked_join_row,
    serialize_presence_payloads, show_wire_str,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod startup_poison_tests;

mod implementation;
pub use implementation::open_for_cluster_mode_with_lineage;
pub use implementation::DatabaseSmPersistence;
