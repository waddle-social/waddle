package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.DeliveryJournalStore.LiveAdmissionResult
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
class WorkerRecoveryClaimLinearizationTest {
    @Test
    fun `N concurrent recovery claims are linearized and cancellation permits an exact retry`() = runTest(timeout = 5.seconds) {
        val prefs = SessionPrefs(FailingPreferencesDataStore())
        prefs.activateSession(OWNER, "claim-linearization")
        val queue = DeliveryJournalStore(prefs)
        val resume = ResumePersistence(prefs, queue)
        resume.start(backgroundScope)
        val ownerJob = Job()
        val ownerScope = CoroutineScope(coroutineContext + ownerJob)
        val drainEntered = CompletableDeferred<Unit>()
        val releaseDrain = CompletableDeferred<Unit>()
        val coordinator = OutboundLifecycleCoordinator(
            activeSession = ActiveSession().also { it.ownBareJid = OWNER },
            journal = queue,
            resume = resume,
            dispatchEvent = { error("terminal dispatch dependency failed") },
            drain = { _, _, _ ->
                drainEntered.complete(Unit)
                releaseDrain.await()
            },
            transitionTimeoutMillis = TEST_TIMEOUT_MILLIS,
            workerExitEvidence = WorkerExitExceptionEvidence(),
        )
        val lifecycle = (coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Started).lifecycle
        val attempt = coordinator.activate(lifecycle).bootstrap.attempt
        stageTerminalRow(queue, attempt)
        val admission = checkNotNull(coordinator.acquireTerminal(attempt))

        assertEquals(DrainSignalOutcome.Accepted, coordinator.signalDrain(attempt))
        withTimeout(TEST_TIMEOUT_MILLIS) { drainEntered.await() }
        val terminal = async {
            coordinator.submitTerminal(OWNER, TERMINAL_ID, attempt, DeliveryTerminalKind.ACK)
        }
        runCurrent()
        assertEquals(
            TerminalCommandOutcome.Failed(TerminalWorkerFailure(WorkerFailureKind.DEPENDENCY_FAILURE)),
            terminal.await(),
        )

        val fenced = coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))
            as LifecycleShutdownOutcome.WorkerFenced
        val fence = (fenced.cause as LifecycleFenceCause.WorkerExited).fence
        val exit = fence.exit
        assertEquals(lifecycle, fenced.lifecycle)
        assertEquals(lifecycle, exit.lifecycle)
        assertEquals(WorkerKind.DELIVERY_TERMINAL, exit.kind)
        assertEquals(
            WorkerExitReason.UnexpectedFailure(WorkerFailureKind.DEPENDENCY_FAILURE),
            exit.reason,
        )
        requireLifecycleRelease(
            coordinator.releaseAdmission(admission),
            admission.capability,
            LifecycleReleaseSite.TERMINAL_COMMAND,
        )

        val winner = async(start = CoroutineStart.UNDISPATCHED) {
            coordinator.recoverFencedWorkers(lifecycle)
        }
        val firstLoser = coordinator.recoverFencedWorkers(lifecycle)
            as WorkerRecoveryOutcome.RecoveryInProgress
        val secondLoser = coordinator.recoverFencedWorkers(lifecycle)
            as WorkerRecoveryOutcome.RecoveryInProgress
        assertEquals(firstLoser.claim, secondLoser.claim)
        assertEquals(lifecycle, firstLoser.claim.lifecycle)
        assertEquals(fence, firstLoser.claim.fence)
        assertEquals(exit.ownership(), firstLoser.claim.fence.exit.ownership())
        assertEquals(exit.generation, firstLoser.claim.fence.exit.generation)
        assertEquals(exit.kind, firstLoser.claim.fence.exit.kind)
        assertEquals(exit.reason, firstLoser.claim.fence.exit.reason)

        val wrongLifecycle = SessionLifecycleRef.create(OWNER)
        assertEquals(
            WorkerRecoveryOutcome.OwnershipMismatch(wrongLifecycle, lifecycle),
            coordinator.recoverFencedWorkers(wrongLifecycle),
        )
        assertEquals(
            firstLoser.claim,
            (coordinator.recoverFencedWorkers(lifecycle) as WorkerRecoveryOutcome.RecoveryInProgress).claim,
        )

        winner.cancelAndJoin()
        assertTrue(winner.isCancelled)
        assertEquals(attempt, queue.activeAttempt(OWNER))

        val retry = async(start = CoroutineStart.UNDISPATCHED) {
            coordinator.recoverFencedWorkers(lifecycle)
        }
        val retryLoser = coordinator.recoverFencedWorkers(lifecycle)
            as WorkerRecoveryOutcome.RecoveryInProgress
        assertEquals(lifecycle, retryLoser.claim.lifecycle)
        assertEquals(fence, retryLoser.claim.fence)
        assertNotEquals(firstLoser.claim.token, retryLoser.claim.token)

        releaseDrain.complete(Unit)
        runCurrent()
        assertEquals(WorkerRecoveryOutcome.Recovered, retry.await())
        assertNull(queue.activeAttempt(OWNER))
        assertEquals(WorkerRecoveryOutcome.NotFenced, coordinator.recoverFencedWorkers(lifecycle))
        assertTrue(ownerJob.isActive)

        val replacement = (coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Started).lifecycle
        assertNotEquals(lifecycle, replacement)
        assertEquals(
            LifecycleShutdownOutcome.Stopped,
            coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(replacement)),
        )
        ownerJob.cancelAndJoin()
    }

    private suspend fun stageTerminalRow(
        queue: DeliveryJournalStore,
        attempt: social.waddle.android.client.prefs.DeliveryAttemptRef,
    ) {
        assertTrue(
            queue.enqueueAndClaimAbsoluteHead(
                QueuedOutboundDraft.create(
                    ownerBareJid = OWNER,
                    clientStanzaId = TERMINAL_ID,
                    enqueuedAtMillis = 1_000,
                    payload = QueuedOutboundPayload(
                        target = QueuedOutboundTarget.Chat(PEER),
                        content = QueuedOutboundContent("recovery claim"),
                    ),
                    source = DeliverySource.Composer,
                ),
                attempt,
            ) is LiveAdmissionResult.Claimed,
        )
    }

    private companion object {
        const val OWNER = "claims@waddle.test"
        const val PEER = "alice@waddle.test"
        const val TERMINAL_ID = "claim-terminal"
        const val TEST_TIMEOUT_MILLIS = 100L
    }
}
