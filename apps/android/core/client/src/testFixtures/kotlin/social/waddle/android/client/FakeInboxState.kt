package social.waddle.android.client

import kotlinx.coroutines.delay
import social.waddle.client.ffi.WaddleInboxMessageContent
import social.waddle.client.ffi.WaddleInboxReadFilter
import social.waddle.client.ffi.WaddleInboxResult
import java.util.concurrent.CopyOnWriteArrayList

/**
 * XEP-0430 fake verb state for [FakeWaddleClient]: canned inbox page,
 * recorded calls, and failure/delay knobs. Extracted so the fake of
 * the full generated FFI interface stays within the LargeClass budget.
 */
class FakeInboxState {
    /** Recorded (read filter, message content) inbox fetches. */
    val fetchInboxCalls = CopyOnWriteArrayList<Pair<WaddleInboxReadFilter, WaddleInboxMessageContent>>()

    /** Canned XEP-0430 page served by [fetchInbox]. */
    @Volatile
    var inboxResult: WaddleInboxResult =
        WaddleInboxResult(total = 0u, unread = 0u, allUnread = 0u, conversations = emptyList())

    /** When set, [fetchInbox] throws instead of answering. */
    @Volatile
    var fetchInboxFailure: Throwable? = null

    /** Suspends [fetchInbox] before answering (race orchestration). */
    @Volatile
    var fetchInboxDelayMillis: Long = 0

    /** Recorded (partnerJid, threadId) inbox mark-read calls. */
    val markInboxReadCalls = CopyOnWriteArrayList<Pair<String, String?>>()

    /** When set, [markInboxRead] throws instead of succeeding. */
    @Volatile
    var markInboxReadFailure: Throwable? = null

    suspend fun fetchInbox(
        readFilter: WaddleInboxReadFilter,
        messageContent: WaddleInboxMessageContent,
    ): WaddleInboxResult {
        fetchInboxCalls += readFilter to messageContent
        if (fetchInboxDelayMillis > 0) delay(fetchInboxDelayMillis)
        fetchInboxFailure?.let { throw it }
        return inboxResult
    }

    fun markInboxRead(partnerJid: String, threadId: String?) {
        markInboxReadCalls += partnerJid to threadId
        markInboxReadFailure?.let { throw it }
    }
}
