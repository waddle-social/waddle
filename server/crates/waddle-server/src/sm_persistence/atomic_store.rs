use super::*;

pub(crate) struct EncodedSession {
    pub(crate) session: PersistedSession,
    pub(crate) detached_at_ms: i64,
    pub(crate) max_resume_duration_ms: i64,
    pub(crate) presence_show: Option<String>,
    pub(crate) presence_payloads: Option<String>,
}

pub(crate) struct EncodedUnacked {
    pub(crate) sequence: u32,
    pub(crate) stanza_xml: String,
    pub(crate) original_receipt_at_ms: i64,
    pub(crate) purpose: SmUnackedStanzaPurpose,
}

pub(crate) struct EncodedSnapshot {
    pub(crate) session: EncodedSession,
    pub(crate) unacked: Vec<EncodedUnacked>,
}

pub(crate) fn definitely_not_committed(error: impl std::fmt::Display) -> SmPersistenceError {
    SmPersistenceError::SnapshotDefinitelyNotCommitted(error.to_string())
}

pub(crate) fn encode_snapshot(
    snapshot: PersistedSmSnapshot,
) -> Result<EncodedSnapshot, SmPersistenceError> {
    let (session, unacked) = snapshot.into_parts();
    let max_resume_duration_ms = i64::try_from(session.max_resume_duration.as_millis())
        .map_err(|_| definitely_not_committed("max_resume_duration overflows i64"))?;
    let detached_at_ms = session.detached_at.timestamp_millis();
    let presence_show = session
        .presence_show
        .as_ref()
        .map(show_wire_str)
        .map(str::to_string);
    let presence_payloads = serialize_presence_payloads(&session.presence_payloads)
        .map_err(definitely_not_committed)?;
    let unacked = unacked
        .into_iter()
        .map(|row| {
            Ok(EncodedUnacked {
                sequence: row.sequence,
                stanza_xml: serialize_stanza(&row.stanza).map_err(definitely_not_committed)?,
                original_receipt_at_ms: row.original_receipt_at.timestamp_millis(),
                purpose: row.purpose,
            })
        })
        .collect::<Result<Vec<_>, SmPersistenceError>>()?;
    Ok(EncodedSnapshot {
        session: EncodedSession {
            session,
            detached_at_ms,
            max_resume_duration_ms,
            presence_show,
            presence_payloads,
        },
        unacked,
    })
}

