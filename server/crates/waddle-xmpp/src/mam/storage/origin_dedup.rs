use waddle_xmpp_core::mam::ArchivedMessage;
use xmpp_parsers::message::MessageType;

pub(super) fn origin_id_dedup_match(
    existing: &ArchivedMessage,
    incoming: &ArchivedMessage,
) -> bool {
    let Some(existing_origin_id) = existing.origin_id.as_ref() else {
        return false;
    };
    let Some(incoming_origin_id) = incoming.origin_id.as_ref() else {
        return false;
    };

    existing_origin_id == incoming_origin_id
        && sender_scope_matches(existing, incoming)
        && archived_content_matches(existing, incoming)
}

fn sender_scope_matches(existing: &ArchivedMessage, incoming: &ArchivedMessage) -> bool {
    match (&existing.message_type, &incoming.message_type) {
        (MessageType::Groupchat, MessageType::Groupchat) => {
            existing.from == incoming.from
                && existing.nickname_generation == incoming.nickname_generation
        }
        (MessageType::Groupchat, _) | (_, MessageType::Groupchat) => false,
        _ => {
            existing.from.to_bare() == incoming.from.to_bare()
                && existing.to.to_bare() == incoming.to.to_bare()
        }
    }
}

fn archived_content_matches(existing: &ArchivedMessage, incoming: &ArchivedMessage) -> bool {
    // Fresh-session retries are re-stamped by the server before this
    // storage boundary, so archive primary key, timestamp, stanza-id,
    // and raw replay XML are intentionally excluded. The projected
    // MAM content must still match before origin-id is accepted as a
    // retry key; a reused origin-id with new content is a distinct
    // message and must be archived separately.
    existing.body == incoming.body
        && existing.thread == incoming.thread
        && existing.reply == incoming.reply
        && existing.message_type == incoming.message_type
        && existing.rich == incoming.rich
}
