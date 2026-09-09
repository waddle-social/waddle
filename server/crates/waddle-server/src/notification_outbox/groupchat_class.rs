//! Pure message-frozen groupchat notification classification.
use jid::{BareJid, Jid};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupchatNotificationClassDecision {
    Deliver(crate::notification_outbox::NotificationClass),
}

/// Outcome of `groupchat_notification_class`. Carries the typed
/// class decision plus the XEP-0513 §304 "mention count exceeded"
/// provenance bit. The class decision already reflects the overflow
/// (it collapses to `NotifyAll` when the cap is exceeded); the
/// separate `mentions_overflowed` field exposes the provenance so
/// tests can assert it directly and so a future T0 hint that
/// genuinely depends on the "overflowed at classification time"
/// signal can read it without re-running the §304 count helpers.
///
/// The candidate-emission caller deliberately does NOT gate the
/// recipient's `<noping/>` derivation on this bit — XEP-0513
/// §"No Ping" is independent of §304's "ignore all mentions" cap.
/// See the comment near `recipient_noping` in
/// `enqueue_groupchat_notification_candidate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GroupchatNotificationClassOutcome {
    pub(crate) decision: GroupchatNotificationClassDecision,
    /// `true` when the per-message mention count exceeded the
    /// XEP-0513 §304 `mentions#count` threshold and the classifier
    /// collapsed every mention TARGET to `NotifyAll`. Production
    /// emission discards this bit (see struct doc above); the
    /// field is consumed in the count-gate tests
    /// (`xep0513_mention_count_*`) and is available to any future
    /// T0 hint that needs the overflow provenance.
    pub(crate) mentions_overflowed: bool,
}

/// Classify a groupchat candidate from a pre-parsed mention slice.
///
/// Callers MUST pass the result of a single
/// `extract_explicit_mentions(message)` parse AND a single
/// `extract_references_from_message(message)` parse so that neither
/// XEP-0513 mentions nor XEP-0372 references are re-walked per
/// derivation. The previous shape took `&Message` and re-walked the
/// XEP-0372 payloads twice per recipient (once in the §304 count
/// gate and once in `groupchat_mentions_owner`) — for an
/// N-occupant room that was 2N payload sweeps per message when 1
/// suffices (perf review on PR #741).
pub(crate) fn groupchat_notification_class(
    mentions: &[waddle_xmpp::xep::ExplicitMention],
    references: &[waddle_xmpp::xep::Reference],
    owner: &BareJid,
    room: &BareJid,
    owner_occupant_id: &str,
    // `is_live_occupant` here is **message-time-frozen presence** — the
    // XMPP room handler computes it at room-dispatch time
    // (`live_recipient_bares.contains(bare)` in
    // `waddle_xmpp::protocol::room::inbox`) and propagates it on the
    // `ProjectGroupchatInbox` event. That makes it a T0 message-frozen
    // input on the same axis as XEP-0513 `<active/>` (sender intent)
    // and XEP-0421 occupant-id (sender provenance), NOT a T1 recipient-
    // state read. Per #506 Q2 the candidate row snapshots message-
    // intrinsic facts and the T1 evaluator reads fresh recipient
    // state; encoding the message-time live-occupant bit into the
    // [`NotificationClass`] taxonomy (`ActiveChannelMention` vs
    // `ChannelMention`) is the snapshot mechanism here. Slice 2 will
    // add a richer `notification_activity` projection so the T1
    // evaluator can additionally consult *current* recipient activity
    // (XEP-0513 §"active mention" §"the receiving server may filter")
    // — that augments this T0 snapshot, it does not relocate it.
    is_live_occupant: bool,
    // XEP-0513 §"Multi-User Chats Permissions" §304: receiving entities
    // SHOULD ignore a channel mention if the sender does not have at
    // least the minimum role required by the room. This is the typed
    // frozen permission snapshot taken at dispatch time in
    // `waddle_xmpp::protocol::room::inbox::sender_may_broadcast_channel_mention`
    // — server default policy is `mentions#channel = moderators`
    // (XEP-0513 example value).
    sender_can_broadcast_channel_mention: bool,
) -> GroupchatNotificationClassOutcome {
    // XEP-0513 §304: "Receiving entities SHOULD ignore all mentions if
    // the message contains more mentions than the threshold specified
    // by `mentions#count`." When the per-message count exceeds the
    // server-internal default, fall through to `NotifyAll` — neither
    // personal-mention nor channel-mention classification applies.
    // The wire payload is preserved (delivery + MAM unchanged per
    // XEP-0513 §526); only the push class is affected. Per-room
    // override of the threshold is deferred to slice 3c.
    //
    // The overflow bit is also propagated to the candidate-emission
    // caller via `GroupchatNotificationClassOutcome` so the noping
    // derivation can reuse it without re-walking the XEP-0372
    // references a second time per recipient (adversarial review on
    // PR #741).
    let mentions_overflowed = waddle_xmpp::xep::mentions_exceed_threshold_from_parts(
        mentions,
        references,
        waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT,
    );
    if mentions_overflowed {
        return GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(
                crate::notification_outbox::NotificationClass::NotifyAll,
            ),
            mentions_overflowed,
        };
    }
    let personal_mention = groupchat_mentions_owner(mentions, references, owner, owner_occupant_id);
    let channel_mention = groupchat_channel_mention_scope(mentions, room)
        .filter(|_| sender_can_broadcast_channel_mention);
    GroupchatNotificationClassOutcome {
        decision: groupchat_notification_class_from_message(
            personal_mention,
            channel_mention,
            is_live_occupant,
        ),
        mentions_overflowed,
    }
}

