use super::DatabaseError;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Boolean(bool),
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
        Self::Boolean(value)
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

/// A single materialized row.
#[derive(Debug, Clone)]
pub struct Row {
    pub(super) values: Vec<Value>,
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
            Value::Boolean(value) => Ok(value.to_string()),
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
            Value::Boolean(value) => Ok(i64::from(value)),
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
        match value {
            Value::Boolean(value) => Ok(value),
            other => Ok(i64::decode(other)? != 0),
        }
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
            Value::Boolean(value) => Ok(if value { 1.0 } else { 0.0 }),
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
            Value::Boolean(_) | Value::Integer(_) | Value::Real(_) => Err(
                DatabaseError::QueryFailed("cannot decode numeric value into blob".to_string()),
            ),
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
    pub(super) rows: Vec<Row>,
    pub(super) cursor: usize,
    pub(super) column_count: usize,
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
