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
    //
    // The server-derived MUC identity fields (`occupant_id`,
    // `muc_sender`) are ALSO excluded via `content_only()`:
    // `muc_sender.jid` carries the sender's per-session full JID (a
    // fresh random resource each reconnect), so comparing it would
    // make every fresh-session retry look like new content and
    // duplicate the archive row.
    existing.body == incoming.body
        && existing.thread == incoming.thread
        && existing.reply == incoming.reply
        && existing.message_type == incoming.message_type
        && rich_content_matches(existing, incoming)
}

fn rich_content_matches(existing: &ArchivedMessage, incoming: &ArchivedMessage) -> bool {
    let existing = existing.rich.as_ref().map(|rich| rich.content_only());
    let incoming = incoming.rich.as_ref().map(|rich| rich.content_only());
    existing == incoming
}
