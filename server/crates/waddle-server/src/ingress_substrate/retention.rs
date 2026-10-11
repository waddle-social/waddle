use super::*;

/// Invalidate only while holding the canonical row lock; this is scheduling
/// state, separate from the executor's terminal receipt proof.
pub(crate) async fn invalidate_retention(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
) -> Result<(), IngressSubstrateError> {
    tx.execute(
        dialect_sql(
            tx.driver(),
            "UPDATE ingress_messages SET retention_eligible_at = NULL WHERE message_key = ?::uuid",
            "UPDATE ingress_messages SET retention_eligible_at = NULL WHERE message_key = ?",
        ),
        crate::db_params![message_key.to_storage().to_string()],
    )
    .await
    .map_err(discard_database_error)?;
    Ok(())
}

/// Start the tail only when every canonical obligation, descendant and SM
/// reference has settled. Caller holds the canonical lock through commit.
pub(crate) async fn refresh_retention(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
    now: DateTime<Utc>,
) -> Result<(), IngressSubstrateError> {
    const POSTGRES: &str = r#"
        UPDATE ingress_messages AS m
        SET retention_eligible_at = CASE WHEN m.terminal_at IS NOT NULL
          AND NOT EXISTS (SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = m.message_key)
          AND NOT EXISTS (SELECT 1 FROM ingress_effect_descendants d WHERE d.message_key = m.message_key AND d.settled_at IS NULL)
          AND NOT EXISTS (SELECT 1 FROM ingress_effect_intents i WHERE i.message_key = m.message_key AND NOT EXISTS (
              SELECT 1 FROM ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash))
          THEN COALESCE(m.retention_eligible_at, ?::timestamptz) ELSE NULL END
        WHERE m.message_key = ?::uuid
    "#;
    const SQLITE: &str = r#"
        UPDATE ingress_messages AS m
        SET retention_eligible_at = CASE WHEN m.terminal_at IS NOT NULL
          AND NOT EXISTS (SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = m.message_key)
          AND NOT EXISTS (SELECT 1 FROM ingress_effect_descendants d WHERE d.message_key = m.message_key AND d.settled_at IS NULL)
          AND NOT EXISTS (SELECT 1 FROM ingress_effect_intents i WHERE i.message_key = m.message_key AND NOT EXISTS (
              SELECT 1 FROM ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash))
          THEN COALESCE(m.retention_eligible_at, ?) ELSE NULL END
        WHERE m.message_key = ?
    "#;
    let query = gc_retained_child_sql(tx, POSTGRES, SQLITE)
        .await
        .map_err(discard_database_error)?;
    tx.execute(
        &query,
        crate::db_params![now.to_rfc3339(), message_key.to_storage().to_string()],
    )
    .await
    .map_err(discard_database_error)?;
    Ok(())
}

pub(crate) async fn refresh_retention_with_db_clock(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
) -> Result<(), IngressSubstrateError> {
    let query = dialect_sql(
        tx.driver(),
        "SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')",
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    );
    let mut rows = tx.query(query, ()).await.map_err(discard_database_error)?;
    let row = rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::InvalidStoredTimestamp)?;
    let value: String = row.get(0).map_err(discard_database_error)?;
    let now = DateTime::parse_from_rfc3339(&value)
        .map_err(|_| IngressSubstrateError::InvalidStoredTimestamp)?
        .with_timezone(&Utc);
    drop(rows);
    refresh_retention(tx, message_key, now).await
}
