use jid::BareJid;
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, Sqlite, SqliteConnection};
use waddle_xmpp_core::mam::ArchivedMessage;
use waddle_xmpp_core::xep0359::{OriginId, StanzaId};

use super::decode::{decode_postgres_message_row, decode_sqlite_message_row};
use super::query::escape_like_pattern;
use super::schema::SELECT_COLUMNS;
use super::SqlxMamStorage;
use crate::mam::storage::{BoundedMamLookupError, MamArchiveKind, MamStorageError};

// This is a refusal threshold, not a guarantee about all historical rows.
// A stored message can contain several copies of one inbound frame. The
// bounded path allows up to sixteen times the 1 MiB frame ceiling in total.
const BOUNDED_KEY_BYTES: i64 = 1024 * 1024;
const BOUNDED_ROW_BYTES: i64 = 16 * 1024 * 1024;

// Every CTE and the final projection run in the same statement snapshot. The
// predicates remain indexable; callers must prove the heap and index catalog
// under the independently enforced mutation barrier described below.
const BOUNDED_LOOKUP: &str = r#"
WITH candidate AS MATERIALIZED (
  SELECT ctid FROM ONLY __MAM_RELATION__
  WHERE room_jid=$1 AND (id=$2 OR stanza_id=$2)
  ORDER BY archive_seq DESC LIMIT 1
), measured AS MATERIALIZED (
  SELECT m.ctid,
    COALESCE(pg_catalog.octet_length(m.id),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.room_jid),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.from_jid),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.to_jid),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.body),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.stanza_id),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.thread_id),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.reply_to_id),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.reply_to_jid),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.origin_id),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.message_type),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.stanza_xml),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.rich_payload),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.parent_thread_id),0)::pg_catalog.int8 AS bytes
  FROM ONLY __MAM_RELATION__ m JOIN candidate c ON m.ctid=c.ctid
)
SELECT CASE WHEN measured.bytes <= $3 THEN m.id END,
       CASE WHEN measured.bytes <= $3 THEN m.room_jid END,
       m.timestamp,
       CASE WHEN measured.bytes <= $3 THEN m.from_jid END,
       CASE WHEN measured.bytes <= $3 THEN m.to_jid END,
       CASE WHEN measured.bytes <= $3 THEN m.body END,
       CASE WHEN measured.bytes <= $3 THEN m.stanza_id END,
       CASE WHEN measured.bytes <= $3 THEN m.thread_id END,
       CASE WHEN measured.bytes <= $3 THEN m.reply_to_id END,
       CASE WHEN measured.bytes <= $3 THEN m.reply_to_jid END,
       CASE WHEN measured.bytes <= $3 THEN m.origin_id END,
       CASE WHEN measured.bytes <= $3 THEN m.message_type END,
       CASE WHEN measured.bytes <= $3 THEN m.stanza_xml END,
       CASE WHEN measured.bytes <= $3 THEN m.rich_payload END,
       m.nickname_generation,
       CASE WHEN measured.bytes <= $3 THEN m.parent_thread_id END,
       m.archive_seq, measured.bytes > $3
FROM ONLY __MAM_RELATION__ m JOIN measured ON m.ctid=measured.ctid
"#;

impl SqlxMamStorage {
    /// Read a message by archive primary key on a caller-owned PostgreSQL
    /// connection. The caller-selected schema is quoted in the SQL relation.
    pub async fn get_message_on_postgres_connection_in_schema(
        conn: &mut PgConnection,
        schema: &str,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        get_postgres(conn, Some(schema), archive_id).await
    }

    /// Read a message by archive primary key on a caller-owned SQLite connection.
    pub async fn get_message_on_sqlite_connection(
        conn: &mut SqliteConnection,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        get_sqlite(conn, archive_id).await
    }

    pub async fn get_message_by_stanza_id_on_postgres_connection_in_schema(
        conn: &mut PgConnection,
        schema: &str,
        archive: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        get_postgres_by_stanza_id(conn, Some(schema), archive, stanza_id).await
    }

    pub async fn get_message_by_stanza_id_on_sqlite_connection(
        conn: &mut SqliteConnection,
        archive: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        get_sqlite_by_stanza_id(conn, archive, stanza_id).await
    }

    pub async fn get_message_by_message_id_on_postgres_connection_in_schema(
        conn: &mut PgConnection,
        schema: &str,
        archive: &BareJid,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        get_postgres_by_message_id(conn, Some(schema), archive, message_id).await
    }

    pub async fn get_message_by_message_id_on_sqlite_connection(
        conn: &mut SqliteConnection,
        archive: &BareJid,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        get_sqlite_by_message_id(conn, archive, message_id).await
    }

