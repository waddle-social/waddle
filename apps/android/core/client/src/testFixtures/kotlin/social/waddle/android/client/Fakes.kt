package social.waddle.android.client

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.emptyPreferences
import java.io.IOException
import java.util.concurrent.ConcurrentLinkedDeque
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.client.ffi.WaddleAvatar
import social.waddle.client.ffi.WaddleBookmarkItem
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleConfig
import social.waddle.client.ffi.WaddleDeliveryAttemptTransition
import social.waddle.client.ffi.WaddleDeliveryStanzaId
import social.waddle.client.ffi.WaddleDmBookmarkItem
import social.waddle.client.ffi.WaddleJingleReason
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleMdsDisplayedEntry
import social.waddle.client.ffi.WaddleNativeDeliverySignal
import social.waddle.client.ffi.WaddleNotifyMode
import social.waddle.client.ffi.WaddlePinEntry
import social.waddle.client.ffi.WaddlePushDeviceCredentials
import social.waddle.client.ffi.WaddlePushEnvironment
import social.waddle.client.ffi.WaddleRegisterDeviceResult
import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSendOptions
import social.waddle.client.ffi.WaddleSessionReadyKind
import social.waddle.client.ffi.WaddleSetDmNotificationModeOutcome
import social.waddle.client.ffi.WaddleSetRoomNotificationModeOutcome
import social.waddle.client.ffi.WaddleSmResumeState
import social.waddle.client.ffi.WaddleTopology
import social.waddle.client.ffi.WaddleUploadSlot

fun testSessionInfo(
    sessionId: String = "sess-1",
    username: String = "icepuma",
    jid: String = "icepuma@waddle.test",
    isExpired: Boolean = false,
): WaddleSessionInfo = WaddleSessionInfo(
    sessionId = sessionId,
    username = username,
    avatarUrl = null,
    xmppLocalpart = username,
    jid = jid,
    xmppWebsocketUrl = "wss://waddle.test/xmpp",
    linkPreviewMediaOrigin = null,
    isExpired = isExpired,
    expiresAt = null,
)

/** In-memory Preferences DataStore: no disk, no Robolectric. */
class InMemoryPreferencesDataStore : DataStore<Preferences> {
    private val mutex = Mutex()
    private val state = MutableStateFlow<Preferences>(emptyPreferences())

    override val data: Flow<Preferences> = state

    override suspend fun updateData(transform: suspend (t: Preferences) -> Preferences): Preferences =
        mutex.withLock { transform(state.value).also { state.value = it } }
}

/** In-memory store with one-shot write failure injection for durability tests. */
class FailingPreferencesDataStore : DataStore<Preferences> {
    private val mutex = Mutex()
    private val state = MutableStateFlow<Preferences>(emptyPreferences())

    val updateAttempts = AtomicInteger()

    @Volatile
    var failNextUpdate: Boolean = false

    @Volatile
    var failAllUpdates: Boolean = false

    /** Exact failure reused by each update while set; used for cleanup classification tests. */
    @Volatile
    var failAllUpdatesWith: Throwable? = null

    /**
     * Runs after the transformed value is committed. Tests use a deferred
     * pair to observe durable commit and hold the writer without sleeps.
     */
    var afterCommitReturns: (suspend () -> Unit)? = null

    private data class OneShotAfterCommit(
        val matches: (before: Preferences, after: Preferences) -> Boolean,
        val hook: suspend () -> Unit,
    )

    private var afterCommitReturnsOnce: OneShotAfterCommit? = null

    private var beforeCommitReturnsOnce: (suspend () -> Unit)? = null

    /** Atomically reserve [hook] for the next committed update only. */
    suspend fun installAfterCommitReturnsOnce(hook: suspend () -> Unit) {
        installAfterCommitReturnsOnceWhen(matches = { _, _ -> true }, hook)
    }

    /** Atomically reserve [hook] for the first committed state matching [matches]. */
    suspend fun installAfterCommitReturnsOnceWhen(
        matches: (before: Preferences, after: Preferences) -> Boolean,
        hook: suspend () -> Unit,
    ) {
        mutex.withLock {
            check(afterCommitReturnsOnce == null) { "one-shot after-commit hook already installed" }
            afterCommitReturnsOnce = OneShotAfterCommit(matches, hook)
        }
    }

