use jid::BareJid;
use sqlx::{PgConnection, SqliteConnection};
use waddle_xmpp_core::mam::ArchiveOrdinal;

/// Take the archive's counter row lock without allocating, creating the row at
/// zero (no ordinal allocated yet) when the archive has never been written.
/// Callers lock several archives in one canonical order before any insert so
/// opposite-direction transactions cannot deadlock on the counters.
const LOCK: &str = "INSERT INTO mam_archive_sequences (archive_jid, next_seq) VALUES ($1, 0) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = mam_archive_sequences.next_seq";

const ALLOCATE: &str = "INSERT INTO mam_archive_sequences (archive_jid, next_seq) VALUES ($1, 1) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = mam_archive_sequences.next_seq + 1 RETURNING next_seq";

pub async fn lock_archive_sequence_on_connection(
    conn: &mut PgConnection,
    archive: &BareJid,
) -> Result<(), sqlx::Error> {
    sqlx::query(LOCK)
        .bind(archive.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn lock_archive_sequence_on_sqlite_connection(
    conn: &mut SqliteConnection,
    archive: &BareJid,
) -> Result<(), sqlx::Error> {
    sqlx::query(LOCK)
        .bind(archive.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

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
