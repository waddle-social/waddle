use jid::BareJid;
use sqlx::{Connection, PgConnection, SqliteConnection};
use waddle_xmpp_core::mam::ArchiveOrdinal;

const LOCK: &str = "INSERT INTO mam_archive_sequences (archive_jid, next_seq) VALUES ($1, 0) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = mam_archive_sequences.next_seq";

const ALLOCATE: &str = "INSERT INTO mam_archive_sequences (archive_jid, next_seq) VALUES ($1, 1) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = mam_archive_sequences.next_seq + 1 RETURNING next_seq";

const SQLITE_LOCK: &str = "INSERT INTO main.mam_archive_sequences (archive_jid, next_seq) VALUES ($1, 0) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = next_seq";

const SQLITE_ALLOCATE: &str = "INSERT INTO main.mam_archive_sequences (archive_jid, next_seq) VALUES ($1, 1) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = next_seq + 1 RETURNING next_seq";

fn require_transaction(in_transaction: bool) -> Result<(), sqlx::Error> {
    if in_transaction {
        Ok(())
    } else {
        Err(sqlx::Error::InvalidArgument(
            "MAM archive counter lock requires an SQLx-tracked transaction".to_owned(),
        ))
    }
}

/// Take the archive counter row lock on an SQLx-tracked transaction,
/// creating a zero-valued row if needed. Callers should lock multiple
/// archives in canonical order and roll back the transaction after errors.
pub async fn lock_archive_sequence_on_connection(
    conn: &mut PgConnection,
    archive: &BareJid,
) -> Result<(), sqlx::Error> {
    require_transaction(conn.is_in_transaction())?;
    sqlx::query(LOCK)
        .bind(archive.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

/// Lock the counter in the supplied PostgreSQL schema on the caller's
/// SQLx-tracked transaction. The caller must roll it back after an error.
pub async fn lock_archive_sequence_on_connection_in_schema(
    conn: &mut PgConnection,
    schema: &str,
    archive: &BareJid,
) -> Result<(), sqlx::Error> {
    require_transaction(conn.is_in_transaction())?;
    let relation = super::postgres_relation(Some(schema), "mam_archive_sequences");
    let sql = format!("INSERT INTO {relation} AS target (archive_jid, next_seq) VALUES ($1, 0) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = target.next_seq");
    sqlx::query(&sql)
        .bind(archive.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

/// Lock the counter on the caller's SQLx-tracked SQLite transaction. The
/// caller must roll it back after an error.
pub async fn lock_archive_sequence_on_sqlite_connection(
    conn: &mut SqliteConnection,
    archive: &BareJid,
) -> Result<(), sqlx::Error> {
    require_transaction(conn.is_in_transaction())?;
    sqlx::query(SQLITE_LOCK)
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
    schema: Option<&str>,
) -> Result<ArchiveOrdinal, sqlx::Error> {
    let sql = schema.map(|schema| format!(
        "INSERT INTO {} AS target (archive_jid, next_seq) VALUES ($1, 1) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = target.next_seq + 1 RETURNING next_seq",
        super::postgres_relation(Some(schema), "mam_archive_sequences")
    ));
    decode_ordinal(
        sqlx::query_scalar(sql.as_deref().unwrap_or(ALLOCATE))
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
        sqlx::query_scalar(SQLITE_ALLOCATE)
            .bind(archive.to_string())
            .fetch_one(conn)
            .await?,
    )
}

pub(super) async fn preserve_postgres(
    conn: &mut PgConnection,
    archive: &BareJid,
    ordinal: ArchiveOrdinal,
    schema: Option<&str>,
) -> Result<(), sqlx::Error> {
    let sql = schema.map(|schema| format!("INSERT INTO {} AS target (archive_jid, next_seq) VALUES ($1, $2) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = GREATEST(target.next_seq, EXCLUDED.next_seq)", super::postgres_relation(Some(schema), "mam_archive_sequences")));
    sqlx::query(sql.as_deref().unwrap_or("INSERT INTO mam_archive_sequences (archive_jid, next_seq) VALUES ($1, $2) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = GREATEST(mam_archive_sequences.next_seq, EXCLUDED.next_seq)"))
        .bind(archive.to_string()).bind(ordinal.to_storage()).execute(conn).await?;
    Ok(())
}

pub(super) async fn preserve_sqlite(
    conn: &mut SqliteConnection,
    archive: &BareJid,
    ordinal: ArchiveOrdinal,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO main.mam_archive_sequences (archive_jid, next_seq) VALUES ($1, $2) ON CONFLICT (archive_jid) DO UPDATE SET next_seq = MAX(next_seq, excluded.next_seq)")
        .bind(archive.to_string()).bind(ordinal.to_storage()).execute(conn).await?;
    Ok(())
}
