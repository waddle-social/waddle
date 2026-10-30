use std::path::PathBuf;
use std::str::FromStr;

use clap::ValueEnum;
use futures::future::ready;
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
const PG_ROWS: &str = r#"
SELECT 'oidc' AS source,
       pg_typeof(u)::oid::BIGINT AS relation_type_oid,
       u.tableoid::oid::BIGINT AS relation_oid,
       TRUE AS valid_types, NULL::BIGINT AS native_id,
       substr(u.jid,1,4097) AS raw_jid,
       substr(u.xmpp_localpart,1,4097) AS localpart,
       NULL::TEXT AS domain, substr(u.localpart_key,1,4097) AS stored_key
FROM ONLY public.users AS u
RIGHT JOIN (VALUES (true)) AS sentinel(present) ON true
UNION ALL
SELECT 'native', pg_typeof(n)::oid::BIGINT, n.tableoid::oid::BIGINT,
       TRUE, n.id::BIGINT, NULL, substr(n.username,1,4097),
       substr(n.domain,1,4097), substr(n.jid_key,1,4097)
FROM ONLY public.native_users AS n
RIGHT JOIN (VALUES (true)) AS sentinel(present) ON true
LIMIT $1
"#;
const SQLITE_ROWS: &str = "SELECT 'oidc' AS source, (typeof(jid)='text' AND typeof(xmpp_localpart)='text' AND typeof(localpart_key) IN ('text','null')) AS valid_types, NULL AS native_id, CAST(substr(CAST(jid AS BLOB),1,4097) AS TEXT) AS raw_jid, CAST(substr(CAST(xmpp_localpart AS BLOB),1,4097) AS TEXT) AS localpart, NULL AS domain, CAST(substr(CAST(localpart_key AS BLOB),1,4097) AS TEXT) AS stored_key FROM main.users UNION ALL SELECT 'native', (typeof(id)='integer' AND typeof(username)='text' AND typeof(domain)='text' AND typeof(jid_key) IN ('text','null')), id, NULL, CAST(substr(CAST(username AS BLOB),1,4097) AS TEXT), CAST(substr(CAST(domain AS BLOB),1,4097) AS TEXT), CAST(substr(CAST(jid_key AS BLOB),1,4097) AS TEXT) FROM main.native_users LIMIT ?";

#[derive(FromRow)]
struct StoredRow {
    source: String,
    valid_types: bool,
    native_id: Option<i64>,
    raw_jid: Option<String>,
    localpart: Option<String>,
    domain: Option<String>,
    stored_key: Option<String>,
}

async fn collect_rows(
    mut rows: impl TryStream<Ok = StoredRow, Error = AuditError> + Unpin,
    oidc_domain: &str,
    max_rows: u32,
) -> Result<Vec<AccountRow>, AuditError> {
    let mut result = Vec::new();
    let mut retained = 0usize;
    while let Some(row) = rows.try_next().await? {
        if !row.valid_types {
            return Err(AuditError::Read);
        }
        if result.len() >= max_rows as usize {
            return Err(AuditError::Limit);
        }
        let localpart = row.localpart.ok_or(AuditError::Read)?;
        let domain = row.domain.as_deref().unwrap_or(oidc_domain);
        for value in [
            Some(localpart.as_str()),
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
            localpart: OriginalValue::new(localpart),
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
            let selected = SqliteSource::inspect(url)?;
            let mut connection = selected.connect().await?;
            let rows = read_sqlite(&mut connection, oidc_domain, max_rows).await?;
            selected.verify_identity()?;
            Ok(rows)
        }
    }
}

/// A literal path, never passed through SQLx's URL decoder or SQLite's file: URI
/// parser. The operator must control this path and its filesystem administration.
struct SqliteSource {
    path: PathBuf,
    identity: (u64, u64),
}

impl SqliteSource {
    fn inspect(url: &str) -> Result<Self, AuditError> {
        let path = url
            .strip_prefix("sqlite://")
            .filter(|path| path.starts_with('/'))
            .ok_or(AuditError::Configuration)?;
        if path.contains(['%', '?', '#', '\0']) {
            return Err(AuditError::Configuration);
        }
        let path = std::fs::canonicalize(path).map_err(|_| AuditError::Connection)?;
        let plain = path.to_str().ok_or(AuditError::Configuration)?;
        if !plain.starts_with('/') || plain.contains(['%', '?', '#', '\0']) {
            return Err(AuditError::Configuration);
        }
        let identity = file_identity(&path)?;
        Ok(Self { path, identity })
    }

    fn verify_identity(&self) -> Result<(), AuditError> {
        if file_identity(&self.path)? != self.identity {
            return Err(AuditError::Connection);
        }
        Ok(())
    }

    async fn connect(&self) -> Result<SqliteConnection, AuditError> {
        let options = SqliteConnectOptions::new()
            .filename(&self.path)
            .read_only(true)
            .create_if_missing(false)
            .immutable(false)
            .pragma("query_only", "ON")
            .disable_statement_logging();
        let connection = SqliteConnection::connect_with(&options)
            .await
            .map_err(|_| AuditError::Connection)?;
        self.verify_identity()?;
        Ok(connection)
    }
}

