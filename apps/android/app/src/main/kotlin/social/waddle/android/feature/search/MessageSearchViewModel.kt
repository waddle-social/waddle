package social.waddle.android.feature.search

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.XmppSessionRuntime
import social.waddle.android.jid.localpartOf
import social.waddle.android.jid.resourcepartOf
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleArchivedMessage
import social.waddle.client.ffi.WaddleMamPage

/**
 * Query state machine for full-text message search (XEP-0313 archive
 * query), ported from the web's `useChannelMessageSearch` /
 * `useDmMessageSearch` composables: keystrokes debounce before hitting
 * the archive, and a generation counter drops stale responses so an
 * older search resolving late can never clobber a newer query's state.
 */
class MessageSearchViewModel(
    private val sessionRuntime: XmppSessionRuntime,
    private val target: MessageSearchTarget,
) : ViewModel() {
    private val _query = MutableStateFlow("")

    /** The raw field text; its debounced trim drives the searches. */
    val query: StateFlow<String> = _query.asStateFlow()

    private val _state = MutableStateFlow<MessageSearchState>(MessageSearchState.Idle)
    val state: StateFlow<MessageSearchState> = _state.asStateFlow()

    /**
     * Race-id guard (web parity): every dispatched search takes a
     * ticket, and only the response holding the newest ticket may
     * write state. Confined to the main dispatcher — all bumps and
     * reads run in [viewModelScope] — so a plain field suffices.
     */
    private var searchGeneration = 0

    init {
        viewModelScope.launch { collectQueries() }
    }

    fun onQueryChanged(value: String) {
        _query.value = value
    }

    /** Sheet dismissed: invalidate in-flight tickets, reset for reopen. */
    fun clear() {
        searchGeneration += 1
        _query.value = ""
        _state.value = MessageSearchState.Idle
    }

    @OptIn(FlowPreview::class)
    private suspend fun collectQueries() {
        _query
            .map { it.trim() }
            .distinctUntilChanged()
            // An emptied field clears instantly; only real queries wait
            // out the typing pause.
            .debounce { trimmed -> if (trimmed.isEmpty()) 0L else DEBOUNCE_MILLIS }
            .collect { trimmed ->
                // Every accepted emission invalidates in-flight tickets,
                // INCLUDING the clear: a response landing after the user
                // emptied the field must not resurrect results.
                val ticket = ++searchGeneration
                if (trimmed.isEmpty()) {
                    _state.value = MessageSearchState.Idle
                } else {
                    _state.value = MessageSearchState.Searching
                    // Launched, not awaited: the collector must stay
                    // responsive so the next debounced query can take a
                    // newer ticket while this one is still in flight.
                    viewModelScope.launch { runSearch(ticket, trimmed) }
                }
            }
    }

    private suspend fun runSearch(ticket: Int, query: String) {
        // Client-layer parity: cancellation must propagate — swallowing
        // it here would report a torn-down screen's search as Failed.
        val page = try {
            searchArchive(query)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }
        if (ticket != searchGeneration) return
        _state.value = when {
            page == null -> MessageSearchState.Failed
            // The FFI reports archive errors as an empty page with
            // isComplete=false; a genuine zero-hit query answers with a
            // complete fin. Only the latter is truly "no matches".
            page.messages.isEmpty() && !page.isComplete -> MessageSearchState.Failed
            else -> page.messages.mapNotNull(::hitOf).let { hits ->
                if (hits.isEmpty()) MessageSearchState.Empty else MessageSearchState.Results(hits)
            }
        }
    }

    private suspend fun searchArchive(query: String): WaddleMamPage? = if (target.isGroupchat) {
        sessionRuntime.searchRoomHistory(target.conversationJid, query, MAX_RESULTS)
    } else {
        sessionRuntime.searchDmHistory(target.conversationJid, query, MAX_RESULTS)
    }

    /** Displayable projection; retracted and bodyless matches drop out. */
    private fun hitOf(message: WaddleArchivedMessage): MessageSearchHit? {
        if (message.isRetracted) return null
        val body = message.body?.takeIf { it.isNotBlank() } ?: return null
        // MessageCard parity: a MUC occupant JID's display name is its
        // resource (nick); a DM author's is their localpart.
        val author = message.from?.let { from ->
            if (target.isGroupchat) resourcepartOf(from) ?: localpartOf(from) else localpartOf(from)
        }
        return MessageSearchHit(
            key = message.mamId,
            author = author,
            body = body,
            timestamp = message.timestamp,
        )
    }

    companion object {
        /** Web parity: wait out a typing pause before hitting the archive. */
        const val DEBOUNCE_MILLIS = 300L

        /**
         * Matches per query. The DM cap of 20 is web parity; applying
         * the same bound to room search is a deliberate Android choice
         * (the web channel side passes no explicit limit) — the sheet's
         * compact list wants a bounded page.
         */
        const val MAX_RESULTS = 20u

        fun factory(graph: AppGraph, target: MessageSearchTarget): ViewModelProvider.Factory =
            viewModelFactoryOf { MessageSearchViewModel(graph.sessionRuntime, target) }
    }
}
