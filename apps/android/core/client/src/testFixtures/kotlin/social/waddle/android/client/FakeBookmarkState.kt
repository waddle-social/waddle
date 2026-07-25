package social.waddle.android.client

import social.waddle.client.ffi.WaddleBookmarkItem
import social.waddle.client.ffi.WaddleDmBookmarkItem

/**
 * XEP-0402 fake verb state for [FakeWaddleClient]: canned bookmark
 * lists and fetch counters. Extracted so the fake of the full
 * generated FFI interface stays within the LargeClass budget (the
 * [FakeInboxState] pattern).
 */
class FakeBookmarkState {
    /** Canned XEP-0492 bookmark lists served by the fetch verbs. */
    @Volatile
    var userBookmarks: List<WaddleBookmarkItem> = emptyList()

    @Volatile
    var dmBookmarks: List<WaddleDmBookmarkItem> = emptyList()

    @Volatile
    var fetchUserBookmarksCalls = 0

    @Volatile
    var fetchDmBookmarksCalls = 0

    fun fetchUserBookmarks(): List<WaddleBookmarkItem> {
        fetchUserBookmarksCalls += 1
        return userBookmarks
    }

    fun fetchDmBookmarks(): List<WaddleDmBookmarkItem> {
        fetchDmBookmarksCalls += 1
        return dmBookmarks
    }
}
