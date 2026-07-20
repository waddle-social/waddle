package social.waddle.android.client

import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import kotlin.time.Duration.Companion.seconds

@OptIn(ExperimentalCoroutinesApi::class)
class WorkerShutdownConcurrencyTest {
    @Test
    fun `O1 concurrent shutdowns preserve exact worker fence across external finalizer race`() = runTest(timeout = 5.seconds) {
        val firstFinalizerEntered = CompletableDeferred<Unit>()
        val secondFinalizerEntered = CompletableDeferred<Unit>()
        val releaseFinalizer = CompletableDeferred<Unit>()
        val finalizerCalls = AtomicInteger()
        val harness = ShutdownHarness(this) { _, _, _ ->
            when (finalizerCalls.incrementAndGet()) {
                1 -> firstFinalizerEntered.complete(Unit)
                2 -> secondFinalizerEntered.complete(Unit)
            }
            releaseFinalizer.await()
            OwnerFinalizationResult.Finalized
        }
        val started = harness.start()
        harness.holdDrain(started.attempt)
        harness.beginShutdownAndQuiesce(started)

        val firstShutdown = async {
            harness.coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(started.lifecycle))
        }
        withTimeout(TEST_TIMEOUT_MILLIS) { firstFinalizerEntered.await() }
        val secondShutdown = async {
            harness.coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(started.lifecycle))
        }
        withTimeout(TEST_TIMEOUT_MILLIS) { secondFinalizerEntered.await() }

        harness.failDrain.complete(Unit)
        runCurrent()
        val expected = harness.currentWorkerFence(started.lifecycle)

        releaseFinalizer.complete(Unit)
        assertEquals(expected, firstShutdown.await())
        assertEquals(expected, secondShutdown.await())
        assertEquals(expected, harness.currentWorkerFence(started.lifecycle))
        harness.recoverAndReplace(started.lifecycle)
    }

    @Test
    fun `O2 begin shutdown observes closing then exact worker fence`() = runTest(timeout = 5.seconds) {
        val finalizerEntered = CompletableDeferred<Unit>()
        val releaseFinalizer = CompletableDeferred<Unit>()
        val harness = ShutdownHarness(this) { _, _, _ ->
            finalizerEntered.complete(Unit)
            releaseFinalizer.await()
            OwnerFinalizationResult.Finalized
        }
        val started = harness.start()
        harness.holdDrain(started.attempt)
        harness.beginShutdownAndQuiesce(started)

        val shutdown = async {
            harness.coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(started.lifecycle))
        }
        withTimeout(TEST_TIMEOUT_MILLIS) { finalizerEntered.await() }
        assertFalse(shutdown.isCompleted)
        assertEquals(
            BeginShutdownDecision.AlreadyClosing(started.lifecycle),
            harness.coordinator.beginShutdown(started.lifecycle),
        )

        harness.failDrain.complete(Unit)
        runCurrent()
        val expected = harness.currentWorkerFence(started.lifecycle)
        assertEquals(
            BeginShutdownDecision.WorkerFenced(started.lifecycle, expected.cause),
            harness.coordinator.beginShutdown(started.lifecycle),
        )
        assertEquals(expected, harness.coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(started.lifecycle)))

        releaseFinalizer.complete(Unit)
        assertEquals(expected, shutdown.await())
        harness.recoverAndReplace(started.lifecycle)
    }

    @Test
    fun `O3 shutdown calls preserve live recovery claim across cancellation retry`() = runTest(timeout = 5.seconds) {
        val harness = ShutdownHarness(this, ownerFinalizer = null)
        val started = harness.start()
        val held = (harness.coordinator.acquireOutbound(DeliverySource.Composer)
            as OutboundAdmissionResult.Granted).lease as OutboundAdmissionLease.OfflineOutbound
        harness.holdDrain(started.attempt)

        harness.failDrain.complete(Unit)
        runCurrent()
        val expected = harness.currentWorkerFence(started.lifecycle)
        val firstFence = (expected.cause as LifecycleFenceCause.WorkerExited).fence

        val winner = async(start = CoroutineStart.UNDISPATCHED) {
            harness.coordinator.recoverFencedWorkers(started.lifecycle)
        }
        val loser = harness.coordinator.recoverFencedWorkers(started.lifecycle)
            as WorkerRecoveryOutcome.RecoveryInProgress
        assertEquals(firstFence, loser.claim.fence)
        assertEquals(expected.lifecycle, loser.claim.lifecycle)
        assertEquals(
            BeginShutdownDecision.WorkerFenced(started.lifecycle, expected.cause),
            harness.coordinator.beginShutdown(started.lifecycle),
        )
        assertEquals(expected, harness.coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(started.lifecycle)))
        assertEquals(
            loser.claim,
            (harness.coordinator.recoverFencedWorkers(started.lifecycle)
                as WorkerRecoveryOutcome.RecoveryInProgress).claim,
        )

        winner.cancelAndJoin()
        assertTrue(winner.isCancelled)
        val retry = async(start = CoroutineStart.UNDISPATCHED) {
            harness.coordinator.recoverFencedWorkers(started.lifecycle)
        }
        val retryLoser = harness.coordinator.recoverFencedWorkers(started.lifecycle)
            as WorkerRecoveryOutcome.RecoveryInProgress
        assertEquals(firstFence, retryLoser.claim.fence)
        assertNotEquals(loser.claim.token, retryLoser.claim.token)

        harness.releaseAdmission(held)
        runCurrent()
        assertEquals(WorkerRecoveryOutcome.Recovered, retry.await())
        assertEquals(WorkerRecoveryOutcome.NotFenced, harness.coordinator.recoverFencedWorkers(started.lifecycle))
        assertTrue(harness.ownerJob.isActive)
        harness.startReplacementAndStop(started.lifecycle)
        harness.ownerJob.cancelAndJoin()
    }

    private class ShutdownHarness(
        private val testScope: TestScope,
        ownerFinalizer: (suspend (OwnerWorkers, SessionLifecycleRef, AttemptRecord?) -> OwnerFinalizationResult)?,
    ) {
        val drainEntered = CompletableDeferred<Unit>()
        val failDrain = CompletableDeferred<Unit>()
        val ownerJob = Job()
        private val ownerScope = CoroutineScope(testScope.coroutineContext + ownerJob)
        private val dataStore = FailingPreferencesDataStore()
        private val prefs = SessionPrefs(dataStore)
        private val queue = DeliveryJournalStore(prefs)
        private val resume = ResumePersistence(prefs, queue)
        private var initialized = false
        val coordinator = OutboundLifecycleStateStore(
            activeSession = ActiveSession().also { it.ownBareJid = OWNER },
            journal = queue,
            resume = resume,
            dispatchEvent = {},
            drain = { _, _, _ ->
                drainEntered.complete(Unit)
                failDrain.await()
                throw java.io.IOException("injected drain dependency failure")
            },
            transitionTimeoutMillis = TEST_TIMEOUT_MILLIS,
            ownerFinalizer = ownerFinalizer,
            workerExitEvidence = WorkerExitExceptionEvidence(),
        )

        suspend fun start(): Started {
            if (!initialized) {
                prefs.activateSession(OWNER, SESSION_ID)
                resume.start(testScope.backgroundScope)
                initialized = true
            }
            val lifecycle = (coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Started).lifecycle
            val activation = coordinator.activate(lifecycle)
            return Started(lifecycle, activation.handle, activation.bootstrap.attempt)
        }

        suspend fun holdDrain(attempt: social.waddle.android.client.prefs.DeliveryAttemptRef) {
            assertEquals(DrainSignalOutcome.Accepted, coordinator.signalDrain(attempt))
            withTimeout(TEST_TIMEOUT_MILLIS) { drainEntered.await() }
        }

        suspend fun beginShutdownAndQuiesce(started: Started) {
            assertEquals(BeginShutdownDecision.Begun(started.lifecycle), coordinator.beginShutdown(started.lifecycle))
            assertEquals(
                AttemptCloseOutcome.OwnedBySessionShutdown,
                coordinator.closeAttempt(started.handle, producerQuiesced = true),
            )
        }

        suspend fun releaseAdmission(lease: OutboundAdmissionLease.OfflineOutbound) {
            requireLifecycleRelease(
                coordinator.releaseAdmission(lease),
                lease.capability,
                LifecycleReleaseSite.OFFLINE_OUTBOUND,
            )
        }

        suspend fun currentWorkerFence(lifecycle: SessionLifecycleRef): LifecycleShutdownOutcome.WorkerFenced =
            coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))
                as LifecycleShutdownOutcome.WorkerFenced

        suspend fun recoverAndReplace(lifecycle: SessionLifecycleRef) {
            assertEquals(WorkerRecoveryOutcome.Recovered, coordinator.recoverFencedWorkers(lifecycle))
            assertTrue(ownerJob.isActive)
            startReplacementAndStop(lifecycle)
            ownerJob.cancelAndJoin()
        }

        suspend fun startReplacementAndStop(previous: SessionLifecycleRef) {
            val replacement = (coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Started).lifecycle
            assertNotEquals(previous, replacement)
            assertEquals(
                LifecycleShutdownOutcome.Stopped,
                coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(replacement)),
            )
        }
    }

    private data class Started(
        val lifecycle: SessionLifecycleRef,
        val handle: ConnectionAttemptHandle,
        val attempt: social.waddle.android.client.prefs.DeliveryAttemptRef,
    )

    private companion object {
        const val OWNER = "shutdown@waddle.test"
        const val SESSION_ID = "shutdown-race"
        const val TEST_TIMEOUT_MILLIS = 100L
    }
}
