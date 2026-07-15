package social.waddle.android.feature.conversation

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.Flow
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
import social.waddle.android.client.MessageSendExtras
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
    private val pending = MutableStateFlow<List<PendingMessage>>(emptyList())
    private val history = MutableStateFlow(HistoryState())
    private var nextLocalId = 0L
    private val ackedIds = mutableSetOf<String>()
    private val failedIds = mutableSetOf<String>()

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

    /** Store handle for action-time (non-composition) reads. */
    private val timelineState = timeline

    private val typingNotifier = ComposerTypingNotifier(
        scope = viewModelScope,
        sendState = { state -> io.sendChatState(state) },
    )

    /** Peers currently composing in this conversation (XEP-0085). */
    val typing: StateFlow<List<String>> =
        typingNames.stateIn(viewModelScope, SharingStarted.Eagerly, emptyList())

    val uiState: StateFlow<ConversationUiState> =
        combine(timeline, pending, history, pinnedIds) { items, unconfirmed, load, pins ->
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
            val visibleItems = if (threadId != null) {
                items.filter { it.threadId == threadId || threadId in it.identityIds }
            } else {
                items.filter { it.isFeedVisible }
            }
            // Reply-count chips on feed roots: replies grouped by the
            // thread they carry, matched to the root via its identities.
            val replyCounts = if (threadId == null) {
                val byThread = HashMap<String, Int>()
                items.forEach { item ->
                    val thread = item.threadId ?: return@forEach
                    if (thread !in item.identityIds) byThread.merge(thread, 1, Int::plus)
                }
                byThread
            } else {
                emptyMap()
            }
            ConversationUiState(
                rows = visibleItems.map { ConversationRow.Stored(it) } +
                    visiblePending.map { ConversationRow.Unconfirmed(it) },
                isLoadingOlder = load.inFlight,
                reachedHistoryStart = load.reachedStart,
                pinnedIds = pins,
                canPin = io.canPin,
                threadReplyCounts = replyCounts,
                threads = if (threadId == null) threadSummariesOf(items) else emptyList(),
            )
        }.stateIn(viewModelScope, SharingStarted.Eagerly, ConversationUiState())

    private val _composerMode = MutableStateFlow<ComposerMode>(ComposerMode.Normal)

    /** Normal sends vs editing an existing own message (XEP-0308). */
    val composerMode: StateFlow<ComposerMode> = _composerMode

    private val _uploadState = MutableStateFlow<UploadState>(UploadState.Idle)

    /** XEP-0363 attachment upload progress for the composer banner. */
    val uploadState: StateFlow<UploadState> = _uploadState

    init {
        // Prune (not just hide) pending rows once the timeline holds their
        // identity: the view-side filter alone let the list grow for the
        // screen's lifetime, and a timeline trim could drop the stored row
        // and resurrect an already-delivered send as an unconfirmed ghost.
        viewModelScope.launch {
            timeline.collect { items ->
                val storedIds = HashSet<String>()
                items.forEach { item ->
                    storedIds += item.id
                    storedIds += item.identityIds
                }
                pending.update { list ->
                    list.filterNot { it.stanzaId != null && it.stanzaId in storedIds }
                }
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
        val mode = _composerMode.value
        if (mode is ComposerMode.Editing) {
            _composerMode.value = ComposerMode.Normal
            viewModelScope.launch {
                val sent = runCatching {
                    io.sendCorrection(mode.targetId, text, mode.threadId)
                }.getOrDefault(false)
                if (sent) {
                    typingNotifier.onMessageSent()
                } else {
                    // Corrections are never queued (a replayed edit could
                    // outrank a newer one) — restore edit mode with the
                    // attempted text instead of silently discarding it.
                    // CAS: only when the composer is still Normal — the
                    // user may have started another edit or a reply while
                    // this send was in flight, and clobbering that state
                    // would destroy their newer intent.
                    _composerMode.compareAndSet(
                        ComposerMode.Normal,
                        ComposerMode.Editing(
                            mode.targetId,
                            originalBody = text,
                            threadId = mode.threadId,
                            attempt = mode.attempt + 1,
                        ),
                    )
                }
            }
            return
        }
        val extras = when {
            mode is ComposerMode.Replying -> MessageSendExtras(
                replyToId = mode.targetId,
                replyToAuthorJid = mode.authorJid,
                replyParentBody = mode.previewBody,
                // A reply echoes the parent's thread (web parity); a
                // thread-screen reply targets the screen's thread.
                threadId = mode.threadId ?: threadId,
            )
            threadId != null -> MessageSendExtras(threadId = threadId)
            else -> null
        }
        if (mode is ComposerMode.Replying) _composerMode.value = ComposerMode.Normal
        val message = PendingMessage(
            localId = nextLocalId++,
            stanzaId = null,
            body = text,
            timestampMillis = clock(),
            failed = false,
            extras = extras,
        )
        pending.update { it + message }
        viewModelScope.launch { dispatch(message, extras) }
    }

    private suspend fun dispatch(
        message: PendingMessage,
        extras: MessageSendExtras?,
        wireBody: String = message.body,
    ) {
        val result = io.send(wireBody, extras)
        val trackedId = when (val outcome = result.outcome) {
            is WaddleSendMessageOutcome.Sent -> outcome.stanzaId
            else -> result.queuedId
        }
        if (trackedId == null) {
            updatePending(message.localId) { it.copy(failed = true) }
            return
        }
        // Delivered (or queued for replay): typing ended with content,
        // not a pause — emit `active` (web parity).
        typingNotifier.onMessageSent()
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
            val result = runCatching { up.upload(uri) }.getOrElse { UploadResult.Failed }
            when (result) {
                is UploadResult.Done -> {
                    _uploadState.value = UploadState.Idle
                    val extras = MessageSendExtras(
                        threadId = threadId,
                        sharedFiles = listOf(result.file),
                    )
                    val message = PendingMessage(
                        localId = nextLocalId++,
                        stanzaId = null,
                        body = result.file.name ?: result.file.url,
                        timestampMillis = clock(),
                        failed = false,
                        extras = extras,
                    )
                    pending.update { it + message }
                    dispatch(message, extras, wireBody = "")
                }
                UploadResult.TooLarge -> _uploadState.value = UploadState.TooLarge
                UploadResult.Failed -> _uploadState.value = UploadState.Failed
            }
        }
    }

    /** Dismiss a failed-upload banner. */
    fun clearUploadState() {
        if (_uploadState.value != UploadState.Uploading) {
            _uploadState.value = UploadState.Idle
        }
    }

    /**
     * XEP-0444 chip/sheet toggle: flip [emoji] in the account's current
     * set for the message and send the COMPLETE remaining set.
     */
    fun toggleReaction(item: TimelineItem, emoji: String) {
        val target = actionTargetIdOf(item) ?: return
        // Resolve the CURRENT reaction state straight from the store
        // (synchronously fresh — the optimistic apply writes before the
        // send suspends), not from a composition snapshot or a collector
        // cache that lags by a dispatch: two rapid toggles computing
        // from the same base would drop the first (XEP-0444 replaces
        // the full set).
        val fresh = timelineState.value.firstOrNull { it.id == item.id } ?: item
        val own = fresh.reactions.filter { it.mine }.map { it.emoji }
        val next = if (emoji in own) own - emoji else own + emoji
        viewModelScope.launch {
            runCatching { io.sendReaction(target, next, previousEmojis = own) }
        }
    }

    /** Enter reply mode targeting [item] (XEP-0461). */
    fun startReply(item: TimelineItem) {
        if (item.tombstone != null) return
        val target = actionTargetIdOf(item) ?: return
        val author = item.from?.let { if (isGroupchat) it else bareJidOf(it) } ?: return
        _composerMode.value = ComposerMode.Replying(
            targetId = target,
            authorJid = author,
            authorName = authorNameOf(item) ?: author,
            previewBody = item.body,
            threadId = item.threadId,
        )
    }

    fun cancelReply() {
        if (_composerMode.value is ComposerMode.Replying) {
            _composerMode.value = ComposerMode.Normal
        }
    }

    /**
     * The thread a "reply in thread" on [item] opens: its own thread,
     * else a new thread rooted at the message (web
     * `thread-action-target` parity). The DM root id must be
     * author-assigned for the same reason as [actionTargetIdOf].
     */
    fun threadIdFor(item: TimelineItem): String = item.threadId
        ?: if (isGroupchat) item.id else (item.originId ?: item.messageId ?: item.id)

    /** Enter edit mode for an own, non-tombstoned message. */
    fun startEdit(item: TimelineItem) {
        if (!item.isMine || item.tombstone != null) return
        val target = correctionTargetIdOf(item) ?: return
        _composerMode.value = ComposerMode.Editing(
            targetId = target,
            originalBody = item.body,
            threadId = item.threadId,
        )
    }

    fun cancelEdit() {
        _composerMode.value = ComposerMode.Normal
    }

    /** XEP-0424 retraction of an own message. */
    fun retract(item: TimelineItem) {
        if (!item.isMine || item.tombstone != null) return
        val target = actionTargetIdOf(item) ?: return
        viewModelScope.launch { runCatching { io.sendRetraction(target) } }
    }

    /** `urn:waddle:pin:0` room pin toggle. */
    fun setPinned(item: TimelineItem, pinned: Boolean) {
        val target = actionTargetIdOf(item) ?: return
        viewModelScope.launch { runCatching { io.setPinned(target, pinned) } }
    }

    /**
     * Reaction/retraction/pin target (web parity): in a MUC strictly
     * the room-assigned XEP-0359 stanza id (`by` must be the room — no
     * id means the action is unavailable); in a DM strictly the
     * AUTHOR-assigned id — the local archive stanza id was stamped by
     * our own server and the peer's copy never carried it, so a
     * reaction/retraction/reply targeting it silently fails to apply
     * on the other side.
     */
    fun actionTargetIdOf(item: TimelineItem): String? = if (isGroupchat) {
        item.stanzaId?.takeIf { item.stanzaIdBy == conversationJid }
    } else {
        item.originId ?: item.messageId
    }

    /** XEP-0308 targets the AUTHOR-assigned id of the original send. */
    private fun correctionTargetIdOf(item: TimelineItem): String? =
        item.originId ?: item.messageId

    private fun authorNameOf(item: TimelineItem): String? {
        val from = item.from ?: return null
        return if (isGroupchat) from.substringAfter('/', from) else bareJidOf(from).substringBefore('@')
    }

    private fun bareJidOf(jid: String): String = jid.substringBefore('/')

    /**
     * Loaded-history threads overview, newest activity first. Purely
     * client-side (the FFI exposes no server thread listing yet): only
     * threads whose messages are in the loaded window appear.
     */
    private fun threadSummariesOf(items: List<TimelineItem>): List<ThreadSummary> {
        val byThread = LinkedHashMap<String, MutableList<TimelineItem>>()
        val lastActivityIndex = HashMap<String, Int>()
        items.forEachIndexed { index, item ->
            val thread = item.threadId ?: return@forEachIndexed
            byThread.getOrPut(thread) { mutableListOf() }.add(item)
            lastActivityIndex[thread] = index
        }
        // Roots carry no <thread/> of their own (Waddle implicit-root):
        // resolve them by identity so previews show the root body.
        return byThread.map { (thread, members) ->
            val root = items.firstOrNull { thread in it.identityIds }
            ThreadSummary(
                threadId = thread,
                rootAuthor = (root ?: members.first()).let(::authorNameOf),
                rootPreview = (root ?: members.first()).body,
                replyCount = members.count { thread !in it.identityIds },
                lastTimestamp = members.last().timestamp,
            )
            // Newest activity first BY TIMELINE ORDER — live replies
            // carry no timestamp and a string sort would sink them.
        }.sortedByDescending { lastActivityIndex[it.threadId] ?: -1 }
    }

    /** Re-send a failed optimistic message (reply/thread extras kept). */
    fun retry(localId: Long) {
        val message = pending.value.firstOrNull { it.localId == localId && it.failed } ?: return
        pending.update { list -> list.filterNot { it.localId == localId } }
        val retried = PendingMessage(
            localId = nextLocalId++,
            stanzaId = null,
            body = message.body,
            timestampMillis = clock(),
            failed = false,
            extras = message.extras,
        )
        pending.update { it + retried }
        viewModelScope.launch {
            // Files-only sends carry an empty wire body (the row shows
            // the file name); everything else resends its body.
            val wireBody = if (message.extras?.sharedFiles.orEmpty().isNotEmpty()) "" else retried.body
            dispatch(retried, message.extras, wireBody)
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
        val was = atNewestEdge
        atNewestEdge = atEdge
        if (!was && atEdge && visible && threadId == null) {
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
