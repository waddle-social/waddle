//! Read-only keyset scans for non-terminal ingress messages.

use chrono::{DateTime, SecondsFormat, Utc};
use waddle_xmpp::ingress::MessageKey;

use super::{dialect_sql, discard_database_error, EffectReceiptKind, IngressSubstrateError};
use crate::db::{DatabaseDriver, Transaction};

/// Select one bounded page without locking canonical rows. The caller installs
/// statement timeouts before this query and releases the scan transaction before
/// taking individual canonical locks. Failed candidates must still advance the
/// cursor, so one contended row cannot starve the rest of the page.
pub async fn receipt_complete_nonterminal_keys(
    tx: &mut Transaction<'_>,
    after: Option<(DateTime<Utc>, MessageKey)>,
    older_than: DateTime<Utc>,
    limit: u32,
) -> Result<Vec<(DateTime<Utc>, MessageKey)>, IngressSubstrateError> {
    page_nonterminal_keys(tx, after, older_than, None, limit).await
}

/// Select non-terminal rows with at least one unreceipted intent of a requested kind.
/// An empty kind list returns without accessing the database.
pub async fn unreceipted_nonterminal_keys(
    tx: &mut Transaction<'_>,
    after: Option<(DateTime<Utc>, MessageKey)>,
    older_than: DateTime<Utc>,
    kinds: &[EffectReceiptKind],
    limit: u32,
) -> Result<Vec<(DateTime<Utc>, MessageKey)>, IngressSubstrateError> {
    if kinds.is_empty() {
        return Ok(Vec::new());
    }
    page_nonterminal_keys(tx, after, older_than, Some(kinds), limit).await
}

async fn page_nonterminal_keys(
    tx: &mut Transaction<'_>,
    after: Option<(DateTime<Utc>, MessageKey)>,
    older_than: DateTime<Utc>,
    kinds: Option<&[EffectReceiptKind]>,
    limit: u32,
) -> Result<Vec<(DateTime<Utc>, MessageKey)>, IngressSubstrateError> {
    const POSTGRES: &str = r#"
        SELECT to_char(m.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
               m.message_key::text
        FROM ingress_messages m
        WHERE m.terminal_at IS NULL AND m.created_at < ?::timestamptz
          AND (?::timestamptz IS NULL OR (m.created_at, m.message_key) > (?::timestamptz, ?::uuid))
    "#;
    const SQLITE: &str = r#"
        SELECT m.created_at, m.message_key FROM ingress_messages m
        WHERE m.terminal_at IS NULL AND m.created_at < ?
          AND (? IS NULL OR (m.created_at, m.message_key) > (strftime('%Y-%m-%dT%H:%M:%fZ', ?), ?))
    "#;
    let (existence, kind_filter) = match kinds {
        Some(kinds) => (
            "EXISTS",
            format!(
                "AND i.kind IN ({})",
                std::iter::repeat_n("?", kinds.len())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        None => ("NOT EXISTS", String::new()),
    };
    let sql = format!(
        "{} AND {existence} (
            SELECT 1 FROM ingress_effect_intents i WHERE i.message_key = m.message_key
              {kind_filter}
              AND NOT EXISTS (SELECT 1 FROM ingress_effect_receipts r
                WHERE r.message_key = i.message_key AND r.kind = i.kind
                  AND r.semantic_identity_hash = i.semantic_identity_hash))
        ORDER BY m.created_at, m.message_key LIMIT ?",
        dialect_sql(tx.driver(), POSTGRES, SQLITE),
    );
    let after_time = after.map(|(created_at, _)| created_at.to_rfc3339());
    let after_key = after.map(|(_, key)| key.to_storage().to_string());
    // SQLite stores created_at as fixed-width UTC milliseconds. Round the
    // exclusive cutoff upward so submillisecond caller timestamps retain their
    // strict chronological meaning, while the indexed column stays untouched.
    let cutoff = if tx.driver() == DatabaseDriver::Sqlite {
        let milliseconds = older_than.timestamp_millis()
            + i64::from(
                !older_than
                    .timestamp_subsec_nanos()
                    .is_multiple_of(1_000_000),
            );
        DateTime::<Utc>::from_timestamp_millis(milliseconds)
            .ok_or(IngressSubstrateError::InvalidStoredTimestamp)?
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    } else {
        older_than.to_rfc3339()
    };
    let mut params = crate::db_params![cutoff, after_time.clone(), after_time, after_key];
    if let Some(kinds) = kinds {
        params.extend(
            kinds
                .iter()
                .map(|kind| crate::db::Value::from(i64::from(kind.to_storage()))),
        );
    }
    params.push(crate::db::Value::from(i64::from(limit)));
    let mut rows = tx
        .query(&sql, params)
        .await
        .map_err(discard_database_error)?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next().await.map_err(discard_database_error)? {
        let created_at: String = row.get(0).map_err(discard_database_error)?;
        let key: String = row.get(1).map_err(discard_database_error)?;
        candidates.push((
            DateTime::parse_from_rfc3339(&created_at)
                .map_err(|_| IngressSubstrateError::InvalidStoredTimestamp)?
                .with_timezone(&Utc),
            key.parse()
                .map(MessageKey::from_storage)
                .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey)?,
        ));
    }
    Ok(candidates)
}
