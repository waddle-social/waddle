package social.waddle.android.feature.community

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.FeedEntry
import social.waddle.android.client.VerbResult
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.viewModelFactoryOf

/**
 * The XEP-0472 community Feed tab (web `FeedPane.vue` +
 * `useSocialFeed` parity): renders [XmppSessionManager.feedStore]
 * (refreshed on open and on pull-to-refresh) and publishes posts with
 * the manager's optimistic-prepend-then-refresh verb.
 */
class CommunityViewModel(
    private val sessionManager: XmppSessionManager,
) : ViewModel() {
    /** Feed entries, newest first (store order). */
    val entries: StateFlow<List<FeedEntry>> = sessionManager.feedStore.entries

    /** True while a refresh is in flight (pull-to-refresh spinner). */
    val isLoading: StateFlow<Boolean> = sessionManager.feedStore.isLoading

    private val _isPosting = MutableStateFlow(false)

    /** True while a post publish is in flight (composer gate). */
    val isPosting: StateFlow<Boolean> = _isPosting.asStateFlow()

    private val _postResults = MutableSharedFlow<VerbResult>(extraBufferCapacity = 4)

    /** One result per attempted post; failures surface a snackbar. */
    val postResults: SharedFlow<VerbResult> = _postResults

    private val _loadFailed = MutableStateFlow(false)

    /**
     * True when the most recent feed load failed (web `FeedPane`
     * error-banner parity) — without it a failed fetch would render as
     * an empty feed.
     */
    val loadFailed: StateFlow<Boolean> = _loadFailed.asStateFlow()

    init {
        refresh()
    }

    /** Pull the latest feed page into the store. */
    fun refresh() {
        viewModelScope.launch {
            _loadFailed.value = sessionManager.refreshFeed() != VerbResult.Ok
        }
    }

    /** Publish a post; the store shows it optimistically on accept. */
    fun post(body: String) {
        if (_isPosting.value) return
        _isPosting.value = true
        viewModelScope.launch {
            val result = sessionManager.publishFeedPost(body)
            _isPosting.value = false
            // Web parity (`useSocialFeed` clears its error on post): a
            // successful publish reconciled the store against the
            // server, so a lingering load-failure banner would be a
            // lie over a freshly loaded feed.
            if (result == VerbResult.Ok) _loadFailed.value = false
            _postResults.emit(result)
        }
    }

    companion object {
        fun factory(graph: AppGraph): ViewModelProvider.Factory = viewModelFactoryOf {
            CommunityViewModel(sessionManager = graph.sessionManager)
        }
    }
}
