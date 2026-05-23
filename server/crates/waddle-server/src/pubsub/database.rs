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
    ///
    /// The `&'static str` bound enforces at the type system level
    /// that `table` cannot come from user input — only string
    /// literals or `const` items resolve. Without this, the
    /// `format!`-into-SQL pattern would be a latent injection
    /// surface; with it, the compiler refuses any call that didn't
    /// originate from a hard-coded identifier.
    pub(super) async fn row_count(&self, table: &'static str) -> Result<i64, XmppError> {
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
