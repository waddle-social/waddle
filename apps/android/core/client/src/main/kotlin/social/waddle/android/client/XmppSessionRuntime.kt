package social.waddle.android.client

import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ConnectionAttemptClientFactory
import social.waddle.android.client.session.ConnectionLoop
import social.waddle.android.client.session.ConnectionLoopConfiguration
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
class XmppSessionRuntime private constructor(
    private val sessionPrefs: SessionPrefs,
    clientFactory: ClientFactory,
    networkSignal: NetworkSignal,
    userPrefs: UserPrefs,
    reconnectPolicy: ReconnectPolicy,
    private val dispatcher: CoroutineDispatcher,
    connectTimeoutMillis: Long,
    lifecyclePhaseObserver: OutboundLifecyclePhaseObserver,
    workerExitEvidence: WorkerExitEvidence,
    workerStartHooks: WorkerStartHooks,
) {
    constructor(
        sessionPrefs: SessionPrefs,
        clientFactory: ClientFactory,
        networkSignal: NetworkSignal,
        userPrefs: UserPrefs,
        reconnectPolicy: ReconnectPolicy = ReconnectPolicy(),
        dispatcher: CoroutineDispatcher = Dispatchers.Default,
        connectTimeoutMillis: Long = CONNECT_TIMEOUT_MILLIS,
    ) : this(
        sessionPrefs,
        clientFactory,
        networkSignal,
        userPrefs,
        reconnectPolicy,
        dispatcher,
        connectTimeoutMillis,
        OutboundLifecyclePhaseObserver.NONE,
        WorkerExitExceptionEvidence(),
        WorkerStartHooks.None,
    )

    internal constructor(
        sessionPrefs: SessionPrefs,
        clientFactory: ClientFactory,
        networkSignal: NetworkSignal,
        userPrefs: UserPrefs,
        reconnectPolicy: ReconnectPolicy = ReconnectPolicy(),
        dispatcher: CoroutineDispatcher = Dispatchers.Default,
        connectTimeoutMillis: Long = CONNECT_TIMEOUT_MILLIS,
        workerExitEvidence: WorkerExitEvidence,
    ) : this(
        sessionPrefs,
        clientFactory,
        networkSignal,
        userPrefs,
        reconnectPolicy,
        dispatcher,
        connectTimeoutMillis,
        OutboundLifecyclePhaseObserver.NONE,
        workerExitEvidence,
        WorkerStartHooks.None,
    )

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

    private val runtimeRootJob = SupervisorJob()
    private val runtimeRootScope = CoroutineScope(runtimeRootJob + dispatcher)
    private var nextGeneration = 0L
    @Volatile
    private var activeRuntime: ActiveRuntimeSession? = null

    @Volatile
    private var lifecycleState: RuntimeLifecycleState = RuntimeLifecycleState.Open
    private var pendingLifecycleShutdown:
        LifecycleShutdownOutcome.FencedWithPending? = null

    private val lifecycleMutex = Mutex()

    private val deliveryJournal = DeliveryJournalStore(sessionPrefs)

    private val resume = ResumePersistence(sessionPrefs, deliveryJournal)

    private val activeSession = ActiveSession()

    private val readState: ReadStateCoordinator =
        ReadStateCoordinator(activeSession, stores, userPrefs) { event ->
            router.emit(event)
        }

    private val router: XmppEventRouter =
        XmppEventRouter(activeSession, stores, resume, readState) { peer, timestamp ->
            persistDmSeen(peer, timestamp)
        }

    private val messenger = OutboundMessenger(
        activeSession = activeSession,
        stores = stores,
        journal = deliveryJournal,
        resume = resume,
        dispatchEvent = router::dispatch,
        phaseObserver = lifecyclePhaseObserver,
        workerExitEvidence = workerExitEvidence,
        workerStartHooks = workerStartHooks,
    )

    private val verbs = ConversationVerbs(activeSession, stores, sessionPrefs)

    private val catchup = SessionCatchup(sessionPrefs, stores, resume, verbs, messenger, readState)

    private val loop = ConnectionLoop(
        attemptClientFactory = ConnectionAttemptClientFactory(clientFactory, sessionPrefs),
        networkSignal = networkSignal,
        resume = resume,
        router = router,
        messenger = messenger,
        configuration = ConnectionLoopConfiguration(
            onReady = ::onSessionReady,
            onAuthenticationStopped = ::onAuthenticationStopped,
            reconnectPolicy = reconnectPolicy,
            connectTimeoutMillis = connectTimeoutMillis,
        ),
    )

    val connectionState: StateFlow<ConnectionState> = loop.state

    /** Every domain event, after store fan-out; drops oldest under burst. */
    val events: SharedFlow<XmppEvent> = router.events

    /** Persist the session and start the connection loop. */
    suspend fun login(session: WaddleSessionInfo) = lifecycleMutex.withLock {
        check(lifecycleState == RuntimeLifecycleState.Open) { "XmppSessionRuntime is closed" }
        val generation = reserveRuntimeGeneration()
        loop.stopAdmissions()
        stopAndCancelCurrentSession()
        clearSessionState()
        val ownerBareJid = bareJid(session.jid)
        sessionPrefs.activateSession(ownerBareJid, session.sessionId)
        activeSession.ownBareJid = ownerBareJid
        timelineStore.setOwnBareJid(session.jid)
        persistQuietly { seedStoresFromPrefs() }

        val childJob = SupervisorJob(runtimeRootJob)
        val childScope = CoroutineScope(childJob + dispatcher)
        _appState.value = WaddleAppState.Ready
        val lifecycle = try {
            resume.start(childScope)
            when (val startup = messenger.start(childScope, ownerBareJid)) {
                is LifecycleStartResult.Started -> startup.lifecycle
                is LifecycleStartResult.Failed -> throw LifecycleStartException(startup)
            }
        } catch (failure: Throwable) {
            cancelAndJoinChild(childJob)
            activeSession.ownBareJid = null
            clearSessionState()
            sessionPrefs.clear()
            loop.resetToIdle()
            _appState.value = WaddleAppState.SignedOut
            throw failure
        }
        loop.startAdmissions()
        val loopJob = childScope.launch(start = CoroutineStart.LAZY) { loop.run(session, lifecycle) }
        activeRuntime = ActiveRuntimeSession(generation, childScope, childJob, loopJob, lifecycle)
        loopJob.start()
        childScope.launch { router.sweepChatStates() }
    }

    /** Disconnect, cancel the loop, and wipe session persistence. */
    suspend fun logout() = lifecycleMutex.withLock {
        loop.stopAdmissions()
        stopAndCancelCurrentSession()
        activeSession.ownBareJid = null
        clearSessionState()
        sessionPrefs.clear()
        loop.resetToIdle()
        _appState.value = WaddleAppState.SignedOut
    }

    /** Terminal, bounded shutdown. A closed runtime cannot be logged in again. */
    suspend fun close() = lifecycleMutex.withLock {
        if (lifecycleState == RuntimeLifecycleState.Closed) return@withLock
        lifecycleState = RuntimeLifecycleState.Closing
        loop.stopAdmissions()
        stopAndCancelCurrentSession()
        activeSession.ownBareJid = null
        clearSessionState()
        sessionPrefs.clear()
        loop.resetToIdle()
        _appState.value = WaddleAppState.SignedOut
        withTimeoutOrNull(SHUTDOWN_TIMEOUT_MILLIS) {
            runtimeRootJob.cancel()
            runtimeRootJob.join()
        } ?: error("runtime root scope did not quiesce within the shutdown bound")
        lifecycleState = RuntimeLifecycleState.Closed
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

    /**
     * Process-death notification reply. [expectedOwnerBareJid] is part of
     * the PendingIntent identity and must still be the authenticated owner.
     */
    suspend fun sendDirectReply(
        expectedOwnerBareJid: String,
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
    ): SendResult = lifecycleMutex.withLock {
        messenger.sendOrEnqueue(
            conversationJid = conversationJid,
            isGroupchat = isGroupchat,
            body = body,
            expectedOwnerBareJid = expectedOwnerBareJid,
            source = DeliverySource.DirectReply(conversationJid, isGroupchat),
        )
    }

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

    /**
     * Process-death notification action. The lifecycle lock makes the owner
     * check atomic with the XMPP mutation, so an account-A PendingIntent can
     * never dispatch a marker through account B during login replacement.
     */
    suspend fun markConversationDisplayedForOwner(
        expectedOwnerBareJid: String,
        conversationJid: String,
        isGroupchat: Boolean,
        explicitTarget: DisplayedTarget? = null,
    ): Boolean = lifecycleMutex.withLock {
        if (activeSession.ownBareJid != expectedOwnerBareJid) {
            return@withLock false
        }
        readState.markConversationDisplayed(conversationJid, isGroupchat, explicitTarget)
        true
    }

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
        lifecycle: SessionLifecycleRef,
    ) {
        val generation = activeRuntimeFor(lifecycle)?.generation ?: return
        attemptScope.launch {
            if (!isActiveGeneration(generation, lifecycle)) return@launch
            catchup.refreshTopology(client)
            if (!isActiveGeneration(generation, lifecycle)) return@launch
            catchup.onSessionReady(client, session, freshStream)
        }
    }

    private suspend fun onAuthenticationStopped(
        lifecycle: SessionLifecycleRef,
        disposition: SaslRetryDisposition,
    ) {
        lifecycleMutex.withLock {
            val active = activeRuntimeFor(lifecycle) ?: return@withLock
            loop.stopAdmissions()
            val credentialsInvalid = disposition == SaslRetryDisposition.STOP_CREDENTIAL
            var shutdownComplete = false
            try {
                when (val shutdown = messenger.beginShutdown(lifecycle)) {
                    is BeginShutdownDecision.Begun,
                    is BeginShutdownDecision.AlreadyClosing,
                    -> Unit
                    is BeginShutdownDecision.WorkerFenced ->
                        throw messenger.workerRecoveryException(
                            WorkerRecoveryOutcome.WorkerFenced(shutdown.lifecycle, shutdown.cause),
                        )
                    is BeginShutdownDecision.Stale ->
                        throw messenger.workerRecoveryException(
                            WorkerRecoveryOutcome.OwnershipMismatch(shutdown.requested, shutdown.actual),
                        )
                }
                requireStopped(
                    lifecycle,
                    messenger.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle)),
                )
                shutdownComplete = true
            } finally {
                if (shutdownComplete) {
                    if (credentialsInvalid) {
                        _appState.value = WaddleAppState.SignedOut
                        activeSession.ownBareJid = null
                        clearSessionState()
                    }
                    // This callback is itself in the child scope. Let the
                    // runtime root initiate cancellation after it returns;
                    // an external transition joins this quiescing record.
                    val quiescing = active.copy(quiescing = true)
                    activeRuntime = quiescing
                    runtimeRootScope.launch {
                        cancelAndJoinChild(quiescing.childJob)
                        if (credentialsInvalid) {
                            lifecycleMutex.withLock {
                                if (activeRuntime == quiescing) {
                                    sessionPrefs.clear()
                                    activeRuntime = null
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /** Persist DM-list recency (UI hook and router callback). */
    private fun persistDmSeen(peer: String, timestamp: String) {
        val active = activeRuntime ?: return
        active.childScope.launch {
            if (!isActiveGeneration(active.generation, active.lifecycle)) return@launch
            persistQuietly { sessionPrefs.setLastSeen(peer, timestamp) }
        }
    }

    private suspend fun seedStoresFromPrefs() {
        stores.seedFromPrefs(sessionPrefs)
        resume.seedFromPrefs()
    }

    private suspend fun cancelRuntimeChild(active: ActiveRuntimeSession) = cancelAndJoinChild(active.childJob)

    /** Reserve before any transition so overflow cannot disturb a live runtime. */
    private fun reserveRuntimeGeneration(): RuntimeGeneration {
        check(nextGeneration != Long.MAX_VALUE) { "runtime generation exhausted" }
        return RuntimeGeneration(++nextGeneration)
    }

    private suspend fun cancelAndJoinChild(childJob: Job) {
        val stopped = withTimeoutOrNull(SHUTDOWN_TIMEOUT_MILLIS) {
            childJob.cancel()
            childJob.join()
            true
        } == true
        check(stopped) {
            "runtime child scope did not quiesce within the shutdown bound"
        }
    }

    /**
     * Recover or stop the durable owner before cancelling its node-local scope.
     * A retryable fencing failure retains the lifecycle and scope for retry.
     */
    private suspend fun stopAndCancelCurrentSession() {
        val active = activeRuntime
        val lifecycle = active?.lifecycle
        var teardownSucceeded = false
        try {
            if (lifecycle != null) {
                if (active.quiescing) {
                    teardownSucceeded = true
                    activeRuntime = null
                    return
                }
                if (recoverFencedWorkers(lifecycle)) {
                    teardownSucceeded = true
                    return
                }
                when (val shutdown = messenger.beginShutdown(lifecycle)) {
                    is BeginShutdownDecision.Begun,
                    is BeginShutdownDecision.AlreadyClosing,
                    -> Unit
                    is BeginShutdownDecision.WorkerFenced -> {
                        recoverTeardownFence(shutdown.lifecycle, shutdown.cause)
                        teardownSucceeded = true
                        return
                    }
                    is BeginShutdownDecision.Stale ->
                        throw messenger.workerRecoveryException(
                            WorkerRecoveryOutcome.OwnershipMismatch(shutdown.requested, shutdown.actual),
                        )
                }
                stopLoop(active, lifecycle)
                when (val shutdown = messenger.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))) {
                    LifecycleShutdownOutcome.Stopped,
                    is LifecycleShutdownOutcome.FencedWithPending,
                    LifecycleShutdownOutcome.AttemptClosed,
                    LifecycleShutdownOutcome.Stale,
                    -> requireStopped(lifecycle, shutdown)
                    is LifecycleShutdownOutcome.WorkerFenced -> {
                        recoverTeardownFence(shutdown.lifecycle, shutdown.cause)
                        teardownSucceeded = true
                        return
                    }
                }
                activeRuntime = null
            }
            teardownSucceeded = true
        } finally {
            if (teardownSucceeded && active != null) cancelRuntimeChild(active)
        }
    }

    private suspend fun stopLoop(active: ActiveRuntimeSession, lifecycle: SessionLifecycleRef) {
        active.loopJob.cancel()
        val stopped = withTimeoutOrNull(SHUTDOWN_TIMEOUT_MILLIS) {
            active.loopJob.join()
            true
        } == true
        if (!stopped) {
            throw LifecycleTransitionException(lifecycle, LifecyclePendingComponent.NATIVE_PRODUCER, 1)
        }
    }

    internal fun requireStopped(
        lifecycle: SessionLifecycleRef,
        outcome: LifecycleShutdownOutcome,
    ) {
        when (outcome) {
            LifecycleShutdownOutcome.Stopped -> pendingLifecycleShutdown = null
            is LifecycleShutdownOutcome.FencedWithPending -> {
                pendingLifecycleShutdown = outcome
                throw LifecycleTransitionException(
                    lifecycle,
                    outcome.component,
                    outcome.pending,
                )
            }
            is LifecycleShutdownOutcome.WorkerFenced ->
                throw messenger.workerRecoveryException(
                    WorkerRecoveryOutcome.WorkerFenced(outcome.lifecycle, outcome.cause),
                )
            LifecycleShutdownOutcome.AttemptClosed,
            LifecycleShutdownOutcome.Stale,
            -> error("current owner shutdown lost lifecycle authority")
        }
    }

    private suspend fun recoverFencedWorkers(
        lifecycle: SessionLifecycleRef,
    ): Boolean {
        when (val recovered = messenger.recoverFencedWorkers(lifecycle)) {
            WorkerRecoveryOutcome.NotFenced -> return false
            WorkerRecoveryOutcome.Recovered -> Unit
            is WorkerRecoveryOutcome.DurableCleanupFailed,
            is WorkerRecoveryOutcome.DurableCleanupPending,
            is WorkerRecoveryOutcome.TerminalReceiptCleanupFailed,
            is WorkerRecoveryOutcome.OwnershipMismatch,
            is WorkerRecoveryOutcome.RecoveryInProgress,
            is WorkerRecoveryOutcome.RetainedOperationsPending,
            is WorkerRecoveryOutcome.WorkerFenced,
            is WorkerRecoveryOutcome.WorkerExitPending,
            -> throw messenger.workerRecoveryException(recovered)
        }
        activeRuntime = null
        pendingLifecycleShutdown = null
        return true
    }

    private suspend fun recoverTeardownFence(
        lifecycle: SessionLifecycleRef,
        cause: LifecycleFenceCause,
    ) {
        if (!recoverFencedWorkers(lifecycle)) {
            throw messenger.workerRecoveryException(WorkerRecoveryOutcome.WorkerFenced(lifecycle, cause))
        }
    }

    private fun clearSessionState() {
        stores.clear()
        readState.clearPending()
        resume.clear()
    }

    private fun activeRuntimeFor(lifecycle: SessionLifecycleRef): ActiveRuntimeSession? =
        activeRuntime?.takeIf { it.lifecycle == lifecycle }

    private fun isActiveGeneration(
        generation: RuntimeGeneration,
        lifecycle: SessionLifecycleRef,
    ): Boolean =
        lifecycleState == RuntimeLifecycleState.Open &&
            activeRuntime?.let { it.generation == generation && it.lifecycle == lifecycle && !it.quiescing } == true

    private data class ActiveRuntimeSession(
        val generation: RuntimeGeneration,
        val childScope: CoroutineScope,
        val childJob: Job,
        val loopJob: Job,
        val lifecycle: SessionLifecycleRef,
        val quiescing: Boolean = false,
    )

    private enum class RuntimeLifecycleState { Open, Closing, Closed }

    companion object {
        /** Internal scheduling visibility; production construction is always no-op observed. */
        internal fun withLifecyclePhaseObserver(
            sessionPrefs: SessionPrefs,
            clientFactory: ClientFactory,
            networkSignal: NetworkSignal,
            userPrefs: UserPrefs,
            reconnectPolicy: ReconnectPolicy,
            dispatcher: CoroutineDispatcher,
            lifecyclePhaseObserver: OutboundLifecyclePhaseObserver,
            connectTimeoutMillis: Long = CONNECT_TIMEOUT_MILLIS,
            workerExitEvidence: WorkerExitEvidence,
            workerStartHooks: WorkerStartHooks = WorkerStartHooks.None,
        ): XmppSessionRuntime = XmppSessionRuntime(
            sessionPrefs,
            clientFactory,
            networkSignal,
            userPrefs,
            reconnectPolicy,
            dispatcher,
            connectTimeoutMillis,
            lifecyclePhaseObserver,
            workerExitEvidence,
            workerStartHooks,
        )

        /** Web parity: 15s budget from connect to `SessionReady`. */
        const val CONNECT_TIMEOUT_MILLIS = ConnectionLoop.CONNECT_TIMEOUT_MILLIS
        private const val SHUTDOWN_TIMEOUT_MILLIS = 5_000L

        /** Newest page per conversation on fresh-stream catch-up. */
        const val CATCHUP_PAGE_SIZE = SessionCatchup.CATCHUP_PAGE_SIZE

        /** Incoming-typing expiry tick (XEP-0085 indicator sweep). */
        const val CHAT_STATE_SWEEP_MILLIS = XmppEventRouter.CHAT_STATE_SWEEP_MILLIS

        /** Only the most recently active DMs catch up (rooms: all joined). */
        const val CATCHUP_DM_LIMIT = SessionCatchup.CATCHUP_DM_LIMIT
    }
}
