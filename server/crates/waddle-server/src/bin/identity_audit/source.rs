use std::str::FromStr;

use clap::ValueEnum;
use futures::{TryStream, TryStreamExt};
use serde::Serialize;
use sqlx::postgres::{PgConnectOptions, PgConnection};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{ConnectOptions, Connection, FromRow, Postgres, Row, Transaction};

use super::model::{
    AccountRow, AccountSource, AuditError, OriginalValue, RecordKey, FIELD_BYTES, RETAINED_BYTES,
};

#[derive(Clone, Copy, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Driver {
    Postgres,
    Sqlite,
}

// Projections bound allocations before SQLx receives text. PostgreSQL returns
// at most 4097 scalars; SQLite uses byte slices so embedded NUL is not silently
// truncated. Runtime SQLite types are checked before trusting any cast.
// The retained-byte checks reject oversized fields and incomplete UTF-8.
const PG_ROWS: &str = "SELECT 'oidc' AS source, TRUE AS valid_types, NULL::BIGINT AS native_id, substr(jid,1,4097) AS raw_jid, substr(xmpp_localpart,1,4097) AS localpart, NULL::TEXT AS domain, substr(localpart_key,1,4097) AS stored_key FROM ONLY public.users UNION ALL SELECT 'native', TRUE, id::BIGINT, NULL, substr(username,1,4097), substr(domain,1,4097), substr(jid_key,1,4097) FROM ONLY public.native_users LIMIT $1";
const SQLITE_ROWS: &str = "SELECT 'oidc' AS source, (typeof(jid)='text' AND typeof(xmpp_localpart)='text' AND typeof(localpart_key) IN ('text','null')) AS valid_types, NULL AS native_id, CAST(substr(CAST(jid AS BLOB),1,4097) AS TEXT) AS raw_jid, CAST(substr(CAST(xmpp_localpart AS BLOB),1,4097) AS TEXT) AS localpart, NULL AS domain, CAST(substr(CAST(localpart_key AS BLOB),1,4097) AS TEXT) AS stored_key FROM main.users UNION ALL SELECT 'native', (typeof(id)='integer' AND typeof(username)='text' AND typeof(domain)='text' AND typeof(jid_key) IN ('text','null')), id, NULL, CAST(substr(CAST(username AS BLOB),1,4097) AS TEXT), CAST(substr(CAST(domain AS BLOB),1,4097) AS TEXT), CAST(substr(CAST(jid_key AS BLOB),1,4097) AS TEXT) FROM main.native_users LIMIT ?";

#[derive(FromRow)]
struct StoredRow {
    source: String,
    valid_types: bool,
    native_id: Option<i64>,
    raw_jid: Option<String>,
    localpart: String,
    domain: Option<String>,
    stored_key: Option<String>,
}

async fn collect_rows(
    mut rows: impl TryStream<Ok = StoredRow, Error = sqlx::Error> + Unpin,
    oidc_domain: &str,
    max_rows: u32,
) -> Result<Vec<AccountRow>, AuditError> {
    let mut result = Vec::new();
    let mut retained = 0usize;
    while let Some(row) = rows.try_next().await.map_err(|_| AuditError::Read)? {
        if !row.valid_types {
            return Err(AuditError::Read);
        }
        if result.len() >= max_rows as usize {
            return Err(AuditError::Limit);
        }
        let domain = row.domain.as_deref().unwrap_or(oidc_domain);
        for value in [
            Some(row.localpart.as_str()),
            Some(domain),
            row.raw_jid.as_deref(),
            row.stored_key.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if value.len() > FIELD_BYTES {
                return Err(AuditError::Limit);
            }
            retained = retained.checked_add(value.len()).ok_or(AuditError::Limit)?;
            if retained > RETAINED_BYTES {
                return Err(AuditError::Limit);
            }
        }
        let (source, record) = match row.source.as_str() {
            "oidc" if row.domain.is_none() && row.native_id.is_none() => (
                AccountSource::Oidc,
                RecordKey::Oidc(OriginalValue::new(row.raw_jid.ok_or(AuditError::Read)?)),
            ),
            "native" if row.domain.is_some() && row.raw_jid.is_none() => (
                AccountSource::Native,
                RecordKey::Native(row.native_id.ok_or(AuditError::Read)?),
            ),
            _ => return Err(AuditError::Read),
        };
        result.push(AccountRow {
            source,
            record,
            localpart: OriginalValue::new(row.localpart),
            domain: OriginalValue::new(row.domain.unwrap_or_else(|| oidc_domain.to_owned())),
            stored_key: row.stored_key.map(OriginalValue::new),
        });
    }
    Ok(result)
}

