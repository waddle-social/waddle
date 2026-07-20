package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import kotlin.time.Duration.Companion.seconds

@OptIn(ExperimentalCoroutinesApi::class)
class WorkerShutdownTimeoutRecoveryTest {
    @Test
    fun `M1 terminal timeout exit before shutdown gate re-entry returns exact worker fence`() = runTest(timeout = 5.seconds) {
        val finalizationReturned = CompletableDeferred<Unit>()
        val releaseShutdownReentry = CompletableDeferred<Unit>()
        val harness = TimeoutHarness(
            this,
            OutboundLifecyclePhaseObserver { phase ->
                if (phase == OutboundLifecyclePhase.SHUTDOWN_OWNER_FINALIZED) {
                    finalizationReturned.complete(Unit)
                    releaseShutdownReentry.await()
                }
            },
        )
        val started = harness.start()
        val terminal = harness.submitBlockedTerminal(started)
        runCurrent()
        withTimeout(TEST_TIMEOUT_MILLIS) { harness.terminalDependencyEntered.await() }

        val shutdown = async {
            harness.coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(started.lifecycle))
        }
        advanceTimeBy(TEST_TIMEOUT_MILLIS + 1)
        runCurrent()
        withTimeout(TEST_TIMEOUT_MILLIS) { finalizationReturned.await() }
        assertFalse(shutdown.isCompleted)

        harness.releaseTerminalDependency.complete(Unit)
        assertEquals(TerminalCommandOutcome.Committed, withTimeout(TEST_TIMEOUT_MILLIS) { terminal.await() })
        advanceUntilIdle()
        releaseShutdownReentry.complete(Unit)

