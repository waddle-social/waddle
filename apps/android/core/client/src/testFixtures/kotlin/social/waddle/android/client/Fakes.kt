package social.waddle.android.client

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.emptyPreferences
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.client.ffi.WaddleAdhocAction
import social.waddle.client.ffi.WaddleAdhocStatus
import social.waddle.client.ffi.WaddleAdminChannelRef
import social.waddle.client.ffi.WaddleAdminChannelsAffiliationsArgs
import social.waddle.client.ffi.WaddleAdminChannelsAffiliationsPage
import social.waddle.client.ffi.WaddleAdminChannelsCreateArgs
import social.waddle.client.ffi.WaddleAdminChannelsKickResult
import social.waddle.client.ffi.WaddleAdminChannelsListArgs
import social.waddle.client.ffi.WaddleAdminChannelsListPage
import social.waddle.client.ffi.WaddleAdminChannelsOccupantsArgs
import social.waddle.client.ffi.WaddleAdminChannelsSetAffiliationResult
import social.waddle.client.ffi.WaddleAdminChannelsUpdateArgs
import social.waddle.client.ffi.WaddleAdminSpaceRef
import social.waddle.client.ffi.WaddleAdminSpacesCreateArgs
import social.waddle.client.ffi.WaddleAdminSpacesListArgs
import social.waddle.client.ffi.WaddleAdminSpacesListPage
import social.waddle.client.ffi.WaddleAdminSpacesMembersArgs
import social.waddle.client.ffi.WaddleAdminSpacesMembersPage
import social.waddle.client.ffi.WaddleAdminSpacesSetRoleResult
import social.waddle.client.ffi.WaddleAdminSpacesUpdateArgs
import social.waddle.client.ffi.WaddleAdminUsersPage
import social.waddle.client.ffi.WaddleAvatar
import social.waddle.client.ffi.WaddleAvatarResult
import social.waddle.client.ffi.WaddleBookmarkItem
import social.waddle.client.ffi.WaddleCallSessionTerminateOutcome
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleConfig
import social.waddle.client.ffi.WaddleDmBookmarkItem
import social.waddle.client.ffi.WaddleEventListener
import social.waddle.client.ffi.WaddleExtensionCommand
import social.waddle.client.ffi.WaddleExtensionCommandResult
import social.waddle.client.ffi.WaddleExtensionFormField
import social.waddle.client.ffi.WaddleExternalService
import social.waddle.client.ffi.WaddleInCallPresenceFlags
import social.waddle.client.ffi.WaddleInboxResult
import social.waddle.client.ffi.WaddleJingleReason
import social.waddle.client.ffi.WaddleLinkPreviewLookup
import social.waddle.client.ffi.WaddleLinkPreviewLookupStatus
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleMdsDisplayedEntry
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleNotifyMode
import social.waddle.client.ffi.WaddlePepProfile
import social.waddle.client.ffi.WaddlePinEntry
import social.waddle.client.ffi.WaddlePushDeviceCredentials
import social.waddle.client.ffi.WaddlePushEnvironment
import social.waddle.client.ffi.WaddleRegisterDeviceResult
import social.waddle.client.ffi.WaddleRoomConfig
import social.waddle.client.ffi.WaddleRoomConfigPatch
import social.waddle.client.ffi.WaddleRoomMemberEntry
import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSendOptions
import social.waddle.client.ffi.WaddleSetDmNotificationModeOutcome
import social.waddle.client.ffi.WaddleSetRoomNotificationModeOutcome
import social.waddle.client.ffi.WaddleSpaceRole
import social.waddle.client.ffi.WaddleStickerPack
import social.waddle.client.ffi.WaddleTopology
import social.waddle.client.ffi.WaddleTune
import social.waddle.client.ffi.WaddleUploadSlot
import social.waddle.client.ffi.WaddleUserSearchEntry
import social.waddle.client.ffi.WaddleVCard4
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

    /** Applied to every new [FakeWaddleClient] before the attempt sees it. */
    @Volatile
    var onCreate: (FakeWaddleClient) -> Unit = {}

    override fun create(config: WaddleConfig, listener: WaddleEventListener): WaddleClientInterface {
        this.listener = listener
        configs += config
        return FakeWaddleClient().also {
            onCreate(it)
            clients += it
        }
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

/** One recorded [FakeWaddleClient] full-text search query. */
data class SearchCall(
    val conversationJid: String,
    val query: String,
    val maxMessages: UInt,
    val isRoom: Boolean,
)

/** One recorded XEP-0363 upload-slot request. */
data class UploadSlotCall(
    val serviceJid: String,
    val filename: String,
    val size: ULong,
    val contentType: String,
)

/**
 * One recorded `sendCall*` wire verb, in send order, so call-engine
 * tests can assert exact sequences (e.g. the tie-break branch's
 * finish-migrated → proceed → session-terminate ordering).
 */
sealed interface RecordedCallVerb {
    data class Propose(
        val peerBareJid: String,
        val sid: String,
        val audio: Boolean,
        val video: Boolean,
    ) : RecordedCallVerb

    data class Ringing(val peerBareJid: String, val sid: String) : RecordedCallVerb

    data class Proceed(val peerFullJid: String, val sid: String) : RecordedCallVerb

    data class Reject(val peerFullJid: String, val sid: String) : RecordedCallVerb

    data class RejectTieBreak(val peerFullJid: String, val sid: String) : RecordedCallVerb

    data class Retract(val peerBareJid: String, val sid: String) : RecordedCallVerb

    data class RetractTieBreak(val peerFullJid: String, val sid: String) : RecordedCallVerb

    data class Finish(val peerFullJid: String, val sid: String) : RecordedCallVerb

    data class FinishWithReason(
        val peerFullJid: String,
        val sid: String,
        val reason: WaddleJingleReason,
    ) : RecordedCallVerb

    data class FinishMigrated(
        val peerFullJid: String,
        val oldSid: String,
        val newSid: String,
    ) : RecordedCallVerb

    data class SessionInitiate(
        val peerFullJid: String,
        val initiatorFullJid: String,
        val sid: String,
        val audio: Boolean,
        val video: Boolean,
    ) : RecordedCallVerb

    data class SessionAccept(
        val peerFullJid: String,
        val responderFullJid: String,
        val sid: String,
        val audio: Boolean,
        val video: Boolean,
    ) : RecordedCallVerb

    data class SessionTerminate(
        val peerFullJid: String,
        val sid: String,
        val reason: WaddleJingleReason?,
    ) : RecordedCallVerb

    data class SessionTerminateWithOutcome(
        val peerFullJid: String,
        val sid: String,
        val reason: WaddleJingleReason?,
    ) : RecordedCallVerb

    data class MujiSessionInitiate(
        val roomJid: String,
        val initiatorFullJid: String,
        val sid: String,
        val video: Boolean,
    ) : RecordedCallVerb

    data class MujiSessionTerminate(val roomJid: String, val sid: String) : RecordedCallVerb

    data class UpdateMujiPresence(
        val roomJid: String,
        val nick: String,
        val active: Boolean,
        val preparing: Boolean,
        val video: Boolean,
        val handRaised: Boolean,
        val muted: Boolean,
    ) : RecordedCallVerb
}

/**
 * One recorded profile-publishing wire verb (XEP-0292/0084/0107/0108/
 * 0118), in send order, mirroring [RecordedCallVerb] for the call
 * family.
 */
sealed interface RecordedProfileVerb {
    data class PublishVcard4(val vcard: WaddleVCard4) : RecordedProfileVerb

    /** Bytes are recorded by count (array equality is referential);
     *  the raw payload lands in `FakeWaddleClient.publishedAvatarBytes`. */
    data class PublishAvatar(
        val byteCount: Int,
        val mimeType: String,
        val width: UInt,
        val height: UInt,
    ) : RecordedProfileVerb

    data object DisableAvatar : RecordedProfileVerb

    data class PublishMood(val kind: String, val text: String?) : RecordedProfileVerb

    data object RetractMood : RecordedProfileVerb

    data class PublishActivity(
        val general: String,
        val specific: String?,
        val text: String?,
    ) : RecordedProfileVerb

    data class PublishTune(val tune: WaddleTune) : RecordedProfileVerb

    data object RetractActivity : RecordedProfileVerb

    data object RetractTune : RecordedProfileVerb
}

/**
 * XEP-0449 sticker-verb slice of [FakeWaddleClient]: canned packs,
 * per-verb failure knobs, and recorded operations.
 */
class FakeStickerVerbs {
    /** Canned packs served by both fetch verbs. */
    @Volatile
    var packs: List<WaddleStickerPack> = emptyList()

    val fetchCalls = CopyOnWriteArrayList<String?>()
    val publishCalls = CopyOnWriteArrayList<WaddleStickerPack>()
    val retractCalls = CopyOnWriteArrayList<String>()

    /** Set to fail the fetch/publish/retract verbs (they throw). */
    @Volatile
    var fetchFailure: Throwable? = null

    @Volatile
    var publishFailure: Throwable? = null

    @Volatile
    var retractFailure: Throwable? = null

    /** Server-derived pack id echoed by [publish]. */
    @Volatile
    var publishedId: String = "published-pack-id"

    /** Canned retract answer (false = the FFI's refusal shape). */
    @Volatile
    var retractResult = true

    fun fetchAll(ownerJid: String?): List<WaddleStickerPack> {
        fetchCalls += ownerJid
        fetchFailure?.let { throw it }
        return packs
    }

    fun fetchOne(ownerJid: String, packId: String): WaddleStickerPack? {
        fetchCalls += ownerJid
        fetchFailure?.let { throw it }
        return packs.firstOrNull { pack -> pack.id == packId }
    }

    fun publish(pack: WaddleStickerPack): String {
        publishCalls += pack
        publishFailure?.let { throw it }
        return publishedId
    }

    fun retract(packId: String): Boolean {
        retractCalls += packId
        retractFailure?.let { throw it }
        return retractResult
    }
}

/**
 * XEP-0492 notify-mode slice of [FakeWaddleClient]: recorded set
 * calls plus canned outcomes mirroring the sparse DM carrier.
 */
class FakeNotifyVerbs {
    /** Recorded (jid, mode, richPayloadOptIn) set calls. */
    val roomCalls = CopyOnWriteArrayList<Triple<String, WaddleNotifyMode, Boolean>>()
    val dmCalls = CopyOnWriteArrayList<Triple<String, WaddleNotifyMode, Boolean>>()

    /** Canned outcome overrides; `null` echoes the request as `Ok`. */
    @Volatile
    var roomOutcome: WaddleSetRoomNotificationModeOutcome? = null

    /** `null` mirrors the sparse DM carrier: default mode → `Removed`. */
    @Volatile
    var dmOutcome: WaddleSetDmNotificationModeOutcome? = null

    fun setRoomMode(
        roomJid: String,
        mode: WaddleNotifyMode,
        name: String?,
        richPayloadOptIn: Boolean,
    ): WaddleSetRoomNotificationModeOutcome {
        roomCalls += Triple(roomJid, mode, richPayloadOptIn)
        return roomOutcome ?: WaddleSetRoomNotificationModeOutcome.Ok(
            WaddleBookmarkItem(
                jid = roomJid,
                name = name,
                autojoin = false,
                notifyMode = mode,
                richPayloadOptIn = richPayloadOptIn,
            ),
        )
    }

    fun setDmMode(
        dmJid: String,
        mode: WaddleNotifyMode,
        richPayloadOptIn: Boolean,
    ): WaddleSetDmNotificationModeOutcome {
        dmCalls += Triple(dmJid, mode, richPayloadOptIn)
        dmOutcome?.let { return it }
        return if (mode == WaddleNotifyMode.ALWAYS && !richPayloadOptIn) {
            WaddleSetDmNotificationModeOutcome.Removed(dmJid)
        } else {
            WaddleSetDmNotificationModeOutcome.Ok(
                WaddleDmBookmarkItem(jid = dmJid, notifyMode = mode, richPayloadOptIn = richPayloadOptIn),
            )
        }
    }
}

/** One recorded [FakeExtensionCommandVerbs.submit] call, in send order. */
data class RecordedExtensionSubmit(
    val serviceJid: String,
    val node: String,
    val sessionId: String?,
    val fields: List<WaddleExtensionFormField>,
    val action: WaddleAdhocAction,
    val roomJid: String?,
)

/**
 * `urn:waddle:extension:1` slash-command slice of [FakeWaddleClient]:
 * canned discovery/invoke/submit answers, per-verb failure knobs, and
 * recorded operations.
 */
class FakeExtensionCommandVerbs {
    /** Canned command set served by [discover]. */
    @Volatile
    var commands: List<WaddleExtensionCommand> = emptyList()

    /** Canned answers; default is a bare `completed` result. */
    @Volatile
    var invokeResult: WaddleExtensionCommandResult = completedResult()

    @Volatile
    var submitResult: WaddleExtensionCommandResult = completedResult()

    /** Set to fail the discover/invoke/submit verbs (they throw). */
    @Volatile
    var discoverFailure: Throwable? = null

    @Volatile
    var invokeFailure: Throwable? = null

    @Volatile
    var submitFailure: Throwable? = null

    @Volatile
    var discoverCalls = 0
    val invokeCalls = CopyOnWriteArrayList<Triple<String, String, String?>>()
    val submitCalls = CopyOnWriteArrayList<RecordedExtensionSubmit>()

    fun discover(): List<WaddleExtensionCommand> {
        discoverCalls += 1
        discoverFailure?.let { throw it }
        return commands
    }

    fun invoke(serviceJid: String, node: String, roomJid: String?): WaddleExtensionCommandResult {
        invokeCalls += Triple(serviceJid, node, roomJid)
        invokeFailure?.let { throw it }
        return invokeResult
    }

    fun submit(call: RecordedExtensionSubmit): WaddleExtensionCommandResult {
        submitCalls += call
        submitFailure?.let { throw it }
        return submitResult
    }

    private companion object {
        fun completedResult() = WaddleExtensionCommandResult(
            status = WaddleAdhocStatus.COMPLETED,
            sessionId = null,
            actions = emptyList(),
            form = null,
            notes = emptyList(),
        )
    }
}

/** Concurrency-safe recorder and outcome controls for message sends. */
class FakeMessageSends {
    val calls = CopyOnWriteArrayList<Pair<String, String>>()
    val options = CopyOnWriteArrayList<WaddleSendOptions?>()
    val outcomes = ConcurrentLinkedDeque<WaddleSendMessageOutcome>()

    @Volatile
    var outcome: WaddleSendMessageOutcome = WaddleSendMessageOutcome.Sent("sent-1")

    @Volatile
    var stall: CompletableDeferred<Unit>? = null

    suspend fun send(
        targetJid: String,
        body: String,
        sendOptions: WaddleSendOptions?,
    ): WaddleSendMessageOutcome {
        calls += targetJid to body
        options += sendOptions
        stall?.await()
        return outcomes.pollFirst() ?: outcome
    }
}

val FakeWaddleClient.sendCalls: CopyOnWriteArrayList<Pair<String, String>> get() = messageSends.calls
val FakeWaddleClient.sendOptions: CopyOnWriteArrayList<WaddleSendOptions?> get() = messageSends.options
val FakeWaddleClient.sendOutcomes: ConcurrentLinkedDeque<WaddleSendMessageOutcome> get() = messageSends.outcomes
var FakeWaddleClient.sendOutcome: WaddleSendMessageOutcome
    get() = messageSends.outcome
    set(value) {
        messageSends.outcome = value
    }
var FakeWaddleClient.sendMessageStall: CompletableDeferred<Unit>?
    get() = messageSends.stall
    set(value) {
        messageSends.stall = value
    }

/**
 * Connect/disconnect no-op; everything unused by the manager rejects.
 * Recorders are concurrency-safe: instrumentation tests poll them from
 * the test thread while the session manager mutates them on its own
 * dispatcher. Cohesive verb families live in sliced-out fakes.
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

    val messageSends = FakeMessageSends()

    override suspend fun connect() {
        connectCalls += 1
    }

    override suspend fun disconnect() {
        disconnectCalls += 1
    }

    /** Topology fake state: canned result, call count, failure knob. */
    val topology = FakeTopologyState()

    override suspend fun discoverTopology(): WaddleTopology = topology.discoverTopology()

    /** Group-DM fake state: verb recorders and failure knobs. */
    val groupDm = FakeGroupDmState()

    override suspend fun createGroupDm(name: String, memberJids: List<String>): String =
        groupDm.createGroupDm(name, memberJids)

    override suspend fun renameGroupDm(roomJid: String, name: String?) =
        groupDm.renameGroupDm(roomJid, name)

    override suspend fun leaveGroupDm(roomJid: String) = groupDm.leaveGroupDm(roomJid)

    override suspend fun inviteToGroupDm(roomJid: String, inviteeJid: String, fullHistory: Boolean) =
        groupDm.inviteToGroupDm(roomJid, inviteeJid, fullHistory)

    /** Every recorded `sendCall*` wire verb, in send order. */
    val callVerbs = CopyOnWriteArrayList<RecordedCallVerb>()

    /** Canned result for the bool-returning call verbs. */
    @Volatile
    var callVerbResult = true

    /** Canned outcome for [sendCallSessionTerminateWithOutcome]. */
    @Volatile
    var callTerminateOutcome: WaddleCallSessionTerminateOutcome =
        WaddleCallSessionTerminateOutcome.OK

    /**
     * Virtual-time stall before [sendCallSessionTerminateWithOutcome]
     * answers — models the FFI's IQ timeout on a dead network.
     */
    @Volatile
    var callTerminateDelayMillis = 0L

    /**
     * Virtual-time stall before [sendCallProceed] answers — lets tests
     * land a concurrent action while a proceed is in flight.
     */
    @Volatile
    var callProceedDelayMillis = 0L

    /** Deterministic gate for a normal call signal holding the transport fence. */
    @Volatile
    var callProposeStall: CompletableDeferred<Unit>? = null

    /**
     * Virtual-time stall before [sendCallRetractTieBreak] answers —
     * lets tests land an inbound event mid-tie-break.
     */
    @Volatile
    var callRetractTieBreakDelayMillis = 0L

    /** Canned XEP-0215 services served by [fetchExternalServices]. */
    @Volatile
    var externalServices: List<WaddleExternalService> = emptyList()

    override suspend fun sendCallFinish(peerFullJid: String, sid: String): Boolean {
        callVerbs += RecordedCallVerb.Finish(peerFullJid, sid)
        return callVerbResult
    }

    override suspend fun sendCallFinishWithReason(
        peerFullJid: String,
        sid: String,
        reason: WaddleJingleReason,
    ): Boolean {
        callVerbs += RecordedCallVerb.FinishWithReason(peerFullJid, sid, reason)
        return callVerbResult
    }

    override suspend fun sendCallFinishMigrated(
        peerFullJid: String,
        oldSid: String,
        newSid: String,
    ): Boolean {
        callVerbs += RecordedCallVerb.FinishMigrated(peerFullJid, oldSid, newSid)
        return callVerbResult
    }

    override suspend fun sendCallProceed(peerFullJid: String, sid: String): Boolean {
        if (callProceedDelayMillis > 0) delay(callProceedDelayMillis)
        callVerbs += RecordedCallVerb.Proceed(peerFullJid, sid)
        return callVerbResult
    }

    override suspend fun sendCallPropose(
        peerBareJid: String,
        sid: String,
        audio: Boolean,
        video: Boolean,
    ): Boolean {
        callVerbs += RecordedCallVerb.Propose(peerBareJid, sid, audio, video)
        callProposeStall?.await()
        return callVerbResult
    }

    override suspend fun sendCallReject(peerFullJid: String, sid: String): Boolean {
        callVerbs += RecordedCallVerb.Reject(peerFullJid, sid)
        return callVerbResult
    }

    override suspend fun sendCallRejectTieBreak(peerFullJid: String, sid: String): Boolean {
        callVerbs += RecordedCallVerb.RejectTieBreak(peerFullJid, sid)
        return callVerbResult
    }

    override suspend fun sendCallRetract(peerBareJid: String, sid: String): Boolean {
        callVerbs += RecordedCallVerb.Retract(peerBareJid, sid)
        return callVerbResult
    }

    override suspend fun sendCallRetractTieBreak(peerFullJid: String, sid: String): Boolean {
        if (callRetractTieBreakDelayMillis > 0) delay(callRetractTieBreakDelayMillis)
        callVerbs += RecordedCallVerb.RetractTieBreak(peerFullJid, sid)
        return callVerbResult
    }

    override suspend fun sendCallRinging(peerBareJid: String, sid: String): Boolean {
        callVerbs += RecordedCallVerb.Ringing(peerBareJid, sid)
        return callVerbResult
    }

    override suspend fun sendCallSessionAccept(
        peerFullJid: String,
        responderFullJid: String,
        sid: String,
        audio: Boolean,
        video: Boolean,
    ): Boolean {
        callVerbs += RecordedCallVerb.SessionAccept(peerFullJid, responderFullJid, sid, audio, video)
        return callVerbResult
    }

    override suspend fun sendCallSessionInitiate(
        peerFullJid: String,
        initiatorFullJid: String,
        sid: String,
        audio: Boolean,
        video: Boolean,
    ): Boolean {
        callVerbs += RecordedCallVerb.SessionInitiate(peerFullJid, initiatorFullJid, sid, audio, video)
        return callVerbResult
    }

    override suspend fun sendCallSessionTerminate(
        peerFullJid: String,
        sid: String,
        reason: WaddleJingleReason?,
    ): Boolean {
        callVerbs += RecordedCallVerb.SessionTerminate(peerFullJid, sid, reason)
        return callVerbResult
    }

    override suspend fun sendCallSessionTerminateWithOutcome(
        peerFullJid: String,
        sid: String,
        reason: WaddleJingleReason?,
    ): WaddleCallSessionTerminateOutcome {
        if (callTerminateDelayMillis > 0) delay(callTerminateDelayMillis)
        callVerbs += RecordedCallVerb.SessionTerminateWithOutcome(peerFullJid, sid, reason)
        return callTerminateOutcome
    }

    /** Set to fail the Unit-returning Muji FFI verbs (they throw on failure). */
    @Volatile
    var mujiSessionInitiateFailure: Throwable? = null

    /**
     * Per-call virtual-time stalls before [updateMujiPresence] answers,
     * consumed in send order — lets tests land a concurrent action
     * while ONE specific muji presence publication is in flight (the
     * proceed/terminate delay hooks' presence twin).
     */
    val updateMujiPresenceDelaysMillis = ConcurrentLinkedDeque<Long>()

    @Volatile
    var mujiSessionTerminateFailure: Throwable? = null

    /**
     * Per-call virtual-time stalls before [sendMujiSessionTerminate]
     * answers, consumed in send order — models the mixer IQ round-trip
     * so tests can land a concurrent action mid-terminate.
     */
    val mujiSessionTerminateDelaysMillis = ConcurrentLinkedDeque<Long>()

    @Volatile
    var updateMujiPresenceFailure: Throwable? = null

    override suspend fun sendMujiSessionInitiate(
        roomJid: String,
        initiatorFullJid: String,
        sid: String,
        video: Boolean,
    ) {
        callVerbs += RecordedCallVerb.MujiSessionInitiate(roomJid, initiatorFullJid, sid, video)
        mujiSessionInitiateFailure?.let { throw it }
    }

    override suspend fun sendMujiSessionTerminate(roomJid: String, sid: String) {
        mujiSessionTerminateDelaysMillis.pollFirst()?.let { if (it > 0) delay(it) }
        callVerbs += RecordedCallVerb.MujiSessionTerminate(roomJid, sid)
        mujiSessionTerminateFailure?.let { throw it }
    }

    override suspend fun updateMujiPresence(
        roomJid: String,
        nick: String,
        active: Boolean,
        preparing: Boolean,
        video: Boolean,
        flags: WaddleInCallPresenceFlags,
    ) {
        updateMujiPresenceDelaysMillis.pollFirst()?.let { if (it > 0) delay(it) }
        callVerbs += RecordedCallVerb.UpdateMujiPresence(
            roomJid = roomJid,
            nick = nick,
            active = active,
            preparing = preparing,
            video = video,
            handRaised = flags.handRaised,
            muted = flags.muted,
        )
        updateMujiPresenceFailure?.let { throw it }
    }

    override suspend fun fetchExternalServices(): List<WaddleExternalService> = externalServices

    /** Canned XEP-0363 discovery answer; `null` means no upload service. */
    @Volatile
    var discoveredUploadService: String? = null

    @Volatile
    var discoverUploadServiceCalls = 0

    /** Recorded upload-slot requests; an absent service must leave this empty. */
    val uploadSlotCalls = CopyOnWriteArrayList<UploadSlotCall>()

    @Volatile
    var uploadSlotResult: WaddleUploadSlot? = null

    override suspend fun discoverUploadService(): String? {
        discoverUploadServiceCalls += 1
        return discoveredUploadService
    }

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

    /** Every recorded profile-publishing wire verb, in send order. */
    val profileVerbs = CopyOnWriteArrayList<RecordedProfileVerb>()

    /** Thrown by every profile publish/retract verb when set (the FFI
     *  signals refusal by throwing `WaddleException`). */
    @Volatile
    var profileVerbFailure: Throwable? = null

    /**
     * Virtual-time stall before every profile verb answers — lets a
     * test land a logout/relogin while a publish ack is in flight
     * (the session-generation store-write gate).
     */
    @Volatile
    var profileVerbDelayMillis = 0L

    private suspend fun profileVerbStall() {
        if (profileVerbDelayMillis > 0) delay(profileVerbDelayMillis)
    }

    /** Canned vCard4 served by [fetchVcard4]; recorded jids. */
    @Volatile
    var vcard4: WaddleVCard4? = null

    @Volatile
    var fetchVcard4Failure: Throwable? = null

    val fetchVcard4Calls = CopyOnWriteArrayList<String>()

    /** Canned PEP status snapshot served by [fetchUserPepProfile]. */
    @Volatile
    var pepProfile: WaddlePepProfile = WaddlePepProfile(mood = null, activity = null, tune = null)

    val fetchPepProfileCalls = CopyOnWriteArrayList<String>()

    /** Canned XEP-0084 avatar served by [requestAvatar]; recorded jids. */
    @Volatile
    var avatar: WaddleAvatar? = null

    /** Recorded (jid, knownIds) avatar fetches. */
    val requestAvatarCalls = CopyOnWriteArrayList<Pair<String, List<String>>>()

    /** Raw payload of the most recent [publishAvatar] call. */
    @Volatile
    var publishedAvatarBytes: ByteArray? = null

    override suspend fun requestAvatar(jid: String, knownIds: List<String>): WaddleAvatarResult? {
        requestAvatarCalls += jid to knownIds
        val current = avatar ?: return null
        // Mirror the FFI's §4.2 contract: a known advertised id answers
        // id-only (no data fetch); an unknown id carries the bytes.
        return if (current.id in knownIds) {
            WaddleAvatarResult(id = current.id, avatar = null)
        } else {
            WaddleAvatarResult(id = current.id, avatar = current)
        }
    }

    override suspend fun fetchVcard4(jid: String): WaddleVCard4? {
        fetchVcard4Calls += jid
        fetchVcard4Failure?.let { throw it }
        return vcard4
    }

    /**
     * Virtual-time stall inside [fetchUserPepProfile] — lets tests
     * retire the session while a profile load is parked mid-flight.
     */
    @Volatile
    var fetchPepProfileDelayMillis = 0L

    override suspend fun fetchUserPepProfile(jid: String): WaddlePepProfile {
        if (fetchPepProfileDelayMillis > 0) delay(fetchPepProfileDelayMillis)
        fetchPepProfileCalls += jid
        return pepProfile
    }

    override suspend fun publishVcard4(vcard: WaddleVCard4) {
        profileVerbStall()
        profileVerbs += RecordedProfileVerb.PublishVcard4(vcard)
        profileVerbFailure?.let { throw it }
    }

    override suspend fun publishAvatar(data: ByteArray, mimeType: String, width: UInt, height: UInt) {
        profileVerbStall()
        publishedAvatarBytes = data
        profileVerbs += RecordedProfileVerb.PublishAvatar(data.size, mimeType, width, height)
        profileVerbFailure?.let { throw it }
    }

    override suspend fun disableAvatar() {
        profileVerbStall()
        profileVerbs += RecordedProfileVerb.DisableAvatar
        profileVerbFailure?.let { throw it }
    }

    override suspend fun publishMood(kind: String, text: String?) {
        profileVerbStall()
        profileVerbs += RecordedProfileVerb.PublishMood(kind, text)
        profileVerbFailure?.let { throw it }
    }

    override suspend fun retractMood() {
        profileVerbStall()
        profileVerbs += RecordedProfileVerb.RetractMood
        profileVerbFailure?.let { throw it }
    }

    override suspend fun publishActivity(general: String, specific: String?, text: String?) {
        profileVerbStall()
        profileVerbs += RecordedProfileVerb.PublishActivity(general, specific, text)
        profileVerbFailure?.let { throw it }
    }

    override suspend fun retractActivity() {
        profileVerbStall()
        profileVerbs += RecordedProfileVerb.RetractActivity
        profileVerbFailure?.let { throw it }
    }

    override suspend fun publishTune(tune: WaddleTune) {
        profileVerbStall()
        profileVerbs += RecordedProfileVerb.PublishTune(tune)
        profileVerbFailure?.let { throw it }
    }

    override suspend fun retractTune() {
        profileVerbStall()
        profileVerbs += RecordedProfileVerb.RetractTune
        profileVerbFailure?.let { throw it }
    }
    override suspend fun requestUploadSlot(
        serviceJid: String,
        filename: String,
        size: ULong,
        contentType: String,
    ): WaddleUploadSlot? {
        uploadSlotCalls += UploadSlotCall(serviceJid, filename, size, contentType)
        return uploadSlotResult
    }

    override suspend fun sendChatMessage(
        peerJid: String,
        body: String,
        options: WaddleSendOptions?,
    ): WaddleSendMessageOutcome = messageSends.send(peerJid, body, options)

    override suspend fun sendGroupchatMessage(
        roomJid: String,
        body: String,
        options: WaddleSendOptions?,
    ): WaddleSendMessageOutcome = messageSends.send(roomJid, body, options)
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

    /** Deterministic gate for correction interleavings with logout. */
    @Volatile
    var correctionStall: CompletableDeferred<Unit>? = null

    override suspend fun sendCorrection(
        peerJid: String,
        targetId: String,
        newBody: String,
        isMuc: Boolean,
        options: WaddleSendOptions?,
    ): WaddleSendMessageOutcome {
        correctionCalls += Triple(peerJid, targetId, newBody)
        correctionStall?.await()
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

    /** Recorded (roomJid, targetStanzaId, reason) XEP-0425 moderations. */
    val moderationCalls = CopyOnWriteArrayList<Triple<String, String, String?>>()

    @Volatile
    var moderationResult = true

    override suspend fun sendModeration(roomJid: String, targetStanzaId: String, reason: String?): Boolean {
        moderationCalls += Triple(roomJid, targetStanzaId, reason)
        return moderationResult
    }

    /**
     * Canned §9.5 member lists per affiliation tier; a tier mapped to
     * `null` (or [memberListFailure] set) throws, mirroring per-tier
     * `forbidden` responses.
     */
    @Volatile
    var roomMembersByTier: Map<WaddleMucAffiliation, List<WaddleRoomMemberEntry>> = emptyMap()

    @Volatile
    var memberListFailure: Throwable? = null

    /** Recorded (roomJid, affiliation) member-list queries. */
    val listMembersCalls = CopyOnWriteArrayList<Pair<String, WaddleMucAffiliation>>()

    override suspend fun listRoomMembers(
        roomJid: String,
        affiliation: WaddleMucAffiliation,
    ): List<WaddleRoomMemberEntry> {
        listMembersCalls += roomJid to affiliation
        memberListFailure?.let { throw it }
        return roomMembersByTier[affiliation] ?: emptyList()
    }

    /** Recorded (roomJid, targetJid, affiliation, reason) affiliation sets. */
    val setAffiliationCalls = CopyOnWriteArrayList<List<Any?>>()

    @Volatile
    var setAffiliationFailure: Throwable? = null

    override suspend fun setRoomAffiliation(
        roomJid: String,
        targetJid: String,
        affiliation: WaddleMucAffiliation,
        reason: String?,
    ) {
        setAffiliationCalls += listOf(roomJid, targetJid, affiliation, reason)
        setAffiliationFailure?.let { throw it }
    }

    /** Recorded (roomJid, nick, reason) §8.2 kicks. */
    val kickCalls = CopyOnWriteArrayList<Triple<String, String, String?>>()

    @Volatile
    var kickFailure: Throwable? = null

    override suspend fun kickOccupant(roomJid: String, nick: String, reason: String?) {
        kickCalls += Triple(roomJid, nick, reason)
        kickFailure?.let { throw it }
    }

    /** Canned §10.2 owner config served by [fetchRoomConfig]. */
    @Volatile
    var roomConfig: WaddleRoomConfig = WaddleRoomConfig(
        name = null,
        description = null,
        membersOnly = null,
        publicRoom = null,
        moderated = null,
        forum = null,
        pinPermission = null,
    )

    @Volatile
    var fetchRoomConfigFailure: Throwable? = null

    override suspend fun fetchRoomConfig(roomJid: String): WaddleRoomConfig {
        fetchRoomConfigFailure?.let { throw it }
        return roomConfig
    }

    /** Recorded (roomJid, patch) §10.2 config submits. */
    val submitConfigCalls = CopyOnWriteArrayList<Pair<String, WaddleRoomConfigPatch>>()

    @Volatile
    var submitConfigFailure: Throwable? = null

    override suspend fun submitRoomConfig(roomJid: String, patch: WaddleRoomConfigPatch) {
        submitConfigCalls += roomJid to patch
        submitConfigFailure?.let { throw it }
    }

    /** Recorded (localpart, nick, patch) §10.1 creations. */
    val createRoomCalls = CopyOnWriteArrayList<Triple<String, String, WaddleRoomConfigPatch>>()

    @Volatile
    var createRoomFailure: Throwable? = null

    /** Domain the fake muc service appends to created localparts. */
    @Volatile
    var createRoomDomain: String = "muc.waddle.test"

    override suspend fun createRoom(localpart: String, nick: String, patch: WaddleRoomConfigPatch): String {
        createRoomCalls += Triple(localpart, nick, patch)
        createRoomFailure?.let { throw it }
        return "$localpart@$createRoomDomain"
    }

    /** Recorded (roomJid, reason) §10.9 destroys. */
    val destroyRoomCalls = CopyOnWriteArrayList<Pair<String, String?>>()

    @Volatile
    var destroyRoomFailure: Throwable? = null

    override suspend fun destroyRoom(roomJid: String, reason: String?) {
        destroyRoomCalls += roomJid to reason
        destroyRoomFailure?.let { throw it }
    }

    /** Directory fake state: XEP-0055 search + community-admin gate. */
    val directory = FakeDirectoryState()

    override suspend fun searchUsers(query: String): List<WaddleUserSearchEntry> =
        directory.searchUsers(query)

    override suspend fun isCommunityOwner(): Boolean = directory.communityOwner

    override suspend fun adminUsersList(
        prefix: String?,
        pageSize: UInt?,
        afterCursor: String?,
    ): WaddleAdminUsersPage = directory.adminUsersList(prefix, pageSize, afterCursor)

    override suspend fun adminSpacesList(args: WaddleAdminSpacesListArgs): WaddleAdminSpacesListPage = unused()
    override suspend fun adminSpacesCreate(args: WaddleAdminSpacesCreateArgs): WaddleAdminSpaceRef = unused()
    override suspend fun adminSpacesUpdate(args: WaddleAdminSpacesUpdateArgs): WaddleAdminSpaceRef = unused()
    override suspend fun adminSpacesDelete(spaceJid: String, spaceNode: String?) = unused()
    override suspend fun adminSpacesMembers(args: WaddleAdminSpacesMembersArgs): WaddleAdminSpacesMembersPage =
        unused()

    override suspend fun adminSpacesSetRole(
        spaceJid: String,
        spaceNode: String?,
        memberJid: String,
        role: WaddleSpaceRole,
    ): WaddleAdminSpacesSetRoleResult = unused()

    override suspend fun adminChannelsList(args: WaddleAdminChannelsListArgs): WaddleAdminChannelsListPage =
        unused()

    override suspend fun adminChannelsCreate(args: WaddleAdminChannelsCreateArgs): WaddleAdminChannelRef =
        unused()

    override suspend fun adminChannelsUpdate(args: WaddleAdminChannelsUpdateArgs): WaddleAdminChannelRef =
        unused()

    override suspend fun adminChannelsDelete(channelJid: String) = unused()
    override suspend fun adminChannelsOccupants(
        args: WaddleAdminChannelsOccupantsArgs,
    ): social.waddle.client.ffi.WaddleAdminChannelsOccupantsPage = unused()

    override suspend fun adminChannelsAffiliations(
        args: WaddleAdminChannelsAffiliationsArgs,
    ): WaddleAdminChannelsAffiliationsPage = unused()

    override suspend fun adminChannelsSetAffiliation(
        channelJid: String,
        memberJid: String,
        affiliation: WaddleMucAffiliation,
        reason: String?,
    ): WaddleAdminChannelsSetAffiliationResult = unused()

    override suspend fun adminChannelsKick(
        channelJid: String,
        occupantJid: String,
        reason: String?,
    ): WaddleAdminChannelsKickResult = unused()

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

    /** XEP-0430 fake verb state: canned page, recorded calls, knobs. */
    val inbox = FakeInboxState()

    override suspend fun fetchInbox(onlyUnread: Boolean, noMessages: Boolean): WaddleInboxResult =
        inbox.fetchInbox(onlyUnread, noMessages)

    override suspend fun markInboxRead(partnerJid: String, threadId: String?) =
        inbox.markInboxRead(partnerJid, threadId)

    /** Canned lookup outcome served by [lookupLinkPreview]; recorded calls. */
    @Volatile
    var linkPreviewLookup: WaddleLinkPreviewLookup =
        WaddleLinkPreviewLookup(status = WaddleLinkPreviewLookupStatus.FAILED, preview = null)
    val linkPreviewLookupCalls = CopyOnWriteArrayList<Pair<String, String>>()

    override suspend fun lookupLinkPreview(url: String, scopeJid: String): WaddleLinkPreviewLookup {
        linkPreviewLookupCalls += url to scopeJid
        return linkPreviewLookup
    }

    /** XEP-0449 sticker verbs, sliced out into [FakeStickerVerbs]. */
    val stickers = FakeStickerVerbs()

    override suspend fun fetchStickerPacks(ownerJid: String?): List<WaddleStickerPack> =
        stickers.fetchAll(ownerJid)

    override suspend fun fetchStickerPack(
        ownerJid: String,
        node: String?,
        packId: String,
    ): WaddleStickerPack? = stickers.fetchOne(ownerJid, packId)

    override suspend fun publishStickerPack(pack: WaddleStickerPack): String = stickers.publish(pack)

    override suspend fun retractStickerPack(packId: String): Boolean = stickers.retract(packId)

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

    /** XEP-0492 notify-mode verbs, sliced out into [FakeNotifyVerbs]. */
    val notify = FakeNotifyVerbs()

    override suspend fun setRoomNotificationMode(
        roomJid: String,
        mode: WaddleNotifyMode,
        name: String?,
        richPayloadOptIn: Boolean,
    ): WaddleSetRoomNotificationModeOutcome = notify.setRoomMode(roomJid, mode, name, richPayloadOptIn)

    override suspend fun setDmNotificationMode(
        dmJid: String,
        mode: WaddleNotifyMode,
        richPayloadOptIn: Boolean,
    ): WaddleSetDmNotificationModeOutcome = notify.setDmMode(dmJid, mode, richPayloadOptIn)

    /** Extension-command verbs, sliced out into [FakeExtensionCommandVerbs]. */
    val extensionCommands = FakeExtensionCommandVerbs()

    override suspend fun discoverExtensionCommands(): List<WaddleExtensionCommand> =
        extensionCommands.discover()

    override suspend fun invokeExtensionCommand(
        serviceJid: String,
        node: String,
        roomJid: String?,
    ): WaddleExtensionCommandResult = extensionCommands.invoke(serviceJid, node, roomJid)

    override suspend fun submitExtensionCommandForm(
        serviceJid: String,
        node: String,
        sessionId: String?,
        fields: List<WaddleExtensionFormField>,
        action: WaddleAdhocAction,
        roomJid: String?,
    ): WaddleExtensionCommandResult = extensionCommands.submit(
        RecordedExtensionSubmit(serviceJid, node, sessionId, fields, action, roomJid),
    )

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
