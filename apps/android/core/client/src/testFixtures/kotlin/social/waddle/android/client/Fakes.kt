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
import social.waddle.client.ffi.WaddleBookmarkItem
import social.waddle.client.ffi.WaddleCallSessionTerminateOutcome
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleConfig
import social.waddle.client.ffi.WaddleDmBookmarkItem
import social.waddle.client.ffi.WaddleEventListener
import social.waddle.client.ffi.WaddleExternalService
import social.waddle.client.ffi.WaddleJingleReason
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleMdsDisplayedEntry
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleNotifyMode
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
import social.waddle.client.ffi.WaddleTopology
import social.waddle.client.ffi.WaddleUploadSlot
import social.waddle.client.ffi.WaddleUserSearchEntry
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

/** One recorded [FakeWaddleClient] full-text search query. */
data class SearchCall(
    val conversationJid: String,
    val query: String,
    val maxMessages: UInt,
    val isRoom: Boolean,
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

    override suspend fun fetchExternalServices(): List<WaddleExternalService> = externalServices

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

    /** Canned XEP-0055 hits; recorded queries. */
    @Volatile
    var userSearchResults: List<WaddleUserSearchEntry> = emptyList()
    val searchUsersCalls = CopyOnWriteArrayList<String>()

    override suspend fun searchUsers(query: String): List<WaddleUserSearchEntry> {
        searchUsersCalls += query
        return userSearchResults
    }

    /** Canned owner-probe answer + V1 users page. */
    @Volatile
    var communityOwner = false

    @Volatile
    var adminUsersPage: WaddleAdminUsersPage = WaddleAdminUsersPage(entries = emptyList(), nextCursor = null)

    /** Recorded (prefix, pageSize, afterCursor) users-list queries. */
    val adminUsersListCalls = CopyOnWriteArrayList<Triple<String?, UInt?, String?>>()

    override suspend fun isCommunityOwner(): Boolean = communityOwner

    override suspend fun adminUsersList(
        prefix: String?,
        pageSize: UInt?,
        afterCursor: String?,
    ): WaddleAdminUsersPage {
        adminUsersListCalls += Triple(prefix, pageSize, afterCursor)
        return adminUsersPage
    }

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

    private fun unused(): Nothing = throw UnsupportedOperationException("not exercised by the session manager")
}
