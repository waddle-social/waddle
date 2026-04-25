//! Shared runtime helpers for projecting live message traffic into Waddle inbox rows.

use jid::BareJid;
use xmpp_parsers::message::Message;

use super::{ConversationKind, InboxEntry};
use crate::mam::STANZA_ID_NS;
use crate::xep::xep0430::InboxQuery;
use crate::xep::{
    has_file_sharing, is_moderation_request_message, is_moderation_result_message,
    is_reaction_message, is_retraction_message, is_sticker_message, should_skip_storage,
};

pub fn should_project_message(msg: &Message) -> bool {
    if should_skip_storage(msg) {
        return false;
    }

    if !msg.bodies.is_empty() || !msg.subjects.is_empty() {
        return true;
    }

    is_reaction_message(msg)
        || is_retraction_message(msg)
        || is_moderation_request_message(msg)
        || is_moderation_result_message(msg)
        || has_file_sharing(msg)
        || is_sticker_message(msg)
}

pub fn preview_text(msg: &Message) -> Option<String> {
    msg.bodies
        .get("")
        .or_else(|| msg.bodies.values().next())
        .map(|body| body.0.clone())
        .or_else(|| {
            msg.subjects
                .get("")
                .or_else(|| msg.subjects.values().next())
                .map(|subject| subject.0.clone())
        })
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

pub fn projection_stanza_id(msg: &Message) -> String {
    msg.id
        .clone()
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
        entry.thread_title = Some(title.to_owned());
    }
    if let Some(author) = author {
        entry.author = Some(author.to_owned());
    }
    if let Some(preview) = preview_text(msg) {
        entry.preview = Some(preview);
    }
    entry
}

pub fn filter_query(mut entries: Vec<InboxEntry>, query: &InboxQuery) -> Vec<InboxEntry> {
    if let Some(since) = query.since {
        entries.retain(|entry| entry.last_updated >= since);
    }
    if query.only_unread {
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
                since: None,
                only_unread: true,
                ..Default::default()
            },
        );
        assert_eq!(unread_only.len(), 2);
        assert_eq!(unread_only[0].partner, jid("c@example.com"));

        let since = filter_query(
            entries,
            &InboxQuery {
                since: Some(20),
                only_unread: false,
                ..Default::default()
            },
        );
        assert_eq!(since.len(), 2);
        assert_eq!(since[0].partner, jid("c@example.com"));
        assert_eq!(since[1].partner, jid("b@example.com"));
    }
}
