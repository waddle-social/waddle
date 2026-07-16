package social.waddle.android.feature.admin

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleAdminUserEntry

data class CommunityUsersUiState(
    val query: String = "",
    val entries: List<WaddleAdminUserEntry> = emptyList(),
    val nextCursor: String? = null,
    val loading: Boolean = false,
    /** The list query failed (offline or not the community owner). */
    val failed: Boolean = false,
)

/**
 * Minimal community-admin Users panel (web `UsersPanel.vue` V1
 * parity): prefix search (200 ms debounce) over the paginated
 * `urn:waddle:admin:users:list:0` command, cursor-threaded load-more,
 * read-only rows. Owner-gated by the settings entry; the server
 * answers `forbidden` for anyone else regardless.
 */
class CommunityUsersViewModel(
    private val sessionManager: XmppSessionManager,
) : ViewModel() {
    private val _uiState = MutableStateFlow(CommunityUsersUiState())
    val uiState: StateFlow<CommunityUsersUiState> = _uiState.asStateFlow()

    private var searchJob: Job? = null
    private var requestTicket = 0

    init {
        loadFirstPage()
    }

    fun onQueryChanged(query: String) {
        _uiState.update { it.copy(query = query) }
        searchJob?.cancel()
        searchJob = viewModelScope.launch {
            delay(SEARCH_DEBOUNCE_MS)
            loadFirstPage()
        }
    }

    fun loadFirstPage() {
        val ticket = ++requestTicket
        _uiState.update { it.copy(loading = true, failed = false) }
        viewModelScope.launch {
            val page = sessionManager.adminUsersList(
                prefix = _uiState.value.query.trim().takeIf { it.isNotEmpty() },
                pageSize = PAGE_SIZE,
            )
            if (ticket != requestTicket) return@launch
            _uiState.update { state ->
                if (page == null) {
                    state.copy(loading = false, failed = true)
                } else {
                    state.copy(loading = false, entries = page.entries, nextCursor = page.nextCursor)
                }
            }
        }
    }

    /** Fetch the next page and concatenate (cursor-threaded). */
    fun loadMore() {
        val cursor = _uiState.value.nextCursor ?: return
        if (_uiState.value.loading) return
        val ticket = ++requestTicket
        _uiState.update { it.copy(loading = true) }
        viewModelScope.launch {
            val page = sessionManager.adminUsersList(
                prefix = _uiState.value.query.trim().takeIf { it.isNotEmpty() },
                pageSize = PAGE_SIZE,
                afterCursor = cursor,
            )
            if (ticket != requestTicket) return@launch
            _uiState.update { state ->
                if (page == null) {
                    state.copy(loading = false, failed = true)
                } else {
                    state.copy(
                        loading = false,
                        entries = state.entries + page.entries,
                        nextCursor = page.nextCursor,
                    )
                }
            }
        }
    }

    companion object {
        private const val SEARCH_DEBOUNCE_MS = 200L

        /** Web AdminView parity; the server clamps to 1..200. */
        private const val PAGE_SIZE = 50u

        fun factory(graph: AppGraph): ViewModelProvider.Factory = viewModelFactoryOf {
            CommunityUsersViewModel(sessionManager = graph.sessionManager)
        }
    }
}
