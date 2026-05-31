use super::*;

pub(super) fn decode_session(row: &crate::db::Row) -> Result<PersistedSession, SmPersistenceError> {
    let stream_id: String = row
        .get(0)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let user_id: String = row
        .get(1)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let full_jid_raw: String = row
        .get(2)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let jid: FullJid = full_jid_raw
        .parse()
        .map_err(|e: jid::Error| SmPersistenceError::Other(e.to_string()))?;
    let inbound_count: i64 = row
        .get(3)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let outbound_count: i64 = row
        .get(4)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let last_acked: i64 = row
        .get(5)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let max_resume_secs: Option<i64> = row
        .get(6)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let detached_at_ms: i64 = row
        .get(7)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let max_resume_duration_ms: i64 = row
        .get(8)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let carbons_enabled: i64 = row
        .get(9)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let roster_interested: i64 = row
        .get(10)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let blocklist_interested: i64 = row
        .get(11)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let presence_available: i64 = row
        .get(12)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let presence_show_raw: Option<String> = row
        .get(13)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let presence_status: Option<String> = row
        .get(14)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let presence_priority: i64 = row
        .get(15)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let replay_gap_through: Option<i64> = row
        .get(16)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

    let detached_at = DateTime::<Utc>::from_timestamp_millis(detached_at_ms)
        .ok_or_else(|| SmPersistenceError::Other("invalid detached_at_ms".into()))?;
    let max_resume_duration =
        std::time::Duration::from_millis(max_resume_duration_ms.max(0) as u64);
    let presence_show = presence_show_raw.as_deref().map(parse_show).transpose()?;

    Ok(PersistedSession {
        stream_id: SmSessionId::new(stream_id),
        user_id,
        jid,
        inbound_count: inbound_count.max(0) as u32,
        outbound_count: outbound_count.max(0) as u32,
        last_acked: last_acked.max(0) as u32,
        replay_gap_through: replay_gap_through.map(|v| v.max(0) as u32),
        max_resume_time: max_resume_secs.map(|v| v.max(0) as u32),
        detached_at,
        max_resume_duration,
        carbons_enabled: carbons_enabled != 0,
        roster_interested: roster_interested != 0,
        blocklist_interested: blocklist_interested != 0,
        presence_available: presence_available != 0,
        presence_show,
        presence_status,
        presence_priority: presence_priority.clamp(i8::MIN as i64, i8::MAX as i64) as i8,
    })
}

pub(super) fn decode_unacked(
    row: &crate::db::Row,
) -> Result<PersistedUnackedStanza, SmPersistenceError> {
    let stream_id: String = row
        .get(0)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let sequence: i64 = row
        .get(1)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let stanza_xml: String = row
        .get(2)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let receipt_ms: i64 = row
        .get(3)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

    let original_receipt_at = DateTime::<Utc>::from_timestamp_millis(receipt_ms)
        .ok_or_else(|| SmPersistenceError::Other("invalid receipt timestamp".into()))?;
    let element: xmpp_parsers::minidom::Element = stanza_xml
        .parse()
        .map_err(|e: xmpp_parsers::minidom::Error| SmPersistenceError::Other(e.to_string()))?;
    let stanza = parse_stanza(element)?;

    Ok(PersistedUnackedStanza {
        stream_id: SmSessionId::new(stream_id),
        sequence: sequence.max(0) as u32,
        stanza: Box::new(stanza),
        original_receipt_at,
    })
}

/// Decode an unacked-stanza row from a JOIN result. Reads
/// `stream_id` from column 0 (the session's stream_id),
/// `stanza_xml` from column 18, and `original_receipt_at_ms`
/// from column 19. Caller already has `sequence` (column 17).
/// Used by `list_all_sessions_with_unacked` (issue #209 PR #405).
pub(super) fn decode_unacked_join_row(
    row: &crate::db::Row,
    sequence_i64: i64,
) -> Result<PersistedUnackedStanza, SmPersistenceError> {
    let stream_id: String = row
        .get(0)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let stanza_xml: String = row
        .get(18)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let receipt_ms: i64 = row
        .get(19)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let original_receipt_at = DateTime::<Utc>::from_timestamp_millis(receipt_ms)
        .ok_or_else(|| SmPersistenceError::Other("invalid unacked receipt timestamp".into()))?;
    let element: xmpp_parsers::minidom::Element = stanza_xml
        .parse()
        .map_err(|e: xmpp_parsers::minidom::Error| SmPersistenceError::Other(e.to_string()))?;
    let stanza = parse_stanza(element)?;
    let sequence =
        u32::try_from(sequence_i64).map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    Ok(PersistedUnackedStanza {
        stream_id: SmSessionId::new(stream_id),
        sequence,
        stanza: Box::new(stanza),
        original_receipt_at,
    })
}

pub(super) fn show_wire_str(show: &Show) -> &'static str {
    match show {
        Show::Away => "away",
        Show::Chat => "chat",
        Show::Dnd => "dnd",
        Show::Xa => "xa",
    }
}

fn parse_show(raw: &str) -> Result<Show, SmPersistenceError> {
    match raw {
        "away" => Ok(Show::Away),
        "chat" => Ok(Show::Chat),
        "dnd" => Ok(Show::Dnd),
        "xa" => Ok(Show::Xa),
        other => Err(SmPersistenceError::Other(format!(
            "unknown presence show value '{other}'"
        ))),
    }
}

pub(super) fn serialize_stanza(stanza: &Stanza) -> Result<String, SmPersistenceError> {
    let element: xmpp_parsers::minidom::Element = match stanza {
        Stanza::Message(m) => m.clone().into(),
        Stanza::Iq(iq) => (*iq.clone()).into(),
        Stanza::Presence(p) => p.clone().into(),
    };
    let mut buf = Vec::new();
    element
        .write_to(&mut buf)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    String::from_utf8(buf).map_err(|e| SmPersistenceError::Other(e.to_string()))
}

fn parse_stanza(element: xmpp_parsers::minidom::Element) -> Result<Stanza, SmPersistenceError> {
    match element.name() {
        "message" => xmpp_parsers::message::Message::try_from(element)
            .map(Stanza::Message)
            .map_err(|e| SmPersistenceError::Other(e.to_string())),
        "iq" => xmpp_parsers::iq::Iq::try_from(element)
            .map(|iq| Stanza::Iq(Box::new(iq)))
            .map_err(|e| SmPersistenceError::Other(e.to_string())),
        "presence" => xmpp_parsers::presence::Presence::try_from(element)
            .map(Stanza::Presence)
            .map_err(|e| SmPersistenceError::Other(e.to_string())),
        other => Err(SmPersistenceError::Other(format!(
            "unknown stanza element '{other}'"
        ))),
    }
}
