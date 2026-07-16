package social.waddle.android.client.store

import social.waddle.client.ffi.WaddleArchivedMessage
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddleStanzaId

/** One emoji's aggregated reaction state on a timeline row. */
data class ReactionGroup(
    val emoji: String,
    val count: Int,
    /** The signed-in account is among the reactors (toggle-off target). */
    val mine: Boolean,
)

/** A row whose content was removed; the UI renders a placeholder. */
sealed interface MessageTombstone {
    /** XEP-0424: retracted by its own sender. */
    data object Retracted : MessageTombstone

    /** XEP-0425: removed by a room moderator. */
    data class Moderated(val moderatedBy: String?, val reason: String?) : MessageTombstone
}

/**
 * Normalized timeline row: the fields the list UI renders, plus the raw
 * FFI record for everything else (replies, files, threads, …).
 */
data class TimelineItem(
    /** Dedupe identity: stanza id, else origin id, else message id. */
    val id: String,
    val conversationJid: String,
    val from: String?,
    val body: String,
    /** RFC 3339 timestamp as delivered; `null` for undelayed live messages. */
    val timestamp: String?,
    val isMine: Boolean,
    val source: TimelineSource,
    /** XEP-0444 aggregation, first-reacted emoji order. */
    val reactions: List<ReactionGroup> = emptyList(),
    /** XEP-0308: [body] is a correction of the original send. */
    val edited: Boolean = false,
    /** Set when retracted/moderated; UI must not render [body]. */
    val tombstone: MessageTombstone? = null,
) {
    /**
     * Every wire identity of the underlying stanza: XEP-0359 stanza id(s),
     * client origin id, and the message id. Pending-send reconciliation
     * must match against all of them — the send returns the origin id
     * while the MUC reflection is keyed by the room-assigned stanza id.
     */
    val identityIds: Set<String>
        get() = setOfNotNull(source.stanzaId, source.originId, source.messageId) +
            source.stanzaIds.map { it.id }

    /** Every XEP-0359 `<stanza-id/>` on the stanza, document order. */
    val stanzaIds: List<WaddleStanzaId> get() = source.stanzaIds

    /**
     * The stanza id ASSIGNED BY [authorities] (case-insensitive bare-JID
     * match), scanning the full XEP-0359 list — the first element is
     * whatever the sender put there and may be occupant/peer-injected;
     * trust comes from the `by` authority, never from position (web
     * `assignedStanzaIdBy` parity).
     */
    fun assignedStanzaId(vararg authorities: String): WaddleStanzaId? {
        val wanted = authorities.map { it.lowercase() }
        stanzaIds.firstOrNull { it.by.lowercase() in wanted }?.let { return it }
        val singleBy = stanzaIdBy?.lowercase() ?: return null
        val single = stanzaId ?: return null
        return if (singleBy in wanted) WaddleStanzaId(id = single, by = stanzaIdBy!!) else null
    }

    /** XEP-0359 stanza id (action target in MUCs when [stanzaIdBy] is the room). */
    val stanzaId: String? get() = source.stanzaId

    /** XEP-0359 assigning authority of [stanzaId]. */
    val stanzaIdBy: String? get() = source.stanzaIdBy

    /** XEP-0359 client origin id (the author-assigned correction target). */
    val originId: String? get() = source.originId

    /** The author's message id. */
    val messageId: String? get() = source.messageId

    /** XEP-0201 thread this message belongs to. */
    val threadId: String? get() = source.thread

    /** XEP-0461 reply target id. */
    val replyToId: String? get() = source.replyToId

    /** XEP-0461 reply target author (string JID). */
    val replyToSender: String? get() = source.replyToSender

    /** `urn:waddle:call-thread:0` anchor riding this message. */
    val hasCallThread: Boolean get() = source.hasCallThread

    /**
     * True when this row renders in the main conversation feed: thread
     * REPLIES render only inside their thread screen, while the thread
     * root (id == threadId) stays in the feed with a replies chip (web
     * `isFeedTimelineMessage` parity). Call-thread anchors carry a
     * thread id that is NOT one of their own ids (the call sid) yet
     * belong in the feed — web's explicit callThread exception.
     */
    val isFeedVisible: Boolean
        get() = threadId == null || threadId in identityIds || hasCallThread
}

