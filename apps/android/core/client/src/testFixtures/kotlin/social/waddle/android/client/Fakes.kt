package social.waddle.android.client

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.emptyPreferences
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.client.ffi.WaddleAvatar
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleConfig
import social.waddle.client.ffi.WaddleEventListener
import social.waddle.client.ffi.WaddleJingleReason
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleMdsDisplayedEntry
import social.waddle.client.ffi.WaddlePinEntry
import social.waddle.client.ffi.WaddlePushDeviceCredentials
import social.waddle.client.ffi.WaddlePushEnvironment
import social.waddle.client.ffi.WaddleRegisterDeviceResult
import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSendOptions
import social.waddle.client.ffi.WaddleTopology
import social.waddle.client.ffi.WaddleUploadSlot
import java.util.concurrent.ConcurrentLinkedDeque
import java.util.concurrent.CopyOnWriteArrayList

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

class FakeNetworkSignal(initiallyOnline: Boolean = true) : NetworkSignal {
    val state = MutableStateFlow(initiallyOnline)
    override val online: Flow<Boolean> = state
}

/**
 * Captures the per-attempt listener + config so tests can drive the
 * session manager by firing FFI events, without loading the native lib.
 */
class FakeClientFactory : ClientFactory {
    val clients = CopyOnWriteArrayList<FakeWaddleClient>()
    val configs = CopyOnWriteArrayList<WaddleConfig>()

    @Volatile
    private var listener: WaddleEventListener? = null

    override fun create(config: WaddleConfig, listener: WaddleEventListener): WaddleClientInterface {
        this.listener = listener
        configs += config
        return FakeWaddleClient().also { clients += it }
    }

    /**
     * Fire an FFI event at the MOST RECENT attempt's listener. Only the
     * latest attempt is addressable: a test that drives a reconnection
     * while asserting on the previous attempt's event flow would deliver
     * here to the wrong listener without any failure.
     */
    fun emit(event: WaddleClientEvent) {
        checkNotNull(listener) { "no client created yet" }.onEvent(event)
    }
}

/**
 * Connect/disconnect no-op; everything unused by the manager rejects.
 * Recorders are concurrency-safe: instrumentation tests poll them from
 * the test thread while the session manager mutates them on its own
 * dispatcher.
 */
class FakeWaddleClient : WaddleClientInterface {
    @Volatile
    var connectCalls = 0

    @Volatile
    var disconnectCalls = 0

    /** Recorded (roomJid, nick) pairs; set [joinRoomFailure] to reject. */
    val joinRoomCalls = CopyOnWriteArrayList<Pair<String, String>>()

    @Volatile
    var joinRoomFailure: Throwable? = null

    /** Recorded (conversationJid, max, beforeId) history queries. */
    val fetchHistoryCalls = CopyOnWriteArrayList<Triple<String, UInt, String?>>()

    @Volatile
    var mamPage: WaddleMamPage =
        WaddleMamPage(messages = emptyList(), firstId = null, lastId = null, isComplete = true)

    /** Recorded (recipientJid, body) sends and the canned outcome. */
    val sendCalls = CopyOnWriteArrayList<Pair<String, String>>()

    @Volatile
    var sendOutcome: WaddleSendMessageOutcome = WaddleSendMessageOutcome.Sent("sent-1")

    /** Options captured per send (the manager passes the stanza id here). */
    val sendOptions = CopyOnWriteArrayList<WaddleSendOptions?>()

    /** Per-call outcome overrides consumed before [sendOutcome]. */
    val sendOutcomes = ConcurrentLinkedDeque<WaddleSendMessageOutcome>()

    override suspend fun connect() {
        connectCalls += 1
    }

    override suspend fun disconnect() {
        disconnectCalls += 1
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

    /** Recorded (conversationJid, query, max) full-text search queries. */
    val searchCalls = CopyOnWriteArrayList<Triple<String, String, UInt>>()

    override suspend fun searchDmHistory(peerJid: String, query: String, maxMessages: UInt): WaddleMamPage {
        searchCalls += Triple(peerJid, query, maxMessages)
        return mamPage
    }

    override suspend fun searchRoomHistory(roomJid: String, query: String, maxMessages: UInt): WaddleMamPage {
        searchCalls += Triple(roomJid, query, maxMessages)
        return mamPage
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
        return sendOutcomes.pollFirst() ?: sendOutcome
    }

    override suspend fun sendGroupchatMessage(
        roomJid: String,
        body: String,
        options: WaddleSendOptions?,
    ): WaddleSendMessageOutcome {
        sendCalls += roomJid to body
        sendOptions += options
        return sendOutcomes.pollFirst() ?: sendOutcome
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

    private fun unused(): Nothing = throw UnsupportedOperationException("not exercised by the session manager")
}
