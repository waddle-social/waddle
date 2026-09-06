//! Database-backed storage for Waddle inbox projections.

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
use waddle_xmpp::xep::CallThreadDuration;
use waddle_xmpp_core::xep0359::StanzaId;

use crate::db::Database;

mod codec;
mod open;
mod schema;

use codec::{decode_row, encode_call_thread_columns, encode_kind, SELECT_COLS};
pub use open::build_inbox_storage;

const GROUPCHAT_NOTIFICATION_RECOVERY_SELECT_COLS: &str = "recipient_bare_jid, room_jid, thread_id, stanza_id_by, stanza_id, sender_jid, is_live_occupant, room_members_only, sender_can_broadcast_channel_mention, created_at_ms";

#[derive(Clone)]
pub struct DatabaseInboxStorage {
    db: Database,
}

/// Typed failures while decoding an inbox row from the database.
///
/// Public deliberately: [`crate::ingress_uow::IngressUowError`] carries
/// [`InboxTxError`] (and through it this type) transparently, so the
/// unit-of-work error surface can stay fully typed instead of collapsing
/// to stringly diagnostics at the repository boundary.
#[derive(Debug, thiserror::Error)]
pub enum InboxDecodeError {
    #[error(transparent)]
    Database(#[from] crate::db::DatabaseError),
    #[error("invalid partner JID: {source}")]
    PartnerJid {
        #[source]
        source: jid::Error,
    },
    #[error("unknown inbox conversation kind '{value}'")]
    UnknownConversationKind { value: String },
}

/// Errors returned by the transaction-taking inbox write helpers.
///
/// Public deliberately — see [`InboxDecodeError`]; the transaction
/// helpers themselves stay `pub(crate)`.
#[derive(Debug, thiserror::Error)]
pub enum InboxTxError {
    #[error(transparent)]
    Database(#[from] crate::db::DatabaseError),
    #[error(transparent)]
    Decode(#[from] InboxDecodeError),
    #[error("RETURNING produced no row")]
    ReturningRowMissing,
    #[error("inbox projection requires a canonical message row")]
    ProjectionMessageMissing,
    #[error("recorded inbox projection has no inbox row")]
    ProjectionEntryMissing,
    #[error("inbox projection has an invalid thread identifier")]
    InvalidProjectionThread,
}

impl From<InboxTxError> for InboxStorageError {
    fn from(error: InboxTxError) -> Self {
        Self::Other(error.to_string())
    }
}

impl From<InboxDecodeError> for InboxStorageError {
    fn from(error: InboxDecodeError) -> Self {
        Self::Other(error.to_string())
    }
}

impl DatabaseInboxStorage {
    pub fn database(&self) -> Database {
        self.db.clone()
    }

    pub async fn open(database_url: Option<&str>) -> Result<Self, InboxStorageError> {
        let db = match database_url {
            Some(database_url) => open::open_database(database_url).await?,
            None => Database::in_memory("inbox")
                .await
                .map_err(|error| InboxStorageError::Other(error.to_string()))?,
        };
        let storage = Self { db };
        schema::initialize(&storage).await?;
        info!(driver = ?storage.db.driver(), "Inbox storage initialized");
        Ok(storage)
    }

    async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, InboxStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))
    }

    async fn execute(&self, sql: &str, params: impl IntoParams) -> Result<u64, InboxStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))
    }

    async fn execute_batch(&self, sql: &str) -> Result<(), InboxStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        conn.execute_batch(sql)
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        Ok(())
    }
}