/// Message-derived classification of a groupchat notification candidate.
///
/// After the T0 → T1 push-decision move (#526 slice 1) the class is a
/// pure function of the message payloads + scope: there is no
/// XEP-0492 recipient-state read here. The T1 evaluator at outbox
/// dispatch time consults the projection store and decides
/// publish-or-suppress based on the recorded class + recipient's
/// effective notification level.
///
/// `channel_mention` carries `None` either when no channel mention is
/// present OR when the sender lacks the XEP-0513 §"Multi-User Chats
/// Permissions" minimum role — see [`groupchat_notification_class`]
/// where the role-filter is applied before this function is called.
pub(crate) fn groupchat_notification_class_from_message(
    personal_mention: bool,
    channel_mention: Option<GroupchatChannelMentionScope>,
    is_live_occupant: bool,
) -> GroupchatNotificationClassDecision {
    if personal_mention {
        return GroupchatNotificationClassDecision::Deliver(
            crate::notification_outbox::NotificationClass::PersonalMention,
        );
    }
    match channel_mention {
        Some(GroupchatChannelMentionScope::Active) if is_live_occupant => {
            return GroupchatNotificationClassDecision::Deliver(
                crate::notification_outbox::NotificationClass::ActiveChannelMention,
            );
        }
        Some(GroupchatChannelMentionScope::All) => {
            return GroupchatNotificationClassDecision::Deliver(
                crate::notification_outbox::NotificationClass::ChannelMention,
            );
        }
        _ => {}
    }
    GroupchatNotificationClassDecision::Deliver(
        crate::notification_outbox::NotificationClass::NotifyAll,
    )
}

/// Returns `true` when any XEP-0513 explicit mention naming `owner`
/// (by JID or occupant-id) also carries `<noping/>`. Snapshotted onto
/// the candidate row at T0 so the T1 evaluator can suppress with
/// `SuppressedReason::Xep0513Noping`. Operates on a pre-parsed slice
/// so the XEP-0513 traversal happens once per message.
pub(crate) fn groupchat_mentions_carry_owner_noping(
    mentions: &[waddle_xmpp::xep::ExplicitMention],
    owner: &BareJid,
    owner_occupant_id: &str,
) -> bool {
    mentions.iter().any(|mention| {
        // Mirror the mixed-attribute guard in `groupchat_mentions_owner`:
        // ANY `<mention/>` carrying `mentions='…'` is group-scope (the
        // presence of `mentions=` declares group intent), not a
        // personal mention naming `owner`. The `<noping/>` on a
        // group-scope `#channel` mention causes
        // `current_room_channel_mention` to return `false` (its first
        // guard short-circuits on `mention.noping`), which collapses
        // the channel scope to `None` and the message classifies as
        // `NotifyAll` — i.e. the channel push is suppressed via the
        // scope path, NOT projected onto the owner's personal
        // notification. For unsupported groups the same fall-through
        // applies. Generalised from the slice 3a precedent
        // (`!is_channel()` was too narrow — unsupported groups like
        // `#space` slipped through; review on PR #756).
        mention.noping
            && mention.mentions.is_none()
            && (mention
                .jid
                .as_ref()
                .is_some_and(|mentioned| mentioned == owner)
                || mention
                    .occupant_id
                    .as_deref()
                    .is_some_and(|mentioned| mentioned == owner_occupant_id))
    })
}