    /** Holds one transformed update before it becomes durable. */
    suspend fun installBeforeCommitReturnsOnce(hook: suspend () -> Unit) {
        mutex.withLock {
            check(beforeCommitReturnsOnce == null) { "one-shot before-commit hook already installed" }
            beforeCommitReturnsOnce = hook
        }
    }

    override val data: Flow<Preferences> = state

    override suspend fun updateData(transform: suspend (t: Preferences) -> Preferences): Preferences {
        val (updated, afterCommit, oneShotAfterCommit) = mutex.withLock {
            updateAttempts.incrementAndGet()
            failAllUpdatesWith?.let { throw it }
            if (failAllUpdates || failNextUpdate) {
                failNextUpdate = false
                throw IOException("injected preferences write failure")
            }
            val before = state.value
            val updated = transform(before)
            beforeCommitReturnsOnce?.let { hook ->
                beforeCommitReturnsOnce = null
                hook()
            }
            state.value = updated
            val oneShot = afterCommitReturnsOnce
                ?.takeIf { it.matches(before, updated) }
                ?.also { afterCommitReturnsOnce = null }
            Triple(updated, afterCommitReturns, oneShot?.hook)
        }
        afterCommit?.invoke()
        oneShotAfterCommit?.invoke()
        return updated
    }
}

class FakeNetworkSignal(initiallyOnline: Boolean = true) : NetworkSignal {
    val state = MutableStateFlow(initiallyOnline)
    override val online: Flow<Boolean> = state
}

/**
 * Captures the per-attempt pull stream + config so tests can drive the
 * session manager by supplying FFI events without loading the native lib.
 */
class FakeClientFactory : ClientFactory {
    val clients = CopyOnWriteArrayList<FakeWaddleClient>()
    val configs = CopyOnWriteArrayList<WaddleConfig>()

    @Volatile
    private var client: FakeWaddleClient? = null

    override fun create(config: WaddleConfig): WaddleClientInterface {
        configs += config
        return FakeWaddleClient().also {
            client = it
            clients += it
        }
    }

    /**
     * Supply an FFI event to the MOST RECENT attempt. Only the
     * latest attempt is addressable: a test that drives a reconnection
     * while asserting on the previous attempt's event flow would deliver
     * here to the wrong pull stream without any failure.
     */
    fun emit(event: WaddleClientEvent) {
        checkNotNull(client) { "no client created yet" }.emit(event)
    }

    /** Fire an event at an immutable historical attempt for stale-generation tests. */
    fun emitAt(attemptIndex: Int, event: WaddleClientEvent) {
        clients[attemptIndex].emit(event)
    }

    fun emitReady(
        kind: WaddleSessionReadyKind = WaddleSessionReadyKind.FRESH,
        attemptIndex: Int = configs.lastIndex,
    ) {
        emitAt(
            attemptIndex,
            WaddleClientEvent.SessionReady(kind, configs[attemptIndex].deliveryAttempt),
        )
    }

    fun emitAcked(
        clientStanzaId: String,
        attemptIndex: Int = configs.lastIndex,
    ) {
        emitAt(
            attemptIndex,
            WaddleClientEvent.DeliveryAcked(
                WaddleNativeDeliverySignal(
                    attempt = configs[attemptIndex].deliveryAttempt,
                    stanzaId = WaddleDeliveryStanzaId(clientStanzaId),
                ),
            ),
        )
    }

    fun emitFailed(
        clientStanzaId: String,
        attemptIndex: Int = configs.lastIndex,
    ) {
        emitAt(
            attemptIndex,
            WaddleClientEvent.DeliveryFailed(
                WaddleNativeDeliverySignal(
                    attempt = configs[attemptIndex].deliveryAttempt,
                    stanzaId = WaddleDeliveryStanzaId(clientStanzaId),
                ),
            ),
        )
    }

