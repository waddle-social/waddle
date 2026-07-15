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
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.isActive
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.SmResumeSnapshot
import social.waddle.android.client.prefs.toFfi
import social.waddle.android.client.prefs.toSnapshot
import social.waddle.android.client.store.DmStore
import social.waddle.android.client.store.PresenceStore
import social.waddle.android.client.store.RoomStore
import social.waddle.android.client.store.TimelineStore
import social.waddle.android.client.store.UnreadStore
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleConfig
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSmResumeState

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
    private val reconnectPolicy: ReconnectPolicy = ReconnectPolicy(),
    private val dispatcher: CoroutineDispatcher = Dispatchers.Default,
    private val connectTimeoutMillis: Long = CONNECT_TIMEOUT_MILLIS,
) {
    val timelineStore = TimelineStore()
    val roomStore = RoomStore()
    val presenceStore = PresenceStore()
    val dmStore = DmStore()
    val unreadStore = UnreadStore()

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

    @Volatile
    private var resumeSnapshots: Channel<ResumeUpdate> = Channel(Channel.CONFLATED)

    /** Persist the session and start the connection loop. */
    suspend fun login(session: WaddleSessionInfo) {
        cancelSessionScope()
        clearStores()
        ownBareJid = bareJid(session.jid)
        timelineStore.setOwnBareJid(session.jid)
        sessionPrefs.setSessionId(session.sessionId)
        seedStoresFromPrefs()

        resumeSnapshots = Channel(Channel.CONFLATED)
        val scope = CoroutineScope(SupervisorJob() + dispatcher)
        sessionScope = scope
        _appState.value = WaddleAppState.Ready
        scope.launch { persistResumeSnapshots(resumeSnapshots) }
        scope.launch { runConnectionLoop(session) }
    }

    /** Disconnect, cancel the loop, and wipe session persistence. */
    suspend fun logout() {
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
            when (runAttempt(session)) {
                AttemptEnd.AUTH_FAILED -> {
                    onTerminalAuthFailure()
                    return
                }
                AttemptEnd.DROPPED_AFTER_READY -> attempt = 0
                AttemptEnd.CONNECT_FAILED -> Unit
            }
            val delayMillis = reconnectPolicy.delayMillisFor(attempt)
            if (delayMillis == null) {
                _connectionState.value = ConnectionState.Failed
                return
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
        val client = clientFactory.create(buildConfig(session), bridge)
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
                val consumer = async { consumeEvents(bridge.events, client, session, this) }
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
     */
    suspend fun joinRoom(roomJid: String, nick: String): Boolean {
        val client = activeClient ?: return false
        try {
            client.joinRoom(roomJid, nick)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return false
        }
        roomStore.markJoined(roomJid)
        sessionPrefs.setJoinedRooms(roomStore.joinedRooms.value)
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

    /** Send a groupchat message on the live connection. */
    suspend fun sendGroupchatMessage(roomJid: String, body: String): WaddleSendMessageOutcome =
        send { client -> client.sendGroupchatMessage(roomJid, body, null) }

    /** Send a 1:1 chat message on the live connection. */
    suspend fun sendChatMessage(peerJid: String, body: String): WaddleSendMessageOutcome =
        send { client -> client.sendChatMessage(peerJid, body, null) }

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
        page.messages.forEach(timelineStore::onArchivedMessage)
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
    ): AttemptEnd {
        val readiness = withTimeoutOrNull(connectTimeoutMillis) { awaitReadiness(events) }
        when (readiness) {
            null, Readiness.CLOSED -> return AttemptEnd.CONNECT_FAILED
            Readiness.AUTH_FAILED -> return AttemptEnd.AUTH_FAILED
            Readiness.READY -> Unit
        }
        activeClient = client
        _connectionState.value = ConnectionState.Ready
        attemptScope.launch { refreshTopology(client) }
        attemptScope.launch { rejoinPersistedRooms(client, session) }
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

    private suspend fun awaitReadiness(events: ReceiveChannel<XmppEvent>): Readiness {
        for (event in events) {
            fanOut(event)
            when {
                event is XmppEvent.SessionReady -> return Readiness.READY
                event is XmppEvent.Error && isAuthShapedError(event.description) ->
                    return Readiness.AUTH_FAILED
                event is XmppEvent.Disconnected -> return Readiness.CLOSED
            }
        }
        return Readiness.CLOSED
    }

    /** Single fan-out point: domain stores first, then the shared stream. */
    private fun fanOut(event: XmppEvent) {
        when (event) {
            is XmppEvent.Message -> {
                persistDmRecency(event)
                timelineStore.onLiveMessage(event.message)
                dmStore.onChatMessage(ownBareJid, event.message)
                conversationKeyOf(
                    ownBareJid = ownBareJid,
                    from = event.message.from,
                    to = event.message.to,
                    isGroupchat = event.message.isMuc || event.message.messageType == "groupchat",
                )?.let { key ->
                    if (event.message.body != null) {
                        unreadStore.onLiveMessage(key.jid, key.isMine)
                    }
                }
            }
            is XmppEvent.Presence -> presenceStore.onPresence(event.presence)
            is XmppEvent.MamResult -> timelineStore.onArchivedMessage(event.message)
            else -> Unit
        }
        _events.tryEmit(event)
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
            sessionPrefs.setSmResume(update.snapshot)
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

    private suspend fun onTerminalAuthFailure() {
        _connectionState.value = ConnectionState.AuthFailed
        _appState.value = WaddleAppState.SignedOut
        sessionPrefs.clear()
    }

    /** UI hook: the DM conversation is on screen — persist recency. */
    fun recordDmSeen(peerJid: String) {
        val scope = sessionScope ?: return
        scope.launch {
            sessionPrefs.setLastSeen(bareJid(peerJid), nowRfc3339())
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
            sessionPrefs.setLastSeen(peer, message.timestamp ?: nowRfc3339())
        }
    }

    private fun nowRfc3339(): String = java.time.OffsetDateTime.now().toString()

    private suspend fun seedStoresFromPrefs() {
        val joinedRooms = sessionPrefs.joinedRooms.first()
        roomStore.replaceJoinedRooms(joinedRooms)
        dmStore.seed(sessionPrefs.lastSeen.first().keys - joinedRooms)
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
    }

    private enum class AttemptEnd { AUTH_FAILED, CONNECT_FAILED, DROPPED_AFTER_READY }

    private enum class Readiness { READY, AUTH_FAILED, CLOSED }

    /** Wrapper so a conflated channel can carry a `null` (= clear) update. */
    private data class ResumeUpdate(val snapshot: SmResumeSnapshot?)

    companion object {
        /** Web parity: 15s budget from connect to `SessionReady`. */
        const val CONNECT_TIMEOUT_MILLIS = 15_000L
        private const val RESOURCE_PREFIX = "waddle-android-"
        private const val EVENT_BUFFER_CAPACITY = 256
    }
}
