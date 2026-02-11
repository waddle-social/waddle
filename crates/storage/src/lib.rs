use std::path::{Path, PathBuf};

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
#[derive(Debug, Default)]
pub struct NativeDatabase;

#[cfg(feature = "native")]
impl NativeDatabase {
    async fn open(path: &Path) -> Result<Self, StorageError> {
        let _ = path;
        Ok(Self)
    }
}

#[cfg(feature = "native")]
impl Database for NativeDatabase {
    async fn execute(&self, sql: &str, params: &[&dyn ToSql]) -> Result<u64, StorageError> {
        let _ = (sql, params);
        Err(StorageError::QueryFailed(
            "native storage backend not implemented".to_string(),
        ))
    }

    async fn query<T: FromRow>(
        &self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<Vec<T>, StorageError> {
        let _ = (sql, params);
        Err(StorageError::QueryFailed(
            "native storage backend not implemented".to_string(),
        ))
    }

    async fn query_one<T: FromRow>(
        &self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<T, StorageError> {
        let _ = (sql, params);
        Err(StorageError::QueryFailed(
            "native storage backend not implemented".to_string(),
        ))
    }

    async fn transaction<F, R>(&self, f: F) -> Result<R, StorageError>
    where
        F: FnOnce(&Transaction) -> Result<R, StorageError> + Send,
    {
        let _ = f;
        Err(StorageError::TransactionFailed(
            "native storage backend not implemented".to_string(),
        ))
    }
}

#[cfg(feature = "web")]
#[derive(Debug, Default)]
pub struct WebDatabase;

#[cfg(feature = "web")]
impl WebDatabase {
    async fn open(path: &Path) -> Result<Self, StorageError> {
        let _ = path;
        Ok(Self)
    }
}

#[cfg(feature = "web")]
impl Database for WebDatabase {
    async fn execute(&self, sql: &str, params: &[&dyn ToSql]) -> Result<u64, StorageError> {
        let _ = (sql, params);
        Err(StorageError::QueryFailed(
            "web storage backend not implemented".to_string(),
        ))
    }

    async fn query<T: FromRow>(
        &self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<Vec<T>, StorageError> {
        let _ = (sql, params);
        Err(StorageError::QueryFailed(
            "web storage backend not implemented".to_string(),
        ))
    }

    async fn query_one<T: FromRow>(
        &self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<T, StorageError> {
        let _ = (sql, params);
        Err(StorageError::QueryFailed(
            "web storage backend not implemented".to_string(),
        ))
    }

    async fn transaction<F, R>(&self, f: F) -> Result<R, StorageError>
    where
        F: FnOnce(&Transaction) -> Result<R, StorageError> + Send,
    {
        let _ = f;
        Err(StorageError::TransactionFailed(
            "web storage backend not implemented".to_string(),
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
