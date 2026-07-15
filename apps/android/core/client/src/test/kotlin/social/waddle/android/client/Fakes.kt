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
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleConfig
import social.waddle.client.ffi.WaddleEventListener
import social.waddle.client.ffi.WaddleJingleReason
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddlePushDeviceCredentials
import social.waddle.client.ffi.WaddlePushEnvironment
import social.waddle.client.ffi.WaddleRegisterDeviceResult
import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSendOptions
import social.waddle.client.ffi.WaddleTopology
import social.waddle.client.ffi.WaddleUploadSlot

fun testSessionInfo(): WaddleSessionInfo = WaddleSessionInfo(
    sessionId = "sess-1",
    username = "icepuma",
    avatarUrl = null,
    xmppLocalpart = "icepuma",
    jid = "icepuma@waddle.test",
    xmppWebsocketUrl = "wss://waddle.test/xmpp",
    linkPreviewMediaOrigin = null,
    isExpired = false,
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
    val clients = mutableListOf<FakeWaddleClient>()
    val configs = mutableListOf<WaddleConfig>()
    private var listener: WaddleEventListener? = null

    override fun create(config: WaddleConfig, listener: WaddleEventListener): WaddleClientInterface {
        this.listener = listener
        configs += config
        return FakeWaddleClient().also { clients += it }
    }

    /** Fire an FFI event at the most recent attempt's listener. */
    fun emit(event: WaddleClientEvent) {
        checkNotNull(listener) { "no client created yet" }.onEvent(event)
    }
}

/** Connect/disconnect no-op; everything unused by the manager rejects. */
class FakeWaddleClient : WaddleClientInterface {
    var connectCalls = 0
    var disconnectCalls = 0

    /** Recorded (roomJid, nick) pairs; set [joinRoomFailure] to reject. */
    val joinRoomCalls = mutableListOf<Pair<String, String>>()
    var joinRoomFailure: Throwable? = null

    /** Recorded (conversationJid, max, beforeId) history queries. */
    val fetchHistoryCalls = mutableListOf<Triple<String, UInt, String?>>()
    var mamPage: WaddleMamPage =
        WaddleMamPage(messages = emptyList(), firstId = null, lastId = null, isComplete = true)

    /** Recorded (recipientJid, body) sends and the canned outcome. */
    val sendCalls = mutableListOf<Pair<String, String>>()
    var sendOutcome: WaddleSendMessageOutcome = WaddleSendMessageOutcome.Sent("sent-1")

    /** Options captured per send (the manager passes the stanza id here). */
    val sendOptions = mutableListOf<WaddleSendOptions?>()

    /** Per-call outcome overrides consumed before [sendOutcome]. */
    val sendOutcomes = ArrayDeque<WaddleSendMessageOutcome>()

    override suspend fun connect() {
        connectCalls += 1
    }

    override suspend fun disconnect() {
        disconnectCalls += 1
    }

    override suspend fun discoverTopology(): WaddleTopology =
        WaddleTopology(spaces = emptyList(), channels = emptyList())

    override suspend fun sendCallFinish(peerFullJid: String, sid: String): Boolean = unused()
    override suspend fun sendCallFinishMigrated(peerFullJid: String, oldSid: String, newSid: String): Boolean = unused()
    override suspend fun sendCallProceed(peerFullJid: String, sid: String): Boolean = unused()
    override suspend fun sendCallPropose(peerBareJid: String, sid: String, audio: Boolean, video: Boolean): Boolean = unused()
    override suspend fun sendCallReject(peerFullJid: String, sid: String): Boolean = unused()
    override suspend fun sendCallRejectTieBreak(peerFullJid: String, sid: String): Boolean = unused()
    override suspend fun sendCallRetract(peerBareJid: String, sid: String): Boolean = unused()
    override suspend fun sendCallRetractTieBreak(peerFullJid: String, sid: String): Boolean = unused()
    override suspend fun sendCallSessionAccept(peerFullJid: String, responderFullJid: String, sid: String, audio: Boolean, video: Boolean): Boolean = unused()
    override suspend fun sendCallSessionInitiate(peerFullJid: String, initiatorFullJid: String, sid: String, audio: Boolean, video: Boolean): Boolean = unused()
    override suspend fun sendCallSessionTerminate(peerFullJid: String, sid: String, reason: WaddleJingleReason?): Boolean = unused()
    override suspend fun discoverUploadService(): String? = unused()

    override suspend fun fetchDmHistory(peerJid: String, maxMessages: UInt, beforeId: String?): WaddleMamPage {
        fetchHistoryCalls += Triple(peerJid, maxMessages, beforeId)
        return mamPage
    }

    override suspend fun fetchRoomHistory(roomJid: String, maxMessages: UInt, beforeId: String?): WaddleMamPage {
        fetchHistoryCalls += Triple(roomJid, maxMessages, beforeId)
        return mamPage
    }

    override suspend fun joinRoom(roomJid: String, nick: String) {
        joinRoomCalls += roomJid to nick
        joinRoomFailure?.let { throw it }
    }

    override suspend fun leaveRoom(roomJid: String, nick: String) = unused()
    override suspend fun requestAvatar(jid: String): WaddleAvatar? = unused()
    override suspend fun requestUploadSlot(serviceJid: String, filename: String, size: ULong, contentType: String): WaddleUploadSlot? = unused()

    override suspend fun sendChatMessage(peerJid: String, body: String, options: WaddleSendOptions?): WaddleSendMessageOutcome {
        sendCalls += peerJid to body
        sendOptions += options
        return sendOutcomes.removeFirstOrNull() ?: sendOutcome
    }

    override suspend fun sendGroupchatMessage(roomJid: String, body: String, options: WaddleSendOptions?): WaddleSendMessageOutcome {
        sendCalls += roomJid to body
        sendOptions += options
        return sendOutcomes.removeFirstOrNull() ?: sendOutcome
    }
    override suspend fun sendPresence(status: String?, show: String?, idleSince: String?) = unused()
    override suspend fun disablePushDevice(pushServiceJid: String, node: String, deviceId: String): Boolean = unused()
    override suspend fun disablePushNotifications(pushServiceJid: String, node: String?): Boolean = unused()
    override suspend fun enablePushNotifications(pushServiceJid: String, node: String): Boolean = unused()
    override suspend fun registerPushDevice(pushServiceJid: String, appId: String, environment: WaddlePushEnvironment, credentials: WaddlePushDeviceCredentials): WaddleRegisterDeviceResult? = unused()

    private fun unused(): Nothing = throw UnsupportedOperationException("not exercised by the session manager")
}
