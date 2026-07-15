package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelChildren
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.merge
import kotlinx.coroutines.isActive
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.prefs.SmResumeSnapshot
import social.waddle.android.client.prefs.toFfi
import social.waddle.android.client.prefs.toSnapshot
import social.waddle.android.client.store.ChatStateStore
import social.waddle.android.client.store.DmStore
import social.waddle.android.client.store.MessageMutation
import social.waddle.android.client.store.PinStore
import social.waddle.android.client.store.ReadCursorStore
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.store.PresenceStore
import social.waddle.android.client.store.RoomStore
import social.waddle.android.client.store.TimelineStore
import social.waddle.android.client.store.UnreadStore
import social.waddle.android.client.store.isTimelineMutation
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleConfig
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleMdsDisplayedEntry
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSaslCondition
import social.waddle.client.ffi.WaddleSmResumeState
import social.waddle.client.ffi.WaddleThreadTarget
import social.waddle.client.ffi.WaddleUploadSlot

/**
 * Owns the XMPP session lifecycle: Kotlin drives reconnect and
 * persistence while Rust owns the live connection (the FFI client is
 * one-shot per attempt, Apple parity). [login] starts a supervised
 * connection loop; each attempt builds a fresh `WaddleConfig` (with the
 * persisted XEP-0198 resume snapshot), a fresh [XmppEventBridge], and a
 * fresh client from the injected [ClientFactory], then waits up to the
 * connect budget for `SessionReady`. Failed attempts back off via
 * [ReconnectPolicy]; auth-shaped errors are terminal and sign out.
 */
