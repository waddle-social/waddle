package social.waddle.android.client.store

import social.waddle.client.ffi.WaddleArchivedMessage
import social.waddle.client.ffi.WaddleMessage

/**
 * Normalized timeline row: the fields the list UI renders, plus the raw
 * FFI record for everything else (reactions, replies, files, …).
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
