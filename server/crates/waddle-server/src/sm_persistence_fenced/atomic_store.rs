//! Fenced snapshot and ingress allocation writes share one transaction.

use super::*;

impl PostgresFencedSmPersistence {
    pub(super) async fn store_snapshot(
        &self,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
        append: Option<PersistedIngressAppend>,
    ) -> Result<KeyedSnapshotOutcome, SmPersistenceError> {
        let stream_id = session.stream_id.clone();
        let fence = self.claim_fence_for(&stream_id).await?;
        let max_resume_duration_ms = i64::try_from(session.max_resume_duration.as_millis())
            .map_err(|_| SmPersistenceError::Other("max_resume_duration overflows i64".into()))?;
        let presence_show_str = session.presence_show.as_ref().map(show_wire_str);
        let presence_payloads_xml = serialize_presence_payloads(&session.presence_payloads)?;

        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let _identity_guard = self.assert_fenced(&mut tx, &stream_id, &fence).await?;

        // Drop any pre-existing unacked rows first (see the portable
        // impl's identical comment on this statement's ordering
        // rationale), then upsert the session row (divergence (a):
        // Postgres `now()`, not `session.detached_at`), then append every
        // supplied unacked stanza.
        tx.execute(
            "DELETE FROM sm_unacked WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
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
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, (EXTRACT(EPOCH FROM now()) * 1000)::bigint, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                stream_id.as_str().to_string(),
                session.user_id.clone(),
                session.jid.to_string(),
                Some(session.occupancy_session.to_string()),
                i64::from(session.inbound_count),
                i64::from(session.outbound_count),
                i64::from(session.last_acked),
                session.max_resume_time.map(i64::from),
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
                None::<String>,
                None::<String>,
                None::<i64>,
                None::<i64>,
            ],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

        for stanza in &unacked {
            let xml = serialize_stanza(&stanza.stanza)?;
            let receipt_ms = stanza.original_receipt_at.timestamp_millis();
            tx.execute(
                "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms, ingress_receipts) \
                 VALUES (?, ?, ?, ?, ?)",
                crate::db_params![
                    stream_id.as_str().to_string(),
                    i64::from(stanza.sequence),
                    xml,
                    receipt_ms,
                crate::sm_persistence::codec::encode_ingress_receipts(&stanza.ingress_receipts),
                ],
            )
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        }

        if let Some(append) = append {
            if let Err(error) =
                crate::sm_persistence::ingress_append::insert(&mut tx, &append).await
            {
                let ledger_conflict =
                    crate::sm_persistence::ingress_append::is_ledger_conflict(&error);
                tx.rollback()
                    .await
                    .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
                if !ledger_conflict {
                    return Err(SmPersistenceError::Other(error.to_string()));
                }
                let winner = self.get_ingress_append(&append.key).await?.ok_or_else(|| {
                    SmPersistenceError::Other(
                        "conflicting ingress append proof disappeared after rollback".into(),
                    )
                })?;
                return Ok(KeyedSnapshotOutcome::ObligationAlreadyAllocated {
                    accepting_stream: winner.accepting_stream,
                });
            }
        }

        tx.commit()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(KeyedSnapshotOutcome::Committed)
    }
}
