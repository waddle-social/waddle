use waddle_xmpp_core::mam::{ArchivedMessage, ArchivedRichPayload};
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

pub(super) fn origin_id_tombstone_match(
    existing: &ArchivedMessage,
    incoming: &ArchivedMessage,
) -> bool {
    matches!(
        (&existing.message_type, &incoming.message_type),
        (MessageType::Groupchat, MessageType::Groupchat)
    ) && existing.origin_id.is_some()
        && existing.origin_id == incoming.origin_id
        && existing.from == incoming.from
        && matches!(
            existing
                .rich
                .as_ref()
                .and_then(|rich| rich.payload.as_ref()),
            Some(ArchivedRichPayload::Tombstone(_))
        )
}

fn sender_scope_matches(existing: &ArchivedMessage, incoming: &ArchivedMessage) -> bool {
    match (&existing.message_type, &incoming.message_type) {
        (MessageType::Groupchat, MessageType::Groupchat) => {
            let existing_sender = existing
                .rich
                .as_ref()
                .and_then(|rich| rich.muc_sender.as_ref());
            let incoming_sender = incoming
                .rich
                .as_ref()
                .and_then(|rich| rich.muc_sender.as_ref());
            existing.from == incoming.from
                && existing_sender.zip(incoming_sender).is_some_and(
                    |(existing_sender, incoming_sender)| {
                        existing_sender.jid.to_bare() == incoming_sender.jid.to_bare()
                    },
                )
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
    // `dedup_content` normalizes an identity-only projection to `None`
    // so a row whose only rich content was the server-stamped
    // occupant-id / real-JID compares equal to a `rich: None` row.
    let existing = existing.rich.as_ref().and_then(|rich| rich.dedup_content());
    let incoming = incoming.rich.as_ref().and_then(|rich| rich.dedup_content());
    existing == incoming
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use jid::Jid;
    use waddle_xmpp_core::mam::{
        ArchivedMessage, ArchivedMucSender, ArchivedRichMessage, ArchivedRichPayload,
        ArchivedTombstone,
    };
    use waddle_xmpp_core::types::{Affiliation, Role};
    use waddle_xmpp_core::xep0359::OriginId;
    use xmpp_parsers::message::MessageType;

    use super::{origin_id_dedup_match, origin_id_tombstone_match};

    fn jid(value: &str) -> Jid {
        value.parse().expect("valid test JID")
    }

    fn groupchat_message(
        from: &str,
        real_jid: Option<&str>,
        origin_id: &str,
        body: &str,
        nickname_generation: u64,
    ) -> ArchivedMessage {
        ArchivedMessage {
            body: Some(body.to_string()),
            origin_id: Some(OriginId::new(origin_id)),
            message_type: MessageType::Groupchat,
            nickname_generation: Some(nickname_generation),
            rich: real_jid.map(|real_jid| ArchivedRichMessage {
                muc_sender: Some(ArchivedMucSender {
                    jid: jid(real_jid),
                    affiliation: Affiliation::Member,
                    role: Role::Participant,
                }),
                ..ArchivedRichMessage::default()
            }),
            ..ArchivedMessage::for_test(jid(from), jid("room@conference.example.com"))
        }
    }

    #[test]
    fn rejoin_retry_matches_same_real_bare_jid_across_generations() {
        let existing = groupchat_message(
            "room@conference.example.com/alice",
            Some("alice@example.com/old-session"),
            "origin-1",
            "hello",
            7,
        );
        let incoming = groupchat_message(
            "room@conference.example.com/alice",
            Some("alice@example.com/new-session"),
            "origin-1",
            "hello",
            8,
        );

        assert!(origin_id_dedup_match(&existing, &incoming));
    }

    #[test]
    fn nick_reuse_by_different_real_bare_jid_does_not_match() {
        let existing = groupchat_message(
            "room@conference.example.com/alice",
            Some("alice@example.com/phone"),
            "origin-1",
            "hello",
            7,
        );
        let incoming = groupchat_message(
            "room@conference.example.com/alice",
            Some("mallory@example.com/phone"),
            "origin-1",
            "hello",
            8,
        );

        assert!(!origin_id_dedup_match(&existing, &incoming));
    }

    #[test]
    fn changed_content_does_not_match() {
        let existing = groupchat_message(
            "room@conference.example.com/alice",
            Some("alice@example.com/old"),
            "origin-1",
            "hello",
            7,
        );
        let incoming = groupchat_message(
            "room@conference.example.com/alice",
            Some("alice@example.com/new"),
            "origin-1",
            "changed",
            8,
        );

        assert!(!origin_id_dedup_match(&existing, &incoming));
    }

    #[test]
    fn missing_muc_sender_on_either_side_does_not_match() {
        let identified = groupchat_message(
            "room@conference.example.com/alice",
            Some("alice@example.com/device"),
            "origin-1",
            "hello",
            7,
        );
        let unidentified = groupchat_message(
            "room@conference.example.com/alice",
            None,
            "origin-1",
            "hello",
            8,
        );

        assert!(!origin_id_dedup_match(&identified, &unidentified));
        assert!(!origin_id_dedup_match(&unidentified, &identified));
    }

    #[test]
    fn different_occupant_jid_does_not_match() {
        let existing = groupchat_message(
            "room@conference.example.com/alice",
            Some("alice@example.com/old"),
            "origin-1",
            "hello",
            7,
        );
        let incoming = groupchat_message(
            "room@conference.example.com/alice-renamed",
            Some("alice@example.com/new"),
            "origin-1",
            "hello",
            8,
        );

        assert!(!origin_id_dedup_match(&existing, &incoming));
    }

    #[test]
    fn tombstone_match_is_groupchat_origin_and_occupant_scoped() {
        let incoming = groupchat_message(
            "room@conference.example.com/alice",
            Some("alice@example.com/new"),
            "origin-1",
            "hello",
            8,
        );
        let mut tombstone =
            groupchat_message("room@conference.example.com/alice", None, "origin-1", "", 7);
        tombstone.body = None;
        tombstone.rich = Some(ArchivedRichMessage {
            payload: Some(ArchivedRichPayload::Tombstone(ArchivedTombstone {
                retraction_id: None,
                stamp: Utc::now(),
                moderation: None,
            })),
            ..ArchivedRichMessage::default()
        });

        assert!(origin_id_tombstone_match(&tombstone, &incoming));

        let different_from = groupchat_message(
            "room@conference.example.com/bob",
            Some("alice@example.com/new"),
            "origin-1",
            "hello",
            8,
        );
        assert!(!origin_id_tombstone_match(&tombstone, &different_from));

        let different_origin = groupchat_message(
            "room@conference.example.com/alice",
            Some("alice@example.com/new"),
            "origin-2",
            "hello",
            8,
        );
        assert!(!origin_id_tombstone_match(&tombstone, &different_origin));

        let live = groupchat_message(
            "room@conference.example.com/alice",
            Some("alice@example.com/old"),
            "origin-1",
            "hello",
            7,
        );
        assert!(!origin_id_tombstone_match(&live, &incoming));
    }
}
