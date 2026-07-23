use std::str::FromStr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
};
use sqlx::Row as SqlxRow;

use super::{DatabaseError, IntoParams, Row, Rows, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseDriver {
    Sqlite,
    Postgres,
}

impl std::str::FromStr for DatabaseDriver {
    type Err = DatabaseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "sqlite" => Ok(Self::Sqlite),
            "postgres" | "postgresql" => Ok(Self::Postgres),
            _ => Err(DatabaseError::ConnectionFailed(format!(
                "unsupported database driver: {}",
                value
            ))),
        }
    }
}

#[async_trait]
trait DatabaseAdapter: Send + Sync {
    async fn connect(
        &self,
        database_url: &str,
        pool_size: u32,
    ) -> Result<DatabaseBackend, DatabaseError>;
}

#[derive(Debug, Default)]
pub struct SqlxSqliteAdapter;

#[async_trait]
impl DatabaseAdapter for SqlxSqliteAdapter {
    async fn connect(
        &self,
        database_url: &str,
        pool_size: u32,
    ) -> Result<DatabaseBackend, DatabaseError> {
        // WAL mode allows concurrent readers + one writer, but two
        // pooled connections that both want the writer slot can race —
        // SQLite returns SQLITE_BUSY/LOCKED. `busy_timeout` lets the
        // engine block briefly and retry instead of surfacing the error
        // immediately, which keeps concurrent worker pipelines (e.g.
        // the publish-job drain that splits claim / dispatch / finalize
        // across three short transactions) from deadlocking.
        let connect_options = SqliteConnectOptions::from_str(database_url)
            .map_err(|e| DatabaseError::ConnectionFailed(e.to_string()))?
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(5))
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(pool_size)
            .connect_with(connect_options)
            .await?;
        Ok(DatabaseBackend::Sqlite(pool))
    }
}

#[derive(Debug, Default)]
pub struct SqlxPostgresAdapter;

#[async_trait]
impl DatabaseAdapter for SqlxPostgresAdapter {
    async fn connect(
        &self,
        database_url: &str,
        pool_size: u32,
    ) -> Result<DatabaseBackend, DatabaseError> {
        let pool = PgPoolOptions::new()
            .max_connections(pool_size)
            .connect(database_url)
            .await?;
        Ok(DatabaseBackend::Postgres(pool))
    }
}

/// Connect a backend pool sized by `pool_size` (ADR-0017 element 12:
/// `DatabaseConfig::pool_size`/`ControlPlanePoolConfig::size` both funnel
/// through this one connect path — no separate connection-management type
/// for the control-plane pool).
pub(super) async fn connect_backend(
    driver: DatabaseDriver,
    database_url: &str,
    pool_size: u32,
) -> Result<DatabaseBackend, DatabaseError> {
    match driver {
        DatabaseDriver::Sqlite => SqlxSqliteAdapter.connect(database_url, pool_size).await,
        DatabaseDriver::Postgres => SqlxPostgresAdapter.connect(database_url, pool_size).await,
    }
}

#[derive(Clone)]
pub(super) enum DatabaseBackend {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

#[derive(Debug, Clone, Copy)]
struct ExecutionResult {
    rows_affected: u64,
    last_insert_rowid: Option<i64>,
}

impl DatabaseBackend {
    async fn execute(
        &self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<ExecutionResult, DatabaseError> {
        match self {
            DatabaseBackend::Sqlite(pool) => {
                let mut query = sqlx::query(sql);
                for value in params {
                    query = bind_sqlite(query, value);
                }
                let result = query.execute(pool).await?;
                Ok(ExecutionResult {
                    rows_affected: result.rows_affected(),
                    last_insert_rowid: Some(result.last_insert_rowid()),
                })
            }
            DatabaseBackend::Postgres(pool) => {
                let sql = rewrite_positional_for_postgres(sql);
                let mut query = sqlx::query(&sql);
                for value in params {
                    query = bind_postgres(query, value);
                }
                let result = query.execute(pool).await?;
                Ok(ExecutionResult {
                    rows_affected: result.rows_affected(),
                    last_insert_rowid: None,
                })
            }
        }
    }

    async fn execute_batch(&self, sql: &str) -> Result<(), DatabaseError> {
        match self {
            DatabaseBackend::Sqlite(pool) => {
                let mut tx = pool.begin().await?;
                for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                    sqlx::query(statement).execute(&mut *tx).await?;
                }
                tx.commit().await?;
            }
            DatabaseBackend::Postgres(pool) => {
                let mut tx = pool.begin().await?;
                for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                    sqlx::query(statement).execute(&mut *tx).await?;
                }
                tx.commit().await?;
            }
        }
        Ok(())
    }

