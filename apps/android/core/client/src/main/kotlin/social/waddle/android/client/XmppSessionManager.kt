package social.waddle.android.client

import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.calls.CallStore
import social.waddle.android.client.calls.ClientCallSignaling
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ConnectionLoop
import social.waddle.android.client.session.ResumePersistence
import social.waddle.android.client.session.SessionCatchup
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleNotifyMode
import social.waddle.client.ffi.WaddleUploadSlot

/**
 * Owns the XMPP session lifecycle: Kotlin drives reconnect and
 * persistence while Rust owns the live connection (the FFI client is
 * one-shot per attempt, Apple parity). [login] starts the supervised
 * [ConnectionLoop]; auth-shaped errors are terminal and sign out.
 *
 * This is a facade: the stores live in [SessionStores], the reconnect
 * loop in [ConnectionLoop], the ready pipeline in [SessionCatchup],
 * event fan-out in [XmppEventRouter], read state in
 * [ReadStateCoordinator], sends in [OutboundMessenger], and the
 * remaining UI passthroughs in [ConversationVerbs] — all sharing the
 * per-attempt [ActiveSession].
 */
class XmppSessionManager(
    private val sessionPrefs: SessionPrefs,
    clientFactory: ClientFactory,
    networkSignal: NetworkSignal,
    userPrefs: UserPrefs,
    reconnectPolicy: ReconnectPolicy = ReconnectPolicy(),
    private val dispatcher: CoroutineDispatcher = Dispatchers.Default,
    connectTimeoutMillis: Long = CONNECT_TIMEOUT_MILLIS,
) {
    private val stores = SessionStores()

    val timelineStore = stores.timelineStore
    val roomStore = stores.roomStore
    val presenceStore = stores.presenceStore
    val dmStore = stores.dmStore
    val unreadStore = stores.unreadStore
    val chatStateStore = stores.chatStateStore
    val readCursorStore = stores.readCursorStore
    val pinStore = stores.pinStore
    val notifySettingsStore = stores.notifySettingsStore

    private val _appState = MutableStateFlow<WaddleAppState>(WaddleAppState.Loading)
    val appState: StateFlow<WaddleAppState> = _appState.asStateFlow()

    private var sessionScope: CoroutineScope? = null

    private val lifecycleMutex = Mutex()

    private val resume = ResumePersistence(sessionPrefs)

    private val activeSession = ActiveSession(resume::queueResumeSnapshot)

    private val readState: ReadStateCoordinator =
        ReadStateCoordinator(activeSession, stores, userPrefs) { event ->
            router.emit(event)
        }

    /**
     * Single-slot DM call engine (reducer + XEP-0353/0166 side
     * effects), fed from the router's serialized dispatch path.
     */
    val callStore: CallStore = CallStore(
        signaling = ClientCallSignaling(activeSession),
        ownBareJid = { activeSession.ownBareJid },
        ownFullJid = { activeSession.ownFullJid },
    )

    private val router: XmppEventRouter =
        XmppEventRouter(activeSession, stores, resume, readState, callStore) { peer, timestamp ->
            persistDmSeen(peer, timestamp)
        }

    private val messenger = OutboundMessenger(activeSession, stores, sessionPrefs, router::dispatch)

    private val verbs = ConversationVerbs(activeSession, stores, sessionPrefs)

    private val catchup = SessionCatchup(sessionPrefs, stores, resume, verbs, messenger, readState)

    private val loop = ConnectionLoop(
        clientFactory = clientFactory,
        networkSignal = networkSignal,
        sessionPrefs = sessionPrefs,
        activeSession = activeSession,
        router = router,
        onReady = ::onSessionReady,
        onTerminalAuthFailure = ::onTerminalAuthFailure,
        reconnectPolicy = reconnectPolicy,
        connectTimeoutMillis = connectTimeoutMillis,
    )

    val connectionState: StateFlow<ConnectionState> = loop.state

    /** Every domain event, after store fan-out; drops oldest under burst. */
    val events: SharedFlow<XmppEvent> = router.events

    /** Persist the session and start the connection loop. */
    suspend fun login(session: WaddleSessionInfo) = lifecycleMutex.withLock {
        cancelSessionScope()
        clearSessionState()
        activeSession.ownBareJid = bareJid(session.jid)
        persistQuietly { sessionPrefs.setOwnerBareJid(bareJid(session.jid)) }
        timelineStore.setOwnBareJid(session.jid)
        persistQuietly { sessionPrefs.setSessionId(session.sessionId) }
        persistQuietly { seedStoresFromPrefs() }

        val scope = CoroutineScope(SupervisorJob() + dispatcher)
        sessionScope = scope
        _appState.value = WaddleAppState.Ready
        resume.start(scope)
        callStore.start(scope)
        scope.launch { router.sweepChatStates() }
        scope.launch { loop.run(session) }
    }

    /** Disconnect, cancel the loop, and wipe session persistence. */
    suspend fun logout() = lifecycleMutex.withLock {
        cancelSessionScope()
        activeSession.ownBareJid = null
        activeSession.ownFullJid = null
        clearSessionState()
        sessionPrefs.clear()
        loop.resetToIdle()
        _appState.value = WaddleAppState.SignedOut
    }

    // UI passthroughs (M1): the app module never touches the FFI client
    // directly — [ConversationVerbs], [OutboundMessenger], and
    // [ReadStateCoordinator] forward to the live attempt's client and
    // keep the stores/prefs consistent, each returning a "not
    // connected" shape when no session is ready instead of throwing.

    /** Join a MUC room; with no live session the intent still persists. */
    suspend fun joinRoom(roomJid: String, nick: String): VerbResult = verbs.joinRoom(roomJid, nick)

    /** Fetch a MAM page for a room and fan it into [timelineStore]. */
    suspend fun fetchRoomHistory(roomJid: String, maxMessages: UInt, beforeId: String?): WaddleMamPage? =
        verbs.fetchRoomHistory(roomJid, maxMessages, beforeId)

    /** DM twin of [fetchRoomHistory]. */
    suspend fun fetchDmHistory(peerJid: String, maxMessages: UInt, beforeId: String?): WaddleMamPage? =
        verbs.fetchDmHistory(peerJid, maxMessages, beforeId)

    /** MAM full-text room search; ephemeral results, no store fan-out. */
    suspend fun searchRoomHistory(roomJid: String, query: String, maxResults: UInt): WaddleMamPage? =
        verbs.searchRoomHistory(roomJid, query, maxResults)

    /** DM twin of [searchRoomHistory]. */
    suspend fun searchDmHistory(peerJid: String, query: String, maxResults: UInt): WaddleMamPage? =
        verbs.searchDmHistory(peerJid, query, maxResults)

    /**
     * Send a groupchat message on the live connection; a session-shaped
     * failure persists the message to the outbound queue for replay (see
     * [OutboundMessenger.sendOrEnqueue]). [extras] carry XEP-0461 reply /
     * XEP-0201 thread annotations and survive queueing.
     */
    suspend fun sendGroupchatMessage(
        roomJid: String,
        body: String,
        extras: MessageSendExtras? = null,
    ): SendResult =
        messenger.sendOrEnqueue(conversationJid = roomJid, isGroupchat = true, body = body, extras = extras)

    /** 1:1 chat twin of [sendGroupchatMessage]. */
    suspend fun sendChatMessage(
        peerJid: String,
        body: String,
        extras: MessageSendExtras? = null,
    ): SendResult =
        messenger.sendOrEnqueue(conversationJid = peerJid, isGroupchat = false, body = body, extras = extras)

    /** XEP-0363: request an upload slot from the account's upload service. */
    suspend fun requestUploadSlot(
        filename: String,
        sizeBytes: ULong,
        contentType: String,
    ): WaddleUploadSlot? = verbs.requestUploadSlot(filename, sizeBytes, contentType)

    /** XEP-0444 toggle: flip [emoji] in the account's current reaction set. */
    suspend fun toggleReaction(
        conversationJid: String,
        isGroupchat: Boolean,
        targetStanzaId: String,
        emoji: String,
    ): VerbResult = verbs.toggleReaction(conversationJid, isGroupchat, targetStanzaId, emoji)

    /** XEP-0308: replace an own message's body ([ConversationVerbs.sendCorrection]). */
    suspend fun sendCorrection(
        conversationJid: String,
        isGroupchat: Boolean,
        targetId: String,
        newBody: String,
        threadId: String? = null,
    ): VerbResult = verbs.sendCorrection(conversationJid, isGroupchat, targetId, newBody, threadId)

    /** XEP-0424: retract an own message; tombstones locally on success. */
    suspend fun sendRetraction(
        conversationJid: String,
        isGroupchat: Boolean,
        targetStanzaId: String,
    ): VerbResult = verbs.sendRetraction(conversationJid, isGroupchat, targetStanzaId)

    /** `urn:waddle:pin:0` room pin/unpin (no optimistic pin-set write). */
    suspend fun pinRoomMessage(roomJid: String, targetStanzaId: String, pin: Boolean): VerbResult =
        verbs.pinRoomMessage(roomJid, targetStanzaId, pin)

    /** Seed [pinStore] with the room's current pin list (room open). */
    suspend fun refreshRoomPins(roomJid: String) = verbs.refreshRoomPins(roomJid)

    /**
     * Mark the newest displayable message of a conversation as read:
     * XEP-0333 `<displayed/>` plus the XEP-0490 MDS publish (see
     * [ReadStateCoordinator.markConversationDisplayed]). [explicitTarget]
     * lets callers without a loaded timeline (the notification
     * mark-as-read action after process death) name the message
     * directly.
     */
    suspend fun markConversationDisplayed(
        conversationJid: String,
        isGroupchat: Boolean,
        explicitTarget: DisplayedTarget? = null,
    ) = readState.markConversationDisplayed(conversationJid, isGroupchat, explicitTarget)

    /** XEP-0085 typing notification: best-effort and live-session-only. */
    suspend fun sendChatState(conversationJid: String, isGroupchat: Boolean, state: WaddleChatState): VerbResult =
        verbs.sendChatState(conversationJid, isGroupchat, state)

    /** XEP-0492: set a room's notification mode ([ConversationVerbs.setRoomNotificationMode]). */
    suspend fun setRoomNotificationMode(
        roomJid: String,
        mode: WaddleNotifyMode,
        name: String? = null,
    ): NotifySettingsResult = verbs.setRoomNotificationMode(roomJid, mode, name)

    /** XEP-0492 DM twin of [setRoomNotificationMode]. */
    suspend fun setDmNotificationMode(peerJid: String, mode: WaddleNotifyMode): NotifySettingsResult =
        verbs.setDmNotificationMode(peerJid, mode)

    /** Manual retry from the Failed banner: fresh budget immediately. */
    fun requestReconnect() {
        loop.requestReconnect()
    }

    /** UI hook: the DM conversation is on screen — persist recency. */
    fun recordDmSeen(peerJid: String) {
        persistDmSeen(bareJid(peerJid), nowRfc3339())
    }

    /** `SessionReady` hook for [ConnectionLoop]: launch the ready work. */
    private fun onSessionReady(
        attemptScope: CoroutineScope,
        client: WaddleClientInterface,
        session: WaddleSessionInfo,
        freshStream: Boolean,
    ) {
        attemptScope.launch { catchup.refreshTopology(client) }
        attemptScope.launch { catchup.onSessionReady(client, session, freshStream) }
    }

    private suspend fun onTerminalAuthFailure() {
        _appState.value = WaddleAppState.SignedOut
        persistQuietly { sessionPrefs.clear() }
        // Last statement on purpose: cancelling the session scope kills
        // this coroutine too, but also the parked snapshot persister that
        // would otherwise leak until the next login.
        sessionScope?.cancel()
        sessionScope = null
    }

    /** Persist DM-list recency (UI hook and router callback). */
    private fun persistDmSeen(peer: String, timestamp: String) {
        val scope = sessionScope ?: return
        scope.launch {
            persistQuietly { sessionPrefs.setLastSeen(peer, timestamp) }
        }
    }

    private suspend fun seedStoresFromPrefs() {
        stores.seedFromPrefs(sessionPrefs)
        resume.seedFromPrefs()
    }

    private suspend fun cancelSessionScope() {
        val scope = sessionScope ?: return
        sessionScope = null
        scope.coroutineContext.job.let { job ->
            job.cancel()
            job.join()
        }
    }

    private fun clearSessionState() {
        stores.clear()
        readState.clearPending()
        resume.clear()
        callStore.clear()
    }

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

    companion object {
        /** Web parity: 15s budget from connect to `SessionReady`. */
        const val CONNECT_TIMEOUT_MILLIS = ConnectionLoop.CONNECT_TIMEOUT_MILLIS

        /** Newest page per conversation on fresh-stream catch-up. */
        const val CATCHUP_PAGE_SIZE = SessionCatchup.CATCHUP_PAGE_SIZE

        /** Incoming-typing expiry tick (XEP-0085 indicator sweep). */
        const val CHAT_STATE_SWEEP_MILLIS = XmppEventRouter.CHAT_STATE_SWEEP_MILLIS

        /** Only the most recently active DMs catch up (rooms: all joined). */
        const val CATCHUP_DM_LIMIT = SessionCatchup.CATCHUP_DM_LIMIT
    }
}
