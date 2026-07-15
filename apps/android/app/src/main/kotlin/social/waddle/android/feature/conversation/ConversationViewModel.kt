package social.waddle.android.feature.conversation

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.UnreadStore
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * Shared timeline/composer logic for channels and DMs: store-backed
 * rows plus optimistic pending sends (deduped against echoes by stanza
 * id), single-flight budgeted MAM paging, delivery-failed and
 * offline-queued markers, and unread clearing while the conversation is
 * on screen.
 */
open class ConversationViewModel(
    private val conversationJid: String,
    timeline: StateFlow<List<TimelineItem>>,
    events: SharedFlow<XmppEvent>,
    private val unreadStore: UnreadStore,
    private val io: ConversationIo,
    private val pageSize: UInt = PAGE_SIZE,
    private val historyPageBudget: Int = HISTORY_PAGE_BUDGET,
    private val clock: () -> Long = System::currentTimeMillis,
    /**
     * Reading a conversation must also retire its shade notification —
     * otherwise a notification for content read in-app lingers, and its
     * retained MessagingStyle history re-surfaces already-read messages
     * alongside the next backgrounded arrival.
     */
    private val onConversationRead: suspend (String) -> Unit = {},
) : ViewModel() {
    private val pending = MutableStateFlow<List<PendingMessage>>(emptyList())
    private val history = MutableStateFlow(HistoryState())
    private var nextLocalId = 0L
    private val ackedIds = mutableSetOf<String>()
    private val failedIds = mutableSetOf<String>()

    val uiState: StateFlow<ConversationUiState> =
        combine(timeline, pending, history) { items, unconfirmed, load ->
            // Union of every stored identity: the MUC reflection is keyed
            // by the room-assigned XEP-0359 stanza-id, but the id the send
            // returned is the client origin-id — matching only the
            // collapsed key would leave every sent channel message
            // duplicated (unconfirmed bubble + stored echo).
            val storedIds = HashSet<String>()
            items.forEach { item ->
                storedIds += item.id
                storedIds += item.identityIds
            }
            val visiblePending = unconfirmed.filter { message ->
                message.stanzaId == null || message.stanzaId !in storedIds
            }
            ConversationUiState(
                rows = items.map { ConversationRow.Stored(it) } +
                    visiblePending.map { ConversationRow.Unconfirmed(it) },
                isLoadingOlder = load.inFlight,
                reachedHistoryStart = load.reachedStart,
            )
        }.stateIn(viewModelScope, SharingStarted.Eagerly, ConversationUiState())

    init {
        viewModelScope.launch {
            io.ensureJoined()
            loadOlder()
        }
        viewModelScope.launch {
            events.collect { event ->
                when (event) {
                    // The 0198 ack carries the client-generated id the
                    // send returned. The row is only MARKED acked, never
                    // removed: a DM has no reflection back to the sending
                    // resource, so deleting here would vanish the message
                    // until the next MAM refetch. MUC rows disappear when
                    // the stored echo matches an identity id.
                    is XmppEvent.DeliveryAcked -> markAcked(event.stanzaId)
                    is XmppEvent.DeliveryFailed -> markFailed(event.stanzaId)
                    // Reconnect catch-up: refetch the newest page.
                    XmppEvent.SessionReady -> {
                        // A join tapped before the first Ready only
                        // persisted its intent; re-ensure it here so the
                        // open screen goes live without waiting for a
                        // reconnect (no-op when already joined).
                        io.ensureJoined()
                        refreshHistory()
                    }
                    else -> Unit
                }
            }
        }
    }

    /** Fetch the next older MAM page; single-flight and budgeted. */
    fun loadOlder() {
        val state = history.value
        if (state.inFlight || state.reachedStart || state.pagesFetched >= historyPageBudget) return
        history.value = state.copy(inFlight = true)
        viewModelScope.launch {
            val page = io.fetchHistory(pageSize, history.value.oldestMamId)
            history.update { current ->
                if (page == null) {
                    current.copy(inFlight = false)
                } else {
                    HistoryState(
                        inFlight = false,
                        reachedStart = page.isComplete || page.messages.isEmpty(),
                        oldestMamId = page.firstId ?: current.oldestMamId,
                        pagesFetched = current.pagesFetched + 1,
                    )
                }
            }
        }
    }

    /**
     * Optimistic send: append a pending row. A `Sent` outcome adopts the
     * returned stanza id; a QUEUED failure (the manager persisted the
     * message for replay — [SendResult.queuedId]) adopts the queue id,
     * which the replay reuses as its XEP-0359 origin-id, so the eventual
     * echo collapses this row and delivery events target it, exactly
     * like a live send. Only non-queued (permanent) outcomes mark the
     * row failed.
     */
    fun send(body: String) {
        val text = body.trim()
        if (text.isEmpty()) return
        val message = PendingMessage(
            localId = nextLocalId++,
            stanzaId = null,
            body = text,
            timestampMillis = clock(),
            failed = false,
        )
        pending.update { it + message }
        viewModelScope.launch {
            val result = io.send(text)
            val trackedId = when (val outcome = result.outcome) {
                is WaddleSendMessageOutcome.Sent -> outcome.stanzaId
                else -> result.queuedId
            }
            if (trackedId == null) {
                updatePending(message.localId) { it.copy(failed = true) }
                return@launch
            }
            updatePending(message.localId) {
                it.copy(
                    stanzaId = trackedId,
                    queued = result.queued && trackedId !in failedIds,
                    // Both the ack AND the failure event can beat this
                    // continuation; failure wins.
                    acked = trackedId in ackedIds && trackedId !in failedIds,
                    failed = trackedId in failedIds,
                )
            }
        }
    }

    /** Re-send a failed optimistic message. */
    fun retry(localId: Long) {
        val message = pending.value.firstOrNull { it.localId == localId && it.failed } ?: return
        pending.update { list -> list.filterNot { it.localId == localId } }
        send(message.body)
    }

    /** The conversation is on screen: suppress and clear unread counts. */
    fun onConversationVisible() {
        unreadStore.setActiveConversation(conversationJid)
        unreadStore.clear(conversationJid)
        io.recordConversationSeen()
        viewModelScope.launch { runCatching { onConversationRead(conversationJid) } }
    }

    fun onConversationHidden() {
        unreadStore.clearActiveConversationIf(conversationJid)
    }

    private fun refreshHistory() {
        if (history.value.inFlight) return
        history.value = HistoryState()
        loadOlder()
    }

    private fun markAcked(stanzaId: String) {
        ackedIds += stanzaId
        pending.update { list ->
            list.map {
                if (it.stanzaId == stanzaId) it.copy(acked = true, failed = false, queued = false) else it
            }
        }
    }

    private fun markFailed(stanzaId: String) {
        failedIds += stanzaId
        pending.update { list ->
            list.map {
                if (it.stanzaId == stanzaId) it.copy(failed = true, queued = false) else it
            }
        }
    }

    private fun updatePending(localId: Long, transform: (PendingMessage) -> PendingMessage) {
        pending.update { list ->
            list.map { if (it.localId == localId) transform(it) else it }
        }
    }

    private data class HistoryState(
        val inFlight: Boolean = false,
        val reachedStart: Boolean = false,
        /** RSM `before` cursor: the mam id of the oldest fetched row. */
        val oldestMamId: String? = null,
        val pagesFetched: Int = 0,
    )

    private companion object {
        val PAGE_SIZE = 50u

        /** Scroll-back budget per screen (50 · 20 = 1000 messages). */
        const val HISTORY_PAGE_BUDGET = 20
    }
}
