use std::time::{Duration, Instant};

use super::{
    DetachedSession, DetachedUnackedStanza, SmRegistryError, DEFAULT_SESSION_TIMEOUT_SECS,
};

/// Convert a [`DetachedSession`] (in-memory shape) to a
/// [`super::persistence::PersistedSession`] (durable shape) for write
/// to [`SmPersistenceStorage`].
pub(super) fn detached_to_persisted(
    session: &DetachedSession,
) -> Result<super::super::persistence::PersistedSession, SmRegistryError> {
    use super::super::persistence::PersistedSession;
    Ok(PersistedSession {
        stream_id: crate::pending_delivery::SmSessionId::new(session.stream_id.clone()),
        user_id: session.user_id.clone(),
        jid: session.jid.clone(),
        inbound_count: session.inbound_count,
        outbound_count: session.outbound_count,
        last_acked: session.last_acked,
        replay_gap_through: session.replay_gap_through,
        max_resume_time: session.max_resume_time,
        // `detached_at: Instant` is process-relative; persistence
        // captures the wall-clock moment of the persist write. The
        // skew vs. the actual detach-event time is bounded by the
        // store_session call latency (microseconds in practice).
        detached_at: chrono::Utc::now(),
        max_resume_duration: Duration::from_secs(
            session
                .max_resume_time
                .map(u64::from)
                .unwrap_or(DEFAULT_SESSION_TIMEOUT_SECS),
        ),
        carbons_enabled: session.carbons_enabled,
        roster_interested: session.roster_interested,
        blocklist_interested: session.blocklist_interested,
        presence_available: session.presence_available,
        presence_show: session.presence_show.clone(),
        presence_status: session.presence_status.clone(),
        presence_priority: session.presence_priority,
        presence_payloads: session.presence_payloads.clone(),
    })
}

/// Convert an in-memory typed stanza to the typed persistence contract.
pub(super) fn typed_to_persisted_unacked(
    stream_id: &str,
    sequence: u32,
    stanza: &crate::Stanza,
    original_receipt_at: chrono::DateTime<chrono::Utc>,
    purpose: super::super::persistence::SmUnackedStanzaPurpose,
) -> super::super::persistence::PersistedUnackedStanza {
    super::super::persistence::PersistedUnackedStanza {
        stream_id: crate::pending_delivery::SmSessionId::new(stream_id.to_string()),
        sequence,
        stanza: Box::new(stanza.clone()),
        original_receipt_at,
        purpose,
    }
}

/// Convert a [`super::persistence::PersistedSession`] + its unacked
/// row set back to a [`DetachedSession`] for the in-memory view.
pub(super) fn persisted_to_detached(
    persisted: &super::super::persistence::PersistedSession,
    unacked: &[super::super::persistence::PersistedUnackedStanza],
) -> Result<DetachedSession, SmRegistryError> {
    // `Instant` cannot be reconstructed from a wall-clock, so we
    // use `Instant::now()` minus the elapsed wall-clock since the
    // persisted detach time. This preserves correct `is_expired`
    // behaviour at the cost of a small bounded skew (the time
    // since the persist write).
    let elapsed_since_detach = chrono::Utc::now()
        .signed_duration_since(persisted.detached_at)
        .to_std()
        .unwrap_or(Duration::ZERO);
    let detached_at = Instant::now()
        .checked_sub(elapsed_since_detach)
        .unwrap_or_else(Instant::now);

    let unacked_stanzas: Vec<DetachedUnackedStanza> = unacked
        .iter()
        .filter(|row| {
            // Defense in depth (issue #1157): each row carries the
            // stream_id it was persisted under. Drop any row whose
            // stream_id is not the session being hydrated, so a
            // grouping bug in a storage backend can never replay one
            // user's stanzas on another user's `<resumed/>`
            // (XEP-0198 §5 retransmission is per-stream).
            let matches = row.stream_id == persisted.stream_id;
            if !matches {
                tracing::warn!(
                    session_stream_id = %persisted.stream_id,
                    row_stream_id = %row.stream_id,
                    sequence = row.sequence,
                    "dropping unacked row labeled with a foreign stream_id during hydration"
                );
            }
            matches
        })
        .map(|row| DetachedUnackedStanza {
            sequence: row.sequence,
            stanza: (*row.stanza).clone(),
            original_receipt_at: row.original_receipt_at,
            purpose: row.purpose,
        })
        .collect();

    Ok(DetachedSession {
        stream_id: persisted.stream_id.as_str().to_string(),
        user_id: persisted.user_id.clone(),
        jid: persisted.jid.clone(),
        inbound_count: persisted.inbound_count,
        outbound_count: persisted.outbound_count,
        last_acked: persisted.last_acked,
        replay_gap_through: persisted.replay_gap_through,
        unacked_stanzas,
        max_resume_time: persisted.max_resume_time,
        detached_at,
        carbons_enabled: persisted.carbons_enabled,
        roster_interested: persisted.roster_interested,
        blocklist_interested: persisted.blocklist_interested,
        presence_available: persisted.presence_available,
        presence_show: persisted.presence_show.clone(),
        presence_status: persisted.presence_status.clone(),
        presence_priority: persisted.presence_priority,
        // Durable rehydration carries the resource's own presence extension
        // payloads (XEP-0115 caps, XEP-0319 idle, ...) so a restart /
        // cross-node resume relays them verbatim on probe rather than
        // reporting a caps-less resource (issue #1206).
        presence_payloads: persisted.presence_payloads.clone(),
        // Not persisted: durable rehydration may re-deliver the pending
        // subscribes once after a restart — acceptable.
        pending_subscribes_flushed: false,
    })
}