pub(crate) async fn current_promotion_attempts(
    tx: &mut crate::db::Transaction<'_>,
    stream_id: &SmSessionId,
) -> Result<Option<u32>, SmPersistenceError> {
    let mut rows = tx
        .query(
            "SELECT promotion_attempts FROM sm_sessions WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(definitely_not_committed)?;
    let Some(row) = rows.next().await.map_err(definitely_not_committed)? else {
        return Ok(None);
    };
    let attempts: i64 = row.get(0).map_err(definitely_not_committed)?;
    Ok(Some(u32::try_from(attempts.max(0)).unwrap_or(u32::MAX)))
}

async fn write_terminal_generation(
    tx: &mut crate::db::Transaction<'_>,
    key: &SmTerminalGenerationKey,
    snapshot: &EncodedSnapshot,
    promotion_attempts: u32,
) -> Result<(), SmPersistenceError> {
    let session = &snapshot.session;
    tx.execute(
        r#"
        INSERT INTO sm_terminal_generations (
            stream_id, generation_id, user_id, full_jid, inbound_count,
            outbound_count, last_acked, max_resume_secs, detached_at_ms,
            max_resume_duration_ms, carbons_enabled, roster_interested,
            blocklist_interested, presence_available, presence_show,
            presence_status, presence_priority, replay_gap_through,
            promotion_attempts, presence_payloads
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (stream_id, generation_id) DO UPDATE SET
            user_id = excluded.user_id,
            full_jid = excluded.full_jid,
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
            presence_payloads = excluded.presence_payloads
        "#,
        crate::db_params![
            key.stream_id().as_str().to_string(),
            key.generation_id().to_string(),
            session.session.user_id.clone(),
            session.session.jid.to_string(),
            i64::from(session.session.inbound_count),
            i64::from(session.session.outbound_count),
            i64::from(session.session.last_acked),
            session.session.max_resume_time.map(i64::from),
            session.detached_at_ms,
            session.max_resume_duration_ms,
            i64::from(session.session.carbons_enabled),
            i64::from(session.session.roster_interested),
            i64::from(session.session.blocklist_interested),
            i64::from(session.session.presence_available),
            session.presence_show.clone(),
            session.session.presence_status.clone(),
            i64::from(session.session.presence_priority),
            session.session.replay_gap_through.map(i64::from),
            i64::from(promotion_attempts),
            session.presence_payloads.clone(),
        ],
    )
    .await
    .map_err(definitely_not_committed)?;

    tx.execute(
        "DELETE FROM sm_terminal_unacked WHERE stream_id = ? AND generation_id = ?",
        crate::db_params![
            key.stream_id().as_str().to_string(),
            key.generation_id().to_string(),
        ],
    )
    .await
    .map_err(definitely_not_committed)?;
    for row in &snapshot.unacked {
        tx.execute(
            "INSERT INTO sm_terminal_unacked (stream_id, generation_id, sequence, \
             stanza_xml, original_receipt_at_ms, purpose) VALUES (?, ?, ?, ?, ?, ?)",
            crate::db_params![
                key.stream_id().as_str().to_string(),
                key.generation_id().to_string(),
                i64::from(row.sequence),
                row.stanza_xml.clone(),
                row.original_receipt_at_ms,
                unacked_purpose_wire_str(row.purpose).to_string(),
            ],
        )
        .await
        .map_err(definitely_not_committed)?;
    }
    Ok(())
}

async fn write_resumable_session(
    tx: &mut crate::db::Transaction<'_>,
    snapshot: &EncodedSnapshot,
    reset_promotion_attempts: bool,
) -> Result<(), SmPersistenceError> {
    let session = &snapshot.session;
    let stream_id = &session.session.stream_id;
    tx.execute(
        "DELETE FROM sm_unacked WHERE stream_id = ?",
        crate::db_params![stream_id.as_str().to_string()],
    )
    .await
    .map_err(definitely_not_committed)?;

    tx.execute(
        r#"
        INSERT INTO sm_sessions (
            stream_id, user_id, full_jid, inbound_count, outbound_count,
            last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
            carbons_enabled, roster_interested, blocklist_interested, presence_available,
            presence_show, presence_status, presence_priority, replay_gap_through,
            presence_payloads
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (stream_id) DO UPDATE SET
            user_id = excluded.user_id,
            full_jid = excluded.full_jid,
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
            presence_payloads = excluded.presence_payloads
        "#,
        crate::db_params![
            stream_id.as_str().to_string(),
            session.session.user_id.clone(),
            session.session.jid.to_string(),
            i64::from(session.session.inbound_count),
            i64::from(session.session.outbound_count),
            i64::from(session.session.last_acked),
            session.session.max_resume_time.map(i64::from),
            session.detached_at_ms,
            session.max_resume_duration_ms,
            i64::from(session.session.carbons_enabled),
            i64::from(session.session.roster_interested),
            i64::from(session.session.blocklist_interested),
            i64::from(session.session.presence_available),
            session.presence_show.clone(),
            session.session.presence_status.clone(),
            i64::from(session.session.presence_priority),
            session.session.replay_gap_through.map(i64::from),
            session.presence_payloads.clone(),
        ],
    )
    .await
    .map_err(definitely_not_committed)?;
    if reset_promotion_attempts {
        tx.execute(
            "UPDATE sm_sessions SET promotion_attempts = 0 WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(definitely_not_committed)?;
    }

    for row in &snapshot.unacked {
        tx.execute(
            "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms, purpose) \
             VALUES (?, ?, ?, ?, ?)",
            crate::db_params![
                stream_id.as_str().to_string(),
                i64::from(row.sequence),
                row.stanza_xml.clone(),
                row.original_receipt_at_ms,
                unacked_purpose_wire_str(row.purpose).to_string(),
            ],
        )
        .await
        .map_err(definitely_not_committed)?;
    }
    Ok(())
}

pub(super) async fn store_session_atomic(
    storage: &DatabaseSmPersistence,
    session: PersistedSession,
    unacked: Vec<PersistedUnackedStanza>,
) -> Result<(), SmPersistenceError> {
    let snapshot = PersistedSmSnapshot::new(session, unacked).map_err(definitely_not_committed)?;
    replace_resumable_session_atomic(storage, snapshot, None).await
}

pub(super) async fn replace_resumable_session_atomic(
    storage: &DatabaseSmPersistence,
    successor: PersistedSmSnapshot,
    displaced_same_id: Option<PersistedTerminalGeneration>,
) -> Result<(), SmPersistenceError> {
    let successor_stream_id = successor.session().stream_id.clone();
    if displaced_same_id
        .as_ref()
        .is_some_and(|terminal| terminal.key().stream_id() != &successor_stream_id)
    {
        return Err(definitely_not_committed(
            "terminal predecessor and resumable successor have different stream ids",
        ));
    }

    let successor = encode_snapshot(successor)?;
    let displaced = displaced_same_id
        .map(|terminal| {
            let (key, snapshot, attempts) = terminal.into_parts();
            encode_snapshot(snapshot).map(|snapshot| (key, snapshot, attempts))
        })
        .transpose()?;
    let lock = storage.lock_for(&successor_stream_id);
    let _guard = lock.lock().await;
    let mut tx = storage.db.begin().await.map_err(definitely_not_committed)?;

    if let Some((key, snapshot, supplied_attempts)) = displaced.as_ref() {
        let attempts = current_promotion_attempts(&mut tx, &successor_stream_id)
            .await?
            .unwrap_or(*supplied_attempts);
        write_terminal_generation(&mut tx, key, snapshot, attempts).await?;
    }
    write_resumable_session(&mut tx, &successor, displaced.is_some()).await?;

    tx.commit()
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    Ok(())
}