    async fn query(&self, sql: &str, params: Vec<Value>) -> Result<Rows, DatabaseError> {
        match self {
            DatabaseBackend::Sqlite(pool) => {
                let mut query = sqlx::query(sql);
                for value in params {
                    query = bind_sqlite(query, value);
                }
                let fetched: Vec<SqliteRow> = query.fetch_all(pool).await?;
                sqlite_rows_to_rows(fetched)
            }
            DatabaseBackend::Postgres(pool) => {
                let sql = rewrite_positional_for_postgres(sql);
                let mut query = sqlx::query(&sql);
                for value in params {
                    query = bind_postgres(query, value);
                }
                let fetched: Vec<PgRow> = query.fetch_all(pool).await?;
                postgres_rows_to_rows(fetched)
            }
        }
    }
}

fn rewrite_positional_for_postgres(sql: &str) -> String {
    let mut counter = 1usize;
    let mut rewritten = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;

    while let Some(ch) = chars.next() {
        if in_line_comment {
            rewritten.push(ch);
            if ch == '\n' {
                in_line_comment = false;
            }
            continue;
        }
        if in_block_comment {
            rewritten.push(ch);
            if ch == '*' && chars.peek() == Some(&'/') {
                rewritten.push('/');
                chars.next();
                in_block_comment = false;
            }
            continue;
        }

        if in_single_quote {
            rewritten.push(ch);
            if ch == '\'' {
                if chars.peek() == Some(&'\'') {
                    rewritten.push('\'');
                    chars.next();
                } else {
                    in_single_quote = false;
                }
            }
            continue;
        }

        if in_double_quote {
            rewritten.push(ch);
            if ch == '"' {
                in_double_quote = false;
            }
            continue;
        }

        if ch == '\'' {
            in_single_quote = true;
            rewritten.push(ch);
            continue;
        }
        if ch == '"' {
            in_double_quote = true;
            rewritten.push(ch);
            continue;
        }
        if ch == '-' && chars.peek() == Some(&'-') {
            rewritten.push('-');
            rewritten.push('-');
            chars.next();
            in_line_comment = true;
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            rewritten.push('/');
            rewritten.push('*');
            chars.next();
            in_block_comment = true;
            continue;
        }

        if ch == '?' {
            let mut explicit = String::new();
            while let Some(peek) = chars.peek() {
                if peek.is_ascii_digit() {
                    explicit.push(*peek);
                    chars.next();
                } else {
                    break;
                }
            }

            rewritten.push('$');
            if explicit.is_empty() {
                rewritten.push_str(&counter.to_string());
                counter += 1;
            } else {
                rewritten.push_str(&explicit);
            }
            continue;
        }
        rewritten.push(ch);
    }
    rewritten
}

fn bind_sqlite<'q>(
    query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    value: Value,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    // SQLite parameters are dynamically typed — every NULL binds the
    // same regardless of declared column type, so we collapse all
    // null variants onto a single bind.
    match value {
        Value::NullInteger | Value::NullReal | Value::NullText | Value::NullBlob => {
            query.bind(Option::<String>::None)
        }
        Value::Integer(v) => query.bind(v),
        Value::Real(v) => query.bind(v),
        Value::Text(v) => query.bind(v),
        Value::Blob(v) => query.bind(v),
    }
}

fn bind_postgres<'q>(
    query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    value: Value,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    // Postgres types every parameter by its bound Rust type. Binding
    // `Option::<String>::None` against a `bigint` column is rejected
    // with "expression is of type text", so each typed NULL must bind
    // through an `Option<T>::None` whose `T` matches the column's SQL
    // type. There is intentionally no untyped-null arm — `Value` has
    // no untyped-null variant, so the bind site cannot accidentally
    // emit a wrong-typed NULL.
    match value {
        Value::NullText => query.bind(Option::<String>::None),
        Value::NullInteger => query.bind(Option::<i64>::None),
        Value::NullReal => query.bind(Option::<f64>::None),
        Value::NullBlob => query.bind(Option::<Vec<u8>>::None),
        Value::Integer(v) => query.bind(v),
        Value::Real(v) => query.bind(v),
        Value::Text(v) => query.bind(v),
        Value::Blob(v) => query.bind(v),
    }
}

