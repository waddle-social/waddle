package social.waddle.android.client

import java.io.IOException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import kotlin.time.Duration.Companion.seconds

@OptIn(ExperimentalCoroutinesApi::class)
class OutboundDurableCleanupRecoveryTest {
    @Test
    fun `P journal inspection IO is typed and retryable`() = runTest(timeout = 5.seconds) {
        assertTypedRetryableFailure(DurableCleanupOperation.JOURNAL_INSPECTION)
    }

    @Test
    fun `P journal fence IO is typed and retryable`() = runTest(timeout = 5.seconds) {
        assertTypedRetryableFailure(DurableCleanupOperation.JOURNAL_FENCE)
    }

    @Test
    fun `P resume retirement IO is typed and retryable`() = runTest(timeout = 5.seconds) {
        assertTypedRetryableFailure(DurableCleanupOperation.RESUME_RETIREMENT)
    }

    @Test
    fun `P active session cleanup IO is typed and retryable`() = runTest(timeout = 5.seconds) {
        assertTypedRetryableFailure(DurableCleanupOperation.ACTIVE_SESSION_CLEANUP)
    }

    @Test
    fun `P cancellation clears its exact claim before the same cancellation escapes`() = runTest(timeout = 5.seconds) {
        val cancellation = CancellationException("cancel durable recovery")
        val script = ScriptedDurableRecoveryCleanup(
            target = DurableCleanupOperation.JOURNAL_INSPECTION,
            firstFailure = cancellation,
            blockCalls = setOf(1, 2),
            failureAfterBlock = true,
        )
        val harness = CleanupHarness(this, script)
        val started = harness.startFenced()

        val firstClaimObserved = CompletableDeferred<WorkerRecoveryClaim>()
        val releaseFirst = async {
            assertEquals(1, withTimeout(TEST_TIMEOUT_MILLIS) { script.entered.receive() })
            firstClaimObserved.complete(harness.inProgress(started.lifecycle).claim)
            script.release(1)
        }
        val observed = runCatching {
            harness.coordinator.recoverFencedWorkers(started.lifecycle)
        }.exceptionOrNull()
        assertSame(cancellation, observed)
        releaseFirst.await()
        val oldClaim = firstClaimObserved.await()
        assertEquals(started.lifecycle, oldClaim.lifecycle)
        assertEquals(harness.fence, oldClaim.fence)

        val retry = async(start = CoroutineStart.UNDISPATCHED) {
            harness.coordinator.recoverFencedWorkers(started.lifecycle)
        }
        assertEquals(2, withTimeout(TEST_TIMEOUT_MILLIS) { script.entered.receive() })
        val retryClaim = harness.inProgress(started.lifecycle).claim
        assertEquals(oldClaim.lifecycle, retryClaim.lifecycle)
        assertEquals(oldClaim.fence, retryClaim.fence)
        assertNotEquals(oldClaim.token, retryClaim.token)
        script.release(2)
        runCurrent()
        assertEquals(WorkerRecoveryOutcome.Recovered, retry.await())
        assertEquals(WorkerRecoveryOutcome.NotFenced, harness.coordinator.recoverFencedWorkers(started.lifecycle))
        harness.assertOwnerAliveAndReplacement(started.lifecycle)
    }

    @Test
    fun `P programmer failure escapes unchanged and clears the exact claim`() = runTest(timeout = 5.seconds) {
        val failure = IllegalStateException("programmer cleanup failure")
        assertFatalFailureEscapes(DurableCleanupOperation.JOURNAL_FENCE, failure)
    }

    @Test
    fun `P error escapes unchanged and clears the exact claim`() = runTest(timeout = 5.seconds) {
        val failure = AssertionError("fatal cleanup failure")
        assertFatalFailureEscapes(DurableCleanupOperation.ACTIVE_SESSION_CLEANUP, failure)
    }

