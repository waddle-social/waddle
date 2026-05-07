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
}
