package social.waddle.android.client.store

import social.waddle.client.ffi.WaddleArchivedMessage
import social.waddle.client.ffi.WaddleMessage

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
        get() = when (source) {
            is TimelineSource.Live -> setOfNotNull(
                source.message.stanzaId,
                source.message.originId,
                source.message.id,
            )
            is TimelineSource.Archived -> setOfNotNull(
                source.message.stanzaId,
                source.message.originId,
                source.message.id,
            )
        }
}

sealed interface TimelineSource {
    data class Live(val message: WaddleMessage) : TimelineSource

    data class Archived(val message: WaddleArchivedMessage) : TimelineSource
}