    fun emitResumeStateChanged(
        state: WaddleSmResumeState?,
        attemptIndex: Int = configs.lastIndex,
    ) {
        emitAt(
            attemptIndex,
            WaddleClientEvent.ResumeStateChanged(
                attempt = configs[attemptIndex].deliveryAttempt,
                state = state,
            ),
        )
    }

    fun emitResumeFailed(
        transition: WaddleDeliveryAttemptTransition,
        affectedStanzaIds: Collection<String>,
        attemptIndex: Int = configs.lastIndex,
    ) {
        emitAt(
            attemptIndex,
            WaddleClientEvent.ResumeFailed(
                transition = transition,
                affected = affectedStanzaIds.map(::WaddleDeliveryStanzaId),
            ),
        )
    }
}

/** One recorded [FakeWaddleClient] full-text search query. */
data class SearchCall(
    val conversationJid: String,
    val query: String,
    val maxMessages: UInt,
    val isRoom: Boolean,
)

/**
 * Connect/disconnect no-op; everything unused by the manager rejects.
 * Recorders are concurrency-safe: instrumentation tests poll them from
 * the test thread while the session manager mutates them on its own
 * dispatcher.
 */
class FakeWaddleClient : WaddleClientInterface {
    private val events = Channel<WaddleClientEvent>(Channel.UNLIMITED)

    @Volatile
    var connectCalls = 0

    @Volatile
    var disconnectCalls = 0

    /** Runs after disconnect is recorded but before the call returns. */
    var beforeDisconnectReturns: (suspend () -> Unit)? = null

    /** Recorded (roomJid, nick) pairs; set [joinRoomFailure] to reject. */
    val joinRoomCalls = CopyOnWriteArrayList<Pair<String, String>>()

    @Volatile
    var joinRoomFailure: Throwable? = null

    /** Recorded (conversationJid, max, beforeId) history queries. */
    val fetchHistoryCalls = CopyOnWriteArrayList<Triple<String, UInt, String?>>()

    @Volatile
    var mamPage: WaddleMamPage =
        WaddleMamPage(messages = emptyList(), firstId = null, lastId = null, isComplete = true)

    /** Recorded (recipientJid, body) sends and an optional canned outcome. */
    val sendCalls = CopyOnWriteArrayList<Pair<String, String>>()

    @Volatile
    var sendOutcome: WaddleSendMessageOutcome? = null

    /** Options captured per send (the manager passes the stanza id here). */
    val sendOptions = CopyOnWriteArrayList<WaddleSendOptions?>()

    /** Per-call outcome overrides consumed before [sendOutcome]. */
    val sendOutcomes = ConcurrentLinkedDeque<WaddleSendMessageOutcome>()

    /** Runs after the call is recorded but before its outcome is returned. */
    var beforeSendReturns: (suspend () -> Unit)? = null

    /**
     * Number of serialized native pulls begun. Tests use this to prove a
     * durability barrier completes before Kotlin requests the next event.
     */
    val nextEventCalls = AtomicInteger()

    /** Pulls currently suspended in the fake native client. */
    val inFlightNextEvents = AtomicInteger()

    /** High-water mark used to reject accidental native prefetch. */
    val maxInFlightNextEvents = AtomicInteger()

    override suspend fun connect() {
        connectCalls += 1
    }

    override suspend fun disconnect() {
        disconnectCalls += 1
        beforeDisconnectReturns?.invoke()
    }

    override suspend fun nextEvent(): WaddleClientEvent {
        nextEventCalls.incrementAndGet()
        val inFlight = inFlightNextEvents.incrementAndGet()
        maxInFlightNextEvents.updateAndGet { previous -> maxOf(previous, inFlight) }
        return try {
            events.receive()
        } finally {
            inFlightNextEvents.decrementAndGet()
        }
    }

    fun emit(event: WaddleClientEvent) {
        events.trySend(event)
    }

    override suspend fun discoverTopology(): WaddleTopology =
        WaddleTopology(spaces = emptyList(), channels = emptyList())

    override suspend fun sendCallFinish(peerFullJid: String, sid: String): Boolean = unused()