/// Shared INSERT … ON CONFLICT for the inbox upsert. The two trailing
/// `?` placeholders carry the unread/reply increment flags; the
/// `call_*` columns COALESCE `excluded` over the stored value so a later
/// non-anchor reply upsert (which carries no call metadata) does not
/// wipe the anchor's kind/media or the ended summary.
fn upsert_sql() -> String {
    format!(
        r#"
        INSERT INTO inbox_entries (
            user_jid, partner_jid, thread_id, kind, last_stanza_id, last_updated,
            unread, preview, thread_title, reply_count, author,
            call_thread_kind, call_thread_media, call_ended_at, call_duration
        ) VALUES (?, ?, ?, ?, ?, ?, CASE WHEN ? != 0 THEN 1 ELSE 0 END, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(user_jid, partner_jid, thread_id) DO UPDATE SET
            kind = CASE
                WHEN (excluded.last_updated, excluded.last_stanza_id) >=
                     (inbox_entries.last_updated, inbox_entries.last_stanza_id)
                THEN excluded.kind ELSE inbox_entries.kind END,
            last_stanza_id = CASE
                WHEN (excluded.last_updated, excluded.last_stanza_id) >=
                     (inbox_entries.last_updated, inbox_entries.last_stanza_id)
                THEN excluded.last_stanza_id ELSE inbox_entries.last_stanza_id END,
            last_updated = CASE
                WHEN (excluded.last_updated, excluded.last_stanza_id) >=
                     (inbox_entries.last_updated, inbox_entries.last_stanza_id)
                THEN excluded.last_updated ELSE inbox_entries.last_updated END,
            preview = CASE
                WHEN (excluded.last_updated, excluded.last_stanza_id) >=
                     (inbox_entries.last_updated, inbox_entries.last_stanza_id)
                THEN excluded.preview ELSE inbox_entries.preview END,
            unread = CASE
                WHEN ? != 0 THEN inbox_entries.unread + 1
                ELSE inbox_entries.unread
            END,
            thread_title = COALESCE(excluded.thread_title, inbox_entries.thread_title),
            reply_count = CASE
                WHEN ? != 0 THEN inbox_entries.reply_count + 1
                ELSE inbox_entries.reply_count
            END,
            author = CASE
                WHEN (excluded.last_updated, excluded.last_stanza_id) >=
                     (inbox_entries.last_updated, inbox_entries.last_stanza_id)
                THEN COALESCE(excluded.author, inbox_entries.author)
                ELSE inbox_entries.author END,
            call_thread_kind = COALESCE(excluded.call_thread_kind, inbox_entries.call_thread_kind),
            call_thread_media = COALESCE(excluded.call_thread_media, inbox_entries.call_thread_media),
            call_ended_at = COALESCE(excluded.call_ended_at, inbox_entries.call_ended_at),
            call_duration = COALESCE(excluded.call_duration, inbox_entries.call_duration)
        RETURNING {SELECT_COLS}
        "#
    )
}

/// Binds the inbox upsert parameters in `upsert_sql` placeholder order.
/// The four `call_*` Values come from the codec encode helper so typed
/// metadata serializes to TEXT/INTEGER only here at the SQL boundary.
fn upsert_params(
    user: &BareJid,
    entry: &InboxEntry,
    increment: i64,
    is_thread: i64,
) -> Vec<crate::db::Value> {
    let thread_id = entry.thread_id.as_deref().unwrap_or("");
    let [call_thread_kind, call_thread_media, call_ended_at, call_duration] =
        encode_call_thread_columns(entry);
    vec![
        crate::db::Value::from(user.to_string()),
        crate::db::Value::from(entry.partner.to_string()),
        crate::db::Value::from(thread_id.to_string()),
        crate::db::Value::from(encode_kind(entry.kind)),
        crate::db::Value::from(entry.last_stanza_id.clone()),
        crate::db::Value::from(entry.last_updated),
        crate::db::Value::from(increment),
        crate::db::Value::from(entry.preview.clone()),
        crate::db::Value::from(entry.thread_title.clone()),
        crate::db::Value::from(entry.reply_count as i64),
        crate::db::Value::from(entry.author.clone()),
        call_thread_kind,
        call_thread_media,
        call_ended_at,
        call_duration,
        crate::db::Value::from(increment),
        crate::db::Value::from(is_thread),
    ]
}

fn insert_groupchat_notification_recovery_sql() -> &'static str {
    r#"
    INSERT INTO groupchat_notification_recovery (
        recipient_bare_jid,
        room_jid,
        thread_id,
        stanza_id_by,
        stanza_id,
        sender_jid,
        is_live_occupant,
        room_members_only,
        sender_can_broadcast_channel_mention,
        created_at_ms,
        completed_at_ms
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)
    ON CONFLICT(recipient_bare_jid, room_jid, thread_id, stanza_id_by, stanza_id) DO UPDATE SET
        sender_jid = excluded.sender_jid,
        is_live_occupant = excluded.is_live_occupant,
        room_members_only = excluded.room_members_only,
        sender_can_broadcast_channel_mention = excluded.sender_can_broadcast_channel_mention,
        created_at_ms = excluded.created_at_ms,
        completed_at_ms = NULL
    "#
}