    pub async fn get_message_by_sender_and_origin_id_on_postgres_connection_in_schema(
        conn: &mut PgConnection,
        schema: &str,
        archive: &BareJid,
        archive_kind: MamArchiveKind,
        sender: &jid::Jid,
        origin_id: &OriginId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        get_postgres_by_sender_and_origin_id(
            conn,
            Some(schema),
            archive,
            archive_kind,
            sender,
            origin_id,
        )
        .await
    }

    pub async fn get_message_by_sender_and_origin_id_on_sqlite_connection(
        conn: &mut SqliteConnection,
        archive: &BareJid,
        archive_kind: MamArchiveKind,
        sender: &jid::Jid,
        origin_id: &OriginId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        get_sqlite_by_sender_and_origin_id(conn, archive, archive_kind, sender, origin_id).await
    }

    pub async fn get_message_by_archive_or_stanza_id_on_postgres_connection_in_schema(
        conn: &mut PgConnection,
        schema: &str,
        archive: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        postgres(conn, Some(schema), archive, stanza_id).await
    }

    pub async fn get_message_by_archive_or_stanza_id_on_sqlite_connection(
        conn: &mut SqliteConnection,
        archive: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        sqlite(conn, archive, stanza_id).await
    }

    /// Resolve an archive or wire stanza ID on the caller's admitted PostgreSQL
    /// connection. The schema must be selected from the caller's attested
    /// database capability, not from user input. Transaction ownership remains
    /// with the caller; this function performs no initialization.
    pub async fn lookup_archive_or_stanza_id_on_postgres_connection_in_schema(
        conn: &mut PgConnection,
        schema: &str,
        id: &StanzaId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive = id.by.to_bare();
        postgres(conn, Some(schema), &archive, id.as_str()).await
    }

    /// Pinned-use variant with an atomic, server-side bound on the returned
    /// text. The caller must already hold the catalog-proved `ONLY` heap
    /// `ACCESS SHARE` lock,
    /// prove the expected nine index identities and built-in index behavior,
    /// and use a `pg_catalog`-first path. An independently enforced mutation
    /// or quiescence barrier must cover preflight through transaction rollback:
    /// the heap lock alone does not prevent `CREATE INDEX CONCURRENTLY`,
    /// `DROP INDEX CONCURRENTLY`, or `REINDEX CONCURRENTLY`. Request keys are
    /// byte-limited; comparisons use catalog-proved built-in text semantics
    /// and can use the checked indexes. The selected-row limit does not bound
    /// all server memory used when comparing stored keys.
    /// A row over the refusal threshold is an error, never a definitive miss.
    ///
    /// The candidate and measured CTEs materialize only a tuple identifier and
    /// scalar lengths, not the text values. PostgreSQL's `octet_length(text)`
    /// obtains the logical TOAST size without decompressing an oversized value.
    /// Every projected text field is guarded by a CASE before it is sent.
    pub async fn lookup_bounded_archive_or_stanza_id_on_postgres_connection_in_schema(
        conn: &mut PgConnection,
        schema: &str,
        id: &StanzaId,
    ) -> Result<Option<ArchivedMessage>, BoundedMamLookupError> {
        let archive = id.by.to_bare();
        if id.as_str().len() > BOUNDED_KEY_BYTES as usize
            || archive.as_str().len() > BOUNDED_KEY_BYTES as usize
        {
            return Err(BoundedMamLookupError::KeyTooLarge);
        }
        let sql = bounded_lookup_sql(schema);
        let row = sqlx::query(&sql)
            .bind(archive.as_str())
            .bind(id.as_str())
            .bind(BOUNDED_ROW_BYTES)
            .fetch_optional(conn)
            .await
            .map_err(MamStorageError::from)?;
        let Some(row) = row else {
            return Ok(None);
        };
        if row.try_get::<bool, _>(17).map_err(MamStorageError::from)? {
            return Err(BoundedMamLookupError::RowTooLarge);
        }
        Ok(Some(decode_postgres_message_row(&row)?))
    }

    /// Explain the exact bounded query against an existing test fixture.
    #[cfg(feature = "test-utils")]
    pub async fn explain_bounded_lookup_for_test(
        conn: &mut PgConnection,
        schema: &str,
        id: &StanzaId,
    ) -> Result<Vec<String>, MamStorageError> {
        let archive = id.by.to_bare();
        let sql = format!(
            "EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) {}",
            bounded_lookup_sql(schema)
        );
        Ok(sqlx::query_scalar(&sql)
            .bind(archive.as_str())
            .bind(id.as_str())
            .bind(BOUNDED_ROW_BYTES)
            .fetch_all(conn)
            .await?)
    }

