package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import social.waddle.android.client.FeedEntry

/**
 * Session cache of the XEP-0472 community feed: seeded by
 * [FeedVerbs.refreshFeed][social.waddle.android.client.XmppSessionManager.refreshFeed]
 * fetches, kept feeling instant by the optimistic prepend on publish
 * (web `useSocialFeed` parity — the feed is pull-only by design, no
 * live subscription path exists).
 */
class FeedStore {
    private val _entries = MutableStateFlow<List<FeedEntry>>(emptyList())

    /** Feed entries, newest first by publish time. */
    val entries: StateFlow<List<FeedEntry>> = _entries.asStateFlow()

    private val _isLoading = MutableStateFlow(false)

    /** True while a refresh is in flight (pull-to-refresh spinner). */
    val isLoading: StateFlow<Boolean> = _isLoading.asStateFlow()

    /** A refresh went on the wire. */
    fun beginLoading() {
        _isLoading.value = true
    }

    /** A refresh answered: replace the cache, newest first (web
     *  parity: entries without a publish time sort last). */
    fun applyLoaded(entries: List<FeedEntry>) {
        _entries.value = entries.sortedByDescending { it.published?.toEpochMilli() ?: 0L }
        _isLoading.value = false
    }

    /** The refresh failed or its session died mid-flight. */
    fun applyLoadFailed() {
        _isLoading.value = false
    }

    /**
     * A publish was accepted: show the local entry at the head
     * immediately (deduped by item id), ahead of the reconciling
     * refresh round-trip.
     */
    fun prepend(entry: FeedEntry) {
        _entries.update { current -> listOf(entry) + current.filterNot { it.id == entry.id } }
    }

    fun clear() {
        _entries.value = emptyList()
        _isLoading.value = false
    }
}
