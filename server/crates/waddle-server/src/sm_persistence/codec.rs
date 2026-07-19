use super::*;

pub(crate) fn decode_session(row: &crate::db::Row) -> Result<PersistedSession, SmPersistenceError> {
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
    let presence_payloads_raw: Option<String> = row
        .get(17)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

    let detached_at = DateTime::<Utc>::from_timestamp_millis(detached_at_ms)
        .ok_or_else(|| SmPersistenceError::Other("invalid detached_at_ms".into()))?;
    let max_resume_duration =
        std::time::Duration::from_millis(max_resume_duration_ms.max(0) as u64);
    let presence_show = presence_show_raw.as_deref().map(parse_show).transpose()?;
    // Degrade a malformed `presence_payloads` cell to caps-less rather than
    // failing the whole session decode. Presence extension payloads are
    // non-essential decoration that the client re-advertises on its next
    // presence broadcast, whereas a decode error here would poison the
    // session on cold start (`list_all_sessions_with_unacked`) and drop its
    // XEP-0198 unacked message queue with it — a strictly worse trade
    // (#1206 review). The column is always server-serialized well-formed XML
    // via the minidom builder, so this only guards against storage-layer
    // corruption of that single cell.
    let presence_payloads =
        parse_presence_payloads(presence_payloads_raw).unwrap_or_else(|error| {
            tracing::warn!(
                stream_id = %stream_id,
                %error,
                "sm session presence_payloads failed to decode; restoring the session \
                 caps-less rather than dropping it (its unacked queue is preserved)"
            );
            Vec::new()
        });

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
        presence_payloads,
    })
}

pub(crate) fn decode_unacked(
    row: &crate::db::Row,
) -> Result<PersistedUnackedStanza, SmPersistenceError> {
    let stream_id: String = row
        .get(0)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let purpose_raw: String =
        row.get(4)
            .map_err(|e| SmPersistenceError::InvalidUnackedPurpose {
                detail: e.to_string(),
            })?;
    // Purpose is the replay-policy boundary. Validate it before decoding any
    // other fallible payload field so a compound-corrupt row cannot be
    // downgraded to a generic row skip before the caller sees the typed
    // quarantine signal.
    let purpose = parse_unacked_purpose(&purpose_raw)?;
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
        purpose,
    })
}

/// Decode an unacked-stanza row from a JOIN result. Reads
/// `stream_id` from column 0 (the session's stream_id), `sequence` from
/// column 18, `stanza_xml` from column 19, `original_receipt_at_ms` from
/// column 20, and `purpose` from column 21. Returns `None` for the all-NULL
/// unacked side of a LEFT JOIN.
/// The unacked columns sit after the 18 session columns
/// (0..=17, presence_payloads added at 17 for #1206).
/// Used by `list_all_sessions_with_unacked` (issue #209 PR #405).
pub(super) fn decode_unacked_join_row(
    row: &crate::db::Row,
) -> Result<Option<PersistedUnackedStanza>, SmPersistenceError> {
    let stream_id: String = row
        .get(0)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let purpose_raw: Option<String> =
        row.get(21)
            .map_err(|e| SmPersistenceError::InvalidUnackedPurpose {
                detail: e.to_string(),
            })?;
    let Some(purpose_raw) = purpose_raw else {
        let sequence: Option<i64> =
            row.get(18)
                .map_err(|e| SmPersistenceError::InvalidUnackedPurpose {
                    detail: format!("missing purpose alongside unreadable sequence: {e}"),
                })?;
        if sequence.is_none() {
            return Ok(None);
        }
        return Err(SmPersistenceError::InvalidUnackedPurpose {
            detail: "missing value on a joined unacked row".to_string(),
        });
    };
    // The JOIN loader quarantines an entire session only for this typed
    // purpose error. Parse it before even decoding the sequence column so no
    // other malformed field can hide an unknown replay policy and restore a
    // partial queue with a replay hole.
    let purpose = parse_unacked_purpose(&purpose_raw)?;
    let sequence_i64: i64 = row
        .get(18)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let stanza_xml: String = row
        .get(19)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let receipt_ms: i64 = row
        .get(20)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    let original_receipt_at = DateTime::<Utc>::from_timestamp_millis(receipt_ms)
        .ok_or_else(|| SmPersistenceError::Other("invalid unacked receipt timestamp".into()))?;
    let element: xmpp_parsers::minidom::Element = stanza_xml
        .parse()
        .map_err(|e: xmpp_parsers::minidom::Error| SmPersistenceError::Other(e.to_string()))?;
    let stanza = parse_stanza(element)?;
    let sequence =
        u32::try_from(sequence_i64).map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    Ok(Some(PersistedUnackedStanza {
        stream_id: SmSessionId::new(stream_id),
        sequence,
        stanza: Box::new(stanza),
        original_receipt_at,
        purpose,
    }))
}

pub(crate) fn unacked_purpose_wire_str(purpose: SmUnackedStanzaPurpose) -> &'static str {
    match purpose {
        SmUnackedStanzaPurpose::Application => "application",
        SmUnackedStanzaPurpose::ResumeBarrier => "resume_barrier",
    }
}

fn parse_unacked_purpose(raw: &str) -> Result<SmUnackedStanzaPurpose, SmPersistenceError> {
    match raw {
        "application" => Ok(SmUnackedStanzaPurpose::Application),
        "resume_barrier" => Ok(SmUnackedStanzaPurpose::ResumeBarrier),
        other => Err(SmPersistenceError::InvalidUnackedPurpose {
            detail: format!("unknown value '{other}'"),
        }),
    }
}

