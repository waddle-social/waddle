//! XEP-0012 last-logout storage statements.
//!
//! One row per account records when it last became unavailable and the
//! status of that unavailable presence (migration 2024). Both engines run the
//! statements unchanged: the conflict target is the table's primary key.

use jid::BareJid;

use super::actor::{DbExecute, DbQueryOne};
use super::Value;

/// Record that `account` became unavailable at `logged_out_at_ms`, replacing
/// any earlier record.
pub(crate) fn record(account: &BareJid, logged_out_at_ms: i64, status: Option<&str>) -> DbExecute {
    DbExecute {
        sql: "INSERT INTO account_last_activity (account_jid, logged_out_at_ms, status) \
              VALUES (?, ?, ?) \
              ON CONFLICT (account_jid) DO UPDATE SET \
                  logged_out_at_ms = excluded.logged_out_at_ms, \
                  status = excluded.status"
            .to_string(),
        params: vec![
            Value::from(account.to_string()),
            Value::from(logged_out_at_ms),
            Value::from(status.map(str::to_owned)),
        ],
    }
}

/// Read the last-logout record of `account`: `(logged_out_at_ms, status)`.
pub(crate) fn lookup(account: &BareJid) -> DbQueryOne {
    DbQueryOne {
        sql: "SELECT logged_out_at_ms, status FROM account_last_activity WHERE account_jid = ?"
            .to_string(),
        params: vec![Value::from(account.to_string())],
    }
}

/// Decode the row [`lookup`] returns into `(logged_out_at_ms, status)`.
pub(crate) fn decode(row: &[Value]) -> Result<(i64, Option<String>), super::DatabaseError> {
    use super::value::{row_value, ValueExt};

    let logged_out_at_ms = match row_value(row, 0)? {
        Value::Integer(ms) => *ms,
        other => {
            return Err(super::DatabaseError::QueryFailed(format!(
                "expected integer, got {other:?}"
            )))
        }
    };
    let status = row_value(row, 1)?.as_optional_string()?;
    Ok((logged_out_at_ms, status))
}