fn groupchat_notification_recovery_params(
    recovery: &GroupchatNotificationRecovery,
) -> Vec<crate::db::Value> {
    crate::db_params![
        recovery.key.recipient.to_string(),
        recovery.key.room.to_string(),
        recovery.key.thread_id.clone().unwrap_or_default(),
        recovery.key.archive_stanza_id.by.to_string(),
        recovery.key.archive_stanza_id.id.clone(),
        recovery.sender_jid.to_string(),
        recovery.is_live_occupant,
        recovery.room_members_only,
        recovery.sender_can_broadcast_channel_mention,
        recovery.created_at_ms,
    ]
}

/// Upsert an inbox entry using the caller's transaction and return the
/// post-upsert state.
pub(crate) async fn upsert_in_transaction(
    tx: &mut crate::db::Transaction<'_>,
    user: &BareJid,
    entry: InboxEntry,
    increment_unread: bool,
) -> Result<InboxEntry, InboxTxError> {
    let increment = i64::from(u8::from(increment_unread));
    let thread_id = entry.thread_id.as_deref().unwrap_or("");
    let is_thread = i64::from(u8::from(!thread_id.is_empty()));
    let sql = upsert_sql();
    let mut rows = tx
        .query(&sql, upsert_params(user, &entry, increment, is_thread))
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(InboxTxError::ReturningRowMissing)?;

    Ok(decode_row(&row)?)
}

/// Read the current projection without replaying unread or reply increments.
pub(crate) async fn get_in_transaction(
    tx: &mut crate::db::Transaction<'_>,
    user: &BareJid,
    entry: &InboxEntry,
) -> Result<InboxEntry, InboxTxError> {
    let mut rows = tx.query(
        &format!("SELECT {SELECT_COLS} FROM inbox_entries WHERE user_jid = ? AND partner_jid = ? AND thread_id = ?"),
        crate::db_params![user.to_string(), entry.partner.to_string(), entry.thread_id.clone().unwrap_or_default()],
    ).await?;
    let row = rows
        .next()
        .await?
        .ok_or(InboxTxError::ProjectionEntryMissing)?;
    Ok(decode_row(&row)?)
}

/// Insert a groupchat notification recovery item using the caller's
/// transaction.
pub(crate) async fn insert_groupchat_notification_recovery_in_transaction(
    tx: &mut crate::db::Transaction<'_>,
    recovery: GroupchatNotificationRecovery,
) -> Result<(), InboxTxError> {
    tx.execute(
        insert_groupchat_notification_recovery_sql(),
        groupchat_notification_recovery_params(&recovery),
    )
    .await?;
    Ok(())
}

fn decode_groupchat_notification_recovery(
    row: &crate::db::Row,
) -> Result<GroupchatNotificationRecovery, InboxStorageError> {
    let recipient: String = row
        .get(0)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let room: String = row
        .get(1)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let thread_id: String = row
        .get(2)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let stanza_id_by: String = row
        .get(3)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let stanza_id: String = row
        .get(4)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let sender_jid: String = row
        .get(5)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let is_live_occupant: i64 = row
        .get(6)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let room_members_only: i64 = row
        .get(7)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let sender_can_broadcast_channel_mention: i64 = row
        .get(8)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let created_at_ms: i64 = row
        .get(9)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    Ok(GroupchatNotificationRecovery {
        key: GroupchatNotificationRecoveryKey {
            recipient: recipient.parse().map_err(|error| {
                InboxStorageError::Other(format!("invalid recipient JID: {error}"))
            })?,
            room: room
                .parse()
                .map_err(|error| InboxStorageError::Other(format!("invalid room JID: {error}")))?,
            thread_id: (!thread_id.is_empty()).then_some(thread_id),
            archive_stanza_id: StanzaId::new(
                stanza_id,
                stanza_id_by.parse().map_err(|error| {
                    InboxStorageError::Other(format!("invalid stanza-id by JID: {error}"))
                })?,
            ),
        },
        sender_jid: sender_jid
            .parse()
            .map_err(|error| InboxStorageError::Other(format!("invalid sender JID: {error}")))?,
        is_live_occupant: is_live_occupant != 0,
        room_members_only: room_members_only != 0,
        sender_can_broadcast_channel_mention: sender_can_broadcast_channel_mention != 0,
        created_at_ms,
    })
}