        val fenced = withTimeout(TEST_TIMEOUT_MILLIS) { shutdown.await() } as LifecycleShutdownOutcome.WorkerFenced
        val cause = fenced.cause as LifecycleFenceCause.WorkerExited
        assertEquals(started.lifecycle, fenced.lifecycle)
        assertEquals(started.lifecycle, cause.fence.exit.lifecycle)
        assertEquals(WorkerKind.DELIVERY_TERMINAL, cause.fence.exit.kind)
        assertEquals(WorkerExitReason.RequestedStop, cause.fence.exit.reason)
        assertEquals(
            fenced,
            harness.coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(started.lifecycle)),
        )
        assertTrue(harness.ownerJob.isActive)

        val recovery = async(start = CoroutineStart.UNDISPATCHED) {
            harness.coordinator.recoverFencedWorkers(started.lifecycle)
        }
        val concurrent = harness.coordinator.recoverFencedWorkers(started.lifecycle)
            as WorkerRecoveryOutcome.RecoveryInProgress
        assertEquals(cause.fence, concurrent.claim.fence)
        assertEquals(cause.fence.exit.ownership(), concurrent.claim.fence.exit.ownership())
        assertEquals(started.lifecycle, concurrent.claim.lifecycle)
        runCurrent()
        assertEquals(WorkerRecoveryOutcome.Recovered, recovery.await())
        assertEquals(WorkerRecoveryOutcome.NotFenced, harness.coordinator.recoverFencedWorkers(started.lifecycle))
        assertTrue(harness.ownerJob.isActive)
        val replacement = harness.start()
        assertNotEquals(started.lifecycle, replacement.lifecycle)
        harness.stop(replacement.lifecycle)
        harness.ownerJob.cancelAndJoin()
    }

    @Test
    fun `M2 awaiting exact terminal exit advances to worker fence then recovers replacement`() = runTest(timeout = 5.seconds) {
        val awaitingInstalled = CompletableDeferred<Unit>()
        val releaseAwaitingObserver = CompletableDeferred<Unit>()
        val harness = TimeoutHarness(
            this,
            OutboundLifecyclePhaseObserver { phase ->
                if (phase == OutboundLifecyclePhase.AWAITING_REQUESTED_WORKER_EXIT_INSTALLED) {
                    awaitingInstalled.complete(Unit)
                    releaseAwaitingObserver.await()
                }
            },
        )
        val started = harness.start()
        val terminal = harness.submitBlockedTerminal(started)
        runCurrent()
        withTimeout(TEST_TIMEOUT_MILLIS) { harness.terminalDependencyEntered.await() }

        val shutdown = async {
            harness.coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(started.lifecycle))
        }
        advanceTimeBy(TEST_TIMEOUT_MILLIS + 1)
        runCurrent()
        withTimeout(TEST_TIMEOUT_MILLIS) { awaitingInstalled.await() }
        assertFalse(shutdown.isCompleted)

        val awaiting = harness.coordinator.shutdown(
            LifecycleShutdownTarget.CurrentOwner(started.lifecycle),
        ) as LifecycleShutdownOutcome.WorkerFenced
        val awaitingCause = awaiting.cause as LifecycleFenceCause.AwaitingRequestedWorkerExit
        assertEquals(started.lifecycle, awaiting.lifecycle)
        assertEquals(started.lifecycle, awaitingCause.ownership.lifecycle)
        assertEquals(WorkerKind.DELIVERY_TERMINAL, awaitingCause.ownership.kind)
        assertEquals(
            WorkerRecoveryOutcome.WorkerExitPending(started.lifecycle, awaitingCause.ownership),
            harness.coordinator.recoverFencedWorkers(started.lifecycle),
        )

        harness.releaseTerminalDependency.complete(Unit)
        assertEquals(TerminalCommandOutcome.Committed, withTimeout(TEST_TIMEOUT_MILLIS) { terminal.await() })
        advanceUntilIdle()
        releaseAwaitingObserver.complete(Unit)

        val fenced = withTimeout(TEST_TIMEOUT_MILLIS) { shutdown.await() } as LifecycleShutdownOutcome.WorkerFenced
        val cause = fenced.cause as LifecycleFenceCause.WorkerExited
        assertEquals(started.lifecycle, fenced.lifecycle)
        assertEquals(started.lifecycle, cause.fence.exit.lifecycle)
        assertEquals(WorkerKind.DELIVERY_TERMINAL, cause.fence.exit.kind)
        assertEquals(awaitingCause.ownership.generation, cause.fence.exit.generation)
        assertEquals(WorkerExitReason.RequestedStop, cause.fence.exit.reason)
        assertTrue(harness.ownerJob.isActive)

        assertEquals(WorkerRecoveryOutcome.Recovered, harness.coordinator.recoverFencedWorkers(started.lifecycle))
        assertEquals(WorkerRecoveryOutcome.NotFenced, harness.coordinator.recoverFencedWorkers(started.lifecycle))
        assertTrue(harness.ownerJob.isActive)
        val replacement = harness.start()
        assertNotEquals(started.lifecycle, replacement.lifecycle)
        harness.stop(replacement.lifecycle)
        harness.ownerJob.cancelAndJoin()
    }

    private class TimeoutHarness(
        private val testScope: TestScope,
        phaseObserver: OutboundLifecyclePhaseObserver,
    ) {
        val terminalDependencyEntered = CompletableDeferred<Unit>()
        val releaseTerminalDependency = CompletableDeferred<Unit>()
        val ownerJob = Job()
        private val ownerScope = CoroutineScope(testScope.coroutineContext + ownerJob)
        private val dataStore = FailingPreferencesDataStore()
        private val prefs = SessionPrefs(dataStore)
        private val queue = DeliveryJournalStore(prefs)
        private val resume = ResumePersistence(prefs, queue)
        val coordinator: OutboundLifecycleCoordinator
        private var initialized = false

        init {
            coordinator = OutboundLifecycleCoordinator(
                activeSession = ActiveSession().also { it.ownBareJid = OWNER },
                journal = queue,
                resume = resume,
                dispatchEvent = {},
                drain = { _, _, _ -> },
                transitionTimeoutMillis = TEST_TIMEOUT_MILLIS,
                phaseObserver = phaseObserver,
                workerExitEvidence = WorkerExitExceptionEvidence(),
            )
        }

        suspend fun start(): Started {
            if (!initialized) {
                prefs.activateSession(OWNER, SESSION_ID)
                resume.start(testScope.backgroundScope)
                initialized = true
            }
            val lifecycle = (coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Started).lifecycle
            return Started(lifecycle, queue.beginAttempt(OWNER).attempt)
        }

        suspend fun submitBlockedTerminal(started: Started): Deferred<TerminalCommandOutcome> {
            assertTrue(
                queue.enqueueAndClaimAbsoluteHead(
                    QueuedOutboundDraft.create(
                        ownerBareJid = OWNER,
                        clientStanzaId = TERMINAL_ID,
                        enqueuedAtMillis = 1_000,
                        payload = QueuedOutboundPayload(
                            target = QueuedOutboundTarget.Chat(PEER),
                            content = QueuedOutboundContent("timeout terminal"),
                        ),
                        source = DeliverySource.Composer,
                    ),
                    started.attempt,
                ) is DeliveryJournalStore.LiveAdmissionResult.Claimed,
            )
            dataStore.afterCommitReturns = {
                terminalDependencyEntered.complete(Unit)
                releaseTerminalDependency.await()
            }
            return testScope.async {
                coordinator.submitTerminal(
                    OWNER,
                    TERMINAL_ID,
                    started.attempt,
                    DeliveryTerminalKind.ACK,
                )
            }
        }

        suspend fun stop(lifecycle: SessionLifecycleRef) {
            assertEquals(
                LifecycleShutdownOutcome.Stopped,
                coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle)),
            )
        }
    }

    private data class Started(
        val lifecycle: SessionLifecycleRef,
        val attempt: social.waddle.android.client.prefs.DeliveryAttemptRef,
    )

    private companion object {
        const val OWNER = "timeout@waddle.test"
        const val PEER = "alice@waddle.test"
        const val SESSION_ID = "timeout-session"
        const val TERMINAL_ID = "timeout-terminal"
        const val TEST_TIMEOUT_MILLIS = 100L
    }
}
