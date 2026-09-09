//! Build candidates from canonical messages and frozen recovery identity, without policy reads.
use super::{
    groupchat_class::*, NotificationCandidate, NotificationOutboxError, NotificationThreadId,
};
use jid::{BareJid, Jid};
use waddle_xmpp::xep::xep0421::OccupantIdSecret;
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::Message;

pub struct GroupchatCandidateIdentity<'a> {
    pub owner: &'a BareJid,
    pub room: &'a BareJid,
    pub sender: &'a Jid,
    pub thread_id: NotificationThreadId,
    pub archive_stanza_id: &'a StanzaId,
    pub is_live_occupant: bool,
    pub sender_can_broadcast_channel_mention: bool,
}

pub fn candidate_from_envelope(
    message: &Message,
    identity: GroupchatCandidateIdentity<'_>,
    secret: &OccupantIdSecret,
) -> Result<NotificationCandidate, NotificationOutboxError> {
    let GroupchatCandidateIdentity {
        owner,
        room,
        sender,
        thread_id,
        archive_stanza_id,
        is_live_occupant,
        sender_can_broadcast_channel_mention,
    } = identity;
    let sender_jid = sender.clone();
    // Parse explicit mentions ONCE per message and derive every
    // XEP-0513 signal (personal-mention bit, channel-mention scope,
    // owner-`<noping/>`) from the same parsed structure. The previous
    // shape ran `extract_explicit_mentions` three times per recipient
    // (class derivation + channel scope + noping), so a 100-member
    // groupchat fan-out paid 300× the parser cost when 1× suffices.
    let owner_occupant_id = waddle_xmpp::xep::generate_occupant_id(owner, room, secret);
    let explicit_mentions = waddle_xmpp::xep::extract_explicit_mentions(message);
    let mentions_slice: &[waddle_xmpp::xep::ExplicitMention] = explicit_mentions
        .as_ref()
        .map_or(&[], |mentions| mentions.mentions.as_slice());
    // Parse XEP-0372 references ONCE per recipient so the §304
    // count gate AND the personal-mention fallback consult the same
    // pre-parsed slice. Previously each helper re-walked
    // `message.payloads` independently (2× per recipient × N
    // recipients = 2N walks per message) — perf review on PR #741.
    let references_vec = waddle_xmpp::xep::extract_references_from_message(message);
    let references_slice: &[waddle_xmpp::xep::Reference] = references_vec.as_slice();
    let GroupchatNotificationClassOutcome {
        decision: GroupchatNotificationClassDecision::Deliver(class),
        // The overflow bit is consumed by the classifier — the
        // class downgrade reflects it. We deliberately do NOT use
        // it to gate `<noping/>` (see below).
        mentions_overflowed: _,
    } = groupchat_notification_class(
        mentions_slice,
        references_slice,
        owner,
        room,
        owner_occupant_id.as_str(),
        is_live_occupant,
        sender_can_broadcast_channel_mention,
    );
    // XEP-0513 §"No Ping": "if the sender includes a `<noping/>`
    // child element in a mention, the receiving entity SHOULD NOT
    // generate a notification (ping) for that mention." That SHOULD
    // is INDEPENDENT of §304's "ignore all mentions" cap — the
    // existing slice-2a T1 suppressor (`SuppressedReason::Xep0513Noping`)
    // honors `<noping/>` unconditionally for every class. A prior
    // shape of this code canceled the noping bit on count-overflow
    // (to prevent a spammer silencing push via `<noping/>` + mention
    // spam), but that contradicts both the §"No Ping" SHOULD and
    // the existing T1 behavior — push candidate creation MUST be
    // suppressed for `<noping/>` recipients even when the message
    // overflows the §304 count cap, while normal delivery + MAM +
    // inbox projection are unaffected (compliance review on
    // PR #741). The class downgrade caused by overflow remains; only
    // the per-recipient `<noping/>` suppression survives the cap.
    let recipient_noping =
        groupchat_mentions_carry_owner_noping(mentions_slice, owner, owner_occupant_id.as_str());
    let hints = crate::notification_outbox::NotificationMessageHints::none()
        .with_noping(recipient_noping)
        .with_xep0334(
            waddle_xmpp::xep::xep0334::has_hint(message, waddle_xmpp::xep::xep0334::Hint::NoStore),
            waddle_xmpp::xep::xep0334::has_hint(
                message,
                waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
            ),
        )
        .with_reaction(waddle_xmpp::xep::xep0444::is_reaction_only_message(message));
    let candidate = crate::notification_outbox::NotificationCandidate::groupchat_with_hints(
        owner.clone(),
        room.clone(),
        sender_jid,
        thread_id,
        archive_stanza_id.clone(),
        class,
        hints,
    )?;
    Ok(candidate.with_last_message_body(
        message
            .bodies
            .get("")
            .or_else(|| message.bodies.values().next())
            .cloned(),
    ))
}