    override suspend fun sendCallFinishMigrated(
        peerFullJid: String,
        oldSid: String,
        newSid: String,
    ): Boolean = unused()

    override suspend fun sendCallProceed(peerFullJid: String, sid: String): Boolean = unused()

    override suspend fun sendCallPropose(
        peerBareJid: String,
        sid: String,
        audio: Boolean,
        video: Boolean,
    ): Boolean = unused()

    override suspend fun sendCallReject(peerFullJid: String, sid: String): Boolean = unused()

    override suspend fun sendCallRejectTieBreak(peerFullJid: String, sid: String): Boolean = unused()

    override suspend fun sendCallRetract(peerBareJid: String, sid: String): Boolean = unused()

    override suspend fun sendCallRetractTieBreak(peerFullJid: String, sid: String): Boolean = unused()

    override suspend fun sendCallSessionAccept(
        peerFullJid: String,
        responderFullJid: String,
        sid: String,
        audio: Boolean,
        video: Boolean,
    ): Boolean = unused()

    override suspend fun sendCallSessionInitiate(
        peerFullJid: String,
        initiatorFullJid: String,
        sid: String,
        audio: Boolean,
        video: Boolean,
    ): Boolean = unused()

    override suspend fun sendCallSessionTerminate(
        peerFullJid: String,
        sid: String,
        reason: WaddleJingleReason?,
    ): Boolean = unused()

    override suspend fun discoverUploadService(): String? = unused()

    override suspend fun fetchDmHistory(peerJid: String, maxMessages: UInt, beforeId: String?): WaddleMamPage {
        fetchHistoryCalls += Triple(peerJid, maxMessages, beforeId)
        return mamPage
    }

    override suspend fun fetchRoomHistory(roomJid: String, maxMessages: UInt, beforeId: String?): WaddleMamPage {
        fetchHistoryCalls += Triple(roomJid, maxMessages, beforeId)
        return mamPage
    }

    /** Recorded full-text search queries, room and DM alike. */
    val searchCalls = CopyOnWriteArrayList<SearchCall>()

    /**
     * Per-call search responses consumed before [mamPage]; parking an
     * uncompleted deferred lets a test control response ordering
     * (stale-response race-guard coverage).
     */
    val searchResponses = ConcurrentLinkedDeque<CompletableDeferred<WaddleMamPage>>()

    override suspend fun searchDmHistory(peerJid: String, query: String, maxMessages: UInt): WaddleMamPage {
        searchCalls += SearchCall(peerJid, query, maxMessages, isRoom = false)
        return searchResponses.pollFirst()?.await() ?: mamPage
    }

    override suspend fun searchRoomHistory(roomJid: String, query: String, maxMessages: UInt): WaddleMamPage {
        searchCalls += SearchCall(roomJid, query, maxMessages, isRoom = true)
        return searchResponses.pollFirst()?.await() ?: mamPage
    }

    override suspend fun joinRoom(roomJid: String, nick: String) {
        joinRoomCalls += roomJid to nick
        joinRoomFailure?.let { throw it }
    }

    override suspend fun leaveRoom(roomJid: String, nick: String) = unused()
    override suspend fun requestAvatar(jid: String): WaddleAvatar? = unused()
    override suspend fun requestUploadSlot(
        serviceJid: String,
        filename: String,
        size: ULong,
        contentType: String,
    ): WaddleUploadSlot? = unused()

    override suspend fun sendChatMessage(
        peerJid: String,
        body: String,
        options: WaddleSendOptions?,
    ): WaddleSendMessageOutcome {
        sendCalls += peerJid to body
        sendOptions += options
        beforeSendReturns?.invoke()
        return sendOutcomes.pollFirst() ?: sendOutcome ?: echoSent(options)
    }

    override suspend fun sendGroupchatMessage(
        roomJid: String,
        body: String,
        options: WaddleSendOptions?,
    ): WaddleSendMessageOutcome {
        sendCalls += roomJid to body
        sendOptions += options
        beforeSendReturns?.invoke()
        return sendOutcomes.pollFirst() ?: sendOutcome ?: echoSent(options)
    }
    override suspend fun sendPresence(status: String?, show: String?, idleSince: String?) = unused()

