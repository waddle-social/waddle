use super::*;
use crate::sm_persistence::codec::encode_session_principal;

pub(super) async fn store_session_atomic(
    storage: &DatabaseSmPersistence,
    session: PersistedSession,
    unacked: Vec<PersistedUnackedStanza>,
) -> Result<(), SmPersistenceError> {
    store_session_atomic_inner(storage, None, session, unacked).await
}

pub(super) async fn store_session_atomic_with_principal(
    storage: &DatabaseSmPersistence,
    principal: &waddle_xmpp::auth::AuthenticatedPrincipalRef,
    session: PersistedSession,
    unacked: Vec<PersistedUnackedStanza>,
) -> Result<(), SmPersistenceError> {
    store_session_atomic_inner(storage, Some(principal), session, unacked).await
}

async fn store_session_atomic_inner(
    storage: &DatabaseSmPersistence,
    principal: Option<&waddle_xmpp::auth::AuthenticatedPrincipalRef>,
    session: PersistedSession,
    unacked: Vec<PersistedUnackedStanza>,
) -> Result<(), SmPersistenceError> {
    let lock = storage.lock_for(&session.stream_id);
    let _guard = lock.lock().await;

    let max_resume_duration_ms = i64::try_from(session.max_resume_duration.as_millis())
        .map_err(|_| SmPersistenceError::Other("max_resume_duration overflows i64".into()))?;
    let detached_at_ms = session.detached_at.timestamp_millis();
    let presence_show_str = session.presence_show.as_ref().map(show_wire_str);
    let presence_payloads_xml = serialize_presence_payloads(&session.presence_payloads)?;
    let encoded_principal = principal.map(encode_session_principal).transpose()?;

    let mut tx = storage
        .db
        .begin()
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

    // Drop any pre-existing unacked rows for this stream_id
    // BEFORE the inserts so a previous `persist_delete_session`
    // failure (eviction path logs-and-swallows) doesn't trip the
    // `(stream_id, sequence)` PRIMARY KEY constraint on the new
    // inserts and roll the whole transaction back — including
    // the session row update we WANT to commit. (Greptile P1
    // review on PR #405.) The session row's `ON CONFLICT DO
    // UPDATE` already handles its own duplicate; the unacked
    // table has no upsert path because it normally only sees
    // appends. The DELETE here makes the atomic-store call
    // idempotent against partial-prior-state on the same
    // stream_id.
    tx.execute(
        "DELETE FROM sm_unacked WHERE stream_id = ?",
        crate::db_params![session.stream_id.as_str().to_string()],
    )
    .await
    .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

    tx.execute(
        r#"
        INSERT INTO sm_sessions (
            stream_id, user_id, full_jid, occupancy_session, inbound_count, outbound_count,
            last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
            carbons_enabled, roster_interested, blocklist_interested, presence_available,
            presence_show, presence_status, presence_priority, replay_gap_through,
            presence_payloads, bare_jid, auth_context_id, auth_context_version,
            principal_auth_epoch
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (stream_id) DO UPDATE SET
            user_id = excluded.user_id,
            full_jid = excluded.full_jid,
            occupancy_session = excluded.occupancy_session,
            inbound_count = excluded.inbound_count,
            outbound_count = excluded.outbound_count,
            last_acked = excluded.last_acked,
            max_resume_secs = excluded.max_resume_secs,
            detached_at_ms = excluded.detached_at_ms,
            max_resume_duration_ms = excluded.max_resume_duration_ms,
            carbons_enabled = excluded.carbons_enabled,
            roster_interested = excluded.roster_interested,
            blocklist_interested = excluded.blocklist_interested,
            presence_available = excluded.presence_available,
            presence_show = excluded.presence_show,
            presence_status = excluded.presence_status,
            presence_priority = excluded.presence_priority,
            replay_gap_through = excluded.replay_gap_through,
            presence_payloads = excluded.presence_payloads,
            bare_jid = COALESCE(excluded.bare_jid, sm_sessions.bare_jid),
            auth_context_id = COALESCE(excluded.auth_context_id, sm_sessions.auth_context_id),
            auth_context_version = COALESCE(excluded.auth_context_version, sm_sessions.auth_context_version),
            principal_auth_epoch = COALESCE(excluded.principal_auth_epoch, sm_sessions.principal_auth_epoch)
        "#,
        crate::db_params![
            session.stream_id.as_str().to_string(),
            session.user_id.clone(),
            session.jid.to_string(),
            Some(session.occupancy_session.to_string()),
            i64::from(session.inbound_count),
            i64::from(session.outbound_count),
            i64::from(session.last_acked),
            session.max_resume_time.map(i64::from),
            detached_at_ms,
            max_resume_duration_ms,
            i64::from(session.carbons_enabled),
            i64::from(session.roster_interested),
            i64::from(session.blocklist_interested),
            i64::from(session.presence_available),
            presence_show_str.map(str::to_string),
            session.presence_status.clone(),
            i64::from(session.presence_priority),
            session.replay_gap_through.map(i64::from),
            presence_payloads_xml,
            encoded_principal
                .as_ref()
                .map(|principal| principal.bare_jid.clone()),
            encoded_principal
                .as_ref()
                .map(|principal| principal.auth_context_id.clone()),
            encoded_principal
                .as_ref()
                .map(|principal| principal.auth_context_version),
            encoded_principal
                .as_ref()
                .map(|principal| principal.principal_auth_epoch),
        ],
    )
    .await
    .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

    for stanza in &unacked {
        let xml = serialize_stanza(&stanza.stanza)?;
        let receipt_ms = stanza.original_receipt_at.timestamp_millis();
        tx.execute(
            "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms) \
             VALUES (?, ?, ?, ?)",
            crate::db_params![
                stanza.stream_id.as_str().to_string(),
                i64::from(stanza.sequence),
                xml,
                receipt_ms,
            ],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    }

    tx.commit()
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    Ok(())
}