    private suspend fun TestScope.assertTypedRetryableFailure(operation: DurableCleanupOperation) {
        val script = ScriptedDurableRecoveryCleanup(
            target = operation,
            firstFailure = IOException("injected durable cleanup failure"),
            blockCalls = setOf(2),
        )
        val harness = CleanupHarness(this, script)
        val started = harness.startFenced()
        val first = async { harness.coordinator.recoverFencedWorkers(started.lifecycle) }
        runCurrent()
        val failed = first.await() as WorkerRecoveryOutcome.DurableCleanupFailed
        val expectedAttempt = if (operation == DurableCleanupOperation.JOURNAL_INSPECTION) {
            null
        } else {
            started.attempt
        }
        assertEquals(started.lifecycle, failed.lifecycle)
        assertEquals(started.lifecycle, failed.claim.lifecycle)
        assertEquals(LifecyclePendingComponent.ATTEMPT_FINALIZATION, failed.component)
        assertEquals(1, failed.count)
        assertEquals(operation, failed.operation)
        assertEquals(DurableCleanupFailureCause.IO_FAILURE, failed.cause)
        assertEquals(expectedAttempt, failed.attempt)
        assertEquals(harness.fence, failed.claim.fence)
        assertEquals(harness.fence.exit.ownership(), failed.claim.fence.exit.ownership())

        val retry = async(start = CoroutineStart.UNDISPATCHED) {
            harness.coordinator.recoverFencedWorkers(started.lifecycle)
        }
        assertEquals(2, withTimeout(TEST_TIMEOUT_MILLIS) { script.entered.receive() })
        val loser = harness.inProgress(started.lifecycle)
        assertEquals(failed.claim.lifecycle, loser.claim.lifecycle)
        assertEquals(failed.claim.fence, loser.claim.fence)
        assertNotEquals(failed.claim.token, loser.claim.token)

        script.release(2)
        runCurrent()
        assertEquals(WorkerRecoveryOutcome.Recovered, retry.await())
        assertEquals(WorkerRecoveryOutcome.NotFenced, harness.coordinator.recoverFencedWorkers(started.lifecycle))
        harness.assertOwnerAliveAndReplacement(started.lifecycle)
    }

    private suspend fun TestScope.assertFatalFailureEscapes(
        operation: DurableCleanupOperation,
        failure: Throwable,
    ) {
        val script = ScriptedDurableRecoveryCleanup(
            target = operation,
            firstFailure = failure,
            blockCalls = setOf(2),
        )
        val harness = CleanupHarness(this, script)
        val started = harness.startFenced()

        val observed = runCatching {
            harness.coordinator.recoverFencedWorkers(started.lifecycle)
        }.exceptionOrNull()
        assertSame(failure, observed)

        val retry = async(start = CoroutineStart.UNDISPATCHED) {
            harness.coordinator.recoverFencedWorkers(started.lifecycle)
        }
        assertEquals(2, withTimeout(TEST_TIMEOUT_MILLIS) { script.entered.receive() })
        val claim = harness.inProgress(started.lifecycle).claim
        assertEquals(harness.fence, claim.fence)
        script.release(2)
        runCurrent()
        assertEquals(WorkerRecoveryOutcome.Recovered, retry.await())
        assertEquals(WorkerRecoveryOutcome.NotFenced, harness.coordinator.recoverFencedWorkers(started.lifecycle))
        harness.assertOwnerAliveAndReplacement(started.lifecycle)
    }