    /** Recorded (conversation, targetId, emojis) reaction sends. */
    val reactionCalls = CopyOnWriteArrayList<Triple<String, String, List<String>>>()

    @Volatile
    var reactionResult = true

    override suspend fun sendReaction(
        targetJid: String,
        targetStanzaId: String,
        emojis: List<String>,
        isMuc: Boolean,
    ): Boolean {
        reactionCalls += Triple(targetJid, targetStanzaId, emojis)
        return reactionResult
    }

    /** Recorded (conversation, targetId, newBody) corrections. */
    val correctionCalls = CopyOnWriteArrayList<Triple<String, String, String>>()

    @Volatile
    var correctionOutcome: WaddleSendMessageOutcome = WaddleSendMessageOutcome.Sent("corr-1")

    override suspend fun sendCorrection(
        peerJid: String,
        targetId: String,
        newBody: String,
        isMuc: Boolean,
        options: WaddleSendOptions?,
    ): WaddleSendMessageOutcome {
        correctionCalls += Triple(peerJid, targetId, newBody)
        return correctionOutcome
    }

    /** Recorded (conversation, targetId) retractions. */
    val retractionCalls = CopyOnWriteArrayList<Pair<String, String>>()

    @Volatile
    var retractionResult = true

    override suspend fun sendRetraction(peerJid: String, targetStanzaId: String, isMuc: Boolean): Boolean {
        retractionCalls += peerJid to targetStanzaId
        return retractionResult
    }

    override suspend fun sendModeration(roomJid: String, targetStanzaId: String, reason: String?): Boolean = unused()

    /** Recorded (conversation, state, isMuc) typing notifications. */
    val chatStateCalls = CopyOnWriteArrayList<Triple<String, WaddleChatState, Boolean>>()

    override suspend fun sendChatState(peerJid: String, state: WaddleChatState, isMuc: Boolean): Boolean {
        chatStateCalls += Triple(peerJid, state, isMuc)
        return true
    }

    /** Recorded (conversation, stanzaId, isMuc) displayed markers. */
    val displayedCalls = CopyOnWriteArrayList<Triple<String, String, Boolean>>()

    override suspend fun sendDisplayed(peerJid: String, stanzaId: String, isMuc: Boolean): Boolean {
        displayedCalls += Triple(peerJid, stanzaId, isMuc)
        return true
    }

    /** Recorded (chatJid, stanzaId, stanzaIdBy) MDS publishes. */
    val mdsPublishCalls = CopyOnWriteArrayList<Triple<String, String, String>>()

    override suspend fun publishMdsDisplayed(chatJid: String, stanzaId: String, stanzaIdBy: String): Boolean {
        mdsPublishCalls += Triple(chatJid, stanzaId, stanzaIdBy)
        return true
    }

    /** Canned XEP-0490 catch-up entries served by [fetchMdsDisplayed]. */
    @Volatile
    var mdsEntries: List<WaddleMdsDisplayedEntry> = emptyList()

    @Volatile
    var mdsSubscribeCalls = 0

    @Volatile
    var mdsPublishOptionsSupported = true

    override suspend fun fetchMdsDisplayed(): List<WaddleMdsDisplayedEntry> = mdsEntries

    override suspend fun subscribeMdsDisplayed(): Boolean {
        mdsSubscribeCalls += 1
        return true
    }

    override suspend fun supportsMdsPublishOptions(): Boolean = mdsPublishOptionsSupported

    /** Canned pin list served by [fetchRoomPins]; recorded pin/unpin ops. */
    @Volatile
    var roomPins: List<WaddlePinEntry> = emptyList()
    val pinCalls = CopyOnWriteArrayList<Triple<String, String, Boolean>>()

    override suspend fun fetchRoomPins(roomJid: String): List<WaddlePinEntry> = roomPins

    override suspend fun pinMessage(roomJid: String, targetStanzaId: String): Boolean {
        pinCalls += Triple(roomJid, targetStanzaId, true)
        return true
    }

