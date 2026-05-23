use waddle_xmpp::XmppError;

use super::DatabasePubSubStorage;
use crate::db::IntoParams;

impl DatabasePubSubStorage {
    pub(super) async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, XmppError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))
    }

    pub(super) async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, XmppError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))
    }

    /// Count rows in a table. Returns `Err` when the table doesn't
    /// exist (used by the version-bump path to size the "we're about
    /// to drop your data" warning).
    pub(super) async fn row_count(&self, table: &str) -> Result<i64, XmppError> {
        // SAFETY: `table` is a `&'static str` from a closed allow-list
        // in the schema-bump caller, never user input.
        let mut rows = self
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await?;
        let row = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
            .ok_or_else(|| XmppError::internal("COUNT(*) returned no row".to_string()))?;
        row.get(0)
            .map_err(|error| XmppError::internal(error.to_string()))
    }
}