#[cfg(unix)]
fn file_identity(path: &std::path::Path) -> Result<(u64, u64), AuditError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = path.metadata().map_err(|_| AuditError::Connection)?;
    if !metadata.is_file() {
        return Err(AuditError::Connection);
    }
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_path: &std::path::Path) -> Result<(u64, u64), AuditError> {
    Err(AuditError::Configuration)
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
    // Relation locks pin the OIDs, but do not freeze their namespace name.
    // The later scan must prove it resolved these same validated objects.
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
    let bindings = validate_postgres(&mut transaction).await?;
    let result = scan_postgres(&mut transaction, &bindings, domain, max_rows).await?;
    transaction.rollback().await.map_err(|_| AuditError::Read)?;
    Ok(result)
}

#[derive(FromRow)]
struct PgBinding {
    source: String,
    relation_oid: i64,
    relation_type_oid: i64,
}

async fn validate_postgres(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<Vec<PgBinding>, AuditError> {
    // Verify actual granted relation locks. A namespace replacement between
    // LOCK and validation must not let a new, unlocked table become the binding.
    let bindings = sqlx::query_as::<_, PgBinding>("SELECT c.relname::TEXT AS source, c.oid::BIGINT AS relation_oid, c.reltype::BIGINT AS relation_type_oid FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname IN ('users','native_users') AND c.relkind='r' AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhparent=c.oid OR i.inhrelid=c.oid) AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid=pg_catalog.pg_backend_pid() AND l.locktype='relation' AND l.relation=c.oid AND l.mode='AccessShareLock' AND l.granted)")
        .fetch_all(&mut **transaction).await.map_err(|_| AuditError::Schema)?;
    let users = bindings
        .iter()
        .find(|binding| binding.source == "users")
        .ok_or(AuditError::Schema)?;
    let native = bindings
        .iter()
        .find(|binding| binding.source == "native_users")
        .ok_or(AuditError::Schema)?;
    if bindings.len() != 2 || users.relation_type_oid == 0 || native.relation_type_oid == 0 {
        return Err(AuditError::Schema);
    }
    let columns: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_attribute a WHERE NOT a.attisdropped AND ((a.attrelid::BIGINT=$1 AND a.attname IN ('jid','xmpp_localpart','localpart_key') AND a.atttypid=25) OR (a.attrelid::BIGINT=$2 AND a.attname IN ('username','domain','jid_key') AND a.atttypid=25) OR (a.attrelid::BIGINT=$2 AND a.attname='id' AND a.atttypid IN (20,23)))")
        .bind(users.relation_oid).bind(native.relation_oid)
        .fetch_one(&mut **transaction).await.map_err(|_| AuditError::Schema)?;
    if columns != 7 {
        return Err(AuditError::Schema);
    }
    Ok(bindings)
}

#[derive(FromRow)]
struct PgStoredRow {
    relation_type_oid: i64,
    relation_oid: Option<i64>,
    #[sqlx(flatten)]
    row: StoredRow,
}

fn bind_postgres_row(
    value: PgStoredRow,
    bindings: &[PgBinding],
    seen: &mut [bool; 2],
) -> Result<Option<StoredRow>, AuditError> {
    let (source, index) = match value.row.source.as_str() {
        "oidc" => ("users", 0),
        "native" => ("native_users", 1),
        _ => return Err(AuditError::Read),
    };
    let binding = bindings
        .iter()
        .find(|binding| binding.source == source)
        .ok_or(AuditError::Schema)?;
    // pg_typeof(base_alias) derives its type from the same resolved FROM item.
    // The outer-join sentinel preserves that type even when the table is empty.
    if value.relation_type_oid != binding.relation_type_oid {
        return Err(AuditError::Schema);
    }
    seen[index] = true;
    match value.relation_oid {
        Some(oid) if oid == binding.relation_oid => Ok(Some(value.row)),
        None => Ok(None),
        Some(_) => Err(AuditError::Schema),
    }
}

async fn scan_postgres(
    transaction: &mut Transaction<'_, Postgres>,
    bindings: &[PgBinding],
    domain: &str,
    max_rows: u32,
) -> Result<Vec<AccountRow>, AuditError> {
    let mut seen = [false; 2];
    let rows = sqlx::query_as::<_, PgStoredRow>(PG_ROWS)
        // At most two empty-source sentinels plus one excess data row.
        .bind(i64::from(max_rows) + 3)
        .fetch(&mut **transaction)
        .map_err(|_| AuditError::Read)
        .try_filter_map(|row| ready(bind_postgres_row(row, bindings, &mut seen)));
    let result = collect_rows(rows, domain, max_rows).await?;
    if seen != [true; 2] {
        return Err(AuditError::Read);
    }
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
        .fetch(&mut *transaction)
        .map_err(|_| AuditError::Read);
    let result = collect_rows(rows, domain, max_rows).await?;
    transaction.rollback().await.map_err(|_| AuditError::Read)?;
    Ok(result)
}

#[cfg(test)]
mod tests;
