use jid::BareJid;
use sqlx::{PgConnection, SqliteConnection};
use waddle_xmpp_core::mam::ArchivedMessage;
use waddle_xmpp_core::xep0359::StanzaId;

use super::decode::{decode_postgres_message_row, decode_sqlite_message_row};
use super::schema::SELECT_COLUMNS;
use super::SqlxMamStorage;
use crate::mam::storage::MamStorageError;

impl SqlxMamStorage {
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

pub(super) async fn sqlite(
    conn: &mut SqliteConnection,
    archive: &BareJid,
    id: &str,
) -> Result<Option<ArchivedMessage>, MamStorageError> {
    sqlite_in_relation(conn, "mam_messages", archive, id).await
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
