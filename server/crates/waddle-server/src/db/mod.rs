//! Database module for Waddle Server.
//!
//! Core infrastructure now uses SQLx adapters and a single logical database.

pub mod actor;
pub mod blocking;
mod migrations;
mod pool;
pub mod roster;

use async_trait::async_trait;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
};
use sqlx::Row as SqlxRow;
#[cfg(test)]
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, info, instrument};

pub use migrations::MigrationRunner;
pub use pool::{DatabasePool, PoolConfig, PoolHealth};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<&String> for Value {
    fn from(value: &String) -> Self {
        Self::Text(value.clone())
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<i16> for Value {
    fn from(value: i16) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<i8> for Value {
    fn from(value: i8) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Self {
        Self::Integer(value as i64)
    }
}

impl From<u32> for Value {
    fn from(value: u32) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<u16> for Value {
    fn from(value: u16) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<u8> for Value {
    fn from(value: u8) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<usize> for Value {
    fn from(value: usize) -> Self {
        Self::Integer(value as i64)
    }
}

impl From<isize> for Value {
    fn from(value: isize) -> Self {
        Self::Integer(value as i64)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Real(value)
    }
}

impl From<f32> for Value {
    fn from(value: f32) -> Self {
        Self::Real(f64::from(value))
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<Vec<u8>> for Value {
    fn from(value: Vec<u8>) -> Self {
        Self::Blob(value)
    }
}

impl From<&[u8]> for Value {
    fn from(value: &[u8]) -> Self {
        Self::Blob(value.to_vec())
    }
}

impl<T> From<Option<T>> for Value
where
    Value: From<T>,
{
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Value::from)
    }
}

pub trait IntoParams {
    fn into_params(self) -> Vec<Value>;
}

impl IntoParams for () {
    fn into_params(self) -> Vec<Value> {
        Vec::new()
    }
}

impl IntoParams for Vec<Value> {
    fn into_params(self) -> Vec<Value> {
        self
    }
}

impl<T, const N: usize> IntoParams for [T; N]
where
    Value: From<T>,
{
    fn into_params(self) -> Vec<Value> {
        self.into_iter().map(Value::from).collect()
    }
}

impl<A, B> IntoParams for (A, B)
where
    Value: From<A>,
    Value: From<B>,
{
    fn into_params(self) -> Vec<Value> {
        vec![Value::from(self.0), Value::from(self.1)]
    }
}

#[macro_export]
macro_rules! db_params {
    () => {
        Vec::<$crate::db::Value>::new()
    };
    ($($value:expr),+ $(,)?) => {
        vec![$($crate::db::Value::from($value)),+]
    };
}

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
    async fn connect(&self, database_url: &str) -> Result<DatabaseBackend, DatabaseError>;
}

#[derive(Debug, Default)]
pub struct SqlxSqliteAdapter;

#[async_trait]
impl DatabaseAdapter for SqlxSqliteAdapter {
    async fn connect(&self, database_url: &str) -> Result<DatabaseBackend, DatabaseError> {
        let connect_options = SqliteConnectOptions::from_str(database_url)
            .map_err(|e| DatabaseError::ConnectionFailed(e.to_string()))?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(10)
            .connect_with(connect_options)
            .await?;
        Ok(DatabaseBackend::Sqlite(pool))
    }
}

#[derive(Debug, Default)]
pub struct SqlxPostgresAdapter;

#[async_trait]
impl DatabaseAdapter for SqlxPostgresAdapter {
    async fn connect(&self, database_url: &str) -> Result<DatabaseBackend, DatabaseError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await?;
        Ok(DatabaseBackend::Postgres(pool))
    }
}

fn adapter_for(driver: DatabaseDriver) -> Box<dyn DatabaseAdapter> {
    match driver {
        DatabaseDriver::Sqlite => Box::<SqlxSqliteAdapter>::default(),
        DatabaseDriver::Postgres => Box::<SqlxPostgresAdapter>::default(),
    }
}

#[derive(Clone)]
enum DatabaseBackend {
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
                Ok(sqlite_rows_to_rows(fetched)?)
            }
            DatabaseBackend::Postgres(pool) => {
                let sql = rewrite_positional_for_postgres(sql);
                let mut query = sqlx::query(&sql);
                for value in params {
                    query = bind_postgres(query, value);
                }
                let fetched: Vec<PgRow> = query.fetch_all(pool).await?;
                Ok(postgres_rows_to_rows(fetched)?)
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
    match value {
        Value::Null => query.bind(Option::<String>::None),
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
    match value {
        Value::Null => query.bind(Option::<String>::None),
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
        return Ok(value.map_or(Value::Null, Value::Integer));
    }

    if let Ok(value) = row.try_get::<Option<f64>, _>(idx) {
        return Ok(value.map_or(Value::Null, Value::Real));
    }

    if let Ok(value) = row.try_get::<Option<String>, _>(idx) {
        return Ok(value.map_or(Value::Null, Value::Text));
    }

    if let Ok(value) = row.try_get::<Option<Vec<u8>>, _>(idx) {
        return Ok(value.map_or(Value::Null, Value::Blob));
    }

    Ok(Value::Null)
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
        return Ok(value.map_or(Value::Null, Value::Integer));
    }

    if let Ok(value) = row.try_get::<Option<i32>, _>(idx) {
        return Ok(value.map_or(Value::Null, |v| Value::Integer(i64::from(v))));
    }

    if let Ok(value) = row.try_get::<Option<bool>, _>(idx) {
        return Ok(value.map_or(Value::Null, |v| Value::Integer(i64::from(v))));
    }

    if let Ok(value) = row.try_get::<Option<f64>, _>(idx) {
        return Ok(value.map_or(Value::Null, Value::Real));
    }

    if let Ok(value) = row.try_get::<Option<String>, _>(idx) {
        return Ok(value.map_or(Value::Null, Value::Text));
    }

    if let Ok(value) = row.try_get::<Option<Vec<u8>>, _>(idx) {
        return Ok(value.map_or(Value::Null, Value::Blob));
    }

    Ok(Value::Null)
}

/// A single materialized row.
#[derive(Debug, Clone)]
pub struct Row {
    values: Vec<Value>,
}

impl Row {
    pub fn get<T: DbDecode>(&self, idx: usize) -> Result<T, DatabaseError> {
        let value = self.get_value(idx)?;
        T::decode(value)
    }

    pub fn get_value(&self, idx: usize) -> Result<Value, DatabaseError> {
        self.values.get(idx).cloned().ok_or_else(|| {
            DatabaseError::QueryFailed(format!(
                "column index {} out of bounds (row has {} columns)",
                idx,
                self.values.len()
            ))
        })
    }
}

pub trait DbDecode: Sized {
    fn decode(value: Value) -> Result<Self, DatabaseError>;
}

impl DbDecode for String {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        match value {
            Value::Text(value) => Ok(value),
            Value::Integer(value) => Ok(value.to_string()),
            Value::Real(value) => Ok(value.to_string()),
            Value::Blob(value) => String::from_utf8(value).map_err(|e| {
                DatabaseError::QueryFailed(format!("failed to decode utf8 string: {}", e))
            }),
            Value::Null => Err(DatabaseError::QueryFailed(
                "cannot decode NULL into String".to_string(),
            )),
        }
    }
}

impl DbDecode for Option<String> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        match value {
            Value::Null => Ok(None),
            other => String::decode(other).map(Some),
        }
    }
}

impl DbDecode for i64 {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        match value {
            Value::Integer(value) => Ok(value),
            Value::Real(value) => Ok(value as i64),
            Value::Text(value) => value.parse::<i64>().map_err(|e| {
                DatabaseError::QueryFailed(format!("failed to parse integer '{}': {}", value, e))
            }),
            Value::Null => Err(DatabaseError::QueryFailed(
                "cannot decode NULL into i64".to_string(),
            )),
            Value::Blob(_) => Err(DatabaseError::QueryFailed(
                "cannot decode blob into i64".to_string(),
            )),
        }
    }
}

