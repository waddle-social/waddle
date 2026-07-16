package social.waddle.android.feature.conversation

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import social.waddle.android.client.VerbResult
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.prefs.SharedFileRef
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.UnreadStore

/**
 * Shared timeline/composer wiring for channels and DMs: store-backed
 * rows plus optimistic pending sends ([PendingSendTracker]), the
 * composer mode machine ([ComposerController]), single-flight budgeted
 * MAM paging, and unread clearing while the conversation is on screen.
 */
open class ConversationViewModel(
    private val conversationJid: String,
    private val isGroupchat: Boolean,
    timeline: StateFlow<List<TimelineItem>>,
    events: SharedFlow<XmppEvent>,
    private val unreadStore: UnreadStore,
    private val io: ConversationIo,
    typingNames: Flow<List<String>> = emptyFlow(),
    // Must emit (combine stalls on a never-emitting source).
    pinnedIds: Flow<Set<String>> = flowOf(emptySet()),
    /**
     * Non-null when this screen shows ONE thread: rows filter to the
     * thread, and every send targets it (XEP-0201). Null = the main
     * feed, which hides thread replies behind reply-count chips.
     */
    private val threadId: String? = null,
    private val uploader: AttachmentUploader? = null,
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
    private val tracker = PendingSendTracker()
    private val composer = ComposerController(conversationJid, isGroupchat, threadId)
    private val history = MutableStateFlow(HistoryState())

    @Volatile
    private var visible = false

    /**
     * Web `isPinnedAtEdge` parity: displayed markers dispatch only
     * while the timeline is scrolled to the newest message — a user
     * reading old history must not mark below-the-fold arrivals read.
     * Conversations open at the newest edge, hence the true default.
     */
    @Volatile
    private var atNewestEdge = true

    private val typingNotifier = ComposerTypingNotifier(
        scope = viewModelScope,
        sendState = { state -> io.sendChatState(state) },
    )

    /** Peers currently composing in this conversation (XEP-0085). */
    val typing: StateFlow<List<String>> =
        typingNames.stateIn(viewModelScope, SharingStarted.Eagerly, emptyList())

    val uiState: StateFlow<ConversationUiState> =
        combine(timeline, tracker.pending, history, pinnedIds) { items, unconfirmed, load, pins ->
            ConversationUiState(
                rows = visibleRows(items, unconfirmed, threadId),
                isLoadingOlder = load.inFlight,
                reachedHistoryStart = load.reachedStart,
                pinnedIds = pins,
                canPin = io.canPin,
                threadReplyCounts = if (threadId == null) replyCountsOf(items) else emptyMap(),
                threads = if (threadId == null) {
                    threadSummariesOf(items) { authorNameOf(it, isGroupchat) }
                } else {
                    emptyList()
                },
            )
        }.stateIn(viewModelScope, SharingStarted.Eagerly, ConversationUiState())

    /** Normal sends vs editing an existing own message (XEP-0308). */
    val composerMode: StateFlow<ComposerMode> = composer.mode

    private val _actionFailures = MutableSharedFlow<VerbResult.Failure>(extraBufferCapacity = 4)

    /** Fired when a conversation action (reaction, retraction, pin/unpin) is refused. */
    val actionFailures: SharedFlow<VerbResult.Failure> = _actionFailures

    private val _uploadState = MutableStateFlow<UploadState>(UploadState.Idle)

    /** XEP-0363 attachment upload progress for the composer banner. */
    val uploadState: StateFlow<UploadState> = _uploadState

    init {
        viewModelScope.launch {
            timeline.collect { items ->
                tracker.pruneAgainst(storedIdentityIdsOf(items))
                // New rows arriving while the conversation is on screen
                // AND scrolled to the newest edge count as read (the
                // marker path dedupes repeats). Guarded: a marker failure
                // must not kill the pruning collector. Thread screens
                // never mark (see onConversationVisible).
                if (visible && atNewestEdge && threadId == null) {
                    runCatching { io.markDisplayed() }
                }
            }
        }
        viewModelScope.launch {
            io.ensureJoined()
            loadOlder()
        }
        viewModelScope.launch { events.collect(::onEvent) }
    }

    private suspend fun onEvent(event: XmppEvent) {
        when (event) {
            is XmppEvent.DeliveryAcked -> tracker.onDeliveryAcked(event.stanzaId)
            is XmppEvent.DeliveryFailed -> tracker.onDeliveryFailed(event.stanzaId)
            // Reconnect catch-up: refetch the newest page.
            XmppEvent.SessionReady -> {
                // A join tapped before the first Ready only persisted
                // its intent; re-ensure it here so the open screen goes
                // live without waiting for a reconnect (no-op when
                // already joined).
                io.ensureJoined()
                refreshHistory()
            }
            else -> Unit
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

    /** Optimistic send: append a pending row and dispatch it. */
    fun send(body: String) {
        val text = body.trim()
        if (text.isEmpty()) return
        val mode = composer.mode.value
        if (mode is ComposerMode.Editing) {
            composer.cancelEdit()
            viewModelScope.launch { sendCorrection(mode, text) }
            return
        }
        val extras = composer.extrasFor(mode)
        if (mode is ComposerMode.Replying) composer.cancelReply()
        val message = tracker.append(text, extras, clock())
        viewModelScope.launch { dispatch(message) }
    }

    private suspend fun sendCorrection(mode: ComposerMode.Editing, text: String) {
        val result = runCatching {
            io.sendCorrection(mode.targetId, text, mode.threadId)
        }.getOrDefault(VerbResult.Rejected)
        if (result is VerbResult.Ok) typingNotifier.onMessageSent() else composer.restoreFailedEdit(mode, text)
    }

    private suspend fun dispatch(message: PendingMessage, wireBody: String = message.body) {
        val result = io.send(wireBody, message.extras)
        // Delivered (or queued for replay): typing ended with content,
        // not a pause — emit `active` (web parity).
        if (tracker.onSendResult(message.localId, result)) typingNotifier.onMessageSent()
    }

    /** The composer's text changed: drive the XEP-0085 state machine. */
    fun onDraftChanged() {
        typingNotifier.onTyping()
    }

    /**
     * Upload a picked document (XEP-0363) and send it as a files-only
     * message (empty wire body, XEP-0447 metadata — web parity); the
     * optimistic row shows the file name.
     */
    fun sendAttachment(uri: android.net.Uri) {
        val up = uploader ?: return
        if (_uploadState.value == UploadState.Uploading) return
        _uploadState.value = UploadState.Uploading
        viewModelScope.launch {
            when (val result = runCatching { up.upload(uri) }.getOrElse { UploadResult.Failed }) {
                is UploadResult.Done -> dispatchUploaded(result.file)
                UploadResult.TooLarge -> _uploadState.value = UploadState.TooLarge
                UploadResult.Failed -> _uploadState.value = UploadState.Failed
            }
        }
    }

    private suspend fun dispatchUploaded(file: SharedFileRef) {
        _uploadState.value = UploadState.Idle
        val mode = composer.mode.value
        val extras = composer.extrasFor(mode, files = listOf(file))
        if (mode is ComposerMode.Replying) composer.cancelReply()
        val message = tracker.append(file.name ?: file.url, extras, clock())
        dispatch(message, wireBody = "")
    }

    /** Dismiss a failed-upload banner. */
    fun clearUploadState() {
        if (_uploadState.value != UploadState.Uploading) {
            _uploadState.value = UploadState.Idle
        }
    }

    /**
     * XEP-0444 chip/sheet toggle. The replacement set is computed by
     * the session manager INSIDE its reaction mutex — computing it here
     * would read a stale base while a prior send holds the lock, and
     * the full-set replace semantics would erase the queued toggle.
     */
    fun toggleReaction(item: TimelineItem, emoji: String) {
        val target = actionTargetIdOf(item) ?: return
        launchVerb { io.toggleReaction(target, emoji) }
    }

    /** Enter reply mode targeting [item] (XEP-0461). */
    fun startReply(item: TimelineItem) = composer.startReply(item)

    fun cancelReply() = composer.cancelReply()

    /** See the top-level [threadIdFor]. */
    fun threadIdFor(item: TimelineItem): String = threadIdFor(item, isGroupchat)

    /** Enter edit mode for an own, non-tombstoned message. */
    fun startEdit(item: TimelineItem) = composer.startEdit(item)

    fun cancelEdit() = composer.cancelEdit()

    /** XEP-0424 retraction of an own message. */
    fun retract(item: TimelineItem) {
        if (!item.isMine || item.tombstone != null) return
        val target = actionTargetIdOf(item) ?: return
        launchVerb { io.sendRetraction(target) }
    }

    /**
     * `urn:waddle:pin:0` room pin toggle. The server gates on
     * Owner/Admin affiliation; a refusal surfaces instead of failing
     * silently (the action is offered to every member because the
     * client does not track its own affiliation yet).
     */
    fun setPinned(item: TimelineItem, pinned: Boolean) {
        val target = actionTargetIdOf(item) ?: return
        launchVerb { io.setPinned(target, pinned) }
    }

    /** Fire a conversation verb; refusals surface via [actionFailures]. */
    private fun launchVerb(verb: suspend () -> VerbResult) {
        viewModelScope.launch {
            val result = runCatching { verb() }.getOrDefault(VerbResult.Rejected)
            if (result is VerbResult.Failure) _actionFailures.tryEmit(result)
        }
    }

    /** See the top-level [actionTargetIdOf]. */
    fun actionTargetIdOf(item: TimelineItem): String? =
        actionTargetIdOf(item, isGroupchat, conversationJid)

    /** Re-send a failed optimistic message (reply/thread extras kept). */
    fun retry(localId: Long) {
        val message = tracker.takeRetry(localId) ?: return
        val retried = tracker.append(message.body, message.extras, clock())
        viewModelScope.launch {
            // Files-only sends carry an empty wire body (the row shows
            // the file name); everything else resends its body.
            val wireBody = if (message.extras?.sharedFiles.orEmpty().isNotEmpty()) "" else retried.body
            dispatch(retried, wireBody)
        }
    }

    /**
     * The conversation is on screen: suppress and clear unread counts.
     * Thread screens do NEITHER — they share the parent conversation's
     * jid, so claiming the active marker would fight the feed screen
     * underneath, and marking displayed would read messages the user
     * has never seen in the main feed.
     */
    fun onConversationVisible() {
        visible = true
        io.recordConversationSeen()
        if (threadId != null) return
        unreadStore.setActiveConversation(conversationJid)
        unreadStore.clear(conversationJid)
        viewModelScope.launch { runCatching { onConversationRead(conversationJid) } }
        if (atNewestEdge) {
            viewModelScope.launch { runCatching { io.markDisplayed() } }
        }
    }

    /** The list crossed the newest-edge boundary (see [atNewestEdge]). */
    fun onAtNewestEdgeChanged(atEdge: Boolean) {
        val crossedIntoLive = !atNewestEdge && atEdge
        atNewestEdge = atEdge
        if (crossedIntoLive && visible && threadId == null) {
            // Scrolling back down to live reads everything above it.
            viewModelScope.launch { runCatching { io.markDisplayed() } }
        }
    }

    fun onConversationHidden() {
        visible = false
        if (threadId != null) return
        unreadStore.clearActiveConversationIf(conversationJid)
    }

    private fun refreshHistory() {
        if (history.value.inFlight) return
        history.value = HistoryState()
        loadOlder()
    }

    private data class HistoryState(
        val inFlight: Boolean = false,
        val reachedStart: Boolean = false,
        /** RSM `before` cursor: the mam id of the oldest fetched row. */
        val oldestMamId: String? = null,
        val pagesFetched: Int = 0,
    )

    private companion object {
        const val PAGE_SIZE = 50u

        /** Scroll-back budget per screen (50 · 20 = 1000 messages). */
        const val HISTORY_PAGE_BUDGET = 20
    }
}
