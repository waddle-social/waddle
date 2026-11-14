//! Shared runtime helpers for projecting live message traffic into Waddle inbox rows.

use std::borrow::Cow;

use jid::BareJid;
use xmpp_parsers::message::Message;

use super::{ConversationKind, InboxEntry};
use crate::mam::STANZA_ID_NS;
use crate::xep::xep0424::extract_retraction_from_message;
use crate::xep::xep0430::InboxQuery;
use crate::xep::{
    has_file_sharing, is_moderation_result_message, is_reaction_message, is_sticker_message,
    should_skip_storage,
};

/// Longest preview, thread title or author label an inbox row carries, in
/// characters.
///
/// These labels travel as XML attribute values, and an attribute value over
/// the parser's token limit (8192 bytes) makes the whole stanza unparseable.
pub const MAX_INBOX_LABEL_CHARS: usize = 256;

/// Cut `text` to at most [`MAX_INBOX_LABEL_CHARS`] characters, ending a cut
/// label with an ellipsis.
pub fn bounded_label(text: &str) -> Cow<'_, str> {
    match text.char_indices().nth(MAX_INBOX_LABEL_CHARS - 1) {
        Some((cut, last)) if cut + last.len_utf8() < text.len() => {
            Cow::Owned(format!("{}…", &text[..cut]))
        }
        _ => Cow::Borrowed(text),
    }
}

pub fn should_project_message(msg: &Message) -> bool {
    if should_skip_storage(msg) {
        return false;
    }

    if !msg.bodies.is_empty() || !msg.subjects.is_empty() {
        return true;
    }

    is_reaction_message(msg)
        || matches!(
            extract_retraction_from_message(msg),
            Some(crate::xep::RetractionKind::Request(_))
        )
        || is_moderation_result_message(msg)
        || has_file_sharing(msg)
        || is_sticker_message(msg)
}

pub fn preview_text(msg: &Message) -> Option<String> {
    msg.bodies
        .get("")
        .or_else(|| msg.bodies.values().next())
        .cloned()
        .or_else(|| {
            msg.subjects
                .get("")
                .or_else(|| msg.subjects.values().next())
                .cloned()
        })
        .map(|text| bounded_label(text.trim()).into_owned())
        .filter(|text| !text.is_empty())
}

