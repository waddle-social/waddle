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

#[cfg(test)]
mod tests;
