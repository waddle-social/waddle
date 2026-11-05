use jid::BareJid;
use sqlx::{PgConnection, Postgres, QueryBuilder, Sqlite, SqliteConnection};
use waddle_xmpp_core::mam::ArchivedMessage;
use waddle_xmpp_core::xep0359::{OriginId, StanzaId};

use super::decode::{decode_postgres_message_row, decode_sqlite_message_row};
use super::query::escape_like_pattern;
use super::schema::SELECT_COLUMNS;
use super::SqlxMamStorage;
use crate::mam::storage::{MamArchiveKind, MamStorageError};

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