class XmppSessionManager(
    private val sessionPrefs: SessionPrefs,
    private val clientFactory: ClientFactory,
    private val networkSignal: NetworkSignal,
    private val userPrefs: UserPrefs,
    private val reconnectPolicy: ReconnectPolicy = ReconnectPolicy(),
    private val dispatcher: CoroutineDispatcher = Dispatchers.Default,
    private val connectTimeoutMillis: Long = CONNECT_TIMEOUT_MILLIS,
) {
    val timelineStore = TimelineStore()
    val roomStore = RoomStore()
    val presenceStore = PresenceStore()
    val dmStore = DmStore()
    val unreadStore = UnreadStore()
    val chatStateStore = ChatStateStore()
    val readCursorStore = ReadCursorStore()
    val pinStore = PinStore()

    private val outboundQueue = OutboundQueue(sessionPrefs)
    private val cursorTracker = ResumeCursorTracker()

    private val _appState = MutableStateFlow<WaddleAppState>(WaddleAppState.Loading)
    val appState: StateFlow<WaddleAppState> = _appState.asStateFlow()

    private val _connectionState = MutableStateFlow<ConnectionState>(ConnectionState.Idle)
    val connectionState: StateFlow<ConnectionState> = _connectionState.asStateFlow()

    private val _events = MutableSharedFlow<XmppEvent>(
        extraBufferCapacity = EVENT_BUFFER_CAPACITY,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )

    /** Every domain event, after store fan-out; drops oldest under burst. */
    val events: SharedFlow<XmppEvent> = _events.asSharedFlow()

    private var sessionScope: CoroutineScope? = null

    /**
     * The client of the attempt that reached `SessionReady`, while that
     * attempt is alive — the target of the UI passthroughs below.
     * Attempts never overlap (the connection loop is sequential), so a
     * plain volatile set-on-ready / clear-on-teardown is race-free.
     */
    @Volatile
    private var activeClient: WaddleClientInterface? = null

    @Volatile
    private var ownBareJid: String? = null

    /** XEP-0490 publish-options probe result, reset per attempt. */
    @Volatile
    private var mdsPublishSupported: Boolean? = null

    /** XEP-0363 upload service JID, discovered once per attempt. */
    @Volatile
    private var uploadService: String? = null

    /** Reads parked while offline, replayed on the next ready session. */
    private val pendingDisplayed =
        java.util.concurrent.ConcurrentHashMap<String, PendingDisplayed>()

    /** The live attempt's bridge, for injecting locally-produced events. */
    @Volatile
    private var currentBridge: XmppEventBridge? = null

    @Volatile
    private var resumeSnapshots: Channel<ResumeUpdate> = Channel(Channel.CONFLATED)

    /** Conflated "cursors changed" ticks; the persister coalesces bursts. */
    @Volatile
    private var cursorWrites: Channel<Unit> = Channel(Channel.CONFLATED)

    /**
     * Set on the first `SessionReady` since process start — half of the
     * fresh-stream heuristic in [consumeEvents].
     */
    @Volatile
    private var hadReadySessionThisProcess = false

    private val retryRequests = Channel<Unit>(Channel.CONFLATED)

    private val lifecycleMutex = Mutex()

    /** Serializes reaction send+rollback pairs (see [sendReaction]). */
    private val reactionMutex = Mutex()

    /** Serializes displayed dispatches (see [markConversationDisplayed]). */
    private val displayedMutex = Mutex()

    /** Persist the session and start the connection loop. */
    suspend fun login(session: WaddleSessionInfo) = lifecycleMutex.withLock {
        cancelSessionScope()
        clearStores()
        ownBareJid = bareJid(session.jid)
        persistQuietly { sessionPrefs.setOwnerBareJid(bareJid(session.jid)) }
        timelineStore.setOwnBareJid(session.jid)
        persistQuietly { sessionPrefs.setSessionId(session.sessionId) }
        persistQuietly { seedStoresFromPrefs() }

        resumeSnapshots = Channel(Channel.CONFLATED)
        cursorWrites = Channel(Channel.CONFLATED)
        val scope = CoroutineScope(SupervisorJob() + dispatcher)
        sessionScope = scope
        _appState.value = WaddleAppState.Ready
        scope.launch { persistResumeSnapshots(resumeSnapshots) }
        scope.launch { persistResumeCursors(cursorWrites) }
        scope.launch { sweepChatStates() }
        scope.launch { runConnectionLoop(session) }
    }

    /** Disconnect, cancel the loop, and wipe session persistence. */
    suspend fun logout() = lifecycleMutex.withLock {
        cancelSessionScope()
        ownBareJid = null
        clearStores()
        sessionPrefs.clear()
        _connectionState.value = ConnectionState.Idle
        _appState.value = WaddleAppState.SignedOut
    }

    private suspend fun runConnectionLoop(session: WaddleSessionInfo) {
        var attempt = 0
        while (currentCoroutineContext().isActive) {
            waitUntilOnline()
            _connectionState.value = ConnectionState.Connecting
            // The attempt touches DataStore (buildConfig reads the resume
            // snapshot/resource suffix): IOException on a corrupt or full
            // store must back off like any failed attempt — escaping this
            // handler-less root coroutine would crash-loop the process.
            val end = try {
                runAttempt(session)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                AttemptEnd.CONNECT_FAILED
            }
            when (end) {
                AttemptEnd.AUTH_FAILED -> {
                    onTerminalAuthFailure()
                    return
                }
                AttemptEnd.DROPPED_AFTER_READY -> attempt = 0
                AttemptEnd.CONNECT_FAILED -> Unit
            }
            val delayMillis = reconnectPolicy.delayMillisFor(attempt)
            if (delayMillis == null) {
                // Budget spent: park instead of abandoning the session.
                // Web parity (armOnlineRecovery/connectWithFreshBudget):
                // a genuine offline->online transition or an explicit user
                // retry restarts the loop with a fresh attempt budget.
                _connectionState.value = ConnectionState.Failed
                awaitRecoveryTrigger()
                attempt = 0
                continue
            }
            attempt += 1
            _connectionState.value = ConnectionState.Reconnecting(attempt, delayMillis)
            awaitRetryWindow(delayMillis)
        }
    }

    /**
     * One connection attempt: fresh config + bridge + client. `connect()`
     * races the event consumer so a thrown connect failure aborts the
     * attempt immediately instead of waiting out the connect budget.
     */
    private suspend fun runAttempt(session: WaddleSessionInfo): AttemptEnd {
        val bridge = XmppEventBridge(::queueResumeSnapshot)
        currentBridge = bridge
        val config = buildConfig(session)
        val client = clientFactory.create(config, bridge)
        val hadResumeSnapshot = config.resumeState != null
        try {
            return coroutineScope {
                val connector = async {
                    try {
                        client.connect()
                        null
                    } catch (cancellation: CancellationException) {
                        throw cancellation
                    } catch (failure: Throwable) {
                        failure
                    }
                }
                val consumer = async {
                    consumeEvents(bridge.events, client, session, this, hadResumeSnapshot)
                }
                val end = select<AttemptEnd?> {
                    consumer.onAwait { it }
                    connector.onAwait { failure ->
                        if (failure == null) null else AttemptEnd.CONNECT_FAILED
                    }
                } ?: consumer.await()
                coroutineContext.job.cancelChildren()
                end
            }
        } finally {
            activeClient = null
            withContext(NonCancellable) {
                runCatching { client.disconnect() }
            }
            (client as? AutoCloseable)?.close()
        }
    }

    // UI passthroughs (M1): the app module never touches the FFI client
    // directly — these forward to the live attempt's client and keep the
    // stores/prefs consistent. Each returns a "not connected" shape when
    // no session is ready instead of throwing.

    /**
     * Join a MUC room on the live connection; on success the room is
     * marked joined in [roomStore] and the joined set is persisted.
     *
     * With no live session yet (e.g. a channel tapped during the 1-3s
     * connect window — the shell is interactive before `SessionReady`)
     * the join INTENT is still persisted so [rejoinPersistedRooms]
     * fires it on the next ready session; silently dropping it left a
     * live channel that never received messages.
     */
    suspend fun joinRoom(roomJid: String, nick: String): Boolean {
        val client = activeClient
        if (client == null) {
            roomStore.markJoined(roomJid)
            persistQuietly { sessionPrefs.setJoinedRooms(roomStore.joinedRooms.value) }
            return false
        }
        try {
            client.joinRoom(roomJid, nick)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return false
        }
        roomStore.markJoined(roomJid)
        persistQuietly { sessionPrefs.setJoinedRooms(roomStore.joinedRooms.value) }
        return true
    }

    /**
     * Fetch a MAM page for a room and fan it into [timelineStore]
     * (dedupe by stanza id keeps replays collapsed). `null` when no
     * session is ready or the query failed.
     */
    suspend fun fetchRoomHistory(roomJid: String, maxMessages: UInt, beforeId: String?): WaddleMamPage? =
        fetchHistory { client -> client.fetchRoomHistory(roomJid, maxMessages, beforeId) }

    /** DM twin of [fetchRoomHistory]. */
    suspend fun fetchDmHistory(peerJid: String, maxMessages: UInt, beforeId: String?): WaddleMamPage? =
        fetchHistory { client -> client.fetchDmHistory(peerJid, maxMessages, beforeId) }

    /**
     * Send a groupchat message on the live connection; a session-shaped
     * failure persists the message to the outbound queue for replay (see
     * [sendOrEnqueue]). [extras] carry XEP-0461 reply / XEP-0201 thread
     * annotations and survive queueing.
     */
    suspend fun sendGroupchatMessage(
        roomJid: String,
        body: String,
        extras: MessageSendExtras? = null,
    ): SendResult =
        sendOrEnqueue(conversationJid = roomJid, isGroupchat = true, body = body, extras = extras)

    /** 1:1 chat twin of [sendGroupchatMessage]. */
    suspend fun sendChatMessage(
        peerJid: String,
        body: String,
        extras: MessageSendExtras? = null,
    ): SendResult =
        sendOrEnqueue(conversationJid = peerJid, isGroupchat = false, body = body, extras = extras)

    /**
     * XEP-0363: request an upload slot from the account's upload
     * service (discovered once per attempt). `null` when offline, no
     * service exists, or the service refused (e.g. size over quota).
     */
    suspend fun requestUploadSlot(
        filename: String,
        sizeBytes: ULong,
        contentType: String,
    ): WaddleUploadSlot? {
        val client = activeClient ?: return null
        val service = uploadService ?: run {
            val discovered = runCatching { client.discoverUploadService() }.getOrNull() ?: return null
            uploadService = discovered
            discovered
        }
        return try {
            client.requestUploadSlot(service, filename, sizeBytes, contentType)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }
    }

    /**
     * XEP-0444 toggle: flip [emoji] in the account's CURRENT reaction
     * set for a message and send the complete replacement set (empty =
     * clear), applied optimistically — a DM send never echoes back to
     * this client. The current set is resolved INSIDE the mutex: a
     * caller-computed set would read a stale base whenever a prior
     * send still holds the lock, and the full-set replace semantics
     * would silently erase the queued toggle.
     */
    suspend fun toggleReaction(
        conversationJid: String,
        isGroupchat: Boolean,
        targetStanzaId: String,
        emoji: String,
    ): Boolean = reactionMutex.withLock {
        // NOTE (deferred, needs an FFI signature change): the reaction
        // stanza does not yet echo the target's XEP-0201 <thread/> like
        // the web client does — send_reaction takes no options today.
        val sender = ownMutationSender(conversationJid, isGroupchat) ?: return false
        val base = ownReactionSet(conversationJid, targetStanzaId) ?: emptyList()
        val next = if (emoji in base) base - emoji else base + emoji
        applyOwnReaction(conversationJid, isGroupchat, sender, targetStanzaId, next)
        var sent = false
        try {
            sent = clientCall {
                it.sendReaction(bareJid(conversationJid), targetStanzaId, next, isGroupchat)
            }
        } finally {
            // Also runs on cancellation (screen closed mid-send): the
            // optimistic apply must never outlive a send that did not
            // happen — in a DM nothing on the wire would ever correct
            // the phantom chip.
            if (!sent) {
                applyOwnReaction(conversationJid, isGroupchat, sender, targetStanzaId, base)
            }
        }
        return sent
    }

    /** The account's current reaction set on a row, from the store. */
    private fun ownReactionSet(conversationJid: String, targetId: String): List<String>? {
        val row = timelineStore.timeline(bareJid(conversationJid)).value
            .firstOrNull { targetId in it.identityIds } ?: return null
        return row.reactions.filter { it.mine }.map { it.emoji }
    }

    /** XEP-0308: replace an own message's body; applies locally on a
     *  successful send (no DM echo). [threadId] repeats the corrected
     *  message's XEP-0201 `<thread/>` (web parity) so the edit stays in
     *  its thread. */
    suspend fun sendCorrection(
        conversationJid: String,
        isGroupchat: Boolean,
        targetId: String,
        newBody: String,
        threadId: String? = null,
    ): Boolean {
        val client = activeClient ?: return false
        val sender = ownMutationSender(conversationJid, isGroupchat) ?: return false
        val options = threadId?.let {
            sendOptionsFor(newClientStanzaId()).copy(thread = WaddleThreadTarget(id = it, parent = null))
        }
        val outcome = try {
            client.sendCorrection(bareJid(conversationJid), targetId, newBody, isGroupchat, options)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return false
        }
        if (outcome !is WaddleSendMessageOutcome.Sent) return false
        // DM only: `Sent` means stream-accepted, and a room can still
        // reject the correction — MUC state waits for the reflection
        // (web parity). A DM has no reflection to wait for.
        if (!isGroupchat) {
            timelineStore.applyLocalMutation(
                conversationJid,
                MessageMutation.Correction(targetId = targetId, from = sender, newBody = newBody),
                isGroupchat,
            )
        }
        return true
    }

    /** XEP-0424: retract an own message; tombstones locally on success. */
    suspend fun sendRetraction(
        conversationJid: String,
        isGroupchat: Boolean,
        targetStanzaId: String,
    ): Boolean {
        val sender = ownMutationSender(conversationJid, isGroupchat) ?: return false
        val sent = clientCall {
            it.sendRetraction(bareJid(conversationJid), targetStanzaId, isGroupchat)
        }
        // DM only (see sendCorrection): a room rejection after stream
        // accept would leave an irreversible local tombstone; the MUC
        // reflection drives room state instead.
        if (sent && !isGroupchat) {
            timelineStore.applyLocalMutation(
                conversationJid,
                MessageMutation.Retraction(targetId = targetStanzaId, from = sender),
                isGroupchat,
            )
        }
        return sent
    }

    /**
     * `urn:waddle:pin:0` room pin/unpin. No optimistic pin-set write —
     * the room broadcasts a `<pin-event/>` that lands in [pinStore]
     * (and a forbidden reply for non-admins surfaces via `on_error`).
     */
    suspend fun pinRoomMessage(roomJid: String, targetStanzaId: String, pin: Boolean): Boolean =
        clientCall { client ->
            if (pin) {
                client.pinMessage(bareJid(roomJid), targetStanzaId)
            } else {
                client.unpinMessage(bareJid(roomJid), targetStanzaId)
            }
        }

    /**
     * Seed [pinStore] with the room's current pin list (room open). The
     * snapshot is injected into the serialized event stream — applying
     * it here would race live pin events and clobber updates that
     * arrived while the fetch was in flight.
     */
    suspend fun refreshRoomPins(roomJid: String) {
        val client = activeClient ?: return
        val room = bareJid(roomJid)
        val fetchedAtVersion = pinStore.eventVersion(room)
        val entries = try {
            client.fetchRoomPins(room)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return
        }
        currentBridge?.submit(XmppEvent.RoomPins(room, entries, fetchedAtVersion))
    }

    private fun applyOwnReaction(
        conversationJid: String,
        isGroupchat: Boolean,
        sender: String,
        targetStanzaId: String,
        emojis: List<String>,
    ) {
        timelineStore.applyLocalMutation(
            conversationJid,
            MessageMutation.Reaction(
                targetId = targetStanzaId,
                from = sender,
                senderKey = sender,
                mine = true,
                emojis = emojis,
            ),
            isGroupchat,
        )
    }

    /**
     * The account's mutation identity in a conversation: the occupant
     * JID (room/nick) in a MUC, the bare account JID in 1:1 — matching
     * how [conversationKeyOf] classifies own incoming copies.
     */
    private fun ownMutationSender(conversationJid: String, isGroupchat: Boolean): String? {
        val own = ownBareJid ?: return null
        return if (isGroupchat) {
            "${bareJid(conversationJid)}/${own.substringBefore('@')}"
        } else {
            own
        }
    }

    /** Boolean client verb with the standard not-connected/error → false. */
    private suspend fun clientCall(op: suspend (WaddleClientInterface) -> Boolean): Boolean {
        val client = activeClient ?: return false
        return try {
            op(client)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            false
        }
    }

    /**
     * Mark the newest displayable message of a conversation as read:
     * sends the XEP-0333 `<displayed/>` (gated on the read-receipts
     * pref, and on `stanza_id_by == room` for MUCs / an explicit marker
     * request for DMs — web parity) and publishes the XEP-0490 MDS
     * cursor (independent of the pref) so sibling devices retire their
     * badges. Equality-deduped per conversation; safe to call on every
     * timeline change while the conversation is visible.
     *
     * Ordering matters: the local badge clears unconditionally (reading
     * offline is still reading), but the cursor dedupe is consumed ONLY
     * once a live client will carry the dispatch — otherwise an offline
     * read would permanently swallow the marker and the MDS publish.
     *
     * [explicitTarget] lets callers without a loaded timeline (the
     * notification mark-as-read action after process death) name the
     * message directly.
     */
    suspend fun markConversationDisplayed(
        conversationJid: String,
        isGroupchat: Boolean,
        explicitTarget: DisplayedTarget? = null,
    ): Unit = displayedMutex.withLock {
        // Serialized: unguarded concurrent dispatches (timeline
        // collector, visibility hooks, the notification receiver) could
        // interleave across the suspend points and let a STALE call's
        // terminal advance/publish land last, regressing the local
        // cursor and the published MDS node.
        val conversation = bareJid(conversationJid)
        val items = timelineStore.timeline(conversation).value
        val ids = explicitTarget ?: run {
            // Only feed-visible rows count: a thread reply the user has
            // never opened must not advance the read cursor.
            val target = items.lastOrNull {
                !it.isMine && it.tombstone == null && it.isFeedVisible
            } ?: return
            displayedTargetOf(target, isGroupchat) ?: return
        }
        // The on-screen path clears the badge unconditionally (looking
        // IS reading, even offline). Explicit targets (parked replays,
        // notification actions) must pass the staleness guard first —
        // a parked replay racing a fresh arrival would otherwise wipe
        // the badge for a message the user has never seen (the tap-time
        // clear already happened in the receiver).
        if (explicitTarget == null) unreadStore.clear(conversation)
        // Explicit targets can be stale: never move the cursor
        // BACKWARDS in timeline order, and when the target cannot be
        // ordered against an EXISTING cursor (not in the loaded
        // timeline) treat it as stale rather than risk regressing the
        // published MDS node.
        var expectedCursor: String? = null
        if (explicitTarget != null) {
            val targetIndex = items.indexOfLast { ids.markerId in it.identityIds }
            val current = readCursorStore.cursor(conversation)
            val currentIndex = current?.let { c -> items.indexOfLast { c in it.identityIds } } ?: -1
            if (targetIndex in 0..currentIndex) return
            if (targetIndex < 0 && current != null) return
            expectedCursor = current
        }
        val client = activeClient ?: run {
            // No live session: park the RESOLVED target and replay it on
            // the next ready (a notification tap during a reconnect gap
            // must not permanently drop the read receipt). Parking null
            // would re-resolve after MAM catch-up and mark messages the
            // user never saw.
            pendingDisplayed[conversation] = PendingDisplayed(isGroupchat, ids)
            return
        }
        if (readCursorStore.cursor(conversation) == ids.markerId) return
        val markerAllowed = when {
            !readReceiptsEnabled() -> false
            isGroupchat -> ids.stanzaIdBy == conversation && ids.markerId == ids.stanzaId
            else -> ids.markerRequested
        }
        // The cursor (which dedupes future dispatches) is taken only
        // when every attempted send went through — a thrown/refused
        // dispatch stays retryable on the next timeline change.
        var failed = false
        if (markerAllowed) {
            val sent = runCatching {
                client.sendDisplayed(conversation, ids.markerId, isGroupchat)
            }.getOrDefault(false)
            failed = failed || !sent
        }
        // XEP-0490 for group chats requires the MUC-ASSIGNED stanza id;
        // an occupant-injected foreign-authority stanza-id (which the
        // room only strips when it claims the room's own by-JID) must
        // not be published as a room read cursor (web parity: the
        // channel path skips both marker and publish).
        val mdsIdTrusted = !isGroupchat || ids.stanzaIdBy == conversation
        if (ids.stanzaId != null && ids.stanzaIdBy != null && mdsIdTrusted && supportsMdsPublish(client)) {
            val published = runCatching {
                client.publishMdsDisplayed(conversation, ids.stanzaId, ids.stanzaIdBy)
            }.getOrDefault(false)
            failed = failed || !published
        }
        if (!failed) {
            if (explicitTarget != null) {
                // CAS for explicit targets: a concurrent local/MDS
                // advance during our sends must not be clobbered.
                readCursorStore.compareAndAdvance(conversation, expectedCursor, ids.markerId)
            } else {
                readCursorStore.advance(conversation, ids.markerId)
            }
        }
    }

    /**
     * Replay reads parked while no session was live (see above).
     * Atomic per-key removal — a snapshot-then-clear would drop a
     * dispatch parked concurrently (notification receiver) between the
     * snapshot and the clear.
     */
    private suspend fun drainPendingDisplayed() {
        while (pendingDisplayed.isNotEmpty()) {
            // A dispatch below re-parks when the session dropped mid-
            // drain; bail instead of spinning on it.
            if (activeClient == null) return
            for (conversation in pendingDisplayed.keys) {
                val pending = pendingDisplayed.remove(conversation) ?: continue
                markConversationDisplayed(conversation, pending.isGroupchat, pending.target)
            }
        }
    }

    private suspend fun readReceiptsEnabled(): Boolean =
        runCatching { userPrefs.readReceiptsEnabled.first() }.getOrDefault(true)

    /**
     * XEP-0490 §3 publish-options probe, once per session attempt
     * (web parity); a failed probe retries on the next dispatch.
     */
    private suspend fun supportsMdsPublish(client: WaddleClientInterface): Boolean {
        mdsPublishSupported?.let { return it }
        val supported = runCatching { client.supportsMdsPublishOptions() }.getOrNull() ?: return false
        mdsPublishSupported = supported
        return supported
    }

    /**
     * The ids a displayed dispatch needs, per XEP-0333 id-class rules:
     * a MUC marker targets the ROOM-assigned XEP-0359 stanza id, but a
     * 1:1 marker must carry the AUTHOR-assigned id — the local archive
     * stanza id was stamped by our own server and the peer never saw
     * it. The stanza-id pair rides along strictly for the MDS publish.
     */
    private fun displayedTargetOf(item: TimelineItem, isGroupchat: Boolean): DisplayedTarget? {
        val markerId = if (isGroupchat) {
            item.stanzaId
        } else {
            item.originId ?: item.messageId ?: item.stanzaId
        } ?: return null
        val markerRequested = when (val source = item.source) {
            is TimelineSource.Live -> source.message.displayedMarkerRequested
            // The archive does not carry `<request/>`; archived DM rows
            // sync via MDS only.
            is TimelineSource.Archived -> false
        }
        return DisplayedTarget(
            markerId = markerId,
            stanzaId = item.stanzaId,
            stanzaIdBy = item.stanzaIdBy,
            markerRequested = markerRequested,
        )
    }

    /**
     * A displayed cursor from ANOTHER device (MDS fetch or live PEP
     * event): advance the local cursor and recompute the badge from the
     * loaded timeline. An entry whose target is not loaded is ignored —
     * the conversation's next open recomputes from scratch anyway.
     */
    private fun applyMdsEntry(entry: WaddleMdsDisplayedEntry) {
        val conversation = bareJid(entry.chatId)
        val items = timelineStore.timeline(conversation).value
        val index = items.indexOfLast { entry.stanzaId in it.identityIds }
        if (index < 0) return
        val current = readCursorStore.cursor(conversation)
        val currentIndex = current?.let { c -> items.indexOfLast { c in it.identityIds } } ?: -1
        if (currentIndex >= index) return
        // Compare-and-advance: a local displayed dispatch (UI scope) can
        // move the cursor between the read above and this write — a
        // stale sibling entry must not regress it and resurrect a badge
        // the user just cleared.
        if (!readCursorStore.compareAndAdvance(conversation, expected = current, stanzaId = entry.stanzaId)) {
            return
        }
        // The active (on-screen) conversation's badge is suppressed and
        // owned by the local read path; don't fight it from here.
        if (unreadStore.isActiveConversation(conversation)) return
        // Feed-visible rows only: hidden thread replies never count.
        val unread = items.subList(index + 1, items.size).count {
            !it.isMine && it.tombstone == null && it.isFeedVisible
        }
        unreadStore.set(conversation, unread)
        // A sibling device read everything: the stale notification in
        // the shade must retire too (the notifier owns that side).
        if (unread == 0) _events.tryEmit(XmppEvent.ReadSynced(conversation))
    }

    /**
     * XEP-0490 connect bootstrap: seed cursors from the account's MDS
     * node, then subscribe for live sibling-device updates. Fetched
     * entries are INJECTED into the event stream rather than applied
     * here — the unread recompute must serialize with live-message
     * increments on the single event consumer, or a concurrent arrival
     * could have its badge erased by a stale recompute.
     */
    private suspend fun bootstrapMdsDisplayed(client: WaddleClientInterface) {
        runCatching { client.fetchMdsDisplayed() }.getOrNull()?.let { entries ->
            if (entries.isNotEmpty()) currentBridge?.submit(XmppEvent.MdsEntries(entries))
        }
        runCatching { client.subscribeMdsDisplayed() }
    }

    /**
     * XEP-0085 typing notification: best-effort and live-session-only —
     * a stale typing state must never replay from a queue, so a
     * disconnected send is simply dropped (web parity).
     */
    suspend fun sendChatState(conversationJid: String, isGroupchat: Boolean, state: WaddleChatState): Boolean {
        val client = activeClient ?: return false
        return try {
            client.sendChatState(conversationJid, state, isGroupchat)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            false
        }
    }

    /**
     * One manager-level send: the client stanza id is generated HERE
     * (not by the FFI) so a queued replay can resend under the same
     * XEP-0359 origin-id. `NotConnected`/`TransportError` mean no live
     * session carried the message — those enqueue for replay on the
     * next `SessionReady` and hand the queue id back via
     * [SendResult.queuedId]; every other outcome passes through
     * untouched (a live session rejected the payload — replaying the
     * identical stanza cannot succeed).
     */
    private suspend fun sendOrEnqueue(
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
        extras: MessageSendExtras? = null,
    ): SendResult {
        val clientStanzaId = newClientStanzaId()
        val outcome = sendMessage(conversationJid, isGroupchat, body, clientStanzaId, extras)
        if (!isQueueableFailure(outcome)) return SendResult(outcome)
        // A logout can race this persist (reply-receiver sends run on the
        // process scope): never enqueue without an owner, and the owned
        // entry gets pruned by the next account's drain if it survives
        // the teardown window. Process-death revivals (notification
        // direct replies) have no in-memory owner yet — fall back to the
        // persisted one so the reply queues instead of being discarded;
        // logout clears that key too, keeping the teardown race safe.
        val owner = ownBareJid
            ?: runCatching { sessionPrefs.ownerBareJid.first() }.getOrNull()
            ?: return SendResult(outcome)
        val evicted = try {
            outboundQueue.enqueue(
            QueuedOutboundMessage(
                ownerBareJid = owner,
                conversationJid = conversationJid,
                isGroupchat = isGroupchat,
                body = body,
                clientStanzaId = clientStanzaId,
                enqueuedAtMillis = System.currentTimeMillis(),
                replyToId = extras?.replyToId,
                replyToAuthorJid = extras?.replyToAuthorJid,
                replyParentBody = extras?.replyParentBody,
                threadId = extras?.threadId,
                threadParent = extras?.threadParent,
                sharedFiles = extras?.sharedFiles.orEmpty(),
            ),
            )
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            // Persistence is best-effort: a failed enqueue write behaves
            // like the pre-queue behavior (the outcome already reports
            // the failure) instead of crashing the sender's scope.
            return SendResult(outcome)
        }
        evicted?.let { reportDroppedQueuedMessage(it, DROP_REASON_QUEUE_FULL) }
        return SendResult(outcome, queuedId = clientStanzaId)
    }

    private suspend fun sendMessage(
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
        stanzaId: String,
        extras: MessageSendExtras? = null,
    ): WaddleSendMessageOutcome {
        val (finalBody, options) = preparedSend(stanzaId, body, extras)
        val outcome = send { client ->
            if (isGroupchat) {
                client.sendGroupchatMessage(conversationJid, finalBody, options)
            } else {
                client.sendChatMessage(conversationJid, finalBody, options)
            }
        }
        // A DM send has no reflection: insert the local echo so peer
        // mutations (reactions, markers) can resolve their target and
        // the sender can edit/retract the fresh message (see ownDmEcho).
        if (!isGroupchat && outcome is WaddleSendMessageOutcome.Sent) {
            ownBareJid?.let { own ->
                timelineStore.onLiveMessage(
                    ownDmEcho(
                        ownJid = own,
                        peerJid = conversationJid,
                        stanzaId = stanzaId,
                        body = finalBody,
                        options = options,
                    ),
                )
            }
        }
        return outcome
    }

    private fun isQueueableFailure(outcome: WaddleSendMessageOutcome): Boolean =
        outcome == WaddleSendMessageOutcome.NotConnected ||
            outcome == WaddleSendMessageOutcome.TransportError

    /**
     * A queued message will never be delivered (cap eviction or a
     * permanent replay rejection): `DeliveryFailed` flips any optimistic
     * row that tracks the id to the retryable failed state — factual,
     * not a faked ack — and the `Error` diagnostic surfaces the drop
     * even when no conversation screen is tracking it.
     */
    private fun reportDroppedQueuedMessage(message: QueuedOutboundMessage, reason: String) {
        fanOut(XmppEvent.DeliveryFailed(message.clientStanzaId))
        fanOut(XmppEvent.Error("dropped queued message to ${message.conversationJid}: $reason"))
    }

    private fun newClientStanzaId(): String = java.util.UUID.randomUUID().toString()

    private suspend fun fetchHistory(
        fetch: suspend (WaddleClientInterface) -> WaddleMamPage,
    ): WaddleMamPage? {
        val client = activeClient ?: return null
        val page = try {
            fetch(client)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return null
        }
        // Per-message guard: one malformed archived stanza must not kill
        // the caller's paging coroutine (and crash-loop on every reopen
        // of the conversation, since the archive re-serves it).
        page.messages.forEach { message ->
            runCatching { timelineStore.onArchivedMessage(message) }
        }
        return page
    }

    private suspend fun send(
        op: suspend (WaddleClientInterface) -> WaddleSendMessageOutcome,
    ): WaddleSendMessageOutcome {
        val client = activeClient ?: return WaddleSendMessageOutcome.NotConnected
        return try {
            op(client)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            WaddleSendMessageOutcome.TransportError
        }
    }

    /**
     * Drains the bridge channel for the lifetime of the attempt. Phase 1
     * waits for `SessionReady` under the connect budget; phase 2 keeps
     * fanning events out until the stream ends.
     */
    private suspend fun consumeEvents(
        events: ReceiveChannel<XmppEvent>,
        client: WaddleClientInterface,
        session: WaddleSessionInfo,
        attemptScope: CoroutineScope,
        hadResumeSnapshot: Boolean,
    ): AttemptEnd {
        val readiness = withTimeoutOrNull(connectTimeoutMillis) { awaitReadiness(events) }
        when (readiness) {
            null, Readiness.CLOSED -> return AttemptEnd.CONNECT_FAILED
            Readiness.AUTH_FAILED -> return AttemptEnd.AUTH_FAILED
            Readiness.READY -> Unit
        }
        activeClient = client
        mdsPublishSupported = null
        uploadService = null
        // Fresh-stream heuristic: the FFI does not report whether the
        // XEP-0198 <resume/> was accepted, so treat the stream as fresh
        // when (a) no resume snapshot was presented (definitely a new
        // stream) or (b) this is the first `SessionReady` of the process
        // (a snapshot that survived process death may resume, but
        // catching up is cheap and dedupe collapses the overlap). Known
        // gap, accepted: a mid-process resume that the server REJECTS
        // looks resumed here and skips catch-up until the next fresh
        // session; the open screen still refetches via its own
        // SessionReady hook.
        val freshStream = !hadResumeSnapshot || !hadReadySessionThisProcess
        hadReadySessionThisProcess = true
        _connectionState.value = ConnectionState.Ready
        attemptScope.launch { refreshTopology(client) }
        // One sequential pipeline, deliberately not parallel: queued
        // groupchat sends need the rejoin's join presence first, and the
        // bounded catch-up must not race the replay or hammer the server.
        attemptScope.launch {
            // Best-effort pipeline: prefs reads and queue writes inside
            // can raise IOException, and an escaped throw on this root
            // coroutine would kill the process ("never throw" contract).
            persistQuietly {
                rejoinPersistedRooms(client, session)
                drainOutboundQueue()
                if (freshStream) catchUpConversations()
                // After catch-up so fetched cursors can resolve against
                // the freshly loaded newest pages.
                bootstrapMdsDisplayed(client)
                drainPendingDisplayed()
            }
        }
        // Auth classification is deliberately confined to the pre-ready
        // phase: after the session is bound, "not-authorized"/"forbidden"
        // shaped text also arrives on per-operation stanza errors, and
        // treating those as terminal would sign the user out mid-session.
        for (event in events) {
            fanOut(event)
            if (event is XmppEvent.Disconnected) return AttemptEnd.DROPPED_AFTER_READY
        }
        return AttemptEnd.DROPPED_AFTER_READY
    }

    /**
     * Re-issues MUC join presence for every persisted room on each fresh
     * session. Room join state does not survive a non-resumed stream, so
     * without this a reconnect silently stops live channel traffic. The
     * duplicate join presence on a resumed stream is benign (XEP-0045
     * treats re-joining an occupied nick from the same full JID as a
     * presence update).
     */
    private suspend fun rejoinPersistedRooms(
        client: WaddleClientInterface,
        session: WaddleSessionInfo,
    ) {
        for (roomJid in sessionPrefs.joinedRooms.first()) {
            try {
                client.joinRoom(roomJid, session.xmppLocalpart)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                // Keep going: one unjoinable room must not block the rest.
            }
        }
    }

    /**
     * Replays the persisted outbound queue through the live attempt's
     * client. Runs unconditionally on every `SessionReady` (a resumed
     * stream replays 0198-unacked stanzas itself, but the persisted
     * queue only ever holds messages NO stream accepted, so replaying
     * them here can never duplicate a resume replay).
     */
    private suspend fun drainOutboundQueue() {
        val owner = ownBareJid ?: return
        outboundQueue.drain(
            ownerBareJid = owner,
            send = { queued ->
                sendMessage(
                    conversationJid = queued.conversationJid,
                    isGroupchat = queued.isGroupchat,
                    body = queued.body,
                    stanzaId = queued.clientStanzaId,
                    extras = queued.sendExtras(),
                )
            },
            onDropped = { queued, outcome ->
                reportDroppedQueuedMessage(queued, outcome::class.simpleName ?: DROP_REASON_UNKNOWN)
            },
        )
    }

    /**
     * Fresh-stream MAM catch-up (web reconnect-catchup parity): fetch
     * the newest archive page for every joined room plus the most
     * recently active DMs, so conversations that are NOT on screen also
     * recover messages missed while the stream was down. The FFI only
     * pages with `before_id`, so instead of the web's `after`-cursor
     * query this fetches the newest page and lets the timeline store's
     * identity dedupe collapse the overlap — the same shape as the
     * per-screen refetch. Sequential and bounded (one page each, DMs
     * capped) to avoid hammering the server after every reconnect.
     */
    private suspend fun catchUpConversations() {
        val rooms = roomStore.joinedRooms.value
        for (roomJid in rooms) {
            fetchRoomHistory(roomJid, CATCHUP_PAGE_SIZE, beforeId = null)
        }
        for (peerJid in cursorTracker.newestFirst(excluding = rooms, limit = CATCHUP_DM_LIMIT)) {
            fetchDmHistory(peerJid, CATCHUP_PAGE_SIZE, beforeId = null)
        }
    }

    private suspend fun awaitReadiness(events: ReceiveChannel<XmppEvent>): Readiness {
        for (event in events) {
            fanOut(event)
            when (event) {
                is XmppEvent.SessionReady -> return Readiness.READY
                // Typed SASL failure from the FFI: the ONLY terminal auth
                // signal, and only for credential-shaped conditions — RFC
                // 6120 §6.5 temporary-auth-failure (and mechanism/encoding
                // conditions) must retry, not wipe the session (web #1164).
                is XmppEvent.AuthenticationFailed ->
                    return if (isTerminalSaslCondition(event.condition)) {
                        Readiness.AUTH_FAILED
                    } else {
                        Readiness.CLOSED
                    }
                is XmppEvent.Disconnected -> return Readiness.CLOSED
                else -> Unit
            }
        }
        return Readiness.CLOSED
    }

    /** Single fan-out point: domain stores first, then the shared stream. */
    private fun fanOut(event: XmppEvent) {
        when (event) {
            is XmppEvent.Message -> {
                // A live MDS PEP event is pure read-state metadata from a
                // sibling device — apply the cursors and skip every chat
                // consumer.
                event.message.mdsDisplayed?.let { entries ->
                    entries.forEach(::applyMdsEntry)
                    _events.tryEmit(event)
                    return
                }
                // Pin/unpin room broadcasts mutate the pin set, not the
                // timeline.
                event.message.pinEvent?.let { pin ->
                    event.message.from?.let { from -> pinStore.onPinEvent(bareJid(from), pin) }
                    _events.tryEmit(event)
                    return
                }
                // Mutation stanzas (reactions/corrections/retractions/
                // moderation) alter existing rows via the timeline store;
                // they are not new DM activity and must not reorder or
                // re-persist recency.
                val isMutation = event.message.isTimelineMutation()
                // Bodyless protocol stanzas (chat states, displayed
                // markers) are not DM activity either — a typing burst
                // must not reorder or create DM-list entries.
                val hasContent = event.message.body != null
                if (!isMutation && hasContent) persistDmRecency(event)
                val newlyInserted = timelineStore.onLiveMessage(event.message)
                if (!isMutation && hasContent) dmStore.onChatMessage(ownBareJid, event.message)
                val message = event.message
                conversationKeyOf(
                    ownBareJid = ownBareJid,
                    ownNick = ownBareJid?.substringBefore('@'),
                    from = message.from,
                    to = message.to,
                    isGroupchat = message.isMuc || message.messageType == "groupchat",
                )?.let { key ->
                    trackChatState(key, message)
                    if (message.body != null) {
                        // Replays the timeline deduped (XEP-0198 resume)
                        // must not inflate the badge for a message that
                        // renders once. Thread REPLIES are hidden from
                        // the feed and never count either (web
                        // feed-only unread parity).
                        val isThreadReply = message.thread != null &&
                            message.thread !in setOfNotNull(message.stanzaId, message.originId, message.id) &&
                            message.callThread == null
                        if (newlyInserted && !isThreadReply) {
                            unreadStore.onLiveMessage(key.jid, key.isMine)
                        }
                        recordResumeCursor(
                            conversationJid = key.jid,
                            stanzaId = message.stanzaId ?: message.originId ?: message.id,
                            timestamp = message.timestamp,
                        )
                    }
                }
            }
            is XmppEvent.MdsEntries -> event.entries.forEach(::applyMdsEntry)
            is XmppEvent.RoomPins ->
                pinStore.seed(event.roomJid, event.entries, event.fetchedAtVersion)
            is XmppEvent.Presence -> presenceStore.onPresence(event.presence)
            is XmppEvent.MamResult -> {
                timelineStore.onArchivedMessage(event.message)
                val message = event.message
                if (message.body != null) {
                    conversationKeyOf(
                        ownBareJid = ownBareJid,
                        ownNick = ownBareJid?.substringBefore('@'),
                        from = message.from,
                        to = message.to,
                        isGroupchat = message.messageType == "groupchat",
                    )?.let { key ->
                        recordResumeCursor(
                            conversationJid = key.jid,
                            stanzaId = message.stanzaId ?: message.originId ?: message.id ?: message.mamId,
                            timestamp = message.timestamp,
                        )
                    }
                }
            }
            else -> Unit
        }
        _events.tryEmit(event)
    }

    /**
     * XEP-0085 bookkeeping from the fan-out: a `composing` state adds
     * the sender to the conversation's typing set, anything else — a
     * different state or a real message — removes them. Sender display
     * name: the occupant nick in a MUC, the peer's LOCALPART in 1:1 —
     * DM states arrive from the peer's full JID, whose resource is a
     * device identifier, never a name.
     */
    private fun trackChatState(key: ConversationKey, message: WaddleMessage) {
        val from = message.from ?: return
        val isGroupchat = message.isMuc || message.messageType == "groupchat"
        val sender = if (isGroupchat) {
            resourcepart(from) ?: bareJid(from).substringBefore('@')
        } else {
            bareJid(from).substringBefore('@')
        }
        val state = message.chatState
        if (state != null) {
            chatStateStore.onChatState(key.jid, sender, state, key.isMine)
        }
        if (message.body != null && !key.isMine) {
            chatStateStore.onLiveMessage(key.jid, sender)
        }
    }

    /**
     * 1s expiry ticker for incoming typing indicators — armed ONLY while
     * someone is composing. An unconditional periodic timer would keep a
     * task perpetually pending on virtual-time test schedulers (and wake
     * the process forever); this one parks on the composing flow when
     * idle and dies with the last composer.
     */
    private suspend fun sweepChatStates() {
        chatStateStore.composing
            .map { it.isNotEmpty() }
            .distinctUntilChanged()
            .collectLatest { anyComposing ->
                if (!anyComposing) return@collectLatest
                // Loop exit is driven ONLY by collectLatest observing the
                // empty state (sweep publishes it, StateFlow guarantees
                // delivery of the latest value) — breaking on sweep()'s
                // return raced a conflated empty→composing flip and left
                // a live composer with no ticker.
                while (true) {
                    delay(CHAT_STATE_SWEEP_MILLIS)
                    chatStateStore.sweep()
                }
            }
    }

    /**
     * Advance-only cursor bookkeeping from the fan-out (never blocks):
     * a moved cursor pokes the conflated write channel and the session-
     * scoped persister flushes the snapshot — bursts coalesce into one
     * DataStore write.
     */
    private fun recordResumeCursor(conversationJid: String, stanzaId: String?, timestamp: String?) {
        stanzaId ?: return
        if (cursorTracker.advance(conversationJid, stanzaId, timestamp ?: nowRfc3339())) {
            cursorWrites.trySend(Unit)
        }
    }

    private suspend fun persistResumeCursors(writes: ReceiveChannel<Unit>) {
        for (write in writes) {
            persistQuietly { sessionPrefs.setResumeCursors(cursorTracker.snapshot()) }
        }
    }

    private suspend fun refreshTopology(client: WaddleClientInterface) {
        runCatching { roomStore.setTopology(client.discoverTopology()) }
    }

    private suspend fun buildConfig(session: WaddleSessionInfo): WaddleConfig = WaddleConfig(
        serverUrl = session.xmppWebsocketUrl,
        jid = session.jid,
        accessToken = session.sessionId,
        resource = RESOURCE_PREFIX + sessionPrefs.resourceSuffix(),
        resumeState = sessionPrefs.smResume.first()?.toFfi(),
    )

    /** Called from Rust threads via the bridge: never blocks. */
    private fun queueResumeSnapshot(state: WaddleSmResumeState?) {
        resumeSnapshots.trySend(ResumeUpdate(state?.toSnapshot()))
    }

    private suspend fun persistResumeSnapshots(updates: ReceiveChannel<ResumeUpdate>) {
        for (update in updates) {
            persistQuietly { sessionPrefs.setSmResume(update.snapshot) }
        }
    }

    /** Park in `Offline` until connectivity exists. */
    private suspend fun waitUntilOnline() {
        if (networkSignal.online.first()) return
        _connectionState.value = ConnectionState.Offline
        networkSignal.online.first { it }
    }

    /**
     * Wait out the backoff delay — unless the network drops, in which
     * case park in `Offline` and retry immediately once it returns
     * (bypassing the remaining timer, web `navigator.onLine` parity).
     */
    private suspend fun awaitRetryWindow(delayMillis: Long) {
        val wentOffline = withTimeoutOrNull(delayMillis) {
            networkSignal.online.first { online -> !online }
        } != null
        if (wentOffline) {
            _connectionState.value = ConnectionState.Offline
            networkSignal.online.first { it }
        }
    }

    /** Manual retry from the Failed banner: fresh budget immediately. */
    fun requestReconnect() {
        retryRequests.trySend(Unit)
    }

    /**
     * Parks the exhausted loop until either a real offline->online edge
     * (`drop(1)` skips the replayed current value of the StateFlow-shaped
     * signal) or an explicit retry request. Covers both failure shapes:
     * connectivity loss recovers on the edge, server-side outages recover
     * via user retry (the device never went offline).
     */
    private suspend fun awaitRecoveryTrigger() {
        merge(
            retryRequests.receiveAsFlow(),
            networkSignal.online.drop(1).filter { it }.map { },
        ).first()
    }

    /**
     * Credential-shaped conditions invalidate the stored token; every
     * other condition (temporary-auth-failure, mechanism/encoding
     * mismatches, Unknown) is treated as a failed attempt and retried —
     * the backoff budget parks the loop if it persists.
     */
    private fun isTerminalSaslCondition(condition: WaddleSaslCondition): Boolean =
        when (condition) {
            WaddleSaslCondition.NOT_AUTHORIZED,
            WaddleSaslCondition.ACCOUNT_DISABLED,
            WaddleSaslCondition.CREDENTIALS_EXPIRED,
            -> true
            else -> false
        }

    private suspend fun onTerminalAuthFailure() {
        _connectionState.value = ConnectionState.AuthFailed
        _appState.value = WaddleAppState.SignedOut
        persistQuietly { sessionPrefs.clear() }
        // Last statement on purpose: cancelling the session scope kills
        // this coroutine too, but also the parked snapshot persister that
        // would otherwise leak until the next login.
        sessionScope?.cancel()
        sessionScope = null
    }

    /** UI hook: the DM conversation is on screen — persist recency. */
    fun recordDmSeen(peerJid: String) {
        val scope = sessionScope ?: return
        scope.launch {
            persistQuietly { sessionPrefs.setLastSeen(bareJid(peerJid), nowRfc3339()) }
        }
    }

    /**
     * Persist DM-list recency for inbound 1:1 traffic; without a write
     * the `lastSeen`-seeded DM list is empty after every restart.
     */
    private fun persistDmRecency(event: XmppEvent.Message) {
        val message = event.message
        if (message.isMuc || message.messageType != "chat") return
        val from = message.from ?: return
        val peer = bareJid(from)
        if (peer == ownBareJid) return
        val scope = sessionScope ?: return
        scope.launch {
            persistQuietly { sessionPrefs.setLastSeen(peer, message.timestamp ?: nowRfc3339()) }
        }
    }

    private fun nowRfc3339(): String = java.time.OffsetDateTime.now().toString()

    private fun parseInstantOrNull(value: String): java.time.Instant? =
        runCatching { java.time.Instant.parse(value) }.getOrNull()
            ?: runCatching { java.time.OffsetDateTime.parse(value).toInstant() }.getOrNull()

    /**
     * Passthroughs document a never-throw contract, but DataStore writes
     * can raise IOException (disk-full, corruption). Persistence best-
     * effort here: losing a prefs write degrades a convenience (queue,
     * recency), while an escaped throw would crash the caller's scope.
     */
    private suspend fun persistQuietly(write: suspend () -> Unit) {
        try {
            write()
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
        }
    }

    private suspend fun seedStoresFromPrefs() {
        val joinedRooms = sessionPrefs.joinedRooms.first()
        roomStore.replaceJoinedRooms(joinedRooms)
        cursorTracker.seed(sessionPrefs.resumeCursors.first())
        dmStore.seed(
            sessionPrefs.lastSeen.first()
                .filterKeys { it !in joinedRooms }
                .entries
                // Parsed comparison: the markers mix server offsets
                // (+00:00) with local ones (+02:00), and a raw string
                // sort would misorder them across timezones.
                .sortedBy { entry -> parseInstantOrNull(entry.value) }
                .map { it.key },
        )
    }

    private suspend fun cancelSessionScope() {
        val scope = sessionScope ?: return
        sessionScope = null
        scope.coroutineContext.job.let { job ->
            job.cancel()
            job.join()
        }
    }

    private fun clearStores() {
        timelineStore.clear()
        timelineStore.setOwnBareJid(null)
        roomStore.clear()
        presenceStore.clear()
        dmStore.clear()
        unreadStore.clearAll()
        chatStateStore.clear()
        readCursorStore.clear()
        pinStore.clear()
        pendingDisplayed.clear()
        cursorTracker.clear()
    }

    private enum class AttemptEnd { AUTH_FAILED, CONNECT_FAILED, DROPPED_AFTER_READY }

    private enum class Readiness { READY, AUTH_FAILED, CLOSED }

    /** Wrapper so a conflated channel can carry a `null` (= clear) update. */
    private data class ResumeUpdate(val snapshot: SmResumeSnapshot?)

    /**
     * A displayed dispatch target: [markerId] is what the XEP-0333
     * marker carries (author-assigned in 1:1, room stanza id in MUCs);
     * the stanza-id pair feeds only the XEP-0490 MDS publish.
     */
    data class DisplayedTarget(
        val markerId: String,
        val stanzaId: String?,
        val stanzaIdBy: String?,
        val markerRequested: Boolean,
    )

    /** A displayed dispatch waiting for a live session. */
    private data class PendingDisplayed(
        val isGroupchat: Boolean,
        val target: DisplayedTarget,
    )

    companion object {
        /** Web parity: 15s budget from connect to `SessionReady`. */
        const val CONNECT_TIMEOUT_MILLIS = 15_000L

        /** Newest page per conversation on fresh-stream catch-up. */
        const val CATCHUP_PAGE_SIZE = 50u

        /** Incoming-typing expiry tick (XEP-0085 indicator sweep). */
        const val CHAT_STATE_SWEEP_MILLIS = 1_000L

        /** Only the most recently active DMs catch up (rooms: all joined). */
        const val CATCHUP_DM_LIMIT = 3

        private const val RESOURCE_PREFIX = "waddle-android-"
        private const val EVENT_BUFFER_CAPACITY = 256
        private const val DROP_REASON_QUEUE_FULL = "outbound queue full, oldest evicted"
        private const val DROP_REASON_UNKNOWN = "rejected"
    }
}