    override suspend fun unpinMessage(roomJid: String, targetStanzaId: String): Boolean {
        pinCalls += Triple(roomJid, targetStanzaId, false)
        return true
    }

    /** Canned XEP-0492 bookmark lists served by the fetch verbs. */
    @Volatile
    var userBookmarks: List<WaddleBookmarkItem> = emptyList()

    @Volatile
    var dmBookmarks: List<WaddleDmBookmarkItem> = emptyList()

    @Volatile
    var fetchUserBookmarksCalls = 0

    @Volatile
    var fetchDmBookmarksCalls = 0

    override suspend fun fetchUserBookmarks(): List<WaddleBookmarkItem> {
        fetchUserBookmarksCalls += 1
        return userBookmarks
    }

    override suspend fun fetchDmBookmarks(): List<WaddleDmBookmarkItem> {
        fetchDmBookmarksCalls += 1
        return dmBookmarks
    }

    /** Recorded (jid, mode, richPayloadOptIn) XEP-0492 set calls. */
    val roomNotifyCalls = CopyOnWriteArrayList<Triple<String, WaddleNotifyMode, Boolean>>()
    val dmNotifyCalls = CopyOnWriteArrayList<Triple<String, WaddleNotifyMode, Boolean>>()

    /** Canned outcome overrides; `null` echoes the request as `Ok`. */
    @Volatile
    var roomNotifyOutcome: WaddleSetRoomNotificationModeOutcome? = null

    /** `null` mirrors the sparse DM carrier: default mode → `Removed`. */
    @Volatile
    var dmNotifyOutcome: WaddleSetDmNotificationModeOutcome? = null

    override suspend fun setRoomNotificationMode(
        roomJid: String,
        mode: WaddleNotifyMode,
        name: String?,
        richPayloadOptIn: Boolean,
    ): WaddleSetRoomNotificationModeOutcome {
        roomNotifyCalls += Triple(roomJid, mode, richPayloadOptIn)
        return roomNotifyOutcome ?: WaddleSetRoomNotificationModeOutcome.Ok(
            WaddleBookmarkItem(
                jid = roomJid,
                name = name,
                autojoin = false,
                notifyMode = mode,
                richPayloadOptIn = richPayloadOptIn,
            ),
        )
    }

    override suspend fun setDmNotificationMode(
        dmJid: String,
        mode: WaddleNotifyMode,
        richPayloadOptIn: Boolean,
    ): WaddleSetDmNotificationModeOutcome {
        dmNotifyCalls += Triple(dmJid, mode, richPayloadOptIn)
        dmNotifyOutcome?.let { return it }
        return if (mode == WaddleNotifyMode.ALWAYS && !richPayloadOptIn) {
            WaddleSetDmNotificationModeOutcome.Removed(dmJid)
        } else {
            WaddleSetDmNotificationModeOutcome.Ok(
                WaddleDmBookmarkItem(jid = dmJid, notifyMode = mode, richPayloadOptIn = richPayloadOptIn),
            )
        }
    }

    override suspend fun pinDirectMessage(peerJid: String, targetStanzaId: String): Boolean = unused()
    override suspend fun unpinDirectMessage(peerJid: String, targetStanzaId: String): Boolean = unused()
    override suspend fun disablePushDevice(pushServiceJid: String, node: String, deviceId: String): Boolean = unused()
    override suspend fun disablePushNotifications(pushServiceJid: String, node: String?): Boolean = unused()
    override suspend fun enablePushNotifications(pushServiceJid: String, node: String): Boolean = unused()
    override suspend fun registerPushDevice(
        pushServiceJid: String,
        appId: String,
        environment: WaddlePushEnvironment,
        credentials: WaddlePushDeviceCredentials,
    ): WaddleRegisterDeviceResult? = unused()

    private fun echoSent(options: WaddleSendOptions?): WaddleSendMessageOutcome =
        options?.stanzaId
            ?.let(WaddleSendMessageOutcome::Sent)
            ?: WaddleSendMessageOutcome.Error

    private fun unused(): Nothing = throw UnsupportedOperationException("not exercised by the session manager")
}
