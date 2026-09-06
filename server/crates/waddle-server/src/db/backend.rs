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

/// A database transaction obtained from [`Database::begin`].
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

    /// Reach the raw Postgres connection for repositories that compose
    /// crate-external, connection-taking SQL (MAM archive writes) into this
    /// transaction. Returns `None` on SQLite.
    pub(crate) fn postgres_connection(&mut self) -> Option<&mut sqlx::PgConnection> {
        match &mut self.inner {
            TransactionInner::Postgres(tx) => Some(&mut **tx),
            TransactionInner::Sqlite(_) => None,
        }
    }

    /// Reach the raw SQLite connection for connection-taking repositories.
    /// Returns `None` on PostgreSQL.
    pub(crate) fn sqlite_connection(&mut self) -> Option<&mut sqlx::SqliteConnection> {
        match &mut self.inner {
            TransactionInner::Sqlite(tx) => Some(&mut **tx),
            TransactionInner::Postgres(_) => None,
        }
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

    /// Execute a batch of SQL statements inside the transaction using the
    /// already-pinned connection, so the whole batch participates in the
    /// caller's commit/rollback boundary.
    pub async fn execute_batch(&mut self, sql: &str) -> Result<(), DatabaseError> {
        match &mut self.inner {
            TransactionInner::Sqlite(tx) => {
                sqlx::raw_sql(sql).execute(&mut **tx).await?;
            }
            TransactionInner::Postgres(tx) => {
                sqlx::raw_sql(sql).execute(&mut **tx).await?;
            }
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use crate::db::{Database, DatabaseConfig};

    use super::*;

    fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
        let mut url = url::Url::parse(database_url).expect("parse postgres url");
        let retained: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
            .append_pair("options", &format!("-c search_path={schema}"));
        url.to_string()
    }

    fn unique_postgres_schema_name(prefix: &str) -> String {
        format!("waddle_test_{prefix}_{}", uuid::Uuid::new_v4().simple())
    }

    async fn drop_postgres_schema(admin: &sqlx::PgPool, schema: &str) {
        let drop_schema = format!("DROP SCHEMA IF EXISTS {schema} CASCADE");
        sqlx::query(&drop_schema)
            .execute(admin)
            .await
            .expect("drop isolated postgres schema");
    }

    async fn isolated_postgres_test_db(
        name: &str,
        schema_prefix: &str,
    ) -> Option<(Database, sqlx::PgPool, String)> {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!(
                "skipping: WADDLE_TEST_POSTGRES_URL not set \
                 (postgres-backed transaction execute_batch foundation)"
            );
            return None;
        };

        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect postgres admin pool");
        let schema = unique_postgres_schema_name(schema_prefix);
        let create_schema = format!("CREATE SCHEMA {schema}");
        sqlx::query(&create_schema)
            .execute(&admin)
            .await
            .expect("create isolated postgres schema");

        let scoped_url = postgres_url_with_search_path(&database_url, &schema);
        let db = match Database::from_config(
            name,
            &DatabaseConfig::new(DatabaseDriver::Postgres, scoped_url),
        )
        .await
        {
            Ok(db) => db,
            Err(error) => {
                drop_postgres_schema(&admin, &schema).await;
                panic!("open isolated postgres database: {error}");
            }
        };

        Some((db, admin, schema))
    }

    async fn read_transaction_batch_rows(db: &Database, table: &str) -> Vec<(i64, String)> {
        let conn = db.guard().await.expect("acquire read guard");
        let mut rows = conn
            .query(&format!("SELECT id, value FROM {table} ORDER BY id"), ())
            .await
            .expect("query execute_batch rows");
        let mut values = Vec::new();
        while let Some(row) = rows.next().await.expect("iterate execute_batch rows") {
            values.push((
                row.get::<i64>(0).expect("row id"),
                row.get::<String>(1).expect("row value"),
            ));
        }
        values
    }

    async fn assert_transaction_batch_table_absent(db: &Database, table: &str) {
        let conn = db.guard().await.expect("acquire read guard");
        let result = conn
            .query(&format!("SELECT id, value FROM {table}"), ())
            .await;
        assert!(
            result.is_err(),
            "uncommitted batch-created table {table} must not survive transaction drop"
        );
    }

    async fn read_transaction_batch_rows_in_tx(
        tx: &mut Transaction<'_>,
        table: &str,
    ) -> Vec<(i64, String)> {
        let mut rows = tx
            .query(&format!("SELECT id, value FROM {table} ORDER BY id"), ())
            .await
            .expect("query execute_batch rows in transaction");
        let mut values = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .expect("iterate execute_batch rows in transaction")
        {
            values.push((
                row.get::<i64>(0).expect("row id"),
                row.get::<String>(1).expect("row value"),
            ));
        }
        values
    }

    async fn assert_execute_batch_commit_atomicity(
        db: &Database,
        table: &str,
        begin_immediate: bool,
    ) {
        let mut tx = if begin_immediate {
            db.begin_immediate()
                .await
                .expect("begin immediate execute_batch commit tx")
        } else {
            db.begin().await.expect("begin execute_batch commit tx")
        };
        tx.execute_batch(&format!(
            "CREATE TABLE {table} (id BIGINT PRIMARY KEY, value TEXT NOT NULL UNIQUE);\
             INSERT INTO {table} (id, value) VALUES (1, 'alpha');\
             INSERT INTO {table} (id, value) VALUES (2, 'beta');"
        ))
        .await
        .expect("execute commit batch");
        assert_eq!(
            read_transaction_batch_rows_in_tx(&mut tx, table).await,
            vec![(1, "alpha".to_string()), (2, "beta".to_string())]
        );
        tx.commit().await.expect("commit execute_batch tx");

        assert_eq!(
            read_transaction_batch_rows(db, table).await,
            vec![(1, "alpha".to_string()), (2, "beta".to_string())]
        );
    }

    async fn assert_execute_batch_rollback_atomicity(
        db: &Database,
        table: &str,
        begin_immediate: bool,
    ) {
        {
            let mut tx = if begin_immediate {
                db.begin_immediate()
                    .await
                    .expect("begin immediate execute_batch rollback tx")
            } else {
                db.begin().await.expect("begin execute_batch rollback tx")
            };
            tx.execute_batch(&format!(
                "CREATE TABLE {table} (id BIGINT PRIMARY KEY, value TEXT NOT NULL UNIQUE);\
                 INSERT INTO {table} (id, value) VALUES (1, 'alpha');"
            ))
            .await
            .expect("execute rollback batch");
            assert_eq!(
                read_transaction_batch_rows_in_tx(&mut tx, table).await,
                vec![(1, "alpha".to_string())]
            );
        }

        assert_transaction_batch_table_absent(db, table).await;
    }

    #[tokio::test]
    async fn transaction_execute_batch_commits_atomically_on_sqlite() {
        let db = Database::in_memory("transaction-execute-batch-sqlite-commit")
            .await
            .expect("open sqlite test database");
        assert_execute_batch_commit_atomicity(&db, "tx_execute_batch_commit", true).await;
    }

    #[tokio::test]
    async fn transaction_execute_batch_rolls_back_atomically_on_sqlite() {
        let db = Database::in_memory("transaction-execute-batch-sqlite-rollback")
            .await
            .expect("open sqlite test database");
        assert_execute_batch_rollback_atomicity(&db, "tx_execute_batch_rollback", true).await;
    }

    #[tokio::test]
    async fn transaction_execute_batch_commits_atomically_on_postgres() {
        let Some((db, admin, schema)) = isolated_postgres_test_db(
            "transaction-execute-batch-postgres-commit",
            "tx_batch_commit",
        )
        .await
        else {
            return;
        };

        assert_execute_batch_commit_atomicity(&db, "tx_execute_batch_commit", false).await;

        drop(db);
        drop_postgres_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn transaction_execute_batch_rolls_back_atomically_on_postgres() {
        let Some((db, admin, schema)) = isolated_postgres_test_db(
            "transaction-execute-batch-postgres-rollback",
            "tx_batch_rollback",
        )
        .await
        else {
            return;
        };

        assert_execute_batch_rollback_atomicity(&db, "tx_execute_batch_rollback", false).await;

        drop(db);
        drop_postgres_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn transaction_postgres_connection_returns_none_for_sqlite_memory() {
        let db = Database::in_memory("connection-guard-postgres-connection-sqlite")
            .await
            .expect("open sqlite test database");
        let mut tx = db.begin().await.expect("begin sqlite transaction");

        assert!(
            tx.postgres_connection().is_none(),
            "SQLite-backed transactions must not expose a PostgreSQL connection"
        );
    }

    #[tokio::test]
    async fn transaction_postgres_connection_returns_connection_for_postgres() {
        let Some((db, admin, schema)) =
            isolated_postgres_test_db("connection-guard-postgres-connection", "guard_pg_conn")
                .await
        else {
            return;
        };

        let mut tx = db.begin().await.expect("begin postgres transaction");

        assert!(
            tx.postgres_connection().is_some(),
            "Postgres-backed transactions must expose their raw PostgreSQL connection"
        );

        drop(tx);
        drop(db);
        drop_postgres_schema(&admin, &schema).await;
    }
}