fn sqlite_rows_to_rows(rows: Vec<SqliteRow>) -> Result<Rows, DatabaseError> {
    let column_count = rows.first().map(SqlxRow::len).unwrap_or(0);
    let converted = rows
        .into_iter()
        .map(|row| {
            let mut values = Vec::with_capacity(row.len());
            for idx in 0..row.len() {
                values.push(sqlite_value_from_row(&row, idx)?);
            }
            Ok(Row { values })
        })
        .collect::<Result<Vec<_>, DatabaseError>>()?;

    Ok(Rows {
        rows: converted,
        cursor: 0,
        column_count,
    })
}

fn sqlite_value_from_row(row: &SqliteRow, idx: usize) -> Result<Value, DatabaseError> {
    if let Ok(value) = row.try_get::<Option<i64>, _>(idx) {
        return Ok(value.map_or(Value::NullInteger, Value::Integer));
    }

    if let Ok(value) = row.try_get::<Option<f64>, _>(idx) {
        return Ok(value.map_or(Value::NullReal, Value::Real));
    }

    if let Ok(value) = row.try_get::<Option<String>, _>(idx) {
        return Ok(value.map_or(Value::NullText, Value::Text));
    }

    if let Ok(value) = row.try_get::<Option<Vec<u8>>, _>(idx) {
        return Ok(value.map_or(Value::NullBlob, Value::Blob));
    }

    Ok(Value::NullText)
}

fn postgres_rows_to_rows(rows: Vec<PgRow>) -> Result<Rows, DatabaseError> {
    let column_count = rows.first().map(SqlxRow::len).unwrap_or(0);
    let converted = rows
        .into_iter()
        .map(|row| {
            let mut values = Vec::with_capacity(row.len());
            for idx in 0..row.len() {
                values.push(postgres_value_from_row(&row, idx)?);
            }
            Ok(Row { values })
        })
        .collect::<Result<Vec<_>, DatabaseError>>()?;

    Ok(Rows {
        rows: converted,
        cursor: 0,
        column_count,
    })
}

fn postgres_value_from_row(row: &PgRow, idx: usize) -> Result<Value, DatabaseError> {
    if let Ok(value) = row.try_get::<Option<i64>, _>(idx) {
        return Ok(value.map_or(Value::NullInteger, Value::Integer));
    }

    if let Ok(value) = row.try_get::<Option<i32>, _>(idx) {
        return Ok(value.map_or(Value::NullInteger, |v| Value::Integer(i64::from(v))));
    }

    if let Ok(value) = row.try_get::<Option<bool>, _>(idx) {
        return Ok(value.map_or(Value::NullInteger, |v| Value::Integer(i64::from(v))));
    }

    if let Ok(value) = row.try_get::<Option<f64>, _>(idx) {
        return Ok(value.map_or(Value::NullReal, Value::Real));
    }

    if let Ok(value) = row.try_get::<Option<String>, _>(idx) {
        return Ok(value.map_or(Value::NullText, Value::Text));
    }

    if let Ok(value) = row.try_get::<Option<Vec<u8>>, _>(idx) {
        return Ok(value.map_or(Value::NullBlob, Value::Blob));
    }

    Ok(Value::NullText)
}

/// Connection guard that delegates to the configured SQLx backend.
#[derive(Clone)]
pub struct ConnectionGuard {
    backend: DatabaseBackend,
    last_insert_rowid: Arc<AtomicI64>,
}

impl ConnectionGuard {
    pub(super) fn new(backend: DatabaseBackend) -> Self {
        Self {
            backend,
            last_insert_rowid: Arc::new(AtomicI64::new(0)),
        }
    }

    pub async fn execute(&self, sql: &str, params: impl IntoParams) -> Result<u64, DatabaseError> {
        let result = self.backend.execute(sql, params.into_params()).await?;
        if let Some(rowid) = result.last_insert_rowid {
            self.last_insert_rowid.store(rowid, Ordering::Relaxed);
        }
        Ok(result.rows_affected)
    }

    pub async fn execute_batch(&self, sql: &str) -> Result<(), DatabaseError> {
        self.backend.execute_batch(sql).await
    }

    pub async fn query(&self, sql: &str, params: impl IntoParams) -> Result<Rows, DatabaseError> {
        self.backend.query(sql, params.into_params()).await
    }
}