    /// Explain the same query with unknown values as a generic PostgreSQL plan.
    #[cfg(feature = "test-utils")]
    pub async fn explain_generic_bounded_lookup_for_test(
        conn: &mut PgConnection,
        schema: &str,
    ) -> Result<Vec<String>, MamStorageError> {
        let sql = format!(
            "EXPLAIN (GENERIC_PLAN, COSTS OFF) {}",
            bounded_lookup_sql(schema)
        );
        sqlx::raw_sql(&sql)
            .fetch_all(conn)
            .await?
            .into_iter()
            .map(|row| row.try_get(0).map_err(MamStorageError::from))
            .collect()
    }

    /// Resolve an archive or wire stanza ID in SQLite's main database on the
    /// caller's admitted connection. Transaction ownership remains with the
    /// caller; this function performs no initialization.
    pub async fn lookup_archive_or_stanza_id_on_sqlite_connection(
        conn: &mut SqliteConnection,
        id: &StanzaId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let archive = id.by.to_bare();
        sqlite_in_relation(conn, "main.mam_messages", &archive, id.as_str()).await
    }
}

fn bounded_lookup_sql(schema: &str) -> String {
    let relation = super::postgres_relation(Some(schema), "mam_messages");
    BOUNDED_LOOKUP.replace("__MAM_RELATION__", &relation)
}

pub(super) async fn get_postgres(
    conn: &mut PgConnection,
    schema: Option<&str>,
    archive_id: &str,
) -> Result<Option<ArchivedMessage>, MamStorageError> {
    let relation = super::postgres_relation(schema, "mam_messages");
    let mut query = QueryBuilder::<Postgres>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM {relation} WHERE id = "
    ));
    query.push_bind(archive_id);
    let row = query.build().fetch_optional(conn).await?;
    row.as_ref().map(decode_postgres_message_row).transpose()
}

pub(super) async fn get_sqlite(
    conn: &mut SqliteConnection,
    archive_id: &str,
) -> Result<Option<ArchivedMessage>, MamStorageError> {
    let mut query = QueryBuilder::<Sqlite>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM main.mam_messages WHERE id = "
    ));
    query.push_bind(archive_id);
    let row = query.build().fetch_optional(conn).await?;
    row.as_ref().map(decode_sqlite_message_row).transpose()
}

pub(super) async fn get_postgres_by_stanza_id(
    conn: &mut PgConnection,
    schema: Option<&str>,
    archive: &BareJid,
    stanza_id: &str,
) -> Result<Option<ArchivedMessage>, MamStorageError> {
    let relation = super::postgres_relation(schema, "mam_messages");
    let mut query = QueryBuilder::<Postgres>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM {relation} WHERE room_jid = "
    ));
    query
        .push_bind(archive.to_string())
        .push(" AND (stanza_id = ")
        .push_bind(stanza_id)
        .push(" OR origin_id = ")
        .push_bind(stanza_id)
        .push(") ORDER BY archive_seq DESC LIMIT 1");
    let row = query.build().fetch_optional(conn).await?;
    row.as_ref().map(decode_postgres_message_row).transpose()
}

pub(super) async fn get_sqlite_by_stanza_id(
    conn: &mut SqliteConnection,
    archive: &BareJid,
    stanza_id: &str,
) -> Result<Option<ArchivedMessage>, MamStorageError> {
    let mut query = QueryBuilder::<Sqlite>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM main.mam_messages WHERE room_jid = "
    ));
    query
        .push_bind(archive.to_string())
        .push(" AND (stanza_id = ")
        .push_bind(stanza_id)
        .push(" OR origin_id = ")
        .push_bind(stanza_id)
        .push(") ORDER BY archive_seq DESC LIMIT 1");
    let row = query.build().fetch_optional(conn).await?;
    row.as_ref().map(decode_sqlite_message_row).transpose()
}

pub(super) async fn get_postgres_by_message_id(
    conn: &mut PgConnection,
    schema: Option<&str>,
    archive: &BareJid,
    message_id: &str,
) -> Result<Option<ArchivedMessage>, MamStorageError> {
    let relation = super::postgres_relation(schema, "mam_messages");
    let mut query = QueryBuilder::<Postgres>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM {relation} WHERE room_jid = "
    ));
    query
        .push_bind(archive.to_string())
        .push(" AND stanza_id = ")
        .push_bind(message_id)
        .push(" ORDER BY archive_seq DESC LIMIT 1");
    let row = query.build().fetch_optional(conn).await?;
    row.as_ref().map(decode_postgres_message_row).transpose()
}

pub(super) async fn get_sqlite_by_message_id(
    conn: &mut SqliteConnection,
    archive: &BareJid,
    message_id: &str,
) -> Result<Option<ArchivedMessage>, MamStorageError> {
    let mut query = QueryBuilder::<Sqlite>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM main.mam_messages WHERE room_jid = "
    ));
    query
        .push_bind(archive.to_string())
        .push(" AND stanza_id = ")
        .push_bind(message_id)
        .push(" ORDER BY archive_seq DESC LIMIT 1");
    let row = query.build().fetch_optional(conn).await?;
    row.as_ref().map(decode_sqlite_message_row).transpose()
}

