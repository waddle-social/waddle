package social.waddle.android.client

import kotlinx.coroutines.delay
import social.waddle.client.ffi.WaddleFeedEntry
import java.util.concurrent.CopyOnWriteArrayList

/**
 * XEP-0472 community feed fake verb state for [FakeWaddleClient]:
 * canned feed page, recorded calls, and failure/delay knobs. Extracted
 * so the fake of the full generated FFI interface stays within the
 * LargeClass budget (the [FakeInboxState] pattern).
 */
class FakeCommunityState {
    /** Recorded `maxItems` arguments of feed fetches. */
    val fetchFeedCalls = CopyOnWriteArrayList<UInt?>()

    /** Canned XEP-0472 page served by [fetchFeed]. */
    @Volatile
    var feedEntries: List<WaddleFeedEntry> = emptyList()

    /** When set, [fetchFeed] throws instead of answering. */
    @Volatile
    var fetchFeedFailure: Throwable? = null

    /** Suspends [fetchFeed] before answering (race orchestration). */
    @Volatile
    var fetchFeedDelayMillis: Long = 0

    /** Recorded (body, title) publish calls. */
    val publishFeedCalls = CopyOnWriteArrayList<Pair<String, String?>>()

    /** When set, [publishFeedPost] throws instead of answering. */
    @Volatile
    var publishFeedFailure: Throwable? = null

    /** Item id echoed by [publishFeedPost]. */
    @Volatile
    var publishedItemId: String = "post-fake-1"

    suspend fun fetchFeed(maxItems: UInt?): List<WaddleFeedEntry> {
        fetchFeedCalls += maxItems
        if (fetchFeedDelayMillis > 0) delay(fetchFeedDelayMillis)
        fetchFeedFailure?.let { throw it }
        return feedEntries
    }

    fun publishFeedPost(body: String, title: String?): String {
        publishFeedCalls += body to title
        publishFeedFailure?.let { throw it }
        return publishedItemId
    }
}