pub(crate) fn groupchat_mentions_owner(
    mentions: &[waddle_xmpp::xep::ExplicitMention],
    references: &[waddle_xmpp::xep::Reference],
    owner: &BareJid,
    owner_occupant_id: &str,
) -> bool {
    let xep0513 = mentions.iter().any(|mention| {
        // XEP-0513 §"Multi-User Chats Permissions" hardening
        // (adversarial review on PR #738 + extension on PR #756):
        // ANY `<mention/>` carrying `mentions='…'` is group-scope —
        // the presence of `mentions=` declares group intent — and
        // MUST NOT be classified as a personal mention even when it
        // also carries `jid=`/`occupantid=` attributes. The original
        // slice 3a guard `!is_channel()` plugged the `#channel`
        // permission-bypass attack but missed unsupported groups
        // (`#space`, `#server`, etc.): a `<mention occupantid='X'
        // mentions='#space'/>` would slip through as PersonalMention
        // and piggyback on the personal pipeline. Tightening to
        // `mention.mentions.is_none()` covers EVERY group URI by
        // the wire-shape attribute, not by a hardcoded URI value.
        !mention.noping
            && mention.mentions.is_none()
            && (mention
                .jid
                .as_ref()
                .is_some_and(|mentioned| mentioned == owner)
                || mention
                    .occupant_id
                    .as_deref()
                    .is_some_and(|mentioned| mentioned == owner_occupant_id))
    });
    let xep0372 = references.iter().any(|reference| {
        reference.is_mention()
            && reference
                .bare_jid()
                .is_some_and(|mentioned| &mentioned == owner)
    });
    xep0513 || xep0372
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupchatChannelMentionScope {
    All,
    Active,
}

pub(crate) fn groupchat_channel_mention_scope(
    mentions: &[waddle_xmpp::xep::ExplicitMention],
    room: &BareJid,
) -> Option<GroupchatChannelMentionScope> {
    if mentions
        .iter()
        .any(|mention| current_room_channel_mention(mention, room) && !mention.active)
    {
        return Some(GroupchatChannelMentionScope::All);
    }
    mentions
        .iter()
        .any(|mention| current_room_channel_mention(mention, room) && mention.active)
        .then_some(GroupchatChannelMentionScope::Active)
}

pub(crate) fn current_room_channel_mention(
    mention: &waddle_xmpp::xep::ExplicitMention,
    room: &BareJid,
) -> bool {
    if !mention.is_channel() || mention.noping {
        return false;
    }
    mention
        .uri
        .as_deref()
        .is_none_or(|uri| xmpp_uri_bare_jid(uri).is_some_and(|target| target == room.clone()))
}

pub(crate) fn xmpp_uri_bare_jid(uri: &str) -> Option<BareJid> {
    // RFC 5122 / RFC 3986: query is introduced by `?`, fragment by
    // `#`. `;` separates key/value pairs WITHIN the query — it does
    // not delimit the start of the query. Stripping on `?` and `#`
    // is sufficient for extracting the JID prefix (Copilot review
    // on PR #738).
    let jid_part = uri.strip_prefix("xmpp:")?.split(['?', '#']).next()?.trim();
    if jid_part.is_empty() {
        return None;
    }
    jid_part.parse::<Jid>().ok().map(|jid| jid.to_bare())
}
