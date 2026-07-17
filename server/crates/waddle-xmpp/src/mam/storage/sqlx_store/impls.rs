use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jid::BareJid;
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;
use sqlx::{Postgres, QueryBuilder, Sqlite};
use tracing::{debug, instrument};
use waddle_xmpp_core::mam::{ArchivedMessage, MamQuery, MamResult};
use waddle_xmpp_core::xep0359::OriginId;

use crate::mam::storage::query_semantics::{finalize_result, uses_backward_pagination};
use crate::mam::storage::{MamStorage, MamStorageError, StoreOutcome, TerminalTombstoneOutcome};
use crate::muc::RoomClaimFenceContext;

use super::decode::{decode_postgres_message_row, decode_sqlite_message_row};
use super::query::{
    ensure_postgres_requested_ids_exist, ensure_sqlite_requested_ids_exist, escape_like_pattern,
    fetch_postgres_cursor, fetch_sqlite_cursor, push_postgres_mam_filters, push_sqlite_mam_filters,
    WithFilter,
};
use super::schema::SELECT_COLUMNS;
use super::{MamDatabaseBackend, SqlxMamStorage};

#[async_trait]
impl MamStorage for SqlxMamStorage {
    #[instrument(skip(self, message), fields(archive = %archive_jid))]
    async fn store_message(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<StoreOutcome, MamStorageError> {
        super::write::store_message(&self.backend, archive_jid, message).await
    }

    #[instrument(skip(self, message, fence), fields(archive = %archive_jid))]
    async fn store_message_fenced(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
        fence: &RoomClaimFenceContext,
    ) -> Result<StoreOutcome, MamStorageError> {
        if !self.fencing_enabled {
            return self.store_message(archive_jid, message).await;
        }
        super::write::store_message_fenced(&self.backend, archive_jid, message, fence).await
    }

    #[instrument(skip(self), fields(archive = %archive_jid))]
    async fn query_messages(
        &self,
        archive_jid: &BareJid,
        archive_kind: crate::mam::MamArchiveKind,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError> {
        let limit = i64::from(query.max.unwrap_or(100).min(500)) + 1;
        // XEP-0313 §4.3.1 `with` filter: an owner-equivalent `with`
        // requires both endpoints to match the owner's bare JID. Otherwise,
        // a bare `with` matches the
        // bare form of the archived sender/recipient (so the row's
        // resource can be anything, or absent); a full `with` matches
        // only the exact full JID. The strings are stable for sqlx's
        // bind lifetime requirements.
        //
        // NOTE: the previous shape was `LIKE '{with}%'` (a single
        // textual prefix bind). That collided across JID structure —
        // `alice@example.com` would match `alice@example.com.evil/...`
        // because the prefix overlaps. The corrected shape is exact
        // equality plus a `LIKE 'bare/%'` for the resource form.
        let with_filter = query
            .with
            .as_ref()
            .map(|with| WithFilter::new(archive_jid, archive_kind, with));
        let archive_jid_str = archive_jid.to_string();

        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let filter_before_cursor = match query.filter_before_id.as_deref() {
                    Some(before_id) => {
                        Some(fetch_sqlite_cursor(pool, archive_jid, before_id).await?)
                    }
                    None => None,
                };
                let filter_after_cursor = match query.filter_after_id.as_deref() {
                    Some(after_id) => Some(fetch_sqlite_cursor(pool, archive_jid, after_id).await?),
                    None => None,
                };
                ensure_sqlite_requested_ids_exist(pool, archive_jid, &query.ids).await?;

                let mut count_builder = QueryBuilder::<Sqlite>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                push_sqlite_mam_filters(
                    &mut count_builder,
                    archive_jid_str.as_str(),
                    query,
                    with_filter.as_ref(),
                    filter_before_cursor.as_ref(),
                    filter_after_cursor.as_ref(),
                );
                let count = count_builder
                    .build_query_scalar::<i64>()
                    .fetch_one(pool)
                    .await?;

                let mut builder = QueryBuilder::<Sqlite>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
                ));
                push_sqlite_mam_filters(
                    &mut builder,
                    archive_jid_str.as_str(),
                    query,
                    with_filter.as_ref(),
                    filter_before_cursor.as_ref(),
                    filter_after_cursor.as_ref(),
                );
                if let Some(before_id) = query.before_id.as_deref().filter(|id| !id.is_empty()) {
                    let cursor = fetch_sqlite_cursor(pool, archive_jid, before_id).await?;
                    builder
                        .push(" AND (timestamp < ")
                        .push_bind(cursor.timestamp.to_rfc3339())
                        .push(" OR (timestamp = ")
                        .push_bind(cursor.timestamp.to_rfc3339())
                        .push(" AND id < ")
                        .push_bind(cursor.id)
                        .push("))");
                }
                if let Some(after_id) = query.after_id.as_deref() {
                    let cursor = fetch_sqlite_cursor(pool, archive_jid, after_id).await?;
                    builder
                        .push(" AND (timestamp > ")
                        .push_bind(cursor.timestamp.to_rfc3339())
                        .push(" OR (timestamp = ")
                        .push_bind(cursor.timestamp.to_rfc3339())
                        .push(" AND id > ")
                        .push_bind(cursor.id)
                        .push("))");
                }
                // XEP-0313 §archive_order: chronological order primary, archive
                // id as deterministic tiebreak for tied timestamps.
                builder.push(if uses_backward_pagination(query) {
                    " ORDER BY timestamp DESC, id DESC"
                } else {
                    " ORDER BY timestamp ASC, id ASC"
                });
                builder.push(" LIMIT ").push_bind(limit);

                let rows: Vec<SqliteRow> = builder.build().fetch_all(pool).await?;
                let messages = rows
                    .iter()
                    .map(decode_sqlite_message_row)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(finalize_result(
                    messages,
                    query,
                    Some(u32::try_from(count).unwrap_or(u32::MAX)),
                ))
            }
            MamDatabaseBackend::Postgres(pool) => {
                let filter_before_cursor = match query.filter_before_id.as_deref() {
                    Some(before_id) => {
                        Some(fetch_postgres_cursor(pool, archive_jid, before_id).await?)
                    }
                    None => None,
                };
                let filter_after_cursor = match query.filter_after_id.as_deref() {
                    Some(after_id) => {
                        Some(fetch_postgres_cursor(pool, archive_jid, after_id).await?)
                    }
                    None => None,
                };
                ensure_postgres_requested_ids_exist(pool, archive_jid, &query.ids).await?;

                let mut count_builder = QueryBuilder::<Postgres>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                push_postgres_mam_filters(
                    &mut count_builder,
                    archive_jid_str.as_str(),
                    query,
                    with_filter.as_ref(),
                    filter_before_cursor.as_ref(),
                    filter_after_cursor.as_ref(),
                );
                let count = count_builder
                    .build_query_scalar::<i64>()
                    .fetch_one(pool)
                    .await?;

                let mut builder = QueryBuilder::<Postgres>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
                ));
                push_postgres_mam_filters(
                    &mut builder,
                    archive_jid_str.as_str(),
                    query,
                    with_filter.as_ref(),
                    filter_before_cursor.as_ref(),
                    filter_after_cursor.as_ref(),
                );
                if let Some(before_id) = query.before_id.as_deref().filter(|id| !id.is_empty()) {
                    let cursor = fetch_postgres_cursor(pool, archive_jid, before_id).await?;
                    builder
                        .push(" AND (timestamp < ")
                        .push_bind(cursor.timestamp)
                        .push(" OR (timestamp = ")
                        .push_bind(cursor.timestamp)
                        .push(" AND id < ")
                        .push_bind(cursor.id)
                        .push("))");
                }
                if let Some(after_id) = query.after_id.as_deref() {
                    let cursor = fetch_postgres_cursor(pool, archive_jid, after_id).await?;
                    builder
                        .push(" AND (timestamp > ")
                        .push_bind(cursor.timestamp)
                        .push(" OR (timestamp = ")
                        .push_bind(cursor.timestamp)
                        .push(" AND id > ")
                        .push_bind(cursor.id)
                        .push("))");
                }
                // XEP-0313 §archive_order: chronological order primary, archive
                // id as deterministic tiebreak for tied timestamps.
                builder.push(if uses_backward_pagination(query) {
                    " ORDER BY timestamp DESC, id DESC"
                } else {
                    " ORDER BY timestamp ASC, id ASC"
                });
                builder.push(" LIMIT ").push_bind(limit);

                let rows: Vec<PgRow> = builder.build().fetch_all(pool).await?;
                let messages = rows
                    .iter()
                    .map(decode_postgres_message_row)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(finalize_result(
                    messages,
                    query,
                    Some(u32::try_from(count).unwrap_or(u32::MAX)),
                ))
            }
        }
    }

    #[instrument(skip(self))]
    async fn get_message(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder = QueryBuilder::<Sqlite>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE id = "
                ));
                builder.push_bind(archive_id);
                let row = builder.build().fetch_optional(pool).await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE id = "
                ));
                builder.push_bind(archive_id);
                let row = builder.build().fetch_optional(pool).await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = ? AND (stanza_id = ? OR origin_id = ?) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid_str.as_str())
                .bind(stanza_id)
                .bind(stanza_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = $1 AND (stanza_id = $2 OR origin_id = $2) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid_str.as_str())
                .bind(stanza_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = ? AND stanza_id = ? ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid_str.as_str())
                .bind(message_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = $1 AND stanza_id = $2 ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid_str.as_str())
                .bind(message_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    async fn get_message_by_sender_and_origin_id(
        &self,
        archive_jid: &BareJid,
        archive_kind: crate::mam::MamArchiveKind,
        sender: &jid::Jid,
        origin_id: &OriginId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid = archive_jid.to_string();
        let sender_full = sender.to_string();
        let sender_bare = sender.to_bare().to_string();
        let sender_resource_prefix = format!("{}/%", escape_like_pattern(&sender_bare));
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder = QueryBuilder::<Sqlite>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
                ));
                builder
                    .push_bind(archive_jid.as_str())
                    .push(" AND (origin_id = ")
                    .push_bind(origin_id.as_str())
                    .push(" OR stanza_id = ")
                    .push_bind(origin_id.as_str())
                    .push(") AND ");
                match archive_kind {
                    crate::mam::MamArchiveKind::Personal => {
                        builder
                            .push("(from_jid = ")
                            .push_bind(sender_bare.as_str())
                            .push(" OR from_jid LIKE ")
                            .push_bind(sender_resource_prefix.as_str())
                            .push(" ESCAPE '\\')");
                    }
                    crate::mam::MamArchiveKind::Room => {
                        builder.push("from_jid = ").push_bind(sender_full.as_str());
                    }
                }
                builder
                    .push(" ORDER BY CASE WHEN origin_id = ")
                    .push_bind(origin_id.as_str())
                    .push(" THEN 0 ELSE 1 END, timestamp ASC, id ASC LIMIT 1");
                let row = builder.build().fetch_optional(pool).await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
                ));
                builder
                    .push_bind(archive_jid.as_str())
                    .push(" AND (origin_id = ")
                    .push_bind(origin_id.as_str())
                    .push(" OR stanza_id = ")
                    .push_bind(origin_id.as_str())
                    .push(") AND ");
                match archive_kind {
                    crate::mam::MamArchiveKind::Personal => {
                        builder
                            .push("(from_jid = ")
                            .push_bind(sender_bare.as_str())
                            .push(" OR from_jid LIKE ")
                            .push_bind(sender_resource_prefix.as_str())
                            .push(" ESCAPE '\\')");
                    }
                    crate::mam::MamArchiveKind::Room => {
                        builder.push("from_jid = ").push_bind(sender_full.as_str());
                    }
                }
                builder
                    .push(" ORDER BY CASE WHEN origin_id = ")
                    .push_bind(origin_id.as_str())
                    .push(" THEN 0 ELSE 1 END, timestamp ASC, id ASC LIMIT 1");
                let row = builder.build().fetch_optional(pool).await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive_jid_str = archive_jid.to_string();
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = ? AND (id = ? OR stanza_id = ?) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid_str.as_str())
                .bind(stanza_id)
                .bind(stanza_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_sqlite_message_row).transpose()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let row = sqlx::query(&format!(
                    "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = $1 AND (id = $2 OR stanza_id = $2) ORDER BY timestamp DESC LIMIT 1"
                ))
                .bind(archive_jid_str.as_str())
                .bind(stanza_id)
                .fetch_optional(pool)
                .await?;
                row.as_ref().map(decode_postgres_message_row).transpose()
            }
        }
    }

    #[instrument(skip(self))]
    async fn count_messages(&self, room_jid: &BareJid) -> Result<u32, MamStorageError> {
        let room_jid_str = room_jid.to_string();
        let count = match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder = QueryBuilder::<Sqlite>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                builder.push_bind(room_jid_str.as_str());
                builder.build_query_scalar::<i64>().fetch_one(pool).await?
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ",
                );
                builder.push_bind(room_jid_str.as_str());
                builder.build_query_scalar::<i64>().fetch_one(pool).await?
            }
        };

        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    #[instrument(skip(self))]
    async fn delete_before(
        &self,
        room_jid: &BareJid,
        before: DateTime<Utc>,
    ) -> Result<u64, MamStorageError> {
        let room_jid_str = room_jid.to_string();
        let deleted = match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder =
                    QueryBuilder::<Sqlite>::new("DELETE FROM mam_messages WHERE room_jid = ");
                builder
                    .push_bind(room_jid_str.as_str())
                    .push(" AND timestamp < ")
                    .push_bind(before.to_rfc3339());
                builder.build().execute(pool).await?.rows_affected()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder =
                    QueryBuilder::<Postgres>::new("DELETE FROM mam_messages WHERE room_jid = ");
                builder
                    .push_bind(room_jid_str.as_str())
                    .push(" AND timestamp < ")
                    .push_bind(before);
                builder.build().execute(pool).await?.rows_affected()
            }
        };

        debug!(archive = %room_jid, deleted, "Deleted old messages from MAM archive");
        Ok(deleted)
    }

    #[instrument(skip(self, tombstone))]
    async fn replace_with_tombstone(
        &self,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<bool, MamStorageError> {
        super::write::replace_with_tombstone(&self.backend, archive_id, tombstone).await
    }

    #[instrument(skip(self, tombstone))]
    async fn replace_with_terminal_tombstone(
        &self,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<TerminalTombstoneOutcome, MamStorageError> {
        super::write::replace_with_terminal_tombstone(&self.backend, archive_id, tombstone).await
    }
}
