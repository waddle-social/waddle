use waddle_xmpp_core::mam::ArchivedMessage;

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