pub(super) async fn get_postgres_by_sender_and_origin_id(
    conn: &mut PgConnection,
    schema: Option<&str>,
    archive: &BareJid,
    archive_kind: MamArchiveKind,
    sender: &jid::Jid,
    origin_id: &OriginId,
) -> Result<Option<ArchivedMessage>, MamStorageError> {
    let relation = super::postgres_relation(schema, "mam_messages");
    let sender_full = sender.to_string();
    let sender_bare = sender.to_bare().to_string();
    let resource_prefix = format!("{}/%", escape_like_pattern(&sender_bare));
    let mut query = QueryBuilder::<Postgres>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM {relation} WHERE room_jid = "
    ));
    query
        .push_bind(archive.to_string())
        .push(" AND (origin_id = ")
        .push_bind(origin_id.as_str())
        .push(" OR stanza_id = ")
        .push_bind(origin_id.as_str())
        .push(") AND ");
    match archive_kind {
        MamArchiveKind::Personal => {
            query
                .push("(from_jid = ")
                .push_bind(sender_bare.as_str())
                .push(" OR from_jid LIKE ")
                .push_bind(resource_prefix.as_str())
                .push(" ESCAPE '\\')");
        }
        MamArchiveKind::Room => {
            query.push("from_jid = ").push_bind(sender_full.as_str());
        }
    }
    query
        .push(" ORDER BY CASE WHEN origin_id = ")
        .push_bind(origin_id.as_str())
        .push(" THEN 0 ELSE 1 END, archive_seq ASC LIMIT 1");
    let row = query.build().fetch_optional(conn).await?;
    row.as_ref().map(decode_postgres_message_row).transpose()
}

pub(super) async fn get_sqlite_by_sender_and_origin_id(
    conn: &mut SqliteConnection,
    archive: &BareJid,
    archive_kind: MamArchiveKind,
    sender: &jid::Jid,
    origin_id: &OriginId,
) -> Result<Option<ArchivedMessage>, MamStorageError> {
    let sender_full = sender.to_string();
    let sender_bare = sender.to_bare().to_string();
    let resource_prefix = format!("{}/%", escape_like_pattern(&sender_bare));
    let mut query = QueryBuilder::<Sqlite>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM main.mam_messages WHERE room_jid = "
    ));
    query
        .push_bind(archive.to_string())
        .push(" AND (origin_id = ")
        .push_bind(origin_id.as_str())
        .push(" OR stanza_id = ")
        .push_bind(origin_id.as_str())
        .push(") AND ");
    match archive_kind {
        MamArchiveKind::Personal => {
            query
                .push("(from_jid = ")
                .push_bind(sender_bare.as_str())
                .push(" OR from_jid LIKE ")
                .push_bind(resource_prefix.as_str())
                .push(" ESCAPE '\\')");
        }
        MamArchiveKind::Room => {
            query.push("from_jid = ").push_bind(sender_full.as_str());
        }
    }
    query
        .push(" ORDER BY CASE WHEN origin_id = ")
        .push_bind(origin_id.as_str())
        .push(" THEN 0 ELSE 1 END, archive_seq ASC LIMIT 1");
    let row = query.build().fetch_optional(conn).await?;
    row.as_ref().map(decode_sqlite_message_row).transpose()
}

pub(super) async fn sqlite(
    conn: &mut SqliteConnection,
    archive: &BareJid,
    id: &str,
) -> Result<Option<ArchivedMessage>, MamStorageError> {
    sqlite_in_relation(conn, "main.mam_messages", archive, id).await
}

async fn sqlite_in_relation(
    conn: &mut SqliteConnection,
    relation: &'static str,
    archive: &BareJid,
    id: &str,
) -> Result<Option<ArchivedMessage>, MamStorageError> {
    let row = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM {relation} WHERE room_jid = ? AND (id = ? OR stanza_id = ?) ORDER BY archive_seq DESC LIMIT 1"
    ))
    .bind(archive.as_str())
    .bind(id)
    .bind(id)
    .fetch_optional(conn)
    .await?;
    row.as_ref().map(decode_sqlite_message_row).transpose()
}

pub(super) async fn postgres(
    conn: &mut PgConnection,
    schema: Option<&str>,
    archive: &BareJid,
    id: &str,
) -> Result<Option<ArchivedMessage>, MamStorageError> {
    let relation = super::postgres_relation(schema, "mam_messages");
    let row = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM {relation} WHERE room_jid = $1 AND (id = $2 OR stanza_id = $2) ORDER BY archive_seq DESC LIMIT 1"
    ))
    .bind(archive.as_str())
    .bind(id)
    .fetch_optional(conn)
    .await?;
    row.as_ref().map(decode_postgres_message_row).transpose()
}
