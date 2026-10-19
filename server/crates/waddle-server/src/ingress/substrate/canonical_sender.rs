//! Read-only canonical sender lookup for authorizing relayed append identities.

use super::{dialect_sql, discard_database_error, IngressSubstrateError, MessageEnvelope};
use crate::db::Database;
use jid::BareJid;
use waddle_xmpp::ingress::MessageKey;

/// Read canonical authority on a pooled connection without opening a transaction.
pub(crate) async fn canonical_sender_pooled(
    db: &Database,
    key: MessageKey,
) -> Result<Option<BareJid>, IngressSubstrateError> {
    const POSTGRES: &str =
        "SELECT envelope_version::int, envelope FROM ingress_messages WHERE message_key = ?::uuid";
    const SQLITE: &str =
        "SELECT envelope_version, envelope FROM ingress_messages WHERE message_key = ?";
    let connection = db.guard().await.map_err(discard_database_error)?;
    let mut rows = connection
        .query(
            dialect_sql(db.driver(), POSTGRES, SQLITE),
            crate::db_params![key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    let Some(row) = rows.next().await.map_err(discard_database_error)? else {
        return Ok(None);
    };
    let version: Option<i64> = row.get(0).map_err(discard_database_error)?;
    let bytes: Option<Vec<u8>> = row.get(1).map_err(discard_database_error)?;
    match (version, bytes) {
        (None, None) => Ok(None),
        (Some(version), Some(bytes)) => Ok(MessageEnvelope::from_storage(version, bytes)?
            .message()
            .from
            .as_ref()
            .map(jid::Jid::to_bare)),
        _ => Err(IngressSubstrateError::InvalidStoredEnvelope),
    }
}
