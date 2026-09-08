use waddle_xmpp_core::mam::{ArchivedMessage, ArchivedRichPayload};
use xmpp_parsers::message::MessageType;

pub(super) fn apply_tombstone(
    message: &mut ArchivedMessage,
    tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
) {
    use waddle_xmpp_core::mam::{ArchivedRichMessage, ArchivedRichPayload};
    // XEP-0424 §Tombstones / XEP-0425 §Tombstones: replace the original
    // contents — `<body/>` AND any related elements which might leak
    // information about the original message — with a `<retracted/>`
    // tombstone. The XEP-0201 thread reference (id + optional parent)
    // and the XEP-0461 reply reference (id + optional sender JID) both
    // fall under that rule — they identify the conversation tree and
    // the message being replied to, leaking the same metadata — and
    // are scrubbed via `message.thread = None` and `message.reply =
    // None` alongside `stanza_xml`/`body`.
    // Tombstones drop the body entirely (XEP-0424 §Tombstones) — None
    // is the correct "no body element" wire form for the replayed
    // tombstone stanza.
    message.body = None;
    message.stanza_xml = None;
    message.thread = None;
    message.reply = None;
    message.rich = Some(ArchivedRichMessage {
        payload: Some(ArchivedRichPayload::Tombstone(tombstone)),
        reply: None,
        references: Vec::new(),
        mentions: Vec::new(),
        subjects: Default::default(),
        // XEP-0424 §Tombstones: the occupant-id and real-JID item
        // identify the original sender and MUST NOT survive the
        // tombstone replacement.
        occupant_id: None,
        muc_sender: None,
    });
}

pub(super) fn origin_id_tombstone_match(
    existing: &ArchivedMessage,
    incoming: &ArchivedMessage,
) -> bool {
    !groupchat_subject_is_state_update(incoming)
        && matches!(
            (&existing.message_type, &incoming.message_type),
            (MessageType::Groupchat, MessageType::Groupchat)
        )
        && existing.origin_id.is_some()
        && existing.origin_id == incoming.origin_id
        && existing.from == incoming.from
        && tombstone_sender_scope_matches(existing, incoming)
}

/// The existing row must be a tombstone, and — when it retained the
/// original sender's internal `sender_scope` — the incoming retry must
/// come from that same real bare JID. Rows tombstoned before the scope
/// was retained fall back to the occupant-JID-only match (fail closed
/// toward swallowing, the pre-existing carve-out). Content matching is
/// impossible either way: XEP-0424 wipes it by design.
fn tombstone_sender_scope_matches(existing: &ArchivedMessage, incoming: &ArchivedMessage) -> bool {
    let Some(rich) = existing.rich.as_ref() else {
        return false;
    };
    let Some(ArchivedRichPayload::Tombstone(tombstone)) = rich.payload.as_ref() else {
        return false;
    };
    let Some(retained_scope) = tombstone.sender_scope.as_ref() else {
        return true;
    };
    incoming
        .rich
        .as_ref()
        .and_then(|rich| rich.muc_sender.as_ref())
        .is_some_and(|sender| sender.jid.to_bare() == *retained_scope)
}

/// XEP-0045 §8.1 subject-only updates modify room state and remain replayable
/// even when an earlier timeline message with the same origin-id was retracted.
/// A subject accompanied by body content or a thread is a timeline message
/// and still obeys the tombstone guard.
pub(super) fn groupchat_subject_is_state_update(message: &ArchivedMessage) -> bool {
    crate::muc::is_groupchat_subject_change(
        &message.message_type,
        message
            .rich
            .as_ref()
            .is_some_and(|rich| !rich.subjects.is_empty()),
        message
            .body
            .as_deref()
            .is_some_and(|body| !body.trim().is_empty()),
        message.thread.is_some(),
    )
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

    use super::origin_id_tombstone_match;

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
                sender_scope: None,
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

        let mut subject_retry = incoming;
        subject_retry.body = None;
        subject_retry
            .rich
            .as_mut()
            .expect("groupchat rich payload")
            .subjects
            .insert(String::new(), "Topic".to_string());
        assert!(!origin_id_tombstone_match(&tombstone, &subject_retry));
        subject_retry.thread = Some(waddle_xmpp_core::xep0201::ThreadInfo::root(
            waddle_xmpp_core::mam::ThreadId::new("timeline").expect("thread"),
        ));
        assert!(origin_id_tombstone_match(&tombstone, &subject_retry));
    }

    #[test]
    fn tombstone_with_retained_sender_scope_only_matches_the_same_real_user() {
        // The internal `sender_scope` retained on newer tombstones
        // restores the identity guarantee XEP-0424's wire wipe removed:
        // a different real user reusing a departed occupant's nick and
        // a tombstoned origin-id must archive distinctly, not be
        // swallowed.
        let mut tombstone =
            groupchat_message("room@conference.example.com/alice", None, "origin-1", "", 7);
        tombstone.body = None;
        tombstone.rich = Some(ArchivedRichMessage {
            payload: Some(ArchivedRichPayload::Tombstone(ArchivedTombstone {
                retraction_id: None,
                stamp: Utc::now(),
                moderation: None,
                sender_scope: Some("alice@example.com".parse().expect("valid test bare JID")),
            })),
            ..ArchivedRichMessage::default()
        });

        let same_user_retry = groupchat_message(
            "room@conference.example.com/alice",
            Some("alice@example.com/new-session"),
            "origin-1",
            "hello",
            8,
        );
        assert!(origin_id_tombstone_match(&tombstone, &same_user_retry));

        let different_user = groupchat_message(
            "room@conference.example.com/alice",
            Some("mallory@example.com/session"),
            "origin-1",
            "hello",
            8,
        );
        assert!(!origin_id_tombstone_match(&tombstone, &different_user));

        let missing_identity = groupchat_message(
            "room@conference.example.com/alice",
            None,
            "origin-1",
            "hello",
            8,
        );
        assert!(!origin_id_tombstone_match(&tombstone, &missing_identity));
    }
}
