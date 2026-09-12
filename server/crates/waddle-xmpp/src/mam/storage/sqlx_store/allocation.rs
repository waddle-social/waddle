use jid::BareJid;
use sqlx::{PgConnection, SqliteConnection};
use waddle_xmpp_core::mam::ArchiveOrdinal;

const ALLOCATE: &str = "INSERT INTO mam_archive_sequences (archive_jid, next_seq) VALUES ($1, 1) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = mam_archive_sequences.next_seq + 1 RETURNING next_seq";

pub(super) fn decode_ordinal(value: i64) -> Result<ArchiveOrdinal, sqlx::Error> {
    ArchiveOrdinal::from_storage(value).map_err(|error| sqlx::Error::Decode(Box::new(error)))
}

pub(super) async fn allocate_postgres(
    conn: &mut PgConnection,
    archive: &BareJid,
) -> Result<ArchiveOrdinal, sqlx::Error> {
    decode_ordinal(
        sqlx::query_scalar(ALLOCATE)
            .bind(archive.to_string())
            .fetch_one(conn)
            .await?,
    )
}

pub(super) async fn allocate_sqlite(
    conn: &mut SqliteConnection,
    archive: &BareJid,
) -> Result<ArchiveOrdinal, sqlx::Error> {
    decode_ordinal(
        sqlx::query_scalar(ALLOCATE)
            .bind(archive.to_string())
            .fetch_one(conn)
            .await?,
    )
}

pub(super) async fn preserve_postgres(
    conn: &mut PgConnection,
    archive: &BareJid,
    ordinal: ArchiveOrdinal,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO mam_archive_sequences (archive_jid, next_seq) VALUES ($1, $2) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = GREATEST(mam_archive_sequences.next_seq, EXCLUDED.next_seq)")
        .bind(archive.to_string()).bind(ordinal.to_storage()).execute(conn).await?;
    Ok(())
}

pub(super) async fn preserve_sqlite(
    conn: &mut SqliteConnection,
    archive: &BareJid,
    ordinal: ArchiveOrdinal,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO mam_archive_sequences (archive_jid, next_seq) VALUES ($1, $2) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = MAX(mam_archive_sequences.next_seq, excluded.next_seq)")
        .bind(archive.to_string()).bind(ordinal.to_storage()).execute(conn).await?;
    Ok(())
}
