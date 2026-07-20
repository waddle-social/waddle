package social.waddle.android.client

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import social.waddle.android.client.DeliveryJournalStore.LiveAdmissionResult
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.prefs.toSnapshot
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ConnectionAttemptClientFactory
import social.waddle.android.client.session.ConnectionLoop
import social.waddle.android.client.session.ConnectionLoopConfiguration
import social.waddle.android.client.session.ResumePersistence
import social.waddle.android.client.store.SessionStores
import java.util.concurrent.CopyOnWriteArrayList

internal class ConnectionLoopPullHarness(
    testScope: TestScope,
    val dataStore: FailingPreferencesDataStore = FailingPreferencesDataStore(),
    connectTimeoutMillis: Long = ConnectionLoop.CONNECT_TIMEOUT_MILLIS,
) {
    val factory = FakeClientFactory()
    val prefs = SessionPrefs(dataStore)
    val queue = DeliveryJournalStore(prefs)
    val activeSession = ActiveSession()
    val stores = SessionStores()
    val deliveryEvents = CopyOnWriteArrayList<XmppEvent>()

    private val ownerJob = SupervisorJob()
    private val ownerScope = CoroutineScope(
        ownerJob + StandardTestDispatcher(testScope.testScheduler),
    )
    private val resume = ResumePersistence(prefs, queue)
    private val readState = ReadStateCoordinator(
        activeSession,
        stores,
        UserPrefs(InMemoryPreferencesDataStore()),
    ) { }
    private val router = XmppEventRouter(
        activeSession,
        stores,
        resume,
        readState,
    ) { _, _ -> }

    val messenger = OutboundMessenger(
        activeSession = activeSession,
        stores = stores,
        journal = queue,
        resume = resume,
        dispatchEvent = { event ->
            deliveryEvents += event
            router.dispatch(event)
        },
        workerExitEvidence = WorkerExitExceptionEvidence(),
    )
    val loop = ConnectionLoop(
        attemptClientFactory = ConnectionAttemptClientFactory(factory, prefs),
        networkSignal = FakeNetworkSignal(),
        resume = resume,
        router = router,
        messenger = messenger,
        configuration = ConnectionLoopConfiguration(
            onReady = { _, _, _, _ -> },
            onAuthenticationStopped = { _, _ -> },
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            connectTimeoutMillis = connectTimeoutMillis,
        ),
    )

    lateinit var loopJob: Job
        private set

    lateinit var lifecycle: SessionLifecycleRef
        private set

    private var terminalWorkerStopped = false

    suspend fun seedResumableRow(stanzaId: String) {
        prefs.activateSession(OWNER, SESSION_ID)
        val previous = queue.beginAttempt(OWNER).attempt
        check(
            queue.enqueueAndClaimAbsoluteHead(
                QueuedOutboundDraft.create(
                    ownerBareJid = OWNER,
                    clientStanzaId = stanzaId,
                    enqueuedAtMillis = 1_000,
                    payload = QueuedOutboundPayload(
                        target = QueuedOutboundTarget.Chat(PEER),
                        content = QueuedOutboundContent("queued before resume"),
                    ),
                    source = DeliverySource.Composer,
                ),
                previous,
            ) is LiveAdmissionResult.Claimed,
        )
        check(
            queue.saveSmResume(
                previous,
                version = 1,
                snapshot = testResumeState(queuedStanzaId = stanzaId).toSnapshot(),
            ),
        )
    }

    suspend fun start(session: WaddleSessionInfo = testSessionInfo()) {
        prefs.activateSession(OWNER, session.sessionId)
        activeSession.ownBareJid = OWNER
        resume.start(ownerScope)
        lifecycle = messenger.start(ownerScope, OWNER).started()
        loop.startAdmissions()
        loopJob = ownerScope.launch { loop.run(session, lifecycle) }
    }

    suspend fun stopTerminalWorker(): LifecycleShutdownOutcome {
        messenger.beginShutdown(lifecycle)
        loopJob.cancelAndJoin()
        return messenger.shutdown(
            LifecycleShutdownTarget.CurrentOwner(lifecycle),
        ).also {
            terminalWorkerStopped = true
        }
    }

    suspend fun startReplacementLifecycle(): SessionLifecycleRef =
        messenger.start(ownerScope, OWNER).started().also {
            lifecycle = it
            terminalWorkerStopped = false
        }

    suspend fun recoverFencedWorkers(
        fencedLifecycle: SessionLifecycleRef,
    ): WorkerRecoveryOutcome = messenger.recoverFencedWorkers(fencedLifecycle)

    suspend fun stopReplacementLifecycle(): LifecycleShutdownOutcome {
        messenger.beginShutdown(lifecycle)
        return messenger.shutdown(
            LifecycleShutdownTarget.CurrentOwner(lifecycle),
        ).also {
            terminalWorkerStopped = true
        }
    }

    suspend fun shutdown() {
        dataStore.failAllUpdates = false
        dataStore.failNextUpdate = false
        loop.stopAdmissions()
        if (!terminalWorkerStopped) {
            messenger.beginShutdown(lifecycle)
            loopJob.cancelAndJoin()
            messenger.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))
            terminalWorkerStopped = true
        }
        ownerJob.cancelAndJoin()
    }

    companion object {
        const val OWNER = "icepuma@waddle.test"
        const val PEER = "alice@waddle.test"
        private const val SESSION_ID = "sess-1"
    }
}

private fun LifecycleStartResult.started(): SessionLifecycleRef =
    (this as? LifecycleStartResult.Started)?.lifecycle
        ?: error("test lifecycle startup failed: $this")
