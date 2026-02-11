use std::path::{Path, PathBuf};

#[cfg(feature = "native")]
use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

#[cfg(feature = "native")]
use rusqlite::{
    Connection, params, params_from_iter,
    types::{Value, ValueRef},
};

#[cfg(feature = "native")]
use tokio::{sync::oneshot, task};

#[cfg(feature = "native")]
use tracing::info;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("failed to open database at {path}: {reason}")]
    ConnectionFailed { path: PathBuf, reason: String },

    #[error("migration {version} failed: {reason}")]
    MigrationFailed { version: u32, reason: String },

    #[error("query failed: {0}")]
    QueryFailed(String),

    #[error("expected one row but found none")]
    NotFound,

    #[error("transaction rolled back: {0}")]
    TransactionFailed(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum SqlValue {
    #[default]
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
    Boolean(bool),
}

pub trait ToSql: Send + Sync {
    fn to_sql_value(&self) -> SqlValue;
}

impl ToSql for bool {
    fn to_sql_value(&self) -> SqlValue {
        SqlValue::Boolean(*self)
    }
}

impl ToSql for i64 {
    fn to_sql_value(&self) -> SqlValue {
        SqlValue::Integer(*self)
    }
}

impl ToSql for i32 {
    fn to_sql_value(&self) -> SqlValue {
        SqlValue::Integer(i64::from(*self))
    }
}

impl ToSql for u64 {
    fn to_sql_value(&self) -> SqlValue {
        SqlValue::Integer(
            i64::try_from(*self).expect("u64 value exceeds i64::MAX; cannot store in SQLite"),
        )
    }
}

impl ToSql for u32 {
    fn to_sql_value(&self) -> SqlValue {
        SqlValue::Integer(i64::from(*self))
    }
}

impl ToSql for f64 {
    fn to_sql_value(&self) -> SqlValue {
        SqlValue::Real(*self)
    }
}

impl ToSql for f32 {
    fn to_sql_value(&self) -> SqlValue {
        SqlValue::Real(f64::from(*self))
    }
}

impl ToSql for String {
    fn to_sql_value(&self) -> SqlValue {
        SqlValue::Text(self.clone())
    }
}

impl ToSql for str {
    fn to_sql_value(&self) -> SqlValue {
        SqlValue::Text(self.to_string())
    }
}

impl ToSql for Vec<u8> {
    fn to_sql_value(&self) -> SqlValue {
        SqlValue::Blob(self.clone())
    }
}

impl ToSql for [u8] {
    fn to_sql_value(&self) -> SqlValue {
        SqlValue::Blob(self.to_vec())
    }
}

impl<T> ToSql for Option<T>
where
    T: ToSql,
{
    fn to_sql_value(&self) -> SqlValue {
        match self {
            Some(value) => value.to_sql_value(),
            None => SqlValue::Null,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    values: Vec<SqlValue>,
}

impl Row {
    pub fn new(values: Vec<SqlValue>) -> Self {
        Self { values }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&SqlValue> {
        self.values.get(index)
    }
}

pub trait FromRow: Sized {
    fn from_row(row: &Row) -> Result<Self, StorageError>;
}

impl FromRow for Row {
    fn from_row(row: &Row) -> Result<Self, StorageError> {
        Ok(row.clone())
    }
}

#[derive(Debug, Default)]
pub struct Transaction {
    _private: (),
}

#[allow(async_fn_in_trait)]
pub trait Database: Send + Sync + 'static {
    async fn execute(&self, sql: &str, params: &[&dyn ToSql]) -> Result<u64, StorageError>;

    async fn query<T: FromRow>(
        &self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<Vec<T>, StorageError>;

    async fn query_one<T: FromRow>(
        &self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<T, StorageError>;

    async fn transaction<F, R>(&self, f: F) -> Result<R, StorageError>
    where
        F: FnOnce(&Transaction) -> Result<R, StorageError> + Send;
}

#[cfg(feature = "native")]
#[derive(Debug)]
pub struct NativeDatabase {
    path: PathBuf,
    writer: Sender<WriteCommand>,
}

#[cfg(feature = "native")]
enum WriteCommand {
    Execute {
        sql: String,
        params: Vec<SqlValue>,
        response: oneshot::Sender<Result<u64, StorageError>>,
    },
}

#[cfg(feature = "native")]
enum WriterState {
    Ready(Connection),
    Failed(String),
}

#[cfg(feature = "native")]
fn collect_params(params: &[&dyn ToSql]) -> Vec<SqlValue> {
    params.iter().map(|param| param.to_sql_value()).collect()
}

#[cfg(feature = "native")]
fn sql_value_to_rusqlite_value(value: &SqlValue) -> Value {
    match value {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(integer) => Value::Integer(*integer),
        SqlValue::Real(real) => Value::Real(*real),
        SqlValue::Text(text) => Value::Text(text.clone()),
        SqlValue::Blob(bytes) => Value::Blob(bytes.clone()),
        SqlValue::Boolean(boolean) => Value::Integer(i64::from(*boolean)),
    }
}

#[cfg(feature = "native")]
fn sql_values_to_rusqlite_values(values: &[SqlValue]) -> Vec<Value> {
    values.iter().map(sql_value_to_rusqlite_value).collect()
}

#[cfg(feature = "native")]
fn value_ref_to_sql_value(value_ref: ValueRef<'_>) -> SqlValue {
    match value_ref {
        ValueRef::Null => SqlValue::Null,
        ValueRef::Integer(integer) => SqlValue::Integer(integer),
        ValueRef::Real(real) => SqlValue::Real(real),
        ValueRef::Text(text) => SqlValue::Text(String::from_utf8_lossy(text).into_owned()),
        ValueRef::Blob(bytes) => SqlValue::Blob(bytes.to_vec()),
    }
}

#[cfg(feature = "native")]
fn open_connection(path: &Path) -> Result<Connection, StorageError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| StorageError::ConnectionFailed {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    }

    Connection::open(path).map_err(|error| StorageError::ConnectionFailed {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })
}

#[cfg(feature = "native")]
fn configure_native_connection(connection: &Connection, path: &Path) -> Result<(), StorageError> {
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|error| StorageError::ConnectionFailed {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| StorageError::ConnectionFailed {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| StorageError::ConnectionFailed {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    Ok(())
}

#[cfg(feature = "native")]
fn open_native_connection(path: &Path) -> Result<Connection, StorageError> {
    let connection = open_connection(path)?;
    configure_native_connection(&connection, path)?;
    Ok(connection)
}

#[cfg(feature = "native")]
fn execute_statement(
    connection: &Connection,
    sql: &str,
    params: &[SqlValue],
) -> Result<u64, StorageError> {
    let values = sql_values_to_rusqlite_values(params);

    connection
        .execute(sql, params_from_iter(values.iter()))
        .map(|rows_affected| rows_affected as u64)
        .map_err(|error| StorageError::QueryFailed(error.to_string()))
}

#[cfg(feature = "native")]
fn query_rows(
    connection: &Connection,
    sql: &str,
    params: &[SqlValue],
) -> Result<Vec<Row>, StorageError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
    let values = sql_values_to_rusqlite_values(params);
    let column_count = statement.column_count();
    let mut rows = statement
        .query(params_from_iter(values.iter()))
        .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
    let mut output = Vec::new();

    while let Some(row) = rows
        .next()
        .map_err(|error| StorageError::QueryFailed(error.to_string()))?
    {
        let mut values = Vec::with_capacity(column_count);
        for index in 0..column_count {
            let value = row
                .get_ref(index)
                .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
            values.push(value_ref_to_sql_value(value));
        }
        output.push(Row::new(values));
    }

    Ok(output)
}

#[cfg(feature = "native")]
struct Migration {
    version: u32,
    sql: &'static str,
}

#[cfg(feature = "native")]
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/001_initial.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("../migrations/002_add_mam_sync_state.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("../migrations/003_add_offline_queue.sql"),
    },
];

#[cfg(feature = "native")]
fn run_migrations(connection: &Connection) -> Result<(), StorageError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS _migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .map_err(|error| StorageError::MigrationFailed {
            version: 0,
            reason: format!("failed to create _migrations table: {error}"),
        })?;

    for migration in MIGRATIONS {
        let is_applied: i64 = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM _migrations WHERE version = ?1)",
                params![migration.version],
                |row| row.get(0),
            )
            .map_err(|error| StorageError::MigrationFailed {
                version: migration.version,
                reason: format!("failed to query migration state: {error}"),
            })?;

        if is_applied != 0 {
            continue;
        }

        let tx =
            connection
                .unchecked_transaction()
                .map_err(|error| StorageError::MigrationFailed {
                    version: migration.version,
                    reason: format!("failed to begin transaction: {error}"),
                })?;

        tx.execute_batch(migration.sql)
            .map_err(|error| StorageError::MigrationFailed {
                version: migration.version,
                reason: error.to_string(),
            })?;

        tx.execute(
            "INSERT INTO _migrations (version) VALUES (?1)",
            params![migration.version],
        )
        .map_err(|error| StorageError::MigrationFailed {
            version: migration.version,
            reason: format!("failed to record migration: {error}"),
        })?;

        tx.commit().map_err(|error| StorageError::MigrationFailed {
            version: migration.version,
            reason: format!("failed to commit migration: {error}"),
        })?;

        info!(version = migration.version, "applied migration");
    }

    Ok(())
}

#[cfg(feature = "native")]
fn run_writer(path: PathBuf, receiver: Receiver<WriteCommand>) {
    let mut state = match open_native_connection(&path) {
        Ok(connection) => WriterState::Ready(connection),
        Err(error) => WriterState::Failed(error.to_string()),
    };

    while let Ok(command) = receiver.recv() {
        match command {
            WriteCommand::Execute {
                sql,
                params,
                response,
            } => {
                let result = match &mut state {
                    WriterState::Ready(connection) => execute_statement(connection, &sql, &params),
                    WriterState::Failed(reason) => Err(StorageError::ConnectionFailed {
                        path: path.clone(),
                        reason: reason.clone(),
                    }),
                };

                let _ = response.send(result);
            }
        }
    }
}

#[cfg(feature = "native")]
impl NativeDatabase {
    async fn open(path: &Path) -> Result<Self, StorageError> {
        let path = path.to_path_buf();
        let setup_path = path.clone();

        task::spawn_blocking(move || {
            let connection = open_native_connection(&setup_path)?;
            run_migrations(&connection)?;
            Ok(())
        })
        .await
        .map_err(|error| StorageError::ConnectionFailed {
            path: path.clone(),
            reason: format!("failed to join native storage setup task: {error}"),
        })??;

        let (writer, receiver) = mpsc::channel();
        let writer_path = path.clone();

        thread::Builder::new()
            .name("storage_writer".to_string())
            .spawn(move || run_writer(writer_path, receiver))
            .map_err(|error| StorageError::ConnectionFailed {
                path: path.clone(),
                reason: format!("failed to spawn storage_writer task: {error}"),
            })?;

        Ok(Self { path, writer })
    }
}

#[cfg(feature = "native")]
impl Database for NativeDatabase {
    async fn execute(&self, sql: &str, params: &[&dyn ToSql]) -> Result<u64, StorageError> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = WriteCommand::Execute {
            sql: sql.to_string(),
            params: collect_params(params),
            response: response_tx,
        };

        self.writer.send(command).map_err(|_| {
            StorageError::QueryFailed("storage writer task is unavailable".to_string())
        })?;

        response_rx.await.map_err(|_| {
            StorageError::QueryFailed(
                "storage writer task terminated before responding".to_string(),
            )
        })?
    }

    async fn query<T: FromRow>(
        &self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<Vec<T>, StorageError> {
        let sql = sql.to_string();
        let params = collect_params(params);
        let path = self.path.clone();
        let rows = task::spawn_blocking(move || {
            let connection = open_native_connection(&path)?;
            query_rows(&connection, &sql, &params)
        })
        .await
        .map_err(|error| {
            StorageError::QueryFailed(format!("failed to join query task: {error}"))
        })??;

        rows.iter().map(T::from_row).collect()
    }

    async fn query_one<T: FromRow>(
        &self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<T, StorageError> {
        let mut rows = self.query(sql, params).await?;
        if rows.is_empty() {
            return Err(StorageError::NotFound);
        }

        Ok(rows.remove(0))
    }

    async fn transaction<F, R>(&self, f: F) -> Result<R, StorageError>
    where
        F: FnOnce(&Transaction) -> Result<R, StorageError> + Send,
    {
        let transaction = Transaction::default();
        f(&transaction).map_err(|error| StorageError::TransactionFailed(error.to_string()))
    }
}

/// Hardcoded database name for the web backend. On web targets, the `storage.path`
/// config setting is ignored; this name is used with OPFS or IndexedDB backing.
#[cfg(feature = "web")]
pub const WEB_DATABASE_NAME: &str = "waddle";

/// Web storage backend backed by wa-sqlite (compiled to WASM) with OPFS persistence
/// (preferred) or IndexedDB fallback. Uses single-connection mode (no WAL, no pooling).
///
/// This is currently a compile-safe placeholder. The full implementation will:
/// - Load wa-sqlite as a WASM module alongside the application
/// - Use OPFS (Origin Private File System) when available, falling back to IndexedDB
/// - Share the same SQL dialect and migration system as the native backend
/// - Run in single-connection mode (all reads and writes serialised)
#[cfg(feature = "web")]
#[derive(Debug)]
pub struct WebDatabase {
    name: String,
}

#[cfg(feature = "web")]
impl WebDatabase {
    /// Open (or create) a web database with the given logical name.
    ///
    /// In the future this will initialise wa-sqlite with OPFS/IndexedDB backing
    /// and run pending migrations. Currently returns a stub that errors on all
    /// operations.
    pub async fn open(path: &Path) -> Result<Self, StorageError> {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(WEB_DATABASE_NAME)
            .to_string();

        Ok(Self { name })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(feature = "web")]
impl Database for WebDatabase {
    async fn execute(&self, sql: &str, params: &[&dyn ToSql]) -> Result<u64, StorageError> {
        let _ = (sql, params);
        Err(StorageError::QueryFailed(
            "web storage backend not yet implemented (wa-sqlite)".to_string(),
        ))
    }

    async fn query<T: FromRow>(
        &self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<Vec<T>, StorageError> {
        let _ = (sql, params);
        Err(StorageError::QueryFailed(
            "web storage backend not yet implemented (wa-sqlite)".to_string(),
        ))
    }

    async fn query_one<T: FromRow>(
        &self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<T, StorageError> {
        let _ = (sql, params);
        Err(StorageError::QueryFailed(
            "web storage backend not yet implemented (wa-sqlite)".to_string(),
        ))
    }

    async fn transaction<F, R>(&self, f: F) -> Result<R, StorageError>
    where
        F: FnOnce(&Transaction) -> Result<R, StorageError> + Send,
    {
        let _ = f;
        Err(StorageError::TransactionFailed(
            "web storage backend not yet implemented (wa-sqlite)".to_string(),
        ))
    }
}

#[cfg(feature = "native")]
pub async fn open_database(path: &Path) -> Result<impl Database, StorageError> {
    NativeDatabase::open(path).await
}

#[cfg(all(not(feature = "native"), feature = "web"))]
pub async fn open_database(path: &Path) -> Result<impl Database, StorageError> {
    WebDatabase::open(path).await
}

#[cfg(not(any(feature = "native", feature = "web")))]
compile_error!("waddle-storage requires either the `native` or `web` feature.");
