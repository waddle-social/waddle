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
    pub async fn open(_path: &Path) -> Result<Self, StorageError> {
        Ok(Self {
            name: WEB_DATABASE_NAME.to_string(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn open_temp_db() -> (NativeDatabase, TempDir) {
        let dir = TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let db = NativeDatabase::open(&db_path)
            .await
            .expect("failed to open database");
        (db, dir)
    }

    fn s(val: &str) -> String {
        val.to_string()
    }

    // ---- Migration sequencing ----

    #[tokio::test]
    async fn migrations_create_all_tables() {
        let (db, _dir) = open_temp_db().await;

        let tables: Vec<Row> = db
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
                &[],
            )
            .await
            .expect("failed to query tables");

        let table_names: Vec<&str> = tables
            .iter()
            .filter_map(|row| match row.get(0) {
                Some(SqlValue::Text(name)) => Some(name.as_str()),
                _ => None,
            })
            .collect();

        assert!(
            table_names.contains(&"_migrations"),
            "missing _migrations table"
        );
        assert!(table_names.contains(&"messages"), "missing messages table");
        assert!(table_names.contains(&"roster"), "missing roster table");
        assert!(
            table_names.contains(&"muc_rooms"),
            "missing muc_rooms table"
        );
        assert!(
            table_names.contains(&"plugin_kv"),
            "missing plugin_kv table"
        );
        assert!(
            table_names.contains(&"mam_sync_state"),
            "missing mam_sync_state table"
        );
        assert!(
            table_names.contains(&"offline_queue"),
            "missing offline_queue table"
        );
    }

    #[tokio::test]
    async fn migrations_record_all_versions() {
        let (db, _dir) = open_temp_db().await;

        let rows: Vec<Row> = db
            .query("SELECT version FROM _migrations ORDER BY version", &[])
            .await
            .expect("failed to query migration versions");

        let versions: Vec<i64> = rows
            .iter()
            .filter_map(|row| match row.get(0) {
                Some(SqlValue::Integer(v)) => Some(*v),
                _ => None,
            })
            .collect();

        assert_eq!(versions, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn migrations_are_idempotent() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");

        let _db1 = NativeDatabase::open(&db_path)
            .await
            .expect("first open failed");
        drop(_db1);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let db2 = NativeDatabase::open(&db_path)
            .await
            .expect("second open failed");

        let rows: Vec<Row> = db2
            .query("SELECT version FROM _migrations ORDER BY version", &[])
            .await
            .expect("failed to query migration versions");

        let versions: Vec<i64> = rows
            .iter()
            .filter_map(|row| match row.get(0) {
                Some(SqlValue::Integer(v)) => Some(*v),
                _ => None,
            })
            .collect();

        assert_eq!(
            versions,
            vec![1, 2, 3],
            "migrations should not duplicate on re-open"
        );
    }

    #[tokio::test]
    async fn migrations_create_expected_indices() {
        let (db, _dir) = open_temp_db().await;

        let rows: Vec<Row> = db
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND name LIKE 'idx_%' ORDER BY name",
                &[],
            )
            .await
            .expect("failed to query indices");

        let index_names: Vec<&str> = rows
            .iter()
            .filter_map(|row| match row.get(0) {
                Some(SqlValue::Text(name)) => Some(name.as_str()),
                _ => None,
            })
            .collect();

        assert!(index_names.contains(&"idx_messages_from"));
        assert!(index_names.contains(&"idx_messages_to"));
        assert!(index_names.contains(&"idx_messages_timestamp"));
        assert!(index_names.contains(&"idx_offline_queue_status"));
    }

    // ---- Query and transaction behaviour ----

    #[tokio::test]
    async fn execute_returns_rows_affected() {
        let (db, _dir) = open_temp_db().await;

        let jid = s("alice@example.com");
        let name = s("Alice");
        let sub = s("both");
        let affected = db
            .execute(
                "INSERT INTO roster (jid, name, subscription) VALUES (?1, ?2, ?3)",
                &[&jid, &name, &sub],
            )
            .await
            .expect("insert failed");

        assert_eq!(affected, 1);
    }

    #[tokio::test]
    async fn query_returns_inserted_rows() {
        let (db, _dir) = open_temp_db().await;

        let alice = s("alice@example.com");
        let alice_name = s("Alice");
        let both = s("both");
        db.execute(
            "INSERT INTO roster (jid, name, subscription) VALUES (?1, ?2, ?3)",
            &[&alice, &alice_name, &both],
        )
        .await
        .expect("insert failed");

        let bob = s("bob@example.com");
        let bob_name = s("Bob");
        let to = s("to");
        db.execute(
            "INSERT INTO roster (jid, name, subscription) VALUES (?1, ?2, ?3)",
            &[&bob, &bob_name, &to],
        )
        .await
        .expect("insert failed");

        let rows: Vec<Row> = db
            .query(
                "SELECT jid, name, subscription FROM roster ORDER BY jid",
                &[],
            )
            .await
            .expect("query failed");

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].get(0),
            Some(&SqlValue::Text(s("alice@example.com")))
        );
        assert_eq!(rows[0].get(1), Some(&SqlValue::Text(s("Alice"))));
        assert_eq!(rows[0].get(2), Some(&SqlValue::Text(s("both"))));
        assert_eq!(rows[1].get(0), Some(&SqlValue::Text(s("bob@example.com"))));
    }

    #[tokio::test]
    async fn query_one_returns_single_row() {
        let (db, _dir) = open_temp_db().await;

        let jid = s("alice@example.com");
        let name = s("Alice");
        let sub = s("both");
        db.execute(
            "INSERT INTO roster (jid, name, subscription) VALUES (?1, ?2, ?3)",
            &[&jid, &name, &sub],
        )
        .await
        .expect("insert failed");

        let qjid = s("alice@example.com");
        let row: Row = db
            .query_one("SELECT name FROM roster WHERE jid = ?1", &[&qjid])
            .await
            .expect("query_one failed");

        assert_eq!(row.get(0), Some(&SqlValue::Text(s("Alice"))));
    }

    #[tokio::test]
    async fn query_one_returns_not_found_for_missing_row() {
        let (db, _dir) = open_temp_db().await;

        let jid = s("nobody@example.com");
        let result: Result<Row, StorageError> = db
            .query_one("SELECT name FROM roster WHERE jid = ?1", &[&jid])
            .await;

        assert!(matches!(result, Err(StorageError::NotFound)));
    }

    #[tokio::test]
    async fn query_empty_table_returns_empty_vec() {
        let (db, _dir) = open_temp_db().await;

        let rows: Vec<Row> = db
            .query("SELECT * FROM roster", &[])
            .await
            .expect("query failed");

        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn execute_with_invalid_sql_returns_error() {
        let (db, _dir) = open_temp_db().await;

        let val = s("val");
        let result = db
            .execute("INSERT INTO nonexistent_table (x) VALUES (?1)", &[&val])
            .await;

        assert!(matches!(result, Err(StorageError::QueryFailed(_))));
    }

    #[tokio::test]
    async fn query_with_parameterised_filter() {
        let (db, _dir) = open_temp_db().await;

        let id1 = s("msg-1");
        let from1 = s("alice@example.com");
        let to1 = s("bob@example.com");
        let body1 = s("Hello");
        let ts1 = s("2025-01-01T00:00:00Z");
        let mt = s("chat");
        db.execute(
            "INSERT INTO messages (id, from_jid, to_jid, body, timestamp, message_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            &[&id1, &from1, &to1, &body1, &ts1, &mt],
        )
        .await
        .expect("insert failed");

        let id2 = s("msg-2");
        let from2 = s("bob@example.com");
        let to2 = s("alice@example.com");
        let body2 = s("Hi back");
        let ts2 = s("2025-01-01T00:01:00Z");
        let mt2 = s("chat");
        db.execute(
            "INSERT INTO messages (id, from_jid, to_jid, body, timestamp, message_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            &[&id2, &from2, &to2, &body2, &ts2, &mt2],
        )
        .await
        .expect("insert failed");

        let filter = s("alice@example.com");
        let rows: Vec<Row> = db
            .query(
                "SELECT id, body FROM messages WHERE from_jid = ?1",
                &[&filter],
            )
            .await
            .expect("query failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(1), Some(&SqlValue::Text(s("Hello"))));
    }

    #[tokio::test]
    async fn execute_update_modifies_existing_row() {
        let (db, _dir) = open_temp_db().await;

        let jid = s("alice@example.com");
        let name = s("Alice");
        let sub = s("none");
        db.execute(
            "INSERT INTO roster (jid, name, subscription) VALUES (?1, ?2, ?3)",
            &[&jid, &name, &sub],
        )
        .await
        .expect("insert failed");

        let new_sub = s("both");
        let ujid = s("alice@example.com");
        let affected = db
            .execute(
                "UPDATE roster SET subscription = ?1 WHERE jid = ?2",
                &[&new_sub, &ujid],
            )
            .await
            .expect("update failed");

        assert_eq!(affected, 1);

        let qjid = s("alice@example.com");
        let row: Row = db
            .query_one("SELECT subscription FROM roster WHERE jid = ?1", &[&qjid])
            .await
            .expect("query_one failed");

        assert_eq!(row.get(0), Some(&SqlValue::Text(s("both"))));
    }

    #[tokio::test]
    async fn execute_delete_removes_row() {
        let (db, _dir) = open_temp_db().await;

        let jid = s("alice@example.com");
        let name = s("Alice");
        let sub = s("both");
        db.execute(
            "INSERT INTO roster (jid, name, subscription) VALUES (?1, ?2, ?3)",
            &[&jid, &name, &sub],
        )
        .await
        .expect("insert failed");

        let djid = s("alice@example.com");
        let affected = db
            .execute("DELETE FROM roster WHERE jid = ?1", &[&djid])
            .await
            .expect("delete failed");

        assert_eq!(affected, 1);

        let rows: Vec<Row> = db
            .query("SELECT * FROM roster", &[])
            .await
            .expect("query failed");

        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn transaction_closure_executes() {
        let (db, _dir) = open_temp_db().await;

        let result = db
            .transaction(|_tx| Ok(42))
            .await
            .expect("transaction failed");

        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn transaction_closure_error_propagates() {
        let (db, _dir) = open_temp_db().await;

        let result: Result<(), StorageError> = db
            .transaction(|_tx| Err(StorageError::QueryFailed("test error".to_string())))
            .await;

        assert!(matches!(result, Err(StorageError::TransactionFailed(_))));
    }

    #[tokio::test]
    async fn null_values_round_trip_correctly() {
        let (db, _dir) = open_temp_db().await;

        let jid = s("alice@example.com");
        let name: Option<String> = None;
        let sub = s("none");
        let groups: Option<String> = None;
        db.execute(
            "INSERT INTO roster (jid, name, subscription, groups) VALUES (?1, ?2, ?3, ?4)",
            &[&jid, &name, &sub, &groups],
        )
        .await
        .expect("insert failed");

        let qjid = s("alice@example.com");
        let row: Row = db
            .query_one("SELECT name, groups FROM roster WHERE jid = ?1", &[&qjid])
            .await
            .expect("query_one failed");

        assert_eq!(row.get(0), Some(&SqlValue::Null));
        assert_eq!(row.get(1), Some(&SqlValue::Null));
    }

    #[tokio::test]
    async fn integer_values_round_trip_correctly() {
        let (db, _dir) = open_temp_db().await;

        let room = s("room@muc.example.com");
        let nick = s("mynick");
        let joined = 1_i64;
        let subject: Option<String> = None;
        db.execute(
            "INSERT INTO muc_rooms (room_jid, nick, joined, subject) VALUES (?1, ?2, ?3, ?4)",
            &[&room, &nick, &joined, &subject],
        )
        .await
        .expect("insert failed");

        let qroom = s("room@muc.example.com");
        let row: Row = db
            .query_one(
                "SELECT joined FROM muc_rooms WHERE room_jid = ?1",
                &[&qroom],
            )
            .await
            .expect("query_one failed");

        assert_eq!(row.get(0), Some(&SqlValue::Integer(1)));
    }

    // ---- Offline queue operations ----

    #[tokio::test]
    async fn offline_queue_enqueue_and_query_pending() {
        let (db, _dir) = open_temp_db().await;

        let stype = s("message");
        let payload = s("<message>hello</message>");
        let ts = s("2025-01-01T00:00:00Z");
        db.execute(
            "INSERT INTO offline_queue (stanza_type, payload, created_at) VALUES (?1, ?2, ?3)",
            &[&stype, &payload, &ts],
        )
        .await
        .expect("enqueue failed");

        let rows: Vec<Row> = db
            .query(
                "SELECT id, stanza_type, payload, status FROM offline_queue WHERE status = 'pending' ORDER BY id ASC",
                &[],
            )
            .await
            .expect("query failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(1), Some(&SqlValue::Text(s("message"))));
        assert_eq!(
            rows[0].get(2),
            Some(&SqlValue::Text(s("<message>hello</message>")))
        );
        assert_eq!(rows[0].get(3), Some(&SqlValue::Text(s("pending"))));
    }

    #[tokio::test]
    async fn offline_queue_default_status_is_pending() {
        let (db, _dir) = open_temp_db().await;

        let stype = s("presence");
        let payload = s("<presence/>");
        let ts = s("2025-01-01T00:00:00Z");
        db.execute(
            "INSERT INTO offline_queue (stanza_type, payload, created_at) VALUES (?1, ?2, ?3)",
            &[&stype, &payload, &ts],
        )
        .await
        .expect("enqueue failed");

        let row: Row = db
            .query_one("SELECT status FROM offline_queue", &[])
            .await
            .expect("query failed");

        assert_eq!(row.get(0), Some(&SqlValue::Text(s("pending"))));
    }

    #[tokio::test]
    async fn offline_queue_fifo_ordering() {
        let (db, _dir) = open_temp_db().await;

        let stype = s("message");
        for i in 1..=5 {
            let payload = format!("payload-{i}");
            let ts = format!("2025-01-01T00:0{i}:00Z");
            db.execute(
                "INSERT INTO offline_queue (stanza_type, payload, created_at) VALUES (?1, ?2, ?3)",
                &[&stype, &payload, &ts],
            )
            .await
            .expect("enqueue failed");
        }

        let rows: Vec<Row> = db
            .query(
                "SELECT payload FROM offline_queue WHERE status = 'pending' ORDER BY id ASC",
                &[],
            )
            .await
            .expect("query failed");

        assert_eq!(rows.len(), 5);
        for (i, row) in rows.iter().enumerate() {
            let expected = format!("payload-{}", i + 1);
            assert_eq!(
                row.get(0),
                Some(&SqlValue::Text(expected)),
                "FIFO order violated at position {i}"
            );
        }
    }

    #[tokio::test]
    async fn offline_queue_update_status() {
        let (db, _dir) = open_temp_db().await;

        let stype = s("message");
        let payload = s("<message/>");
        let ts = s("2025-01-01T00:00:00Z");
        db.execute(
            "INSERT INTO offline_queue (stanza_type, payload, created_at) VALUES (?1, ?2, ?3)",
            &[&stype, &payload, &ts],
        )
        .await
        .expect("enqueue failed");

        let row: Row = db
            .query_one("SELECT id FROM offline_queue", &[])
            .await
            .expect("query failed");

        let id = match row.get(0) {
            Some(SqlValue::Integer(id)) => *id,
            _ => panic!("expected integer id"),
        };

        // Transition: pending -> sent
        let sent = s("sent");
        db.execute(
            "UPDATE offline_queue SET status = ?1 WHERE id = ?2",
            &[&sent, &id],
        )
        .await
        .expect("update to sent failed");

        let row: Row = db
            .query_one("SELECT status FROM offline_queue WHERE id = ?1", &[&id])
            .await
            .expect("query failed");

        assert_eq!(row.get(0), Some(&SqlValue::Text(s("sent"))));

        // Transition: sent -> confirmed
        let confirmed = s("confirmed");
        db.execute(
            "UPDATE offline_queue SET status = ?1 WHERE id = ?2",
            &[&confirmed, &id],
        )
        .await
        .expect("update to confirmed failed");

        let row: Row = db
            .query_one("SELECT status FROM offline_queue WHERE id = ?1", &[&id])
            .await
            .expect("query failed");

        assert_eq!(row.get(0), Some(&SqlValue::Text(s("confirmed"))));
    }

    #[tokio::test]
    async fn offline_queue_pending_excludes_non_pending() {
        let (db, _dir) = open_temp_db().await;

        let items: Vec<(String, String)> = vec![
            (s("pending-item"), s("pending")),
            (s("sent-item"), s("sent")),
            (s("confirmed-item"), s("confirmed")),
            (s("failed-item"), s("failed")),
        ];
        let payload = s("<payload/>");
        let ts = s("2025-01-01T00:00:00Z");
        for (stype, status) in &items {
            db.execute(
                "INSERT INTO offline_queue (stanza_type, payload, created_at, status) VALUES (?1, ?2, ?3, ?4)",
                &[stype, &payload, &ts, status],
            )
            .await
            .expect("insert failed");
        }

        let rows: Vec<Row> = db
            .query(
                "SELECT stanza_type FROM offline_queue WHERE status = 'pending'",
                &[],
            )
            .await
            .expect("query failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0), Some(&SqlValue::Text(s("pending-item"))));
    }

    #[tokio::test]
    async fn offline_queue_delete_confirmed() {
        let (db, _dir) = open_temp_db().await;

        let stype = s("message");
        let payload = s("<message/>");
        let ts1 = s("2025-01-01T00:00:00Z");
        let confirmed = s("confirmed");
        db.execute(
            "INSERT INTO offline_queue (stanza_type, payload, created_at, status) VALUES (?1, ?2, ?3, ?4)",
            &[&stype, &payload, &ts1, &confirmed],
        )
        .await
        .expect("insert failed");

        let ts2 = s("2025-01-01T00:01:00Z");
        let pending = s("pending");
        db.execute(
            "INSERT INTO offline_queue (stanza_type, payload, created_at, status) VALUES (?1, ?2, ?3, ?4)",
            &[&stype, &payload, &ts2, &pending],
        )
        .await
        .expect("insert failed");

        let deleted = db
            .execute("DELETE FROM offline_queue WHERE status = 'confirmed'", &[])
            .await
            .expect("delete failed");

        assert_eq!(deleted, 1);

        let rows: Vec<Row> = db
            .query("SELECT status FROM offline_queue", &[])
            .await
            .expect("query failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0), Some(&SqlValue::Text(s("pending"))));
    }

    #[tokio::test]
    async fn offline_queue_autoincrement_ids() {
        let (db, _dir) = open_temp_db().await;

        let stype = s("message");
        let payload = s("<msg/>");
        let ts = s("2025-01-01T00:00:00Z");
        for _ in 0..3 {
            db.execute(
                "INSERT INTO offline_queue (stanza_type, payload, created_at) VALUES (?1, ?2, ?3)",
                &[&stype, &payload, &ts],
            )
            .await
            .expect("enqueue failed");
        }

        let rows: Vec<Row> = db
            .query("SELECT id FROM offline_queue ORDER BY id ASC", &[])
            .await
            .expect("query failed");

        let ids: Vec<i64> = rows
            .iter()
            .filter_map(|row| match row.get(0) {
                Some(SqlValue::Integer(id)) => Some(*id),
                _ => None,
            })
            .collect();

        assert_eq!(ids.len(), 3);
        assert!(
            ids[0] < ids[1] && ids[1] < ids[2],
            "IDs should be monotonically increasing"
        );
    }

    #[tokio::test]
    async fn plugin_kv_composite_key() {
        let (db, _dir) = open_temp_db().await;

        let pid_a = s("plugin-a");
        let pid_b = s("plugin-b");
        let key = s("setting");
        let val1 = b"value1".to_vec();
        let val2 = b"value2".to_vec();
        db.execute(
            "INSERT INTO plugin_kv (plugin_id, key, value) VALUES (?1, ?2, ?3)",
            &[&pid_a, &key, &val1],
        )
        .await
        .expect("insert failed");

        db.execute(
            "INSERT INTO plugin_kv (plugin_id, key, value) VALUES (?1, ?2, ?3)",
            &[&pid_b, &key, &val2],
        )
        .await
        .expect("insert failed");

        let qpid = s("plugin-a");
        let qkey = s("setting");
        let row: Row = db
            .query_one(
                "SELECT value FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2",
                &[&qpid, &qkey],
            )
            .await
            .expect("query_one failed");

        assert_eq!(row.get(0), Some(&SqlValue::Blob(b"value1".to_vec())));
    }

    #[tokio::test]
    async fn mam_sync_state_upsert() {
        let (db, _dir) = open_temp_db().await;

        let jid = s("alice@example.com");
        let sid1 = s("stanza-1");
        let ts1 = s("2025-01-01T00:00:00Z");
        db.execute(
            "INSERT INTO mam_sync_state (jid, last_stanza_id, last_sync_at) VALUES (?1, ?2, ?3)",
            &[&jid, &sid1, &ts1],
        )
        .await
        .expect("insert failed");

        let jid2 = s("alice@example.com");
        let sid2 = s("stanza-2");
        let ts2 = s("2025-01-02T00:00:00Z");
        db.execute(
            "INSERT OR REPLACE INTO mam_sync_state (jid, last_stanza_id, last_sync_at) VALUES (?1, ?2, ?3)",
            &[&jid2, &sid2, &ts2],
        )
        .await
        .expect("upsert failed");

        let qjid = s("alice@example.com");
        let row: Row = db
            .query_one(
                "SELECT last_stanza_id, last_sync_at FROM mam_sync_state WHERE jid = ?1",
                &[&qjid],
            )
            .await
            .expect("query_one failed");

        assert_eq!(row.get(0), Some(&SqlValue::Text(s("stanza-2"))));
        assert_eq!(row.get(1), Some(&SqlValue::Text(s("2025-01-02T00:00:00Z"))));
    }
}