pub async fn read(
    driver: Driver,
    url: &str,
    oidc_domain: &str,
    max_rows: u32,
) -> Result<Vec<AccountRow>, AuditError> {
    match driver {
        Driver::Postgres => {
            let options = PgConnectOptions::from_str(url)
                .map_err(|_| AuditError::Configuration)?
                .disable_statement_logging();
            let mut connection = PgConnection::connect_with(&options)
                .await
                .map_err(|_| AuditError::Connection)?;
            read_postgres(&mut connection, oidc_domain, max_rows).await
        }
        Driver::Sqlite => {
            // URI options such as immutable or a non-locking VFS can invalidate
            // snapshot claims. Accept only a plain existing-file SQLite URL.
            if url.contains(['?', '#']) {
                return Err(AuditError::Configuration);
            }
            let options = SqliteConnectOptions::from_str(url)
                .map_err(|_| AuditError::Configuration)?
                .read_only(true)
                .create_if_missing(false)
                .pragma("query_only", "ON")
                .disable_statement_logging();
            if !options.get_filename().is_file() {
                return Err(AuditError::Connection);
            }
            let mut connection = SqliteConnection::connect_with(&options)
                .await
                .map_err(|_| AuditError::Connection)?;
            read_sqlite(&mut connection, oidc_domain, max_rows).await
        }
    }
}

async fn postgres_snapshot(
    connection: &mut PgConnection,
) -> Result<Transaction<'_, Postgres>, AuditError> {
    let mut transaction = connection
        .begin()
        .await
        .map_err(|_| AuditError::Connection)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *transaction)
        .await
        .map_err(|_| AuditError::Read)?;
    // RLS must error when this role would receive a filtered subset. It must not
    // silently turn an incomplete account inventory into a clean report.
    for statement in [
        "SET LOCAL row_security = off",
        "SET LOCAL search_path = pg_catalog",
        "SET LOCAL statement_timeout = '30000ms'",
        "SET LOCAL lock_timeout = '5000ms'",
    ] {
        sqlx::query(statement)
            .execute(&mut *transaction)
            .await
            .map_err(|_| AuditError::Read)?;
    }
    // Pin names and column layouts before catalog validation. ONLY also keeps
    // newly attached inheritance descendants outside this snapshot's scope.
    sqlx::query("LOCK TABLE ONLY public.users, ONLY public.native_users IN ACCESS SHARE MODE")
        .execute(&mut *transaction)
        .await
        .map_err(|_| AuditError::Schema)?;
    Ok(transaction)
}

async fn read_postgres(
    connection: &mut PgConnection,
    domain: &str,
    max_rows: u32,
) -> Result<Vec<AccountRow>, AuditError> {
    let mut transaction = postgres_snapshot(connection).await?;
    let tables: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname IN ('users','native_users') AND c.relkind='r' AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhparent=c.oid OR i.inhrelid=c.oid)")
        .fetch_one(&mut *transaction).await.map_err(|_| AuditError::Schema)?;
    let columns: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid=a.attrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND NOT a.attisdropped AND ((c.relname='users' AND a.attname IN ('jid','xmpp_localpart','localpart_key') AND a.atttypid=25) OR (c.relname='native_users' AND a.attname IN ('username','domain','jid_key') AND a.atttypid=25) OR (c.relname='native_users' AND a.attname='id' AND a.atttypid IN (20,23)))")
        .fetch_one(&mut *transaction).await.map_err(|_| AuditError::Schema)?;
    if tables != 2 || columns != 7 {
        return Err(AuditError::Schema);
    }
    let rows = sqlx::query_as::<_, StoredRow>(PG_ROWS)
        .bind(i64::from(max_rows) + 1)
        .fetch(&mut *transaction);
    let result = collect_rows(rows, domain, max_rows).await?;
    transaction.rollback().await.map_err(|_| AuditError::Read)?;
    Ok(result)
}

async fn read_sqlite(
    connection: &mut SqliteConnection,
    domain: &str,
    max_rows: u32,
) -> Result<Vec<AccountRow>, AuditError> {
    let mut transaction = connection
        .begin()
        .await
        .map_err(|_| AuditError::Connection)?;
    let tables = sqlx::query("SELECT name, type, substr(sql,1,128) AS definition FROM main.sqlite_schema WHERE name IN ('users','native_users')")
        .fetch_all(&mut *transaction).await.map_err(|_| AuditError::Schema)?;
    if tables.len() != 2 {
        return Err(AuditError::Schema);
    }
    for row in tables {
        let kind: &str = row.try_get("type").map_err(|_| AuditError::Schema)?;
        let definition: &str = row.try_get("definition").map_err(|_| AuditError::Schema)?;
        if kind != "table" || !definition.to_ascii_uppercase().starts_with("CREATE TABLE") {
            return Err(AuditError::Schema);
        }
    }
    for (statement, expected) in [
        ("SELECT count(*) FROM pragma_table_info('users','main') WHERE name IN ('jid','xmpp_localpart','localpart_key') AND upper(type)='TEXT'", 3i64),
        ("SELECT count(*) FROM pragma_table_info('native_users','main') WHERE (name IN ('username','domain','jid_key') AND upper(type)='TEXT') OR (name='id' AND upper(type)='INTEGER')", 4i64),
    ] {
        let count: i64 = sqlx::query_scalar(statement).fetch_one(&mut *transaction).await.map_err(|_| AuditError::Schema)?;
        if count != expected { return Err(AuditError::Schema); }
    }
    let rows = sqlx::query_as::<_, StoredRow>(SQLITE_ROWS)
        .bind(i64::from(max_rows) + 1)
        .fetch(&mut *transaction);
    let result = collect_rows(rows, domain, max_rows).await?;
    transaction.rollback().await.map_err(|_| AuditError::Read)?;
    Ok(result)
}

#[cfg(test)]
mod tests;
