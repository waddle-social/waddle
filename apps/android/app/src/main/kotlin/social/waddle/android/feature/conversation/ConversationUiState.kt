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
    /** Oldest-first feed (pending messages after stored history), ready to draw. */
    val entries: List<FeedEntry> = emptyList(),
    val isLoadingOlder: Boolean = false,
    val reachedHistoryStart: Boolean = false,
    /** No history fetch has answered yet: show the skeleton, not the empty state. */
    val awaitingFirstPage: Boolean = true,
    /** `urn:waddle:pin:0`: pinned stanza ids (rooms only). */
    val pinnedIds: Set<String> = emptySet(),
    val canPin: Boolean = false,
    /** XEP-0425: own occupant may moderate others' messages (rooms only). */
    val canModerate: Boolean = false,
    /** Feed mode: loaded-history threads overview, newest first. */
    val threads: List<ThreadSummary> = emptyList(),
    /**
     * Social layout: newest row on top under a top-pinned composer.
     * Derived with [entries] so the layout and the grouping always agree.
     */
    val newestFirst: Boolean = false,
) {
    /** The rows behind [entries], oldest first. */
    val rows: List<ConversationRow> get() = entries.map { it.row }
}

/** One thread of the loaded history (overview sheet row). */
data class ThreadSummary(
    val threadId: String,
    val rootAuthor: String?,
    /** The row [rootAuthor] was taken from (avatar resolution). */
    val rootItem: TimelineItem? = null,
    val rootPreview: String,
    /** The preview source was retracted/moderated; render a placeholder. */
    val rootTombstoned: Boolean,
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

    data class Editing(
        val targetId: String,
        val originalBody: String,
        /** XEP-0201: the corrected message's thread, repeated on the edit. */
        val threadId: String? = null,
        /**
         * Bumped on each failed-send restore so the composer's
         * transition detection re-runs the prefill even when the
         * attempted text equals the original body.
         */
        val attempt: Int = 0,
    ) : ComposerMode

    data class Replying(
        val targetId: String,
        val authorJid: String,
        val authorName: String,
        val previewBody: String,
        val threadId: String?,
    ) : ComposerMode
}
