package social.waddle.android.client.store

import social.waddle.android.client.bareJid
import social.waddle.android.client.stripReplyFallback
import social.waddle.client.ffi.WaddleArchivedMessage
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddleSafetyScoresFastening

/**
 * A message that mutates an existing timeline row instead of inserting a
 * new one: XEP-0444 reactions, XEP-0308 corrections, XEP-0424
 * retractions, XEP-0425 moderation, and XEP-0422 safety-score
 * fastenings. Extraction precedence follows destructiveness —
 * moderation and retraction are terminal, a correction replaces
 * content, a reaction or safety-score fastening only annotates.
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
        val sourceRevisionId: String? = null,
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

    /**
     * XEP-0422 `urn:waddle:safety-scores:1` fastening: the room's
     * automated per-category scores for the target. Only the
     * room itself (bare room JID) may apply it. [fasteningIds] are the
     * fastening stanza's own wire identities, so a MAM re-delivery of a
     * fastening already applied is recognised as a replay.
     */
    data class SafetyScores(
        override val targetId: String,
        override val from: String,
        val fastening: WaddleSafetyScoresFastening,
        val fasteningIds: Set<String>,
    ) : MessageMutation
}

/**
 * True when the message's sole timeline effect is mutating another row —
 * such a message must never notify, bump unread, or reorder DM recency
 * as if it were new content.
 */
fun WaddleMessage.isTimelineMutation(): Boolean = TimelineSource.Live(this).isTimelineMutation()

/** Archived twin of [WaddleMessage.isTimelineMutation]. */
fun WaddleArchivedMessage.isTimelineMutation(): Boolean =
    TimelineSource.Archived(this).isTimelineMutation()

private fun TimelineSource.isTimelineMutation(): Boolean =
    moderationTargetId != null || retractsId != null || replacesId != null ||
        reactionTargetId != null || safetyScores != null

internal fun mutationOf(message: WaddleMessage, isGroupchat: Boolean, mine: Boolean): MessageMutation? =
    mutationOf(TimelineSource.Live(message), isGroupchat = isGroupchat, mine = mine)

internal fun mutationOf(
    message: WaddleArchivedMessage,
    isGroupchat: Boolean,
    mine: Boolean,
): MessageMutation? =
    mutationOf(TimelineSource.Archived(message), isGroupchat = isGroupchat, mine = mine)

private fun mutationOf(source: TimelineSource, isGroupchat: Boolean, mine: Boolean): MessageMutation? {
    val from = source.from ?: return null
    val moderationTargetId = source.moderationTargetId
    val retractsId = source.retractsId
    val replacesId = source.replacesId
    val reactionTargetId = source.reactionTargetId
    val safetyScores = source.safetyScores
    // A correction of a reply re-sends the quoted fallback prefix; strip
    // it like the insert path does or edits render the quote twice.
    val body = source.body?.let {
        stripReplyFallback(it, source.replyFallbackStart, source.replyFallbackEnd)
    }
    return when {
        // XEP-0425 is a MUC feature: only a room service moderates.
        // A DM peer's stanza claiming moderation is ignored outright
        // (web parity: moderation is channel-only).
        moderationTargetId != null && isGroupchat -> MessageMutation.Moderation(
            targetId = moderationTargetId,
            from = from,
            moderatedBy = source.moderatedBy,
            reason = source.moderationReason,
        )
        retractsId != null -> MessageMutation.Retraction(targetId = retractsId, from = from)
        replacesId != null && body != null -> MessageMutation.Correction(
            targetId = replacesId,
            from = from,
            newBody = body,
            sourceRevisionId = source.stanzaIds.firstOrNull {
                it.by.equals(bareJid(from), ignoreCase = true)
            }?.id ?: source.stanzaId.takeIf {
                source.stanzaIdBy?.equals(bareJid(from), ignoreCase = true) == true
            },
        )
        reactionTargetId != null -> MessageMutation.Reaction(
            targetId = reactionTargetId,
            from = from,
            senderKey = if (isGroupchat) from else bareJid(from),
            mine = mine,
            emojis = source.reactionEmojis,
        )
        // Room-only, like XEP-0425: no 1:1 sender is trusted to score
        // messages yet (the Rust parser already drops non-room senders).
        safetyScores != null && isGroupchat -> MessageMutation.SafetyScores(
            targetId = safetyScores.targetStanzaId,
            from = from,
            fastening = safetyScores,
            fasteningIds = setOfNotNull(source.stanzaId, source.originId, source.messageId) +
                source.stanzaIds.map { it.id },
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
