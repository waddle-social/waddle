package social.waddle.android.client.store

import social.waddle.android.client.bareJid
import social.waddle.android.client.stripReplyFallback
import social.waddle.client.ffi.WaddleArchivedMessage
import social.waddle.client.ffi.WaddleMessage

/**
 * A message that mutates an existing timeline row instead of inserting a
 * new one: XEP-0444 reactions, XEP-0308 corrections, XEP-0424
 * retractions, and XEP-0425 moderation. Extraction precedence follows
 * destructiveness — moderation and retraction are terminal, a correction
 * replaces content, a reaction only annotates.
 */
sealed interface MessageMutation {
    /** The id of the row this mutation targets (any XEP-0359 identity). */
    val targetId: String

    /** Full `from` of the mutating stanza; `null` senders are dropped. */
    val from: String

    /**
     * XEP-0444: [emojis] is the sender's complete current reaction set
     * for the target — it REPLACES that sender's previous set (an empty
     * set clears it). [senderKey] identifies the reacting sender: the
     * full occupant JID in a MUC, the bare JID in 1:1.
     */
    data class Reaction(
        override val targetId: String,
        override val from: String,
        val senderKey: String,
        val mine: Boolean,
        val emojis: List<String>,
    ) : MessageMutation

    /** XEP-0308: replace the target's body; only the original sender may. */
    data class Correction(
        override val targetId: String,
        override val from: String,
        val newBody: String,
    ) : MessageMutation

    /** XEP-0424: the original sender retracts their own message. */
    data class Retraction(
        override val targetId: String,
        override val from: String,
    ) : MessageMutation

    /** XEP-0425: a moderator removes any message (no sender match). */
    data class Moderation(
        override val targetId: String,
        override val from: String,
        val moderatedBy: String?,
        val reason: String?,
    ) : MessageMutation
}

/**
 * True when the message's sole timeline effect is mutating another row —
 * such a message must never notify, bump unread, or reorder DM recency
 * as if it were new content.
 */
fun WaddleMessage.isTimelineMutation(): Boolean =
    moderationTargetId != null || retractsId != null || replacesId != null ||
        reactionTargetId != null

/** Archived twin of [WaddleMessage.isTimelineMutation]. */
fun WaddleArchivedMessage.isTimelineMutation(): Boolean =
    moderationTargetId != null || retractsId != null || replacesId != null ||
        reactionTargetId != null

internal fun mutationOf(message: WaddleMessage, isGroupchat: Boolean, mine: Boolean): MessageMutation? =
    mutationOf(
        from = message.from,
        isGroupchat = isGroupchat,
        mine = mine,
        moderationTargetId = message.moderationTargetId,
        moderatedBy = message.moderatedBy,
        moderationReason = message.moderationReason,
        retractsId = message.retractsId,
        replacesId = message.replacesId,
        // A correction of a reply re-sends the quoted fallback prefix;
        // strip it like the insert path does or edits render the quote
        // twice.
        body = message.body?.let {
            stripReplyFallback(it, message.replyFallbackStart, message.replyFallbackEnd)
        },
        reactionTargetId = message.reactionTargetId,
        reactionEmojis = message.reactionEmojis,
    )

internal fun mutationOf(
    message: WaddleArchivedMessage,
    isGroupchat: Boolean,
    mine: Boolean,
): MessageMutation? =
    mutationOf(
        from = message.from,
        isGroupchat = isGroupchat,
        mine = mine,
        moderationTargetId = message.moderationTargetId,
        moderatedBy = message.moderatedBy,
        moderationReason = message.moderationReason,
        retractsId = message.retractsId,
        replacesId = message.replacesId,
        body = message.body?.let {
            stripReplyFallback(it, message.replyFallbackStart, message.replyFallbackEnd)
        },
        reactionTargetId = message.reactionTargetId,
        reactionEmojis = message.reactionEmojis,
    )

private fun mutationOf(
    from: String?,
    isGroupchat: Boolean,
    mine: Boolean,
    moderationTargetId: String?,
    moderatedBy: String?,
    moderationReason: String?,
    retractsId: String?,
    replacesId: String?,
    body: String?,
    reactionTargetId: String?,
    reactionEmojis: List<String>,
): MessageMutation? {
    from ?: return null
    return when {
        // XEP-0425 is a MUC feature: only a room service moderates.
        // A DM peer's stanza claiming moderation is ignored outright
        // (web parity: moderation is channel-only).
        moderationTargetId != null && isGroupchat -> MessageMutation.Moderation(
            targetId = moderationTargetId,
            from = from,
            moderatedBy = moderatedBy,
            reason = moderationReason,
        )
        retractsId != null -> MessageMutation.Retraction(targetId = retractsId, from = from)
        replacesId != null && body != null -> MessageMutation.Correction(
            targetId = replacesId,
            from = from,
            newBody = body,
        )
        reactionTargetId != null -> MessageMutation.Reaction(
            targetId = reactionTargetId,
            from = from,
            senderKey = if (isGroupchat) from else bareJid(from),
            mine = mine,
            emojis = reactionEmojis,
        )
        else -> null
    }
}

/**
 * XEP-0424/0308 sender authorization: in a MUC the full occupant JID
 * must match (a different occupant may not rewrite history); in 1:1 any
 * resource of the same account may.
 */
internal fun sameSender(mutationFrom: String, originalFrom: String?, isGroupchat: Boolean): Boolean {
    originalFrom ?: return false
    return if (isGroupchat) {
        mutationFrom == originalFrom
    } else {
        bareJid(mutationFrom) == bareJid(originalFrom)
    }
}