    private class CleanupHarness(
        private val testScope: TestScope,
        script: ScriptedDurableRecoveryCleanup,
    ) {
        private val dataStore = FailingPreferencesDataStore()
        private val prefs = SessionPrefs(dataStore)
        private val queue = OutboundQueue(prefs)
        private val resume = ResumePersistence(prefs, queue)
        private val activeSession = ActiveSession().also { it.ownBareJid = OWNER }
        val ownerJob = Job()
        private val ownerScope = CoroutineScope(testScope.coroutineContext + ownerJob)
        private val drainEntered = CompletableDeferred<Unit>()
        private val failDrain = CompletableDeferred<Unit>()
        val coordinator: OutboundLifecycleCoordinator
        lateinit var fence: WorkerFence
            private set

        init {
            val production = ProductionDurableRecoveryCleanup(queue, resume, activeSession)
            script.delegate = production
            coordinator = OutboundLifecycleCoordinator(
                activeSession = activeSession,
                journal = queue,
                resume = resume,
                dispatchEvent = {},
                drain = { _, _, _ ->
                    drainEntered.complete(Unit)
                    failDrain.await()
                    throw IOException("injected drain dependency failure")
                },
                transitionTimeoutMillis = TEST_TIMEOUT_MILLIS,
                durableRecoveryCleanup = script,
            )
        }

        suspend fun startFenced(): Started {
            prefs.activateSession(OWNER, SESSION_ID)
            resume.start(testScope.backgroundScope)
            val lifecycle = (coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Started).lifecycle
            val activation = coordinator.activate(lifecycle)
            assertEquals(DrainSignalOutcome.Accepted, coordinator.signalDrain(activation.bootstrap.attempt))
            withTimeout(TEST_TIMEOUT_MILLIS) { drainEntered.await() }
            failDrain.complete(Unit)
            testScope.runCurrent()
            val fenced = coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))
                as LifecycleShutdownOutcome.WorkerFenced
            fence = (fenced.cause as LifecycleFenceCause.WorkerExited).fence
            assertEquals(WorkerKind.OUTBOUND_DRAIN, fence.exit.kind)
            assertEquals(
                WorkerExitReason.UnexpectedFailure(WorkerFailureKind.DEPENDENCY_FAILURE),
                fence.exit.reason,
            )
            return Started(lifecycle, activation.bootstrap.attempt)
        }

        suspend fun inProgress(lifecycle: SessionLifecycleRef): WorkerRecoveryOutcome.RecoveryInProgress =
            coordinator.recoverFencedWorkers(lifecycle) as WorkerRecoveryOutcome.RecoveryInProgress

        suspend fun assertOwnerAliveAndReplacement(previous: SessionLifecycleRef) {
            assertTrue(ownerJob.isActive)
            val replacement = (coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Started).lifecycle
            assertNotEquals(previous, replacement)
            assertEquals(
                LifecycleShutdownOutcome.Stopped,
                coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(replacement)),
            )
            ownerJob.cancelAndJoin()
        }
    }

    private class ScriptedDurableRecoveryCleanup(
        private val target: DurableCleanupOperation,
        private val firstFailure: Throwable? = null,
        private val blockCalls: Set<Int> = emptySet(),
        private val failureAfterBlock: Boolean = false,
    ) : DurableRecoveryCleanup {
        lateinit var delegate: DurableRecoveryCleanup
        val entered = Channel<Int>(Channel.UNLIMITED)
        private val gates = mutableMapOf<Int, CompletableDeferred<Unit>>()
        private var targetCalls = 0

        fun release(call: Int) {
            gates.getOrPut(call) { CompletableDeferred() }.complete(Unit)
        }

        override suspend fun inspectActiveAttempt(lifecycle: SessionLifecycleRef) =
            invoke(DurableCleanupOperation.JOURNAL_INSPECTION) {
                delegate.inspectActiveAttempt(lifecycle)
            }

        override suspend fun fenceAttempt(attempt: social.waddle.android.client.prefs.DeliveryAttemptRef) {
            invoke(DurableCleanupOperation.JOURNAL_FENCE) { delegate.fenceAttempt(attempt) }
        }

        override suspend fun retireAttempt(attempt: social.waddle.android.client.prefs.DeliveryAttemptRef) {
            invoke(DurableCleanupOperation.RESUME_RETIREMENT) { delegate.retireAttempt(attempt) }
        }

        override suspend fun endActiveSessionAttempt(attempt: social.waddle.android.client.prefs.DeliveryAttemptRef) {
            invoke(DurableCleanupOperation.ACTIVE_SESSION_CLEANUP) { delegate.endActiveSessionAttempt(attempt) }
        }

        private suspend fun <T> invoke(
            operation: DurableCleanupOperation,
            block: suspend () -> T,
        ): T {
            if (operation == target) {
                targetCalls += 1
                if (targetCalls == 1 && firstFailure != null && !failureAfterBlock) throw firstFailure
                if (targetCalls in blockCalls) {
                    entered.send(targetCalls)
                    gates.getOrPut(targetCalls) { CompletableDeferred() }.await()
                }
                if (targetCalls == 1 && firstFailure != null && failureAfterBlock) throw firstFailure
            }
            return block()
        }
    }

    private data class Started(
        val lifecycle: SessionLifecycleRef,
        val attempt: social.waddle.android.client.prefs.DeliveryAttemptRef,
    )

    private companion object {
        const val OWNER = "durable-recovery@waddle.test"
        const val SESSION_ID = "durable-recovery"
        const val TEST_TIMEOUT_MILLIS = 100L
    }
}
