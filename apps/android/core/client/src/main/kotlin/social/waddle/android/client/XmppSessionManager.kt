package social.waddle.android.client

import kotlinx.coroutines.CancellationException
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
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.calls.CallState
import social.waddle.android.client.calls.CallStore
import social.waddle.android.client.calls.ClientCallSignaling
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ConnectionLoop
import social.waddle.android.client.session.ResumePersistence
import social.waddle.android.client.session.SessionCatchup
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleAdminUsersPage
import social.waddle.client.ffi.WaddleAvatar
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleLinkPreviewLookup
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleNotifyMode
import social.waddle.client.ffi.WaddleRoomConfig
import social.waddle.client.ffi.WaddleRoomConfigPatch
import social.waddle.client.ffi.WaddleTune
import social.waddle.client.ffi.WaddleUploadSlot
import social.waddle.client.ffi.WaddleUserSearchEntry
import social.waddle.client.ffi.WaddleVCard4

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
    val inboxStore = stores.inboxStore
    val chatStateStore = stores.chatStateStore
    val readCursorStore = stores.readCursorStore
    val pinStore = stores.pinStore
    val notifySettingsStore = stores.notifySettingsStore
    val roomMembersStore = stores.roomMembersStore
    val stickerPackStore = stores.stickerPackStore
    val profileStore = stores.profileStore
    val extensionCommandStore = stores.extensionCommandStore
    val feedStore = stores.feedStore

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
        mucSessionCache = sessionPrefs.mucCallSessions,
    )

    private val router: XmppEventRouter =
        XmppEventRouter(activeSession, stores, resume, readState, callStore) { peer, timestamp ->
            persistDmSeen(peer, timestamp)
        }

    private val messenger = OutboundMessenger(activeSession, stores, sessionPrefs, router::dispatch)

    private val verbs = ConversationVerbs(activeSession, stores, sessionPrefs)

    private val roomAdmin = RoomAdminVerbs(activeSession, stores)

    private val stickers = StickerVerbs(activeSession, stores)
    private val profile = ProfileVerbs(activeSession, stores)
    private val extensions = ExtensionCommandVerbs(activeSession, stores)
    private val feed = FeedVerbs(activeSession, stores)

    private val catchup =
        SessionCatchup(sessionPrefs, stores, resume, verbs, messenger, readState, activeSession)

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

    /**
     * The live attempt's bound FULL JID (account bare JID + resource) —
     * the identity the XEP-0272 group-call verbs key preparation echoes
     * and the session cache on; `null` before the first connect.
     */
    fun ownFullJid(): String? = activeSession.ownFullJid

    /** Persist the session and start the connection loop. */
    suspend fun login(session: WaddleSessionInfo) = lifecycleMutex.withLock {
        cancelSessionScope()
        // BEFORE the store clear (logout parity): a parked verb ack
        // from the previous session resuming between the clear and the
        // bump would pass its generation check and write stale state
        // into the freshly seeded stores.
        activeSession.advanceGeneration()
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
        // Best-effort call teardown BEFORE the stream closes (web
        // client.ts disconnect parity): the peer must get the
        // retract/reject/terminate + XEP-0353 <finish/> bookend instead
        // of ringing into a dead session until their timeout. Bounded:
        // on a silently-dead network the terminate IQ only fails after
        // the FFI's full 30 s timeout, and sign-out must not hold
        // [lifecycleMutex] that long. Cancellation must propagate —
        // swallowing it would run the teardown below inside an
        // already-cancelled coroutine and abort it halfway.
        if (callStore.state.value != CallState.Idle) {
            try {
                withTimeoutOrNull(LOGOUT_CALL_TEARDOWN_MILLIS) { callStore.hangUp() }
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                // Best-effort: sign-out proceeds even if teardown fails.
            }
        }
        cancelSessionScope()
        activeSession.advanceGeneration()
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

    /** Composer `urn:waddle:link-preview:0` lookup; `null` when offline. */
    suspend fun lookupLinkPreview(url: String, scopeJid: String): WaddleLinkPreviewLookup? =
        verbs.lookupLinkPreview(url, scopeJid)

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

    /**
     * XEP-0430 server-side mark-read for one conversation (optionally
     * one room thread). The displayed path co-fires this automatically;
     * exposed for callers without a displayed target (thread reads).
     */
    suspend fun markInboxRead(conversationJid: String, threadId: String? = null) =
        readState.markInboxRead(conversationJid, threadId)

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

    /** XEP-0425: ask the room to moderate (remove) another user's message. */
    suspend fun sendModeration(roomJid: String, targetStanzaId: String, reason: String? = null): VerbResult =
        verbs.sendModeration(roomJid, targetStanzaId, reason)

    /** Refresh a room's §9.5 member list into [roomMembersStore]. */
    suspend fun refreshRoomMembers(roomJid: String) = roomAdmin.refreshRoomMembers(roomJid)

    /** XEP-0045 §5.2 affiliation change (ban = outcast, remove = none). */
    suspend fun setRoomAffiliation(
        roomJid: String,
        targetJid: String,
        affiliation: WaddleMucAffiliation,
        reason: String? = null,
    ): RoomAdminResult = roomAdmin.setRoomAffiliation(roomJid, targetJid, affiliation, reason)

    /** XEP-0045 §8.2 kick by nick (role → none; affiliation kept). */
    suspend fun kickOccupant(roomJid: String, nick: String, reason: String? = null): RoomAdminResult =
        roomAdmin.kickOccupant(roomJid, nick, reason)

    /** XEP-0045 §10.2 owner config fetch; `null` offline / not owner. */
    suspend fun fetchRoomConfig(roomJid: String): WaddleRoomConfig? = roomAdmin.fetchRoomConfig(roomJid)

    /** XEP-0045 §10.2 GET-merge-SET owner config submit. */
    suspend fun submitRoomConfig(roomJid: String, patch: WaddleRoomConfigPatch): RoomAdminResult =
        roomAdmin.submitRoomConfig(roomJid, patch)

    /** XEP-0045 §10.1: create + configure a channel; refreshes topology. */
    suspend fun createRoom(
        name: String,
        nick: String,
        description: String? = null,
        forum: Boolean = false,
    ): CreateRoomResult = roomAdmin.createRoom(name, nick, description, forum)

    /** XEP-0045 §10.9: destroy a room (owner only); refreshes topology. */
    suspend fun destroyRoom(roomJid: String, reason: String? = null): RoomAdminResult =
        roomAdmin.destroyRoom(roomJid, reason)

    /**
     * `urn:waddle:group-dm:create:0`: create a group DM (membership
     * including self), refresh the topology so the bookmarked room is
     * known, then join it as our own nick so live messages flow
     * immediately.
     */
    suspend fun createGroupDm(name: String, memberJids: List<String>): CreateRoomResult {
        val result = roomAdmin.createGroupDm(name, memberJids)
        if (result is CreateRoomResult.Created) {
            activeSession.ownBareJid?.let { own ->
                joinRoom(result.roomJid, own.substringBefore('@'))
            }
        }
        return result
    }

    /** `urn:waddle:group-dm:rename:0`: set (or clear) the display name. */
    suspend fun renameGroupDm(roomJid: String, name: String?): RoomAdminResult =
        roomAdmin.renameGroupDm(roomJid, name)

    /** `urn:waddle:group-dm:leave:0`: leave; the room drops off the DM surface. */
    suspend fun leaveGroupDm(roomJid: String): RoomAdminResult = roomAdmin.leaveGroupDm(roomJid)

    /** XEP-0045 §7.8.2 mediated group-DM invite (history: from-join or full). */
    suspend fun inviteToGroupDm(
        roomJid: String,
        inviteeJid: String,
        fullHistory: Boolean = false,
    ): RoomAdminResult = roomAdmin.inviteToGroupDm(roomJid, inviteeJid, fullHistory)

    /** XEP-0055 user directory search backing the add-member flow. */
    suspend fun searchUsers(query: String): List<WaddleUserSearchEntry>? = roomAdmin.searchUsers(query)

    /** Best-effort community-owner probe gating the admin UI entry. */
    suspend fun isCommunityOwner(): Boolean = roomAdmin.isCommunityOwner()

    /** `urn:waddle:admin:users:list:0` page; `null` offline / refused. */
    suspend fun adminUsersList(
        prefix: String? = null,
        pageSize: UInt? = null,
        afterCursor: String? = null,
    ): WaddleAdminUsersPage? = roomAdmin.adminUsersList(prefix, pageSize, afterCursor)

    /** XEP-0449: load the own PEP sticker packs into [stickerPackStore]. */
    suspend fun loadStickerPacks() = stickers.loadStickerPacks()

    /** XEP-0449: publish a new own sticker pack (id derived FFI-side). */
    suspend fun publishStickerPack(
        name: String,
        summary: String?,
        items: List<StickerItem>,
    ): VerbResult = stickers.publishPack(name, summary, items)

    /** XEP-0449: retract an own sticker pack. */
    suspend fun removeStickerPack(packId: String): VerbResult = stickers.removePack(packId)

    /** Load the account's vCard4 + PEP status + avatar into [profileStore]. */
    suspend fun loadSelfProfile(): VerbResult = profile.loadSelfProfile()

    /** XEP-0292: optimistic vCard4 publish with rollback on failure. */
    suspend fun publishProfile(vcard: WaddleVCard4): VerbResult = profile.publishProfile(vcard)

    /** XEP-0084 §3: publish the account's avatar (cached on success). */
    suspend fun publishAvatar(data: ByteArray, mimeType: String, width: UInt, height: UInt): VerbResult =
        profile.publishAvatar(data, mimeType, width, height)

    /** XEP-0084 §4.3: publish the "no avatar" item and drop the local one. */
    suspend fun disableAvatar(): VerbResult = profile.disableAvatar()

    /** XEP-0084 avatar fetch for any JID, honoring the §4.2 item-id cache. */
    suspend fun fetchAvatar(jid: String, knownId: String? = null): WaddleAvatar? =
        profile.fetchAvatar(jid, knownId)

    /** XEP-0107: publish a mood ([ProfileVerbs.setMood]). */
    suspend fun setMood(kind: String, text: String? = null): VerbResult = profile.setMood(kind, text)

    /** XEP-0107 §2.2: retract the mood. */
    suspend fun clearMood(): VerbResult = profile.clearMood()

    /** XEP-0108: publish an activity ([ProfileVerbs.setActivity]). */
    suspend fun setActivity(general: String, specific: String? = null, text: String? = null): VerbResult =
        profile.setActivity(general, specific, text)

    /** XEP-0108: retract the activity. */
    suspend fun clearActivity(): VerbResult = profile.clearActivity()

    /**
     * XEP-0118 tune publish. User-initiated and immediate (web
     * parity): the XEP's "SHOULD wait several seconds" targets
     * automatic players skipping tracks, and no automatic publisher
     * exists here — a manual form submit publishes right away and the
     * caller gets the real [VerbResult] to surface.
     */
    suspend fun setTune(tune: WaddleTune): VerbResult = profile.publishTune(tune)

    /** XEP-0118 §3.2: retract the tune via the empty payload. */
    suspend fun clearTune(): VerbResult = profile.clearTune()

    /** XEP-0472: fetch the latest community feed page into [feedStore]. */
    suspend fun refreshFeed(): VerbResult = feed.refreshFeed()

    /** XEP-0472: publish a feed post (optimistic prepend, then refresh). */
    suspend fun publishFeedPost(body: String, title: String? = null): VerbResult =
        feed.publishFeedPost(body, title)

    /**
     * `urn:waddle:extension:1`: discover the slash-command set via
     * XEP-0050 disco (cached once per session in
     * [extensionCommandStore]).
     */
    suspend fun discoverExtensionCommands(): List<ExtensionCommand> =
        extensions.discoverExtensionCommands()

    /** XEP-0050 §2.4: start an extension command with `execute`. */
    suspend fun invokeExtensionCommand(
        serviceJid: String,
        node: String,
        roomJid: String? = null,
    ): ExtensionCommandCall = extensions.invokeExtensionCommand(serviceJid, node, roomJid)

    /** XEP-0050 §3: submit a stage of an extension command session. */
    suspend fun submitExtensionCommandForm(
        submission: ExtensionCommandSubmission,
    ): ExtensionCommandCall = extensions.submitExtensionCommandForm(submission)

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
        // Topology discovery now heads the sequential ready pipeline:
        // the bookmark-driven rejoin derives its join set from it.
        attemptScope.launch { catchup.onSessionReady(client, session, freshStream) }
        // Once per connect: retry the XEP-0166 mixer terminates a
        // previous group-call leave still owes (terminate-pending
        // session-cache entries survive process death and reconnects).
        attemptScope.launch { callStore.muc.retryPendingTerminates(activeSession.ownFullJid) }
    }

    private suspend fun onTerminalAuthFailure() {
        _appState.value = WaddleAppState.SignedOut
        // The dead session's in-flight verb acks (caller-scoped, up to
        // the 30 s IQ timeout away) must not write during the signed-out
        // idle period or into the next login's stores.
        activeSession.advanceGeneration()
        // A live call slot must not outlive the session: the shell is
        // about to render the login screen with no in-app hang-up, and
        // the app-scoped collectors (FGS, media, ring notification)
        // tear down off this transition.
        callStore.clear()
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

        /** Sign-out budget for the pre-disconnect call hang-up. */
        const val LOGOUT_CALL_TEARDOWN_MILLIS = 5_000L
    }
}
