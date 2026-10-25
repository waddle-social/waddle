use super::{
    async_trait, debug, info, instrument, BareJid, Database, DatabaseConfig, DatabaseDriver,
    InsertOutcome, IntoParams, PendingDeliveryStorage, PendingPayload, PendingRow, PendingRowId,
    PendingStorageError, QuotaPolicy, SmSessionId,
};
use crate::db::Rows;
use crate::db::Transaction;
use crate::db::Value;
use std::collections::HashSet;
use waddle_xmpp::pending_delivery::storage::{PendingClaim, PendingClaimPhase, PendingClaimToken};

mod ack_windows;
mod custody;
mod recovery_order;
mod schema;

use super::codec::{decode_row, serialize_message, PAYLOAD_KIND_ARCHIVED, PAYLOAD_KIND_TRANSIENT};

use waddle_xmpp::ownership::{
    ClaimError, ClaimStore, CurrentNodeIdentityGuard, Entity, EntityType, SharedNodeIdentity,
};
use waddle_xmpp::stream_management::persistence::SmClaimFence;

mod implementation;
use implementation::assert_insert_fence;
use implementation::claim_error_to_pending_storage_error;
pub(crate) use implementation::contains_in_transaction;
pub(crate) use implementation::insert_in_transaction;
pub(crate) use implementation::mark_notification_outboxed_in_transaction;
pub use implementation::DatabasePendingDeliveryStorage;
pub(crate) use implementation::PendingInsertFence;
