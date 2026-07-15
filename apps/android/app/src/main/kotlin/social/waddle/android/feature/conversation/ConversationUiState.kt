package social.waddle.android.feature.conversation

import social.waddle.android.client.store.TimelineItem

/** An optimistic own message that has not echoed back from the server. */
data class PendingMessage(
    val localId: Long,
    /** Stanza id from the send outcome; matches the echo for dedupe. */
    val stanzaId: String?,
    val body: String,
    val timestampMillis: Long,
    val failed: Boolean,
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
)
