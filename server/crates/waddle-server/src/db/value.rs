use super::DatabaseError;

/// Typed query value. Each NULL variant is paired with the SQL column
/// type it binds to so Postgres (which types every parameter) accepts
/// the bind. There is no untyped-null variant: callers must produce
/// `None` through `Value::from(Option<T>::None)` (which dispatches via
/// [`TypedNull`]) or by constructing the matching `NullX` variant
/// explicitly. SQLite ignores parameter types, so the variant choice
/// is only load-bearing on Postgres.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    NullInteger,
    NullReal,
    NullText,
    NullBlob,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(
            self,
            Value::NullInteger | Value::NullReal | Value::NullText | Value::NullBlob
        )
    }
}

/// Maps a concrete Rust type `T` to the typed-NULL `Value` variant that
/// matches the SQL column type `T` binds to. Used by the blanket
/// `From<Option<T>> for Value` so that `None` arrives at the
/// Postgres bind site with a type the target column accepts.
pub(crate) trait TypedNull {
    fn typed_null() -> Value;
}

impl TypedNull for i64 {
    fn typed_null() -> Value {
        Value::NullInteger
    }
}
impl TypedNull for i32 {
    fn typed_null() -> Value {
        Value::NullInteger
    }
}
impl TypedNull for i16 {
    fn typed_null() -> Value {
        Value::NullInteger
    }
}
impl TypedNull for i8 {
    fn typed_null() -> Value {
        Value::NullInteger
    }
}
impl TypedNull for u64 {
    fn typed_null() -> Value {
        Value::NullInteger
    }
}
impl TypedNull for u32 {
    fn typed_null() -> Value {
        Value::NullInteger
    }
}
impl TypedNull for u16 {
    fn typed_null() -> Value {
        Value::NullInteger
    }
}
impl TypedNull for u8 {
    fn typed_null() -> Value {
        Value::NullInteger
    }
}
impl TypedNull for usize {
    fn typed_null() -> Value {
        Value::NullInteger
    }
}
impl TypedNull for isize {
    fn typed_null() -> Value {
        Value::NullInteger
    }
}
impl TypedNull for bool {
    fn typed_null() -> Value {
        Value::NullInteger
    }
}
impl TypedNull for f64 {
    fn typed_null() -> Value {
        Value::NullReal
    }
}
impl TypedNull for f32 {
    fn typed_null() -> Value {
        Value::NullReal
    }
}
impl TypedNull for String {
    fn typed_null() -> Value {
        Value::NullText
    }
}
impl TypedNull for &str {
    fn typed_null() -> Value {
        Value::NullText
    }
}
impl TypedNull for &String {
    fn typed_null() -> Value {
        Value::NullText
    }
}
impl TypedNull for Vec<u8> {
    fn typed_null() -> Value {
        Value::NullBlob
    }
}
impl TypedNull for &[u8] {
    fn typed_null() -> Value {
        Value::NullBlob
    }
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
    T: TypedNull,
{
    fn from(value: Option<T>) -> Self {
        value.map_or_else(T::typed_null, Value::from)
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
        if value.is_null() {
            return Err(DatabaseError::QueryFailed(
                "cannot decode NULL into String".to_string(),
            ));
        }
        match value {
            Value::Text(value) => Ok(value),
            Value::Integer(value) => Ok(value.to_string()),
            Value::Real(value) => Ok(value.to_string()),
            Value::Blob(value) => String::from_utf8(value).map_err(|e| {
                DatabaseError::QueryFailed(format!("failed to decode utf8 string: {}", e))
            }),
            Value::NullInteger | Value::NullReal | Value::NullText | Value::NullBlob => {
                unreachable!("guarded by is_null check above")
            }
        }
    }
}

impl DbDecode for Option<String> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        if value.is_null() {
            return Ok(None);
        }
        String::decode(value).map(Some)
    }
}

impl DbDecode for i64 {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        if value.is_null() {
            return Err(DatabaseError::QueryFailed(
                "cannot decode NULL into i64".to_string(),
            ));
        }
        match value {
            Value::Integer(value) => Ok(value),
            Value::Real(value) => Ok(value as i64),
            Value::Text(value) => value.parse::<i64>().map_err(|e| {
                DatabaseError::QueryFailed(format!("failed to parse integer '{}': {}", value, e))
            }),
            Value::Blob(_) => Err(DatabaseError::QueryFailed(
                "cannot decode blob into i64".to_string(),
            )),
            Value::NullInteger | Value::NullReal | Value::NullText | Value::NullBlob => {
                unreachable!("guarded by is_null check above")
            }
        }
    }
}

impl DbDecode for Option<i64> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        if value.is_null() {
            return Ok(None);
        }
        i64::decode(value).map(Some)
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
        if value.is_null() {
            return Ok(None);
        }
        i32::decode(value).map(Some)
    }
}

impl DbDecode for bool {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        Ok(i64::decode(value)? != 0)
    }
}

impl DbDecode for Option<bool> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        if value.is_null() {
            return Ok(None);
        }
        bool::decode(value).map(Some)
    }
}