/**
 * The raw FFI record behind a [TimelineItem], with the fields shared by
 * the live and archived shapes projected onto the interface so consumers
 * (and the mutation extractor) never branch on the variant.
 */
sealed interface TimelineSource {
    val from: String?
    val body: String?

    /** XEP-0359 stanza id. */
    val stanzaId: String?

    /** XEP-0359 assigning authority of [stanzaId]. */
    val stanzaIdBy: String?

    /** Every XEP-0359 `<stanza-id/>` on the stanza, document order. */
    val stanzaIds: List<WaddleStanzaId>

    /** XEP-0359 client origin id. */
    val originId: String?

    /** The author's message id. */
    val messageId: String?

    /** XEP-0201 thread. */
    val thread: String?

    /** XEP-0461 reply target id. */
    val replyToId: String?

    /** XEP-0461 reply target author (string JID). */
    val replyToSender: String?

    /** XEP-0428 fallback range of the reply quote, Unicode code points. */
    val replyFallbackStart: UInt?
    val replyFallbackEnd: UInt?

    /** XEP-0425 moderation payload. */
    val moderationTargetId: String?
    val moderatedBy: String?
    val moderationReason: String?

    /** XEP-0424 retraction target. */
    val retractsId: String?

    /** XEP-0308 correction target. */
    val replacesId: String?

    /** XEP-0444 reaction payload. */
    val reactionTargetId: String?
    val reactionEmojis: List<String>

    /** `urn:waddle:call-thread:0` anchor riding this message. */
    val hasCallThread: Boolean

    data class Live(val message: WaddleMessage) : TimelineSource {
        override val from: String? get() = message.from
        override val body: String? get() = message.body
        override val stanzaId: String? get() = message.stanzaId
        override val stanzaIdBy: String? get() = message.stanzaIdBy
        override val stanzaIds: List<WaddleStanzaId> get() = message.stanzaIds
        override val originId: String? get() = message.originId
        override val messageId: String? get() = message.id
        override val thread: String? get() = message.thread
        override val replyToId: String? get() = message.replyToId
        override val replyToSender: String? get() = message.replyToSender
        override val replyFallbackStart: UInt? get() = message.replyFallbackStart
        override val replyFallbackEnd: UInt? get() = message.replyFallbackEnd
        override val moderationTargetId: String? get() = message.moderationTargetId
        override val moderatedBy: String? get() = message.moderatedBy
        override val moderationReason: String? get() = message.moderationReason
        override val retractsId: String? get() = message.retractsId
        override val replacesId: String? get() = message.replacesId
        override val reactionTargetId: String? get() = message.reactionTargetId
        override val reactionEmojis: List<String> get() = message.reactionEmojis
        override val hasCallThread: Boolean get() = message.callThread != null
    }

    data class Archived(val message: WaddleArchivedMessage) : TimelineSource {
        override val from: String? get() = message.from
        override val body: String? get() = message.body
        override val stanzaId: String? get() = message.stanzaId
        override val stanzaIdBy: String? get() = message.stanzaIdBy
        override val stanzaIds: List<WaddleStanzaId> get() = message.stanzaIds
        override val originId: String? get() = message.originId
        override val messageId: String? get() = message.id
        override val thread: String? get() = message.thread
        override val replyToId: String? get() = message.replyToId
        override val replyToSender: String? get() = message.replyToSender
        override val replyFallbackStart: UInt? get() = message.replyFallbackStart
        override val replyFallbackEnd: UInt? get() = message.replyFallbackEnd
        override val moderationTargetId: String? get() = message.moderationTargetId
        override val moderatedBy: String? get() = message.moderatedBy
        override val moderationReason: String? get() = message.moderationReason
        override val retractsId: String? get() = message.retractsId
        override val replacesId: String? get() = message.replacesId
        override val reactionTargetId: String? get() = message.reactionTargetId
        override val reactionEmojis: List<String> get() = message.reactionEmojis
        override val hasCallThread: Boolean get() = message.callThread != null
    }
}