impl DbDecode for Option<i64> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        match value {
            Value::Null => Ok(None),
            other => i64::decode(other).map(Some),
        }
    }
}

impl DbDecode for i32 {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        let value = i64::decode(value)?;
        i32::try_from(value).map_err(|e| {
            DatabaseError::QueryFailed(format!("failed to convert {} to i32: {}", value, e))
        })
    }
}

impl DbDecode for Option<i32> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        match value {
            Value::Null => Ok(None),
            other => i32::decode(other).map(Some),
        }
    }
}

impl DbDecode for bool {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        Ok(i64::decode(value)? != 0)
    }
}

impl DbDecode for Option<bool> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        match value {
            Value::Null => Ok(None),
            other => bool::decode(other).map(Some),
        }
    }
}

impl DbDecode for f64 {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        match value {
            Value::Real(value) => Ok(value),
            Value::Integer(value) => Ok(value as f64),
            Value::Text(value) => value.parse::<f64>().map_err(|e| {
                DatabaseError::QueryFailed(format!("failed to parse float '{}': {}", value, e))
            }),
            Value::Null => Err(DatabaseError::QueryFailed(
                "cannot decode NULL into f64".to_string(),
            )),
            Value::Blob(_) => Err(DatabaseError::QueryFailed(
                "cannot decode blob into f64".to_string(),
            )),
        }
    }
}

