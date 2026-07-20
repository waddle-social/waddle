package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleSendMessageOutcome
import java.io.IOException

internal suspend fun TestScope.stateStoreFixture(
    dataStore: FailingPreferencesDataStore = FailingPreferencesDataStore(),
    transitionTimeoutMillis: Long = 5_000L,
    scope: CoroutineScope = backgroundScope,
    phaseObserver: OutboundLifecyclePhaseObserver =
        OutboundLifecyclePhaseObserver.NONE,
    ownerFinalizer: (suspend (OwnerWorkers, SessionLifecycleRef, AttemptRecord?) -> OwnerFinalizationResult)? = null,
    workerStartHooks: WorkerStartHooks = WorkerStartHooks.None,
    workerExitEvidence: WorkerExitEvidence = WorkerExitExceptionEvidence(),
    admissionReleaseOperations: OutboundAdmissionReleaseOperations =
        OutboundAdmissionReleaseOperations.COORDINATOR,
): StateStoreFixture {
    val prefs = SessionPrefs(dataStore)
    prefs.activateSession(STATE_STORE_OWNER, STATE_STORE_SESSION_ID)
    val queue = DeliveryJournalStore(prefs)
    val resume = ResumePersistence(prefs, queue)
    resume.start(backgroundScope)
    val activeSession = ActiveSession().also {
        it.ownBareJid = STATE_STORE_OWNER
    }
    val messenger = OutboundMessenger(
        activeSession = activeSession,
        stores = SessionStores(),
        journal = queue,
        resume = resume,
        dispatchEvent = {},
        transitionTimeoutMillis = transitionTimeoutMillis,
        phaseObserver = phaseObserver,
        ownerFinalizer = ownerFinalizer,
        workerStartHooks = workerStartHooks,
        workerExitEvidence = workerExitEvidence,
        admissionReleaseOperations = admissionReleaseOperations,
    )
    return StateStoreFixture(
        messenger,
        scope,
        queue,
        activeSession,
    )
}

internal data class StateStoreFixture(
    val messenger: OutboundMessenger,
    val scope: CoroutineScope,
    val queue: DeliveryJournalStore,
    val activeSession: ActiveSession,
) {
    suspend fun start(): SessionLifecycleRef =
        messenger.start(scope, STATE_STORE_OWNER).startedStateStoreLifecycle()

    suspend fun stop(lifecycle: SessionLifecycleRef) {
        assertTrue(messenger.beginShutdown(lifecycle) is BeginShutdownDecision.Begun)
        assertEquals(
            LifecycleShutdownOutcome.Stopped,
            messenger.shutdown(
                LifecycleShutdownTarget.CurrentOwner(lifecycle),
            ),
        )
    }

    suspend fun retryShutdownAndStartReplacement(
        fencedLifecycle: SessionLifecycleRef,
    ) {
        assertEquals(
            LifecycleShutdownOutcome.Stopped,
            messenger.shutdown(
                LifecycleShutdownTarget.CurrentOwner(fencedLifecycle),
            ),
        )
        val replacement = start()
        assertNotEquals(fencedLifecycle, replacement)
        stop(replacement)
    }
}

internal const val STATE_STORE_OWNER = "icepuma@waddle.test"
internal const val STATE_STORE_PEER = "alice@waddle.test"
internal const val STATE_STORE_SESSION_ID = "session-1"
internal const val STATE_STORE_TEST_TIMEOUT_MILLIS = 100L

internal fun LifecycleStartResult.startedStateStoreLifecycle(): SessionLifecycleRef =
    (this as? LifecycleStartResult.Started)?.lifecycle
        ?: error("test lifecycle startup failed: $this")