impl DbDecode for f64 {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        if value.is_null() {
            return Err(DatabaseError::QueryFailed(
                "cannot decode NULL into f64".to_string(),
            ));
        }
        match value {
            Value::Real(value) => Ok(value),
            Value::Integer(value) => Ok(value as f64),
            Value::Text(value) => value.parse::<f64>().map_err(|e| {
                DatabaseError::QueryFailed(format!("failed to parse float '{}': {}", value, e))
            }),
            Value::Blob(_) => Err(DatabaseError::QueryFailed(
                "cannot decode blob into f64".to_string(),
            )),
            Value::NullInteger | Value::NullReal | Value::NullText | Value::NullBlob => {
                unreachable!("guarded by is_null check above")
            }
        }
    }
}

impl DbDecode for Option<f64> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        if value.is_null() {
            return Ok(None);
        }
        f64::decode(value).map(Some)
    }
}

impl DbDecode for Vec<u8> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        if value.is_null() {
            return Ok(Vec::new());
        }
        match value {
            Value::Blob(value) => Ok(value),
            Value::Text(value) => Ok(value.into_bytes()),
            Value::Integer(_) | Value::Real(_) => Err(DatabaseError::QueryFailed(
                "cannot decode numeric value into blob".to_string(),
            )),
            Value::NullInteger | Value::NullReal | Value::NullText | Value::NullBlob => {
                unreachable!("guarded by is_null check above")
            }
        }
    }
}

impl DbDecode for Option<Vec<u8>> {
    fn decode(value: Value) -> Result<Self, DatabaseError> {
        if value.is_null() {
            return Ok(None);
        }
        Vec::<u8>::decode(value).map(Some)
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
        if self.is_null() {
            return Err(DatabaseError::QueryFailed("expected text, got null".into()));
        }
        match self {
            Value::Text(s) => Ok(s.clone()),
            other => Err(DatabaseError::QueryFailed(format!(
                "expected text, got {:?}",
                other
            ))),
        }
    }

    fn as_optional_string(&self) -> Result<Option<String>, DatabaseError> {
        if self.is_null() {
            return Ok(None);
        }
        match self {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_i64_none_picks_typed_integer_null() {
        // Regression: `Value::from(Option::<i64>::None)` used to fold
        // every nullable type onto a single untyped null, which the
        // Postgres bind site bound as `Option::<String>::None` (text
        // NULL). That was rejected on `bigint`/`integer` columns with
        // "column is of type bigint but expression is of type text",
        // and in production rolled back every SM session detach (the
        // "Failed to detach SM session; falling back to full cleanup"
        // log line). The fix: typed-null variants per Rust type,
        // dispatched through `TypedNull`.
        assert_eq!(Value::from(Option::<i64>::None), Value::NullInteger);
        assert_eq!(Value::from(Option::<i32>::None), Value::NullInteger);
        assert_eq!(Value::from(Option::<u32>::None), Value::NullInteger);
        assert_eq!(Value::from(Option::<bool>::None), Value::NullInteger);
    }

    #[test]
    fn option_string_none_picks_typed_text_null() {
        assert_eq!(Value::from(Option::<String>::None), Value::NullText);
        assert_eq!(Value::from(Option::<&str>::None), Value::NullText);
    }

    #[test]
    fn option_f64_none_picks_typed_real_null() {
        assert_eq!(Value::from(Option::<f64>::None), Value::NullReal);
        assert_eq!(Value::from(Option::<f32>::None), Value::NullReal);
    }

    #[test]
    fn option_blob_none_picks_typed_blob_null() {
        assert_eq!(Value::from(Option::<Vec<u8>>::None), Value::NullBlob);
    }

    #[test]
    fn option_some_unwraps_to_concrete_variant() {
        assert_eq!(Value::from(Some(7_i64)), Value::Integer(7));
        assert_eq!(
            Value::from(Some("hi".to_string())),
            Value::Text("hi".to_string())
        );
    }

    #[test]
    fn db_decode_option_treats_every_null_variant_as_none() {
        // Decoders accept every typed null variant uniformly — read-side
        // code never has to know which variant the writer chose, only
        // that the cell is null.
        for v in [
            Value::NullInteger,
            Value::NullReal,
            Value::NullText,
            Value::NullBlob,
        ] {
            assert!(<Option<i64> as DbDecode>::decode(v.clone())
                .unwrap()
                .is_none());
            assert!(<Option<i32> as DbDecode>::decode(v.clone())
                .unwrap()
                .is_none());
            assert!(<Option<String> as DbDecode>::decode(v.clone())
                .unwrap()
                .is_none());
            assert!(<Option<f64> as DbDecode>::decode(v.clone())
                .unwrap()
                .is_none());
            assert!(<Option<bool> as DbDecode>::decode(v.clone())
                .unwrap()
                .is_none());
            assert!(<Option<Vec<u8>> as DbDecode>::decode(v).unwrap().is_none());
        }
    }
}