/// A database transaction obtained from [`Database::begin`] or
/// [`Database::begin_control_plane`].
///
/// Holds a single pooled connection so multiple `execute` calls observe each
/// other's writes atomically. Drop without calling [`Transaction::commit`] to
/// roll back.
pub struct Transaction<'a> {
    inner: TransactionInner<'a>,
}

enum TransactionInner<'a> {
    Sqlite(sqlx::Transaction<'a, sqlx::Sqlite>),
    Postgres(sqlx::Transaction<'a, sqlx::Postgres>),
}

impl<'a> Transaction<'a> {
    pub(super) async fn begin(backend: &'a DatabaseBackend) -> Result<Self, DatabaseError> {
        let inner = match backend {
            DatabaseBackend::Sqlite(pool) => TransactionInner::Sqlite(pool.begin().await?),
            DatabaseBackend::Postgres(pool) => TransactionInner::Postgres(pool.begin().await?),
        };
        Ok(Self { inner })
    }

    /// Begin a transaction that acquires the database write lock
    /// immediately. SQLite's default `BEGIN DEFERRED` upgrades from
    /// reader to writer on the first write, which can deadlock when two
    /// pooled connections both start as readers and then both try to
    /// upgrade (SQLITE_LOCKED, not BUSY — `busy_timeout` doesn't help).
    /// `BEGIN IMMEDIATE` resolves that.
    ///
    /// For Postgres this method falls through to plain `begin`: the
    /// `BEGIN IMMEDIATE` upgrade race is SQLite-specific. Postgres at
    /// the default READ COMMITTED isolation does NOT prevent two
    /// concurrent worker phase 1's from both selecting `status='queued'`
    /// rows simultaneously — serialization on Postgres comes from the
    /// conditional `UPDATE ... WHERE status='queued'` (only one CAS
    /// wins; the loser sees 0 rows changed and short-circuits) and the
    /// `claim_token` interlock checked in phase 3's UPDATE.
    pub(super) async fn begin_immediate(
        backend: &'a DatabaseBackend,
    ) -> Result<Self, DatabaseError> {
        let inner = match backend {
            DatabaseBackend::Sqlite(pool) => {
                TransactionInner::Sqlite(pool.begin_with("BEGIN IMMEDIATE").await?)
            }
            DatabaseBackend::Postgres(pool) => TransactionInner::Postgres(pool.begin().await?),
        };
        Ok(Self { inner })
    }

    /// Execute a write statement inside the transaction.
    pub async fn execute(
        &mut self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, DatabaseError> {
        let params = params.into_params();
        match &mut self.inner {
            TransactionInner::Sqlite(tx) => {
                let mut q = sqlx::query(sql);
                for value in params {
                    q = bind_sqlite(q, value);
                }
                let result = q.execute(&mut **tx).await?;
                Ok(result.rows_affected())
            }
            TransactionInner::Postgres(tx) => {
                let sql = rewrite_positional_for_postgres(sql);
                let mut q = sqlx::query(&sql);
                for value in params {
                    q = bind_postgres(q, value);
                }
                let result = q.execute(&mut **tx).await?;
                Ok(result.rows_affected())
            }
        }
    }

    /// Query rows inside the transaction.
    pub async fn query(
        &mut self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<Rows, DatabaseError> {
        let params = params.into_params();
        match &mut self.inner {
            TransactionInner::Sqlite(tx) => {
                let mut q = sqlx::query(sql);
                for value in params {
                    q = bind_sqlite(q, value);
                }
                let fetched: Vec<SqliteRow> = q.fetch_all(&mut **tx).await?;
                sqlite_rows_to_rows(fetched)
            }
            TransactionInner::Postgres(tx) => {
                let sql = rewrite_positional_for_postgres(sql);
                let mut q = sqlx::query(&sql);
                for value in params {
                    q = bind_postgres(q, value);
                }
                let fetched: Vec<PgRow> = q.fetch_all(&mut **tx).await?;
                postgres_rows_to_rows(fetched)
            }
        }
    }

    /// Commit the transaction. Drops without commit roll back automatically.
    pub async fn commit(self) -> Result<(), DatabaseError> {
        match self.inner {
            TransactionInner::Sqlite(tx) => tx.commit().await?,
            TransactionInner::Postgres(tx) => tx.commit().await?,
        }
        Ok(())
    }
}
