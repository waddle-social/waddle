use super::*;

pub(crate) fn decode_row(row: &crate::db::Row) -> Result<InboxEntry, InboxStorageError> {
    let partner_raw: String = row
        .get(0)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let partner: BareJid = partner_raw
        .parse()
        .map_err(|error| InboxStorageError::Other(format!("invalid partner JID: {error}")))?;
    let thread_id_raw: String = row
        .get(1)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let kind_raw: String = row
        .get(2)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let last_stanza_id: String = row
        .get(3)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let last_updated: i64 = row
        .get(4)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let unread: i64 = row
        .get(5)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let preview: Option<String> = row
        .get(6)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let thread_title: Option<String> = row
        .get(7)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let reply_count: i64 = row
        .get(8)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let author: Option<String> = row
        .get(9)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;

    Ok(InboxEntry {
        partner,
        kind: decode_kind(&kind_raw)?,
        last_stanza_id,
        last_updated,
        unread: unread.max(0) as u32,
        preview,
        thread_id: if thread_id_raw.is_empty() {
            None
        } else {
            Some(thread_id_raw)
        },
        thread_title,
        reply_count: reply_count.max(0) as u32,
        author,
    })
}

pub(super) fn encode_kind(kind: ConversationKind) -> &'static str {
    match kind {
        ConversationKind::Direct => "direct",
        ConversationKind::MucRoom => "muc",
    }
}

fn decode_kind(raw: &str) -> Result<ConversationKind, InboxStorageError> {
    match raw {
        "direct" => Ok(ConversationKind::Direct),
        "muc" => Ok(ConversationKind::MucRoom),
        other => Err(InboxStorageError::Other(format!(
            "unknown inbox conversation kind '{other}'"
        ))),
    }
}

pub const SELECT_COLS: &str = "partner_jid, thread_id, kind, last_stanza_id, last_updated, unread, preview, thread_title, reply_count, author";