impl DbDecode for Option<f64> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        match value {
            Value::Null => Ok(None),
            other => f64::decode(other).map(Some),
        }
    }
}

impl DbDecode for Vec<u8> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        match value {
            Value::Blob(value) => Ok(value),
            Value::Text(value) => Ok(value.into_bytes()),
            Value::Null => Ok(Vec::new()),
            Value::Integer(_) | Value::Real(_) => Err(DatabaseError::QueryFailed(
                "cannot decode numeric value into blob".to_string(),
            )),
        }
    }
}

impl DbDecode for Option<Vec<u8>> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        match value {
            Value::Null => Ok(None),
            other => Vec::<u8>::decode(other).map(Some),
        }
    }
}

/// Materialized query result set with async-compatible iteration.
#[derive(Debug, Clone)]
pub struct Rows {
    rows: Vec<Row>,
    cursor: usize,
    column_count: usize,
}

impl Rows {
    pub fn column_count(&self) -> usize {
        self.column_count
    }

    pub async fn next(&mut self) -> Result<Option<Row>, DatabaseError> {
        if self.cursor >= self.rows.len() {
            return Ok(None);
        }
        let row = self.rows[self.cursor].clone();
        self.cursor += 1;
        Ok(Some(row))
    }
}

/// Connection guard that delegates to the configured SQLx backend.
#[derive(Clone)]
pub struct ConnectionGuard {
    backend: DatabaseBackend,
    last_insert_rowid: Arc<AtomicI64>,
}

impl ConnectionGuard {
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

/// Extension trait for extracting typed values from row values.
pub trait ValueExt {
    fn as_string(&self) -> Result<String, DatabaseError>;
    fn as_optional_string(&self) -> Result<Option<String>, DatabaseError>;
}

impl ValueExt for Value {
    fn as_string(&self) -> Result<String, DatabaseError> {
        match self {
            Value::Text(s) => Ok(s.clone()),
            Value::Null => Err(DatabaseError::QueryFailed("expected text, got null".into())),
            other => Err(DatabaseError::QueryFailed(format!(
                "expected text, got {:?}",
                other
            ))),
        }
    }

    fn as_optional_string(&self) -> Result<Option<String>, DatabaseError> {
        match self {
            Value::Null => Ok(None),
            Value::Text(s) => Ok(Some(s.clone())),
            other => Err(DatabaseError::QueryFailed(format!(
                "expected text or null, got {:?}",
                other
            ))),
        }
    }
}

/// Get a value from a row by index with bounds checking.
pub fn row_value(row: &[Value], idx: usize) -> Result<&Value, DatabaseError> {
    row.get(idx).ok_or_else(|| {
        DatabaseError::QueryFailed(format!(
            "column index {} out of bounds (row has {} columns)",
            idx,
            row.len()
        ))
    })
}

/// Database-specific errors.
#[derive(Error, Debug)]
pub enum DatabaseError {
    #[error("Failed to connect to database: {0}")]
    ConnectionFailed(String),