pub fn projection_stanza_id(msg: &Message) -> String {
    msg.id
        .as_ref()
        .map(|id| id.0.clone())
        .or_else(|| {
            msg.payloads
                .iter()
                .find(|payload| payload.name() == "origin-id" && payload.ns() == STANZA_ID_NS)
                .and_then(|payload| payload.attr("id").map(ToOwned::to_owned))
        })
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

pub fn direct_message_entry(partner: BareJid, msg: &Message, timestamp: i64) -> InboxEntry {
    let mut entry = InboxEntry::new(
        partner,
        ConversationKind::Direct,
        projection_stanza_id(msg),
        timestamp,
    );
    if let Some(preview) = preview_text(msg) {
        entry.preview = Some(preview);
    }
    entry
}

pub fn groupchat_entry(room: BareJid, msg: &Message, timestamp: i64) -> InboxEntry {
    let mut entry = InboxEntry::new(
        room,
        ConversationKind::MucRoom,
        projection_stanza_id(msg),
        timestamp,
    );
    if let Some(preview) = preview_text(msg) {
        entry.preview = Some(preview);
    }
    entry
}

/// Build a thread-level inbox entry from a groupchat message that carries a
/// `<thread/>` element.
pub fn groupchat_thread_entry(
    room: BareJid,
    msg: &Message,
    timestamp: i64,
    thread_id: &str,
    thread_title: Option<&str>,
    author: Option<&str>,
) -> InboxEntry {
    let mut entry = InboxEntry::new(
        room,
        ConversationKind::MucRoom,
        projection_stanza_id(msg),
        timestamp,
    );
    entry.thread_id = Some(thread_id.to_owned());
    if let Some(title) = thread_title {
        entry.thread_title = Some(bounded_label(title).into_owned());
    }
    if let Some(author) = author {
        entry.author = Some(bounded_label(author).into_owned());
    }
    if let Some(preview) = preview_text(msg) {
        entry.preview = Some(preview);
    }
    entry
}

/// Apply XEP-0430 query filters to a list of inbox entries.
///
/// XEP-0430 §"Querying" defines `unread-only` as the only protocol-level
/// filter on the result set (the `messages` knob controls payload shape,
/// not row selection). Entries come back newest-first with a stable
/// tiebreak on partner JID so RSM cursors remain deterministic.
pub fn filter_query(mut entries: Vec<InboxEntry>, query: &InboxQuery) -> Vec<InboxEntry> {
    if query.unread_only {
        entries.retain(|entry| entry.unread > 0);
    }
    entries.sort_by(|left, right| {
        right
            .last_updated
            .cmp(&left.last_updated)
            .then_with(|| left.partner.cmp(&right.partner))
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0446::FileMetadata;
    use crate::xep::xep0447::{set_file_sharing, FileSharing};
    use crate::xep::xep0448::{set_encrypted_file, Cipher, EncryptedFile};

    fn jid(value: &str) -> BareJid {
        value.parse().expect("valid JID")
    }

    #[test]
    fn runtime_projects_file_sharing_messages_without_bodies() {
        let mut msg = Message::new(Some(jid::Jid::from(jid("bob@example.com"))));
        set_file_sharing(
            &mut msg,
            &FileSharing::new(
                FileMetadata::new()
                    .with_name("secret.bin")
                    .with_media_type("application/octet-stream"),
            )
            .with_url("https://files.example.com/secret.enc"),
        );
        set_encrypted_file(
            &mut msg,
            &EncryptedFile::new(Cipher::Aes256GcmNoPadding, "a2V5", "aXY=")
                .with_hash("sha-256", "aGFzaA==")
                .with_source("https://files.example.com/secret.enc"),
        )
        .expect("encrypted payload");

        assert!(should_project_message(&msg));
        assert!(preview_text(&msg).is_none());
    }

    #[test]
    fn bounded_label_keeps_short_text_and_cuts_long_text_on_a_char_boundary() {
        let exact = "å".repeat(MAX_INBOX_LABEL_CHARS);
        assert_eq!(bounded_label("hello"), "hello");
        assert_eq!(bounded_label(&exact), exact.as_str());

        let cut = bounded_label(&"å".repeat(MAX_INBOX_LABEL_CHARS + 1)).into_owned();
        assert_eq!(cut.chars().count(), MAX_INBOX_LABEL_CHARS);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn runtime_bounds_the_preview_of_a_long_message() {
        let mut msg = Message::new(Some(jid::Jid::from(jid("bob@example.com"))));
        msg.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            format!("  {}  ", "å".repeat(9000)),
        );

        let entry = groupchat_thread_entry(
            jid("room@muc.example.com"),
            &msg,
            1,
            "thread-1",
            preview_text(&msg).as_deref(),
            Some(&"n".repeat(9000)),
        );

        for label in [entry.preview, entry.thread_title, entry.author] {
            let label = label.expect("label is projected");
            assert_eq!(label.chars().count(), MAX_INBOX_LABEL_CHARS);
        }
    }

    #[test]
    fn runtime_skips_inbound_tombstones() {
        let mut msg = Message::new(Some(jid::Jid::from(jid("bob@example.com"))));
        msg.payloads.push(
            minidom::Element::builder("retracted", crate::xep::xep0424::NS_MESSAGE_RETRACT)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "retract-1")
                .attr(
                    minidom::rxml::xml_ncname!("stamp").to_owned(),
                    "2024-06-01T09:00:00Z",
                )
                .build(),
        );

        assert!(!should_project_message(&msg));
    }

    #[test]
    fn runtime_filters_queries_and_preserves_order() {
        let entries = vec![
            InboxEntry::new(jid("a@example.com"), ConversationKind::Direct, "s1", 10)
                .with_unread(0),
            InboxEntry::new(jid("b@example.com"), ConversationKind::Direct, "s2", 20)
                .with_unread(2),
            InboxEntry::new(jid("c@example.com"), ConversationKind::Direct, "s3", 30)
                .with_unread(1),
        ];

        let unread_only = filter_query(
            entries.clone(),
            &InboxQuery {
                unread_only: true,
                ..Default::default()
            },
        );
        assert_eq!(unread_only.len(), 2);
        assert_eq!(unread_only[0].partner, jid("c@example.com"));

        let all = filter_query(
            entries,
            &InboxQuery {
                unread_only: false,
                ..Default::default()
            },
        );
        assert_eq!(all.len(), 3);
        // newest-first
        assert_eq!(all[0].partner, jid("c@example.com"));
        assert_eq!(all[1].partner, jid("b@example.com"));
        assert_eq!(all[2].partner, jid("a@example.com"));
    }
}
