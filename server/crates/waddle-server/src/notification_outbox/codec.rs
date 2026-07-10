//! Row/JSON codecs between durable storage and typed values.

use super::*;

pub(super) fn encode_sender_jids(sender_jids: &[Jid]) -> Result<String, NotificationOutboxError> {
    let values: Vec<String> = sender_jids.iter().map(ToString::to_string).collect();
    serde_json::to_string(&values)
        .map_err(|error| NotificationOutboxError::InvalidSenderJids(error.to_string()))
}

pub(super) fn decode_sender_jids(raw: &str) -> Result<Vec<Jid>, NotificationOutboxError> {
    let values: Vec<String> = serde_json::from_str(raw)
        .map_err(|error| NotificationOutboxError::InvalidSenderJids(error.to_string()))?;
    values
        .into_iter()
        .map(|value| {
            value
                .parse()
                .map_err(|_| NotificationOutboxError::InvalidSenderJid(value))
        })
        .collect()
}

pub(super) fn decode_candidate(
    row: &Row,
) -> Result<NotificationCandidate, NotificationOutboxError> {
    let recipient_raw: String = row.get(0)?;
    let conversation_raw: String = row.get(1)?;
    let sender_raw = row
        .get::<Option<String>>(2)?
        .ok_or_else(|| NotificationOutboxError::InvalidSenderJid("<null>".to_string()))?;
    let sender_jid = sender_raw
        .parse()
        .map_err(|_| NotificationOutboxError::InvalidSenderJid(sender_raw))?;
    require_full_sender_jid(&sender_jid)?;
    let conversation_jid: BareJid = conversation_raw
        .parse()
        .map_err(|_| NotificationOutboxError::InvalidConversationJid(conversation_raw))?;
    require_sender_matches_conversation(&sender_jid, &conversation_jid)?;
    let stanza_id_by_raw: String = row.get(4)?;
    Ok(NotificationCandidate {
        recipient_bare_jid: recipient_raw
            .parse()
            .map_err(|_| NotificationOutboxError::InvalidRecipientBareJid(recipient_raw))?,
        conversation_jid,
        sender_jid,
        thread_id: NotificationThreadId::new(row.get::<String>(3)?),
        archive_stanza_id: StanzaId::new(
            row.get::<String>(5)?,
            stanza_id_by_raw
                .parse()
                .map_err(|_| NotificationOutboxError::InvalidArchiveStanzaIdBy(stanza_id_by_raw))?,
        ),
        class: NotificationClass::from_db_value(&row.get::<String>(6)?)?,
        reason: NotificationReason::from_db_value(&row.get::<String>(7)?)?,
        policy_error_count: row.get(8)?,
        noping: row.get::<i64>(9)? != 0,
        no_store: row.get::<i64>(10)? != 0,
        no_permanent_store: row.get::<i64>(11)? != 0,
        last_message_body: row.get::<Option<String>>(12)?,
        reaction: row.get::<i64>(13)? != 0,
    })
}

pub(super) fn decode_outbox_job(
    row: &Row,
) -> Result<NotificationOutboxJob, NotificationOutboxError> {
    let recipient_raw: String = row.get(1)?;
    let push_service_raw: String = row.get(2)?;
    let conversation_raw: String = row.get(4)?;
    let sender_raw = row
        .get::<Option<String>>(5)?
        .ok_or_else(|| NotificationOutboxError::InvalidSenderJid("<null>".to_string()))?;
    let sender_jids_raw = row
        .get::<Option<String>>(6)?
        .ok_or(NotificationOutboxError::MissingSenderJidSet)?;
    let message_count: i64 = row.get(9)?;
    let context_xml: String = row.get(10)?;
    let summary_sender_raw: Option<String> = row.get(15)?;
    let summary_body: Option<String> = row.get(16)?;
    let sender_jid: Jid = sender_raw
        .parse()
        .map_err(|_| NotificationOutboxError::InvalidSenderJid(sender_raw))?;
    require_full_sender_jid(&sender_jid)?;
    let conversation_jid: BareJid = conversation_raw
        .parse()
        .map_err(|_| NotificationOutboxError::InvalidConversationJid(conversation_raw))?;
    require_sender_matches_conversation(&sender_jid, &conversation_jid)?;
    let sender_jids = decode_sender_jids(&sender_jids_raw)?;
    require_full_sender_jid_set(&sender_jids)?;
    require_sender_set_matches_conversation(&sender_jids, &conversation_jid)?;
    require_sender_set_contains_scalar(&sender_jids, &sender_jid)?;
    // Rehydrate the T1-resolved rich summary directly from its own
    // columns — `summary_sender_jid` is the `last-message-sender`
    // (present iff the recipient opted in), `summary_body` the
    // (hint-stripped) `last-message-body`. Stored explicitly, so no
    // inference from the routing `sender_jid` is needed.
    let summary_sender = summary_sender_raw
        .map(|raw| {
            raw.parse::<Jid>()
                .map_err(|_| NotificationOutboxError::InvalidSenderJid(raw))
        })
        .transpose()?;
    let rich_summary = RichSummary {
        sender: summary_sender,
        body: summary_body,
    };
    Ok(NotificationOutboxJob {
        job_id: NotificationOutboxJobId::from(row.get::<String>(0)?),
        recipient_bare_jid: recipient_raw
            .parse()
            .map_err(|_| NotificationOutboxError::InvalidRecipientBareJid(recipient_raw))?,
        push_service_jid: push_service_raw
            .parse()
            .map_err(|_| NotificationOutboxError::InvalidPushServiceBareJid(push_service_raw))?,
        node: PushServiceNodeName::new(row.get::<String>(3)?)?,
        conversation_jid,
        sender_jid,
        sender_jids,
        thread_id: NotificationThreadId::new(row.get::<String>(7)?),
        class: NotificationClass::from_db_value(&row.get::<String>(8)?)?,
        message_count: u32::try_from(message_count)
            .map_err(|_| NotificationOutboxError::InvalidMessageCount(message_count))?,
        context: context_xml
            .parse::<Element>()
            .map_err(|error| NotificationOutboxError::InvalidContextXml(error.to_string()))?,
        rich_summary,
        status: NotificationOutboxStatus::from_db_value(&row.get::<String>(11)?)?,
        attempt_count: row.get(12)?,
        policy_error_count: row.get(13)?,
        claim_token: row.get(14)?,
    })
}
