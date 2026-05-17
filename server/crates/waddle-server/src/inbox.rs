//! Database-backed storage for Waddle inbox projections.

use std::path::Path;
use std::sync::Arc;

use crate::db::{DatabaseConfig, DatabaseDriver, IntoParams};
use async_trait::async_trait;
use jid::BareJid;
use tracing::{info, instrument};
use waddle_xmpp::inbox::storage::{InboxStorage, InboxStorageError};
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};

use crate::db::Database;

mod codec;
mod open;
mod schema;

use codec::{decode_row, encode_kind, SELECT_COLS};
pub(crate) use codec::{decode_row as decode_inbox_row, SELECT_COLS as INBOX_SELECT_COLS};
pub use open::build_inbox_storage;

#[derive(Clone)]
pub struct DatabaseInboxStorage {
    db: Database,
}

impl DatabaseInboxStorage {
    /// Borrow the underlying logical database — used by sibling
    /// projections (e.g. the threads view) so they share the same pool
    /// and schema lifecycle.
    pub fn db_handle(&self) -> Database {
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

#[async_trait]
impl InboxStorage for DatabaseInboxStorage {
    #[instrument(skip(self), fields(user = %user))]
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

    #[instrument(skip(self), fields(user = %user, room = %room))]
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

    #[instrument(skip(self, entry), fields(user = %user, partner = %entry.partner))]
    async fn upsert(
        &self,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
    ) -> Result<InboxEntry, InboxStorageError> {
        let increment = i64::from(u8::from(increment_unread));
        let thread_id = entry.thread_id.as_deref().unwrap_or("");
        let is_thread = i64::from(u8::from(!thread_id.is_empty()));
        let sql = format!(
            r#"
            INSERT INTO inbox_entries (
                user_jid, partner_jid, thread_id, kind, last_stanza_id, last_updated,
                unread, preview, thread_title, reply_count, author
            ) VALUES (?, ?, ?, ?, ?, ?, CASE WHEN ? != 0 THEN 1 ELSE 0 END, ?, ?, ?, ?)
            ON CONFLICT(user_jid, partner_jid, thread_id) DO UPDATE SET
                kind = excluded.kind,
                last_stanza_id = excluded.last_stanza_id,
                last_updated = excluded.last_updated,
                preview = excluded.preview,
                unread = CASE
                    WHEN ? != 0 THEN inbox_entries.unread + 1
                    ELSE inbox_entries.unread
                END,
                thread_title = COALESCE(excluded.thread_title, inbox_entries.thread_title),
                reply_count = CASE
                    WHEN ? != 0 THEN inbox_entries.reply_count + 1
                    ELSE inbox_entries.reply_count
                END,
                author = COALESCE(excluded.author, inbox_entries.author)
            RETURNING {SELECT_COLS}
            "#
        );
        let mut rows = self
            .query(
                &sql,
                crate::db_params![
                    user.to_string(),
                    entry.partner.to_string(),
                    thread_id.to_string(),
                    encode_kind(entry.kind),
                    entry.last_stanza_id,
                    entry.last_updated,
                    increment,
                    entry.preview,
                    entry.thread_title,
                    entry.reply_count as i64,
                    entry.author,
                    increment,
                    is_thread,
                ],
            )
            .await?;

        let row = rows
            .next()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?
            .ok_or_else(|| InboxStorageError::Other("RETURNING produced no row".to_string()))?;

        decode_row(&row)
    }

    #[instrument(skip(self), fields(user = %user, partner = %partner))]
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

    #[instrument(skip(self), fields(user = %user))]
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
}

#[cfg(test)]
mod tests;
