//! MAM pagination on a caller-owned PostgreSQL transaction.

use std::collections::HashSet;

use jid::BareJid;
use sqlx::{Connection as _, PgConnection, Postgres, QueryBuilder};
use waddle_xmpp_core::mam::{ArchiveOrdinal, MamQuery, MamResult};

use super::decode::decode_postgres_message_row;
use super::query::{push_postgres_mam_filters, WithFilter};
use super::schema::SELECT_COLUMNS;
use super::{postgres_relation, SqlxMamStorage};
use crate::mam::storage::query_semantics::{
    finalize_result, missing_requested_id_in_set, uses_backward_pagination,
};
use crate::mam::storage::{MamArchiveKind, MamStorageError};

impl SqlxMamStorage {
    /// Query a MAM archive using one caller-owned PostgreSQL transaction.
    ///
    /// The schema must come from the caller's independently attested database
    /// identity. All relations are schema-qualified, and this method neither
    /// opens a pool nor commits, rolls back, initializes, or repairs schema.
    /// The caller chooses isolation; a repeatable-read transaction keeps cursor,
    /// count, and page observations in one snapshot.
    pub async fn query_on_postgres_connection_in_schema(
        conn: &mut PgConnection,
        schema: &str,
        archive: &BareJid,
        archive_kind: MamArchiveKind,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError> {
        if !conn.is_in_transaction() {
            return Err(MamStorageError::Database(
                "MAM query requires a caller-owned transaction".to_owned(),
            ));
        }
        let relation = postgres_relation(Some(schema), "mam_messages");
        let filter_before = match query.filter_before_id.as_deref() {
            Some(id) => Some(cursor(conn, &relation, archive, id).await?),
            None => None,
        };
        let filter_after = match query.filter_after_id.as_deref() {
            Some(id) => Some(cursor(conn, &relation, archive, id).await?),
            None => None,
        };
        ensure_requested_ids_exist(conn, &relation, archive, &query.ids).await?;

        let with_filter = query
            .with
            .as_ref()
            .map(|with| WithFilter::new(archive, archive_kind, with));
        let archive_jid = archive.to_string();
        let mut count = QueryBuilder::<Postgres>::new(format!(
            "SELECT COUNT(*) FROM {relation} WHERE room_jid = "
        ));
        push_postgres_mam_filters(
            &mut count,
            &archive_jid,
            query,
            with_filter.as_ref(),
            filter_before,
            filter_after,
        );
        let count = count
            .build_query_scalar::<i64>()
            .fetch_one(&mut *conn)
            .await?;

        let mut page = QueryBuilder::<Postgres>::new(format!(
            "SELECT {SELECT_COLUMNS} FROM {relation} WHERE room_jid = "
        ));
        push_postgres_mam_filters(
            &mut page,
            &archive_jid,
            query,
            with_filter.as_ref(),
            filter_before,
            filter_after,
        );
        if let Some(id) = query.before_id.as_deref().filter(|id| !id.is_empty()) {
            let position = cursor(conn, &relation, archive, id).await?;
            page.push(" AND archive_seq < ")
                .push_bind(position.to_storage());
        }
        if let Some(id) = query.after_id.as_deref() {
            let position = cursor(conn, &relation, archive, id).await?;
            page.push(" AND archive_seq > ")
                .push_bind(position.to_storage());
        }
        page.push(if uses_backward_pagination(query) {
            " ORDER BY archive_seq DESC"
        } else {
            " ORDER BY archive_seq ASC"
        });
        page.push(" LIMIT ")
            .push_bind(i64::from(query.max.unwrap_or(100).min(500)) + 1);
        let rows = page.build().fetch_all(&mut *conn).await?;
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

async fn cursor(
    conn: &mut PgConnection,
    relation: &str,
    archive: &BareJid,
    id: &str,
) -> Result<ArchiveOrdinal, MamStorageError> {
    let mut query = QueryBuilder::<Postgres>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM {relation} WHERE room_jid = "
    ));
    query.push_bind(archive.to_string());
    query.push(" AND id = ").push_bind(id);
    let row = query.build().fetch_optional(conn).await?;
    let message = row
        .as_ref()
        .map(decode_postgres_message_row)
        .transpose()?
        .ok_or_else(|| MamStorageError::NotFound(id.to_owned()))?;
    message
        .ordinal
        .ok_or_else(|| MamStorageError::MissingOrdinal(message.id))
}

async fn ensure_requested_ids_exist(
    conn: &mut PgConnection,
    relation: &str,
    archive: &BareJid,
    requested: &[String],
) -> Result<(), MamStorageError> {
    if requested.is_empty() {
        return Ok(());
    }
    let mut query =
        QueryBuilder::<Postgres>::new(format!("SELECT id FROM {relation} WHERE room_jid = "));
    query.push_bind(archive.to_string());
    query.push(" AND id IN (");
    let mut ids = query.separated(", ");
    for id in requested {
        ids.push_bind(id.as_str());
    }
    ids.push_unseparated(")");
    let available = query.build_query_scalar::<String>().fetch_all(conn).await?;
    let available = available.iter().map(String::as_str).collect::<HashSet<_>>();
    if let Some(missing) = missing_requested_id_in_set(&available, requested) {
        return Err(MamStorageError::NotFound(missing));
    }
    Ok(())
}