#[async_trait]
impl InboxStorage for DatabaseInboxStorage {
    #[instrument(skip(self, user), fields(user = %user))]
    async fn list(&self, user: &BareJid) -> Result<Vec<InboxEntry>, InboxStorageError> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM inbox_entries WHERE user_jid = ? AND thread_id = '' ORDER BY last_updated DESC, partner_jid ASC"
        );
        let mut rows = self
            .query(&sql, crate::db_params![user.to_string()])
            .await?;

        let mut entries = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
        {
            entries.push(decode_row(&row)?);
        }
        Ok(entries)
    }

    #[instrument(skip(self, user, room), fields(user = %user, room = %room))]
    async fn list_threads(
        &self,
        user: &BareJid,
        room: &BareJid,
    ) -> Result<Vec<InboxEntry>, InboxStorageError> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM inbox_entries WHERE user_jid = ? AND partner_jid = ? AND thread_id != '' ORDER BY last_updated DESC"
        );
        let mut rows = self
            .query(&sql, crate::db_params![user.to_string(), room.to_string()])
            .await?;

        let mut entries = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
        {
            entries.push(decode_row(&row)?);
        }
        Ok(entries)
    }

    #[instrument(skip(self, user), fields(user = %user))]
    async fn list_all_threads(&self, user: &BareJid) -> Result<Vec<InboxEntry>, InboxStorageError> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM inbox_entries \
             WHERE user_jid = ? AND thread_id != '' \
             ORDER BY last_updated DESC, partner_jid ASC, thread_id ASC"
        );
        let mut rows = self
            .query(&sql, crate::db_params![user.to_string()])
            .await?;
        let mut entries = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
        {
            entries.push(decode_row(&row)?);
        }
        Ok(entries)
    }

    #[instrument(skip(self, entry, user), fields(user = %user, partner = %entry.partner))]
    async fn upsert(
        &self,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
    ) -> Result<InboxEntry, InboxStorageError> {
        let increment = i64::from(u8::from(increment_unread));
        let thread_id = entry.thread_id.as_deref().unwrap_or("");
        let is_thread = i64::from(u8::from(!thread_id.is_empty()));
        let sql = upsert_sql();
        let mut rows = self
            .query(&sql, upsert_params(user, &entry, increment, is_thread))
            .await?;

        let row = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
            .ok_or_else(|| InboxStorageError::Other("RETURNING produced no row".to_string()))?;

        decode_row(&row).map_err(InboxStorageError::from)
    }

    #[instrument(skip(self, entry, recovery, user), fields(user = %user, partner = %entry.partner))]
    async fn upsert_with_groupchat_notification_recovery(
        &self,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
        recovery: Option<GroupchatNotificationRecovery>,
    ) -> Result<InboxEntry, InboxStorageError> {
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        let updated = upsert_in_transaction(&mut tx, user, entry, increment_unread)
            .await
            .map_err(InboxStorageError::from)?;
        if let Some(recovery) = recovery {
            insert_groupchat_notification_recovery_in_transaction(&mut tx, recovery)
                .await
                .map_err(InboxStorageError::from)?;
        }
        tx.commit()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        Ok(updated)
    }

    async fn insert_groupchat_notification_recovery(
        &self,
        recovery: GroupchatNotificationRecovery,
    ) -> Result<(), InboxStorageError> {
        self.execute(
            insert_groupchat_notification_recovery_sql(),
            groupchat_notification_recovery_params(&recovery),
        )
        .await?;
        Ok(())
    }

    async fn list_pending_groupchat_notification_recoveries(
        &self,
        limit: usize,
    ) -> Result<Vec<GroupchatNotificationRecovery>, InboxStorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {GROUPCHAT_NOTIFICATION_RECOVERY_SELECT_COLS} \
             FROM groupchat_notification_recovery \
             WHERE completed_at_ms IS NULL \
             ORDER BY created_at_ms ASC, recipient_bare_jid ASC, room_jid ASC, thread_id ASC, stanza_id ASC \
             LIMIT ?"
        );
        let mut rows = self.query(&sql, crate::db_params![limit]).await?;
        let mut recoveries = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
        {
            recoveries.push(decode_groupchat_notification_recovery(&row)?);
        }
        Ok(recoveries)
    }

    async fn mark_groupchat_notification_recovery_completed(
        &self,
        key: &GroupchatNotificationRecoveryKey,
    ) -> Result<u64, InboxStorageError> {
        self.execute(
            "UPDATE groupchat_notification_recovery \
             SET completed_at_ms = ? \
             WHERE recipient_bare_jid = ? \
               AND room_jid = ? \
               AND thread_id = ? \
               AND stanza_id_by = ? \
               AND stanza_id = ? \
               AND completed_at_ms IS NULL",
            crate::db_params![
                crate::time::now_ms(),
                key.recipient.to_string(),
                key.room.to_string(),
                key.thread_id.clone().unwrap_or_default(),
                key.archive_stanza_id.by.to_string(),
                key.archive_stanza_id.id.clone(),
            ],
        )
        .await
    }

    async fn prune_completed_groupchat_notification_recoveries(
        &self,
        cutoff_ms: i64,
        limit: usize,
    ) -> Result<u64, InboxStorageError> {
        if limit == 0 {
            return Ok(0);
        }
        self.execute(
            "DELETE FROM groupchat_notification_recovery \
             WHERE (recipient_bare_jid, room_jid, thread_id, stanza_id_by, stanza_id) IN ( \
                 SELECT recipient_bare_jid, room_jid, thread_id, stanza_id_by, stanza_id \
                 FROM groupchat_notification_recovery \
                 WHERE completed_at_ms IS NOT NULL \
                   AND completed_at_ms < ? \
                 ORDER BY completed_at_ms ASC, recipient_bare_jid ASC, room_jid ASC, thread_id ASC, stanza_id_by ASC, stanza_id ASC \
                 LIMIT ? \
             )",
            crate::db_params![cutoff_ms, limit],
        )
        .await
    }

    #[instrument(skip(self, user, partner), fields(user = %user, partner = %partner))]
    async fn mark_read(
        &self,
        user: &BareJid,
        partner: &BareJid,
        thread_id: Option<&str>,
    ) -> Result<Option<InboxEntry>, InboxStorageError> {
        let tid = thread_id.unwrap_or("");
        let sql = format!(
            "UPDATE inbox_entries SET unread = 0 \
             WHERE user_jid = ? AND partner_jid = ? AND thread_id = ? \
             RETURNING {SELECT_COLS}"
        );
        let mut rows = self
            .query(
                &sql,
                crate::db_params![user.to_string(), partner.to_string(), tid.to_string()],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_row(&row)?))
    }

    #[instrument(skip(self, user), fields(user = %user))]
    async fn total_unread(&self, user: &BareJid) -> Result<u64, InboxStorageError> {
        let mut rows = self
            .query(
                "SELECT COALESCE(SUM(unread), 0) FROM inbox_entries WHERE user_jid = ? AND thread_id = ''",
                crate::db_params![user.to_string()],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
        else {
            return Ok(0);
        };
        let total: i64 = row
            .get(0)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        Ok(total.max(0) as u64)
    }

    #[instrument(skip(self, room), fields(room = %room, thread_id))]
    async fn mark_call_thread_ended(
        &self,
        room: &BareJid,
        thread_id: &str,
        ended: DateTime<Utc>,
        duration: &CallThreadDuration,
    ) -> Result<(), InboxStorageError> {
        // Only stamp the ended summary onto genuine call-thread rows.
        // The wire emits `<call>` only when BOTH kind and media are
        // present, so an ended summary on a row missing either would
        // serialize as `<call-ended>` WITHOUT `<call>` and be silently
        // dropped by the frontend. Guard on both columns to match the
        // wire condition exactly. A reply-only inbox row — a durable
        // user who received a thread reply but not the anchor-root
        // projection — has both NULL and is skipped.
        self.execute(
            "UPDATE inbox_entries SET call_ended_at = ?, call_duration = ? \
             WHERE partner_jid = ? AND thread_id = ? \
             AND call_thread_kind IS NOT NULL AND call_thread_media IS NOT NULL",
            crate::db_params![
                ended.timestamp(),
                duration.as_str().to_owned(),
                room.to_string(),
                thread_id.to_string(),
            ],
        )
        .await?;
        Ok(())
    }

    #[instrument(skip(self, user, partner), fields(user = %user, partner = %partner, thread_id))]
    async fn mark_direct_call_thread_ended(
        &self,
        user: &BareJid,
        partner: &BareJid,
        thread_id: &str,
        ended: DateTime<Utc>,
        duration: &CallThreadDuration,
    ) -> Result<(), InboxStorageError> {
        self.execute(
            "UPDATE inbox_entries SET call_ended_at = ?, call_duration = ? \
             WHERE user_jid = ? AND partner_jid = ? AND thread_id = ? \
             AND kind = 'direct' AND call_thread_kind = 'dm' AND call_thread_media IS NOT NULL",
            crate::db_params![
                ended.timestamp(),
                duration.as_str().to_owned(),
                user.to_string(),
                partner.to_string(),
                thread_id.to_string(),
            ],
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
