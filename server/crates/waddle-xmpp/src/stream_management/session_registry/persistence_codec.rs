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
        shadow_ordinal: session.shadow_ordinal,
        outbound_count: session.outbound_count,
        last_acked: session.last_acked,
        replay_gap_through: session.replay_gap_through,
        max_resume_time: session.max_resume_time,
        // `detached_at: Instant` is process-relative; translate the
        // actual detach instant to wall clock (now minus elapsed) so
        // re-persisting a session NEVER refreshes its durable detach
        // time — a no-op append retried long after detach must not
        // extend the resume window (gpt-5.6-sol round-2 finding on
        // PR #1676).
        detached_at: chrono::Utc::now()
            - chrono::Duration::from_std(session.detached_at.elapsed())
                .unwrap_or_else(|_| chrono::Duration::zero()),
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

/// Parse a wire-XML fragment back to a typed persisted unacked stanza.
pub(super) fn parse_xml_to_persisted_unacked(
    stream_id: &str,
    sequence: u32,
    stanza_xml: &str,
    original_receipt_at: chrono::DateTime<chrono::Utc>,
) -> Result<super::super::persistence::PersistedUnackedStanza, SmRegistryError> {
    let element: minidom::Element = stanza_xml.parse().map_err(|e: minidom::Error| {
        SmRegistryError::Internal(format!("parse unacked stanza for persistence: {e}"))
    })?;
    let stanza = match element.name() {
        "message" => crate::Stanza::Message(
            xmpp_parsers::message::Message::try_from(element)
                .map_err(|e| SmRegistryError::Internal(e.to_string()))?,
        ),
        "iq" => crate::Stanza::Iq(Box::new(
            xmpp_parsers::iq::Iq::try_from(element)
                .map_err(|e| SmRegistryError::Internal(e.to_string()))?,
        )),
        "presence" => crate::Stanza::Presence(
            xmpp_parsers::presence::Presence::try_from(element)
                .map_err(|e| SmRegistryError::Internal(e.to_string()))?,
        ),
        other => {
            return Err(SmRegistryError::Internal(format!(
                "unknown unacked stanza element '{other}'"
            )));
        }
    };
    Ok(super::super::persistence::PersistedUnackedStanza {
        stream_id: crate::pending_delivery::SmSessionId::new(stream_id.to_string()),
        sequence,
        stanza: Box::new(stanza),
        original_receipt_at,
    })
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
        .map(|row| {
            let element: minidom::Element = match &*row.stanza {
                crate::Stanza::Message(m) => m.clone().into(),
                crate::Stanza::Iq(iq) => (**iq).clone().into(),
                crate::Stanza::Presence(p) => p.clone().into(),
            };
            let mut buf = Vec::new();
            element
                .write_to(&mut buf)
                .map_err(|e| SmRegistryError::Internal(format!("serialize unacked stanza: {e}")))?;
            let xml = String::from_utf8(buf)
                .map_err(|e| SmRegistryError::Internal(format!("serialize unacked stanza: {e}")))?;
            Ok(DetachedUnackedStanza {
                sequence: row.sequence,
                stanza_xml: xml,
                original_receipt_at: row.original_receipt_at,
            })
        })
        .collect::<Result<_, SmRegistryError>>()?;

    // The database's `ORDER BY sequence ASC` is only a stable numeric pre-sort:
    // it is wrong after the counter wraps. This is the authoritative
    // wrap-aware re-sort for every hydration caller, relative to `last_acked`.
    let mut unacked_stanzas = unacked_stanzas;
    unacked_stanzas.sort_by_key(|entry| entry.sequence.wrapping_sub(persisted.last_acked));

    Ok(DetachedSession {
        stream_id: persisted.stream_id.as_str().to_string(),
        user_id: persisted.user_id.clone(),
        jid: persisted.jid.clone(),
        inbound_count: persisted.inbound_count,
        shadow_ordinal: persisted.shadow_ordinal,
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use xmpp_parsers::message::{Lang, Message};

    use super::super::super::persistence::{PersistedSession, PersistedUnackedStanza};
    use super::persisted_to_detached;

    fn persisted_session(last_acked: u32, outbound_count: u32) -> PersistedSession {
        PersistedSession {
            stream_id: crate::pending_delivery::SmSessionId::new("codec-wrap".to_string()),
            user_id: "alice".to_string(),
            jid: "alice@example.com/web".parse().expect("full jid"),
            inbound_count: 0,
            shadow_ordinal: crate::stream_management::ShadowOrdinal::from_storage(7),
            outbound_count,
            last_acked,
            replay_gap_through: None,
            max_resume_time: None,
            detached_at: chrono::Utc::now(),
            max_resume_duration: Duration::from_secs(300),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
        }
    }

    fn persisted_row(sequence: u32) -> PersistedUnackedStanza {
        let mut message = Message::new(None::<jid::Jid>);
        message
            .bodies
            .insert(Lang::new(), format!("seq-{sequence}"));
        PersistedUnackedStanza {
            stream_id: crate::pending_delivery::SmSessionId::new("codec-wrap".to_string()),
            sequence,
            stanza: Box::new(crate::Stanza::Message(message)),
            original_receipt_at: chrono::Utc::now(),
        }
    }

    /// The SQL backends return rows in numeric `ORDER BY sequence ASC`,
    /// which is wrong across the 2^32 wrap (post-wrap low sequences sort
    /// first). The codec's re-sort is the single authoritative fix for
    /// every hydration caller; feed it exactly the numeric order SQL
    /// produces and assert wrap order comes out.
    #[test]
    fn hydration_re_sorts_numeric_sql_order_into_wrap_order() {
        let last_acked = u32::MAX - 2;
        let wrapped: Vec<u32> = vec![0, 1, u32::MAX - 1, u32::MAX];
        let session = persisted_session(last_acked, 1);
        let rows: Vec<PersistedUnackedStanza> =
            wrapped.iter().copied().map(persisted_row).collect();

        let detached = persisted_to_detached(&session, &rows).expect("codec hydration");

        let hydrated: Vec<u32> = detached
            .unacked_stanzas
            .iter()
            .map(|entry| entry.sequence)
            .collect();
        assert_eq!(
            hydrated,
            vec![u32::MAX - 1, u32::MAX, 0, 1],
            "numeric ASC input must be re-sorted relative to last_acked"
        );
    }
}