    #[error("Database query failed: {0}")]
    QueryFailed(String),

    #[error("Migration failed: {0}")]
    MigrationFailed(String),

    #[error("Internal database error: {0}")]
    Internal(#[from] sqlx::Error),
}

/// Configuration for database connections.
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub driver: DatabaseDriver,
    pub database_url: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            driver: DatabaseDriver::Sqlite,
            database_url: "sqlite::memory:".to_string(),
        }
    }
}

impl DatabaseConfig {
    pub fn new(driver: DatabaseDriver, database_url: impl Into<String>) -> Self {
        Self {
            driver,
            database_url: database_url.into(),
        }
    }
}

/// Logical database handle.
#[derive(Clone, kameo::Reply)]
pub struct Database {
    backend: DatabaseBackend,
    name: String,
    driver: DatabaseDriver,
}

impl Database {
    pub async fn in_memory(name: &str) -> Result<Self, DatabaseError> {
        Self::from_config(name, &DatabaseConfig::default()).await
    }

    #[cfg(test)]
    #[instrument(skip_all, fields(path = %path.as_ref().display()))]
    pub async fn open_local(name: &str, path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DatabaseError::ConnectionFailed(format!(
                    "Failed to create database directory: {}",
                    e
                ))
            })?;
        }
        let database_url = format!("sqlite://{}", path.to_string_lossy());
        Self::from_config(
            name,
            &DatabaseConfig::new(DatabaseDriver::Sqlite, database_url),
        )
        .await
    }

    #[instrument(skip_all, fields(name = %name))]
    pub async fn from_config(name: &str, config: &DatabaseConfig) -> Result<Self, DatabaseError> {
        debug!(driver = ?config.driver, "Opening database");
        let adapter = adapter_for(config.driver);
        let backend = adapter.connect(&config.database_url).await?;

        info!(
            name = %name,
            driver = ?config.driver,
            "Opened database"
        );

        Ok(Self {
            backend,
            name: name.to_string(),
            driver: config.driver,
        })
    }

    pub async fn guard(&self) -> Result<ConnectionGuard, DatabaseError> {
        Ok(ConnectionGuard {
            backend: self.backend.clone(),
            last_insert_rowid: Arc::new(AtomicI64::new(0)),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn driver(&self) -> DatabaseDriver {
        self.driver
    }

    #[instrument(skip_all, fields(name = %self.name))]
    pub async fn health_check(&self) -> Result<bool, DatabaseError> {
        let conn = self.guard().await?;
        match conn.query("SELECT 1", ()).await {
            Ok(_) => Ok(true),
            Err(e) => {
                tracing::warn!(error = %e, "Database health check failed");
                Ok(false)
            }
        }
    }

    #[cfg(test)]
    #[instrument(skip_all, fields(name = %self.name))]
    pub async fn execute(&self, sql: &str) -> Result<u64, DatabaseError> {
        let conn = self.guard().await?;
        conn.execute(sql, ()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_database() {
        let db = Database::in_memory("test").await.unwrap();
        assert_eq!(db.name(), "test");
    }

    #[tokio::test]
    async fn test_health_check() {
        let db = Database::in_memory("test").await.unwrap();
        let healthy = db.health_check().await.unwrap();
        assert!(healthy);
    }

    #[tokio::test]
    async fn test_execute_query() {
        let db = Database::in_memory("test").await.unwrap();

        db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)")
            .await
            .unwrap();
        db.execute("INSERT INTO test (name) VALUES ('hello')")
            .await
            .unwrap();

        let conn = db.guard().await.unwrap();
        let mut rows = conn.query("SELECT * FROM test", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let name: String = row.get(1).unwrap();
        assert_eq!(name, "hello");
    }
}
