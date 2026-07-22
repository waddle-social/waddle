package social.waddle.android.feature.dm

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
import social.waddle.android.client.CreateRoomResult
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleUserSearchEntry

data class NewGroupDmUiState(
    val searchQuery: String = "",
    val searchResults: List<WaddleUserSearchEntry> = emptyList(),
    /** Selected members, jid → display label, in selection order. */
    val selected: Map<String, String> = emptyMap(),
    val name: String = "",
    val isSubmitting: Boolean = false,
    val createFailed: Boolean = false,
) {
    /** Web `NewGroupDmDialog.defaultName` parity: comma-joined labels. */
    val defaultName: String get() = selected.values.joinToString(", ")

    /** Web parity: a group needs at least two members besides self. */
    val canCreate: Boolean get() = selected.size >= MIN_SELECTED_MEMBERS && !isSubmitting

    companion object {
        const val MIN_SELECTED_MEMBERS = 2
    }
}

/**
 * Create-group-DM flow (web `NewGroupDmDialog` parity): XEP-0055
 * multi-select over the user directory, an optional name defaulting to
 * the comma-joined member labels, and the
 * `urn:waddle:group-dm:create:0` submit with self included in the
 * membership.
 */
class NewGroupDmViewModel(
    private val sessionManager: XmppSessionManager,
    private val selfJid: String?,
    initialMembers: Map<String, String> = emptyMap(),
) : ViewModel() {
    private val _uiState = MutableStateFlow(NewGroupDmUiState(selected = initialMembers))
    val uiState: StateFlow<NewGroupDmUiState> = _uiState.asStateFlow()

    private var searchJob: Job? = null
    private var searchTicket = 0

    fun onSearchQueryChanged(query: String) {
        _uiState.update { it.copy(searchQuery = query) }
        searchJob?.cancel()
        val ticket = ++searchTicket
        val trimmed = query.trim()
        if (trimmed.isEmpty()) {
            _uiState.update { it.copy(searchResults = emptyList()) }
            return
        }
        searchJob = viewModelScope.launch {
            delay(SEARCH_DEBOUNCE_MS)
            val excluded = _uiState.value.selected.keys + setOfNotNull(selfJid)
            val results = sessionManager.searchUsers(trimmed)
                .orEmpty()
                .filter { it.jid !in excluded }
            if (ticket == searchTicket) {
                _uiState.update { it.copy(searchResults = results) }
            }
        }
    }

    fun onNameChanged(name: String) {
        _uiState.update { it.copy(name = name) }
    }

    /** Toggle a directory hit in/out of the selection. */
    fun toggleMember(entry: WaddleUserSearchEntry) {
        _uiState.update { state ->
            val selected = state.selected.toMutableMap()
            if (selected.remove(entry.jid) == null) {
                selected[entry.jid] = memberLabelOf(entry)
            }
            state.copy(selected = selected, createFailed = false)
        }
    }

    fun removeMember(jid: String) {
        _uiState.update { it.copy(selected = it.selected - jid, createFailed = false) }
    }

    /**
     * Submit the create: membership = selection + self, name = the
     * typed name or the comma-joined default. On success the manager
     * has already refreshed the topology and joined the room.
     */
    fun create(onCreated: (roomJid: String, name: String) -> Unit) {
        val state = _uiState.value
        if (!state.canCreate) return
        val name = state.name.trim().ifEmpty { state.defaultName }
        val memberJids = buildList {
            selfJid?.let(::add)
            addAll(state.selected.keys)
        }
        _uiState.update { it.copy(isSubmitting = true, createFailed = false) }
        viewModelScope.launch {
            when (val result = sessionManager.createGroupDm(name, memberJids)) {
                is CreateRoomResult.Created -> {
                    // Fresh draft for the next open (the ViewModel
                    // outlives the sheet's composition).
                    _uiState.value = NewGroupDmUiState()
                    onCreated(result.roomJid, name)
                }
                else -> _uiState.update { it.copy(isSubmitting = false, createFailed = true) }
            }
        }
    }

    companion object {
        const val SEARCH_DEBOUNCE_MS = 220L

        fun factory(graph: AppGraph, initialMembers: Map<String, String>): ViewModelProvider.Factory =
            viewModelFactoryOf {
                NewGroupDmViewModel(
                    sessionManager = graph.sessionManager,
                    selfJid = graph.currentSession.value?.jid?.let(::bareJidOf),
                    initialMembers = initialMembers,
                )
            }
    }
}

/** Web `contactLabel` parity: display name, else username, else localpart. */
internal fun memberLabelOf(entry: WaddleUserSearchEntry): String =
    entry.displayName?.trim().orEmpty().ifEmpty {
        entry.username.trim().ifEmpty { localpartOf(entry.jid) }
    }
