use super::*;

pub(super) async fn list_all_sessions_with_unacked(
    storage: &DatabaseSmPersistence,
) -> Result<Vec<(PersistedSession, Vec<PersistedUnackedStanza>)>, SmPersistenceError> {
    let mut rows = storage
        .query(
            "SELECT s.stream_id, s.user_id, s.full_jid, s.inbound_count, \
                    s.shadow_ordinal, s.outbound_count, s.last_acked, s.max_resume_secs, \
                    s.detached_at_ms, s.max_resume_duration_ms, \
                    s.carbons_enabled, s.roster_interested, s.blocklist_interested, s.presence_available, \
                    s.presence_show, s.presence_status, s.presence_priority, \
                    s.replay_gap_through, s.presence_payloads, \
                    u.sequence, u.stanza_xml, u.original_receipt_at_ms \
             FROM sm_sessions s \
             LEFT JOIN sm_unacked u ON s.stream_id = u.stream_id \
             ORDER BY s.stream_id ASC, u.sequence ASC",
            (),
        )
        .await?;
    let mut out: Vec<(PersistedSession, Vec<PersistedUnackedStanza>)> = Vec::new();
    // Track the most recently seen stream_id so we only call
    // `decode_session` once per group rather than per JOIN row
    // (Copilot review on PR #405 — re-decoding for every unacked
    // row in the same session was undercutting the cold-start
    // perf goal for large queues).
    let mut current_stream_id: Option<String> = None;
    // Explicit "the current group's session failed to decode" flag.
    // Inferring this from `out.last().is_none()` is wrong once any
    // valid session precedes the poison one: `out` is non-empty, so
    // the poison group's unacked rows would be appended to the
    // preceding session's queue and replayed on the wrong user's
    // `<resumed/>` (issue #1157).
    let mut skipping_current_group = false;
    let mut poison_sessions = 0usize;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?
    {
        // Read the session's stream_id (column 0) up-front so
        // we can decide whether this row starts a new group
        // before paying for the full session decode.
        let row_stream_id: String = match row.get(0) {
            Ok(s) => s,
            Err(error) => {
                tracing::debug!(
                    error = %error,
                    "list_all_sessions_with_unacked: skipping row with unreadable stream_id"
                );
                continue;
            }
        };
        let starts_new_group = current_stream_id.as_deref() != Some(row_stream_id.as_str());
        if starts_new_group {
            // Decode the session columns (0..=18, 19 columns;
            // `shadow_ordinal` added at 4, `presence_payloads` at 18)
            // exactly once per stream_id group. On decode
            // failure, skip the entire group's rows so a single
            // poison-pill session can't brick cold startup
            // (Greptile/Copilot/Qodo P1 review on PR #405).
            let session = match decode_session(&row) {
                Ok(s) => s,
                Err(error) => {
                    tracing::debug!(
                        stream_id = %row_stream_id,
                        error = %error,
                        "list_all_sessions_with_unacked: skipping session whose row \
                         failed to decode (poison pill)"
                    );
                    // Mark current_stream_id so subsequent rows
                    // for the same stream_id are recognized as
                    // belonging to the same skipped group and
                    // also dropped.
                    current_stream_id = Some(row_stream_id);
                    skipping_current_group = true;
                    poison_sessions += 1;
                    continue;
                }
            };
            current_stream_id = Some(row_stream_id);
            skipping_current_group = false;
            out.push((session, Vec::new()));
        } else if skipping_current_group {
            // Same stream_id as a previously-skipped poison
            // session; drop this unacked row too.
            continue;
        }
        // Unacked columns: sequence (19), stanza_xml (20),
        // original_receipt_at_ms (21) — shifted by `shadow_ordinal`
        // (session column 4) and `presence_payloads` (session column 18).
        // NULL when LEFT JOIN had no match. Per-row decode failure
        // skips that row but keeps the rest of the session's queue.
        let sequence_opt: Option<i64> = match row.get(19) {
            Ok(v) => v,
            Err(error) => {
                tracing::debug!(
                    error = %error,
                    "list_all_sessions_with_unacked: skipping row with unreadable sequence"
                );
                continue;
            }
        };
        let Some(sequence_i64) = sequence_opt else {
            continue;
        };
        let entry = match decode_unacked_join_row(&row, sequence_i64) {
            Ok(entry) => entry,
            Err(error) => {
                tracing::debug!(
                    stream_id = %current_stream_id.as_deref().unwrap_or("<unknown>"),
                    sequence = sequence_i64,
                    error = %error,
                    "list_all_sessions_with_unacked: skipping unacked row with \
                     decode failure (poison pill)"
                );
                continue;
            }
        };
        if let Some(group) = out.last_mut() {
            group.1.push(entry);
        }
    }
    if poison_sessions > 0 {
        tracing::warn!(
            count = poison_sessions,
            "list_all_sessions_with_unacked: skipped poison-pill session(s); \
             cold startup proceeds with the remaining sessions"
        );
    }
    Ok(out)
}