pub(crate) fn show_wire_str(show: &Show) -> &'static str {
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

pub(crate) fn serialize_stanza(stanza: &Stanza) -> Result<String, SmPersistenceError> {
    let element = stanza.to_element();
    let mut buf = Vec::new();
    element
        .write_to(&mut buf)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    String::from_utf8(buf).map_err(|e| SmPersistenceError::Other(e.to_string()))
}

/// Wrapper element name+namespace for encoding a resource's presence
/// extension payloads into the single `sm_sessions.presence_payloads`
/// TEXT column. This is an internal storage encoding only — the wrapper
/// is never written to the XMPP wire (only its children are, individually,
/// on probe/subscription delivery), so a `urn:waddle:*` namespace is
/// appropriate and does not fall under the XEP wire-conformance rule.
const PRESENCE_PAYLOADS_WRAPPER_NAME: &str = "payloads";
const PRESENCE_PAYLOADS_WRAPPER_NS: &str = "urn:waddle:sm:presence-payloads:0";

/// Serialize a resource's presence extension payloads to a single XML
/// string for durable storage, or `None` when there are none (so the
/// column stays NULL). The children are wrapped in one container element
/// built via the minidom element builder — never `format!`/string concat,
/// per the XML hard rule.
pub(crate) fn serialize_presence_payloads(
    payloads: &[xmpp_parsers::minidom::Element],
) -> Result<Option<String>, SmPersistenceError> {
    if payloads.is_empty() {
        return Ok(None);
    }
    let mut builder = xmpp_parsers::minidom::Element::builder(
        PRESENCE_PAYLOADS_WRAPPER_NAME,
        PRESENCE_PAYLOADS_WRAPPER_NS,
    );
    for child in payloads {
        builder = builder.append(child.clone());
    }
    let mut buf = Vec::new();
    builder
        .build()
        .write_to(&mut buf)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))
}

/// Parse the durable `presence_payloads` column back into typed elements
/// (exactly once, per the typed-payloads hard rule). A NULL / empty column
/// yields an empty vec.
pub(crate) fn parse_presence_payloads(
    raw: Option<String>,
) -> Result<Vec<xmpp_parsers::minidom::Element>, SmPersistenceError> {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return Ok(Vec::new());
    };
    let wrapper: xmpp_parsers::minidom::Element = raw
        .parse()
        .map_err(|e: xmpp_parsers::minidom::Error| SmPersistenceError::Other(e.to_string()))?;
    // Validate the wrapper shape so a well-formed-but-wrong-root cell (e.g. a
    // corruption that leaves only a lone `<c/>`) is a typed error, not a
    // silent empty/wrong result — otherwise it would bypass the caller's
    // warn+degrade path in `decode_session` (Greptile/Qodo review).
    if wrapper.name() != PRESENCE_PAYLOADS_WRAPPER_NAME
        || wrapper.ns() != PRESENCE_PAYLOADS_WRAPPER_NS
    {
        return Err(SmPersistenceError::Other(format!(
            "unexpected presence_payloads wrapper <{} xmlns='{}'>",
            wrapper.name(),
            wrapper.ns()
        )));
    }
    Ok(wrapper.children().cloned().collect())
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

#[cfg(test)]
mod presence_payloads_tests {
    use super::{parse_presence_payloads, serialize_presence_payloads};
    use xmpp_parsers::minidom::Element;

    #[test]
    fn empty_payloads_serialize_to_none_so_the_column_stays_null() {
        assert_eq!(serialize_presence_payloads(&[]).unwrap(), None);
    }

    #[test]
    fn null_or_empty_column_parses_to_no_payloads() {
        assert!(parse_presence_payloads(None).unwrap().is_empty());
        assert!(parse_presence_payloads(Some(String::new()))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn multiple_payloads_round_trip_verbatim_and_in_order() {
        let caps: Element = r#"<c xmlns='http://jabber.org/protocol/caps' hash='sha-1' node='https://example.com/client' ver='zHyEOgxTrkpSdGcQKH8EFPLsriY='/>"#
            .parse()
            .unwrap();
        let idle: Element = r#"<idle xmlns='urn:xmpp:idle:1' since='2026-07-08T10:00:00+00:00'/>"#
            .parse()
            .unwrap();
        let encoded = serialize_presence_payloads(&[caps.clone(), idle.clone()])
            .unwrap()
            .expect("non-empty payloads serialize to Some");
        let decoded = parse_presence_payloads(Some(encoded)).unwrap();
        assert_eq!(decoded, vec![caps, idle]);
    }

    #[test]
    fn malformed_column_is_a_typed_error_not_a_panic() {
        // A corrupted / truncated column must surface as a typed decode
        // error — never a panic — so `decode_session` can catch it and
        // degrade the session to caps-less rather than crashing cold startup.
        assert!(parse_presence_payloads(Some("<c xmlns='urn:x'".to_string())).is_err());
    }

    #[test]
    fn well_formed_wrong_root_is_a_typed_error_so_the_caller_can_degrade() {
        // A cell that still parses as valid XML but is NOT the server-written
        // wrapper (e.g. corruption leaving a lone `<c/>`) must be a typed
        // error, not a silent empty result — otherwise it would bypass
        // `decode_session`'s warn+degrade path and lose payloads without a
        // trace.
        let lone_caps =
            r#"<c xmlns='http://jabber.org/protocol/caps' hash='sha-1' node='n' ver='v'/>"#;
        assert!(parse_presence_payloads(Some(lone_caps.to_string())).is_err());
    }
}
