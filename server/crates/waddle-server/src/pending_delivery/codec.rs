use super::*;

pub(super) const PAYLOAD_KIND_ARCHIVED: &str = "archived";
pub(super) const PAYLOAD_KIND_TRANSIENT: &str = "transient";

pub(super) fn decode_row(row: &crate::db::Row) -> Result<PendingRow, PendingStorageError> {
    let row_id: String = row
        .get(0)
        .map_err(|e| PendingStorageError::Other(e.to_string()))?;
    let id = PendingRowId::new(row_id);
    let recipient_jid: String = row
        .get(1)
        .map_err(|e| PendingStorageError::Other(e.to_string()))?;
    let recipient: BareJid = recipient_jid
        .parse()
        .map_err(|e: jid::Error| PendingStorageError::Other(e.to_string()))?;
    let original_receipt_at_ms: i64 = row
        .get(2)
        .map_err(|e| PendingStorageError::Other(e.to_string()))?;
    let original_receipt_at =
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(original_receipt_at_ms)
            .ok_or_else(|| PendingStorageError::Other("invalid receipt timestamp".into()))?;
    let payload_kind: String = row
        .get(3)
        .map_err(|e| PendingStorageError::Other(e.to_string()))?;
    let archive_stanza_by: Option<String> = row
        .get(4)
        .map_err(|e| PendingStorageError::Other(e.to_string()))?;
    let archive_stanza_id: Option<String> = row
        .get(5)
        .map_err(|e| PendingStorageError::Other(e.to_string()))?;
    let transient_xml: Option<String> = row
        .get(6)
        .map_err(|e| PendingStorageError::Other(e.to_string()))?;
    let flushed_in_session: Option<String> = row
        .get(7)
        .map_err(|e| PendingStorageError::Other(e.to_string()))?;
    let outbound_sequence_i64: Option<i64> = row
        .get(8)
        .map_err(|e| PendingStorageError::Other(e.to_string()))?;
    let outbound_sequence = outbound_sequence_i64
        .map(|v| u32::try_from(v).map_err(|e| PendingStorageError::Other(e.to_string())))
        .transpose()?;

    let payload = match payload_kind.as_str() {
        PAYLOAD_KIND_ARCHIVED => {
            let by_str = archive_stanza_by.ok_or_else(|| {
                PendingStorageError::Other("archived row missing archive_stanza_by".into())
            })?;
            let by: BareJid = by_str
                .parse()
                .map_err(|e: jid::Error| PendingStorageError::Other(e.to_string()))?;
            let id_str = archive_stanza_id.ok_or_else(|| {
                PendingStorageError::Other("archived row missing archive_stanza_id".into())
            })?;
            let archive_jid: jid::Jid = by.into();
            PendingPayload::Archived(StanzaId::new(id_str, archive_jid))
        }
        PAYLOAD_KIND_TRANSIENT => {
            let xml = transient_xml.ok_or_else(|| {
                PendingStorageError::Other("transient row missing transient_xml".into())
            })?;
            let element: xmpp_parsers::minidom::Element =
                xml.parse().map_err(|e: xmpp_parsers::minidom::Error| {
                    PendingStorageError::Other(e.to_string())
                })?;
            let message = xmpp_parsers::message::Message::try_from(element)
                .map_err(|e| PendingStorageError::Other(e.to_string()))?;
            PendingPayload::Transient(Box::new(message))
        }
        other => {
            return Err(PendingStorageError::Other(format!(
                "unknown payload_kind '{other}'"
            )))
        }
    };
    Ok(PendingRow {
        id,
        recipient,
        original_receipt_at,
        payload,
        flushed_in_session: flushed_in_session.map(SmSessionId::new),
        outbound_sequence,
    })
}

pub(super) fn serialize_message(
    message: &xmpp_parsers::message::Message,
) -> Result<String, PendingStorageError> {
    let element = xmpp_parsers::minidom::Element::from(message.clone());
    let mut buf = Vec::new();
    element
        .write_to(&mut buf)
        .map_err(|e| PendingStorageError::Other(e.to_string()))?;
    String::from_utf8(buf).map_err(|e| PendingStorageError::Other(e.to_string()))
}
