//! XEP-0160 offline-message flush orchestration (issue #209,
//! waddle-server side).
//!
//! [`waddle_xmpp::pending_delivery::flush::build_replay_stanza`] is the
//! pure wire-shape builder. This module ties it to the live system:
//! it reads rows out of the [`PendingDeliveryStorage`], resolves
//! Archived rows against MAM, and pushes the replay stanzas to the
//! recovering resource via the [`ConnectionRegistry`].
//!
//! Locked design points consumed here:
//!
//! - **Q7a/Q7d** — caller (presence handler) gates this on the first
//!   non-negative-priority presence of a fresh session via
//!   [`ConnectionEntry::claim_offline_flush`].
//! - **Q7b** — SM-ack-keyed deletion. The flush no longer deletes
//!   rows on push; it tags each [`OutboundStanza`] with its source
//!   [`PendingRowId`] so the recipient's main loop can stamp the
//!   assigned XEP-0198 outbound counter via
//!   [`PendingDeliveryStorage::record_pushed_at`]. Rows are deleted
//!   only on SM `<a h>` ack via
//!   [`PendingDeliveryStorage::delete_acked_in_window`].
//! - **Q7c** — `claim_for_session` atomically tags rows with the
//!   recipient's resource so a concurrent presence from another
//!   resource sees an empty pool. On pre-ack session death the SM
//!   janitor / shutdown drain calls
//!   [`PendingDeliveryStorage::release_claim`] to restore the rows
//!   for re-flush by the next recovering resource.
//! - **Q5** — wire shape (`<delay/>` with original receipt time, server
//!   `from`, preserved `to`/extensions, no `<stanza-id/>` for Transient).

use std::sync::Arc;

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use tracing::{debug, info, instrument, warn};
use waddle_xmpp::pending_delivery::flush::{
    build_replay_stanza, MaterializedPayload, ReplayReason,
};
use waddle_xmpp::pending_delivery::storage::{PendingDeliveryStorage, PendingStorageError};
use waddle_xmpp::pending_delivery::{
    InsertOutcome, PendingPayload, PendingRow, PendingRowId, QuotaPolicy, SmSessionId,
};
use waddle_xmpp::registry::{ConnectionRegistry, SendResult};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::xep0359::StanzaId;

use crate::db::{Database, DatabaseConfig, DatabaseDriver, IntoParams};

mod codec;
mod database;
mod flush;

pub use database::DatabasePendingDeliveryStorage;
pub use flush::{
    flush_for_resource, ArchiveResolveError, ArchiveResolver, FlushContext, FlushOutcome,
    MamArchiveResolver, NullArchiveResolver,
};
// Crate-internal: an implementation constant the tests assert against, kept
// out of the public API surface (Greptile review on PR #1234). Test-only, so
// it never counts as an unused re-export in production builds.
#[cfg(test)]
pub(crate) use flush::FLUSH_BATCH_SIZE;

/// Open the pending_delivery storage for cluster mode (ADR-0017 Phase 3
/// Slice 5 FIX 3, council-adjudicated) — mirrors
/// `sm_persistence::open_for_cluster_mode`'s identical
/// co-location-then-construct pattern, one table over.
///
/// When clustering is enabled, an explicit Postgres `database_url`, exact
/// co-location, and live claim/identity handles are mandatory. These
/// requirements are checked BEFORE this storage is opened: the resolved URL
/// must be an EXACT string match for
/// `global_db.database_url()` (deliberately no DSN normalization — see
/// `sm_persistence::open_for_cluster_mode`'s identical comment) — the
/// fencing `SELECT ... FOR SHARE` used by exact-fence pending mutations
/// issues targets `clustering_claims`, which only exists in the
/// clustering global database. A mismatch fails startup with
/// [`waddle_xmpp::pending_delivery::storage::PendingStorageError::ClusterColocationMismatch`]
/// rather than silently fencing (or, on a build without this check,
/// silently NOT fencing) against a table that may not exist wherever
/// `database_url` actually points.
///
/// When clustering is disabled, this is exactly
/// [`DatabasePendingDeliveryStorage::open`] + `quota`, with no fencing
/// attached. A clustered request never falls back to that unfenced path.
pub async fn open_for_cluster_mode(
    database_url: Option<&str>,
    quota: waddle_xmpp::pending_delivery::QuotaPolicy,
    clustering_enabled: bool,
    claim_pair: Option<(
        std::sync::Arc<dyn waddle_xmpp::ownership::ClaimStore>,
        waddle_xmpp::ownership::SharedNodeIdentity,
    )>,
    global_db: &Database,
) -> Result<DatabasePendingDeliveryStorage, PendingStorageError> {
    #[cfg(feature = "clustering")]
    {
        if clustering_enabled {
            let Some(resolved_url) = database_url
                .filter(|url| url.starts_with("postgres://") || url.starts_with("postgresql://"))
            else {
                return Err(PendingStorageError::ClusterDatabaseRequired {
                    configured_database_url: database_url
                        .map(crate::db::redact_database_url)
                        .unwrap_or_else(|| "<unset>".to_string()),
                });
            };
            // FIX 3 — co-location invariant, checked before this storage's
            // fencing is ever attached: clustered pending_delivery and the
            // clustering claims tables must live in the same Postgres
            // database, or exact-fence checks would run
            // against a `clustering_claims` table that does not exist in
            // this storage's own database.
            if resolved_url != global_db.database_url() {
                return Err(PendingStorageError::ClusterColocationMismatch {
                    pending_delivery_database_url: crate::db::redact_database_url(resolved_url),
                    global_database_url: crate::db::redact_database_url(global_db.database_url()),
                });
            }
            let Some((_claim_store, node_identity)) = claim_pair else {
                return Err(PendingStorageError::ClusterClaimHandlesMissing);
            };
            let storage = DatabasePendingDeliveryStorage::open(Some(resolved_url), quota).await?;
            return Ok(storage.with_cluster_fencing(node_identity));
        }
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (&claim_pair, global_db);
        if clustering_enabled {
            return Err(PendingStorageError::ClusteringFeatureUnavailable);
        }
    }
    DatabasePendingDeliveryStorage::open(database_url, quota).await
}

#[cfg(test)]
mod tests;
