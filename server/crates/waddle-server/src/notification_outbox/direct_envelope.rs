//! Pure direct-message candidate reconstruction from the canonical envelope.
use super::{NotificationCandidate, NotificationMessageHints, NotificationOutboxError};
use jid::{BareJid, Jid};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::Message;

/// Rebuild message-intrinsic fields without consulting recipient policy.
pub(crate) fn direct_candidate_from_envelope(
    message: &Message,
    recipient: &BareJid,
    sender_jid: &Jid,
    archive_stanza_id: &StanzaId,
) -> Result<NotificationCandidate, NotificationOutboxError> {
    // XEP-0203 `<delay/>` filter NOT applied here (Copilot review on
    // PR #738): see the matching comment in `groupchat_inbox.rs`'s
    // `enqueue_groupchat_notification_candidate`. An earlier shape
    // suppressed pushes for messages carrying any `<delay/>`, but
    // that check is trivially spoofable — a sender can inject
    // `<delay/>` into their own stanza to suppress the recipient's
    // push. Until inbound `<delay/>` stripping lands at the C2S
    // session boundary, the safer behavior is to push as live and
    // accept the (currently theoretical) case where an S2S-buffered
    // delivery generates a real-time push (Waddle has no S2S yet).
    // XEP-0513 mention bit is message-intrinsic and frozen at T0; T1
    // reads it back from the candidate row when running the XEP-0492
    // dispatch gate.
    //
    // Self-directed candidates (sender bare JID == recipient bare JID)
    // are rejected at the `NotificationCandidate::direct_message`
    // constructor as `SelfDirectedNotificationCandidate` — no row is
    // persisted, satisfying the compliance requirement that
    // self-notifications produce no candidate/outbox entry. This is
    // input validation, not recipient-state suppression, so it lives
    // at the typed constructor boundary alongside the existing
    // full-sender-JID and archive-id owner checks.
    // Parse explicit mentions ONCE per message and derive both the
    // mention bit and the `<noping/>` bit from the same parsed
    // structure. The previous shape re-ran `extract_explicit_mentions`
    // twice per recipient (one for `is_mention`, one for noping); for
    // DM the recipient count is 1, but the same pattern is fanned out
    // N× in groupchat so the unified helper keeps both surfaces
    // consistent and avoids redundant XML traversals on the hot path.
    let RecipientMentionBits { is_mention, noping } =
        mention_bits_for_recipient(message, recipient);
    let hints = NotificationMessageHints::none()
        .with_noping(noping)
        .with_xep0334(
            waddle_xmpp::xep::xep0334::has_hint(message, waddle_xmpp::xep::xep0334::Hint::NoStore),
            waddle_xmpp::xep::xep0334::has_hint(
                message,
                waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
            ),
        )
        .with_reaction(waddle_xmpp::xep::xep0444::is_reaction_only_message(message));
    let candidate = NotificationCandidate::direct_message_with_hints(
        recipient.clone(),
        sender_jid.clone(),
        archive_stanza_id.clone(),
        is_mention,
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

/// Typed pair of message-frozen XEP-0513 mention signals for a single
/// recipient, derived from one `extract_explicit_mentions` parse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RecipientMentionBits {
    /// `true` when any `<mention jid='…'/>` names `recipient`,
    /// regardless of `<noping/>`. Matches the pre-dedup
    /// `ExplicitMentions::mentions_jid` predicate exactly — a `<noping/>`
    /// mention still counts as a mention for class derivation; the T1
    /// evaluator handles the `<noping/>` suppression independently via
    /// the [`Self::noping`] bit.
    is_mention: bool,
    /// `true` when any `<mention jid='…'/>` naming `recipient` also
    /// carries `<noping/>`. Message-frozen at T0 onto the candidate row;
    /// the T1 evaluator reads it back and suppresses with
    /// `SuppressedReason::Xep0513Noping`.
    noping: bool,
}

/// Single-pass derivation of `(is_mention, noping)` for a DM
/// recipient.
///
/// The recipient JID is the bare JID that owns the offline queue; that
/// is the canonical identity referenced by `<mention jid='…'/>` per
/// XEP-0513 §3. Channel-wide
/// `<mention mentions='urn:xmpp:mentions:0#channel'/>` is intentionally
/// NOT treated as an individual mention here — the XEP-0492
/// `<on-mention/>` semantics target explicit user mentions; the
/// channel-mention surface is for MUC reflector announcements, which
/// do not flow through the DM `QueueOfflineDelivery` arm.
fn mention_bits_for_recipient(message: &Message, recipient: &BareJid) -> RecipientMentionBits {
    // Pre-parse XEP-0513 mentions AND XEP-0372 references once so
    // the §304 count gate and the per-recipient is_mention/noping
    // derivation share the same pre-parsed slices.
    let mentions_slice: Vec<_> = waddle_xmpp::xep::extract_explicit_mentions(message)
        .map(|m| m.mentions)
        .unwrap_or_default();
    let references = waddle_xmpp::xep::extract_references_from_message(message);

    // Derive the per-recipient mention bits BEFORE applying the
    // §304 count gate. Both is_mention and noping flow from the
    // per-recipient match; the count gate then downgrades is_mention
    // (the "ignore all mentions" SHOULD) but PRESERVES the noping
    // suppression bit (XEP-0513 §"No Ping" is a separate SHOULD
    // that operates independently of the count cap — compliance
    // review on PR #741). Concretely:
    //   - Without `<noping/>`: count gate downgrades is_mention →
    //     class drops to plain `dm` (XEP-0492 `<on-mention/>`
    //     recipients then get suppressed at T1 via
    //     `Xep0492OnMentionMiss`).
    //   - With `<noping/>`: noping bit persists → the existing
    //     T0/T1 `Xep0513Noping` suppressor fires for that recipient
    //     even on overflowed messages. Spam-targeted `<always/>`
    //     recipients are correctly silenced by the sender's
    //     explicit `<noping/>`, matching the spec-mandated behavior
    //     for non-overflowed messages.
    let mut bits = RecipientMentionBits::default();
    for mention in &mentions_slice {
        let names_recipient = mention
            .jid
            .as_ref()
            .is_some_and(|mentioned| mentioned == recipient);
        if !names_recipient {
            continue;
        }
        bits.is_mention = true;
        if mention.noping {
            bits.noping = true;
        }
    }
    // XEP-0372 mention-reference fallback for the `is_mention` bit:
    // a DM sender naming the recipient via
    // `<reference type='mention' uri='xmpp:recipient@host'/>`
    // (instead of, or alongside, an XEP-0513 `<mention jid='…'/>`)
    // MUST still promote the recipient to `dm_mention` — otherwise
    // the count gate counts XEP-0372 references TOWARD the threshold
    // (promoting spam) but the classifier doesn't count them
    // FOR promotion (demoting legitimate XEP-0372 mentions to plain
    // dm). The groupchat path already does this in
    // `groupchat_mentions_owner`; mirror it here for DM consistency
    // (cross-XEP review on PR #741).
    if !bits.is_mention {
        bits.is_mention = references.iter().any(|reference| {
            reference.is_mention()
                && reference
                    .bare_jid()
                    .is_some_and(|mentioned| &mentioned == recipient)
        });
    }
    // XEP-0513 §304 "ignore all mentions" applied AFTER the noping
    // bit is captured: when the per-message count exceeds the
    // threshold, the recipient's mention-class is downgraded (plain
    // `dm`) but the explicit `<noping/>` suppression — being a
    // separate §"No Ping" SHOULD — is preserved.
    if waddle_xmpp::xep::mentions_exceed_threshold_from_parts(
        &mentions_slice,
        &references,
        waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT,
    ) {
        bits.is_mention = false;
    }
    bits
}

#[cfg(test)]
#[path = "direct_envelope_tests.rs"]
mod tests;
