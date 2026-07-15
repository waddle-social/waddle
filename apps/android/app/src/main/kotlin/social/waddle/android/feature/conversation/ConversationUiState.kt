package social.waddle.android.feature.conversation

import social.waddle.android.client.store.TimelineItem

/** An optimistic own message that has not echoed back from the server. */
data class PendingMessage(
    val localId: Long,
    /**
     * Stanza id from the send outcome — or the outbound-queue id for a
     * queued send (the replay reuses it); matches the echo for dedupe.
     */
    val stanzaId: String?,
    val body: String,
    val timestampMillis: Long,
    val failed: Boolean,
    /** XEP-0198-acked: delivered, but (DMs) never reflected back. */
    val acked: Boolean = false,
    /** Persisted to the outbound queue; sends itself on reconnect. */
    val queued: Boolean = false,
)

/** One timeline row: a store-backed message or an optimistic pending one. */
sealed interface ConversationRow {
    data class Stored(val item: TimelineItem) : ConversationRow

    data class Unconfirmed(val message: PendingMessage) : ConversationRow
}

data class ConversationUiState(
    /** Oldest-first rows (pending messages appended after stored history). */
    val rows: List<ConversationRow> = emptyList(),
    val isLoadingOlder: Boolean = false,
    val reachedHistoryStart: Boolean = false,
    /** `urn:waddle:pin:0`: pinned stanza ids (rooms only). */
    val pinnedIds: Set<String> = emptySet(),
    val canPin: Boolean = false,
)

/** Composer target: a fresh send, or an XEP-0308 edit of an own row. */
sealed interface ComposerMode {
    data object Normal : ComposerMode

    data class Editing(val targetId: String, val originalBody: String) : ComposerMode
}
