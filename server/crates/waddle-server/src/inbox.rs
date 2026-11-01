//! Database-backed storage for Waddle inbox projections.

use crate::db::DatabaseError;
use crate::db::Row;
use crate::db::Rows;
use crate::db::Transaction;
use crate::db::Value;
use std::path::Path;
use std::sync::Arc;

use crate::db::{DatabaseConfig, DatabaseDriver, IntoParams};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jid::BareJid;
use tracing::{info, instrument};
use waddle_xmpp::inbox::storage::{
    GroupchatNotificationRecovery, GroupchatNotificationRecoveryKey, InboxStorage,
    InboxStorageError,
};
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::xep::CallThreadDuration;
use waddle_xmpp_core::xep0359::StanzaId;

use crate::db::Database;

mod codec;
mod open;
mod schema;

use codec::{decode_row, encode_call_thread_columns, encode_kind, SELECT_COLS};
pub use open::build_inbox_storage;

#[cfg(test)]
mod tests;

mod implementation;
pub(crate) use implementation::find_in_transaction;
pub(crate) use implementation::get_in_transaction;
pub(crate) use implementation::insert_groupchat_notification_recovery_in_transaction;
pub(crate) use implementation::mark_read_in_transaction;
pub(crate) use implementation::upsert_in_transaction;
pub use implementation::DatabaseInboxStorage;
pub use implementation::InboxDecodeError;
pub use implementation::InboxTxError;
pub use schema::bootstrap_schema_owner as bootstrap_inbox_owner_schema;
