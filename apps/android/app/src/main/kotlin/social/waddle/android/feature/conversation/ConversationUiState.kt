package social.waddle.android.feature.conversation

import social.waddle.android.client.MessageSendExtras
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
    /** Reply/thread annotations, kept so retry preserves the wire shape. */
    val extras: MessageSendExtras? = null,
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
    /** Feed mode: thread id → reply count for the roots' chips. */
    val threadReplyCounts: Map<String, Int> = emptyMap(),
    /** Feed mode: loaded-history threads overview, newest first. */
    val threads: List<ThreadSummary> = emptyList(),
)

/** One thread of the loaded history (overview sheet row). */
data class ThreadSummary(
    val threadId: String,
    val rootAuthor: String?,
    val rootPreview: String,
    val replyCount: Int,
    val lastTimestamp: String?,
)

/** XEP-0363 attachment upload progress. */
sealed interface UploadState {
    data object Idle : UploadState

    data object Uploading : UploadState

    data object TooLarge : UploadState

    data object Failed : UploadState
}

/**
 * Composer target: a fresh send, an XEP-0308 edit of an own row, or an
 * XEP-0461 reply.
 */
sealed interface ComposerMode {
    data object Normal : ComposerMode

    data class Editing(val targetId: String, val originalBody: String) : ComposerMode

    data class Replying(
        val targetId: String,
        val authorJid: String,
        val authorName: String,
        val previewBody: String,
        val threadId: String?,
    ) : ComposerMode
}
