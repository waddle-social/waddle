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

@OptIn(ExperimentalCoroutinesApi::class)
class OutboundLifecycleCoordinatorRecoveryTest {
    @Test
    fun `B cancellation before both workers are ready returns reachable lifecycle and replacement coordinator starts`() = runTest {
        val ownerJob = Job()
        val ownerScope = CoroutineScope(coroutineContext + ownerJob)
        val fixture = coordinatorFixture(scope = ownerScope)
        val startup = async { fixture.messenger.start(ownerScope, COORDINATOR_OWNER) }

        ownerJob.cancel()
        runCurrent()

        val failed = startup.await() as LifecycleStartResult.Failed
        assertEquals(LifecycleStartFailure.CANCELLED, failed.cause)
        assertNotNull(failed.lifecycle)
        assertEquals(WorkerRecoveryOutcome.Recovered, fixture.messenger.recoverFencedWorkers(failed.lifecycle))
        val replacement = fixture.messenger.start(backgroundScope, COORDINATOR_OWNER).startedCoordinatorLifecycle()
        assertNotEquals(failed.lifecycle, replacement)
        fixture.stop(replacement)
    }

    @Test
    fun `B cancellation after first exact worker ready returns reachable lifecycle and replacement coordinator starts`() = runTest {
        val ownerJob = Job()
        val ownerScope = CoroutineScope(coroutineContext + ownerJob)
        val firstReady = CompletableDeferred<Unit>()
        val fixture = coordinatorFixture(
            scope = ownerScope,
            phaseObserver = OutboundLifecyclePhaseObserver { phase ->
                if (phase == OutboundLifecyclePhase.TERMINAL_WORKER_READY) {
                    firstReady.complete(Unit)
                    ownerJob.cancel()
                }
            },
        )
        val startup = async { fixture.messenger.start(ownerScope, COORDINATOR_OWNER) }

        runCurrent()
        firstReady.await()
        runCurrent()

        val failed = startup.await() as LifecycleStartResult.Failed
        assertEquals(LifecycleStartFailure.CANCELLED, failed.cause)
        assertNotNull(failed.lifecycle)
        assertEquals(WorkerRecoveryOutcome.Recovered, fixture.messenger.recoverFencedWorkers(failed.lifecycle))
        val replacement = fixture.messenger.start(backgroundScope, COORDINATOR_OWNER).startedCoordinatorLifecycle()
        assertNotEquals(failed.lifecycle, replacement)
        fixture.stop(replacement)
    }

    @Test
    fun `F2 owner scope cancellation while shutdown is outside gate preserves worker fence`() = runTest {
        val ownerJob = Job()
        val ownerScope = CoroutineScope(coroutineContext + ownerJob)
        val finalizerEntered = CompletableDeferred<Unit>()
        val releaseFinalizer = CompletableDeferred<Unit>()
        val fixture = coordinatorFixture(
            scope = ownerScope,
            ownerFinalizer = { _, _, _ ->
                finalizerEntered.complete(Unit)
                releaseFinalizer.await()
                OwnerFinalizationResult.Finalized
            },
        )
        val lifecycle = fixture.start()
        val shutdown = async {
            fixture.messenger.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))
        }

        finalizerEntered.await()
        ownerJob.cancel()
        releaseFinalizer.complete(Unit)

        val outcome = shutdown.await()
        assertTrue(outcome is LifecycleShutdownOutcome.WorkerFenced)
        val fence = (outcome as LifecycleShutdownOutcome.WorkerFenced).cause
            as LifecycleFenceCause.WorkerExited
        assertEquals(lifecycle, fence.fence.exit.lifecycle)
        assertEquals(WorkerExitReason.OwnerScopeCancelled, fence.fence.exit.reason)
    }

    @Test
    fun `F3 awaiting requested worker yields to exact unexpected sibling fence`() = runTest {
        val requestedDependencyEntered = CompletableDeferred<Unit>()
        val releaseRequestedDependency = CompletableDeferred<Unit>()
        val siblingDependencyEntered = CompletableDeferred<Unit>()
        val failSiblingDependency = CompletableDeferred<Unit>()
        val dataStore = FailingPreferencesDataStore()
        val prefs = SessionPrefs(dataStore)
        prefs.activateSession(COORDINATOR_OWNER, COORDINATOR_SESSION_ID)
        val queue = OutboundQueue(prefs)
        val resume = ResumePersistence(prefs, queue)
        resume.start(backgroundScope)
        val activeSession = ActiveSession().also { it.ownBareJid = COORDINATOR_OWNER }
        val coordinator = OutboundLifecycleCoordinator(
            activeSession = activeSession,
            journal = queue,
            resume = resume,
            dispatchEvent = {},
            drain = { _, _, _ ->
                siblingDependencyEntered.complete(Unit)
                failSiblingDependency.await()
                throw IOException("injected drain dependency failure")
            },
            transitionTimeoutMillis = COORDINATOR_TEST_TIMEOUT_MILLIS,
        )

        val lifecycle = coordinator.start(backgroundScope, COORDINATOR_OWNER).startedCoordinatorLifecycle()
        val activation = coordinator.activate(lifecycle)
        val terminalId = "f3-terminal"
        assertTrue(
            queue.enqueueAndClaimAbsoluteHead(
                QueuedOutboundDraft.create(
                    ownerBareJid = COORDINATOR_OWNER,
                    clientStanzaId = terminalId,
                    enqueuedAtMillis = 1_000,
                    payload = QueuedOutboundPayload(
                        target = QueuedOutboundTarget.Chat(COORDINATOR_PEER),
                        content = QueuedOutboundContent("f3 terminal"),
                    ),
                    source = DeliverySource.Composer,
                ),
                activation.bootstrap.attempt,
            ) is OutboundQueue.LiveAdmissionResult.Claimed,
        )
        dataStore.afterCommitReturns = {
            requestedDependencyEntered.complete(Unit)
            releaseRequestedDependency.await()
        }
        val terminal = async {
            coordinator.submitTerminal(
                COORDINATOR_OWNER,
                terminalId,
                activation.bootstrap.attempt,
                DeliveryTerminalKind.ACK,
            )
        }
        requestedDependencyEntered.await()
        coordinator.signalDrain(activation.bootstrap.attempt)
        siblingDependencyEntered.await()

        val shutdown = async {
            coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))
        }
        runCurrent()
        assertFalse(shutdown.isCompleted)

        failSiblingDependency.complete(Unit)
        runCurrent()
        val first = coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))
            as LifecycleShutdownOutcome.WorkerFenced
        val firstFence = first.cause as LifecycleFenceCause.WorkerExited
        assertEquals(lifecycle, first.lifecycle)
        assertEquals(WorkerKind.OUTBOUND_DRAIN, firstFence.fence.exit.kind)
        assertEquals(
            WorkerExitReason.UnexpectedFailure(WorkerFailureKind.DEPENDENCY_FAILURE),
            firstFence.fence.exit.reason,
        )

        val recovering = async { coordinator.recoverFencedWorkers(lifecycle) }
        runCurrent()
        val losingRecovery = coordinator.recoverFencedWorkers(lifecycle)
            as WorkerRecoveryOutcome.RecoveryInProgress
        assertEquals(firstFence.fence, losingRecovery.claim.fence)
        assertEquals(lifecycle, losingRecovery.claim.lifecycle)
        recovering.cancelAndJoin()

        releaseRequestedDependency.complete(Unit)
        assertEquals(TerminalCommandOutcome.Committed, terminal.await())
        runCurrent()
        assertEquals(first, shutdown.await())
        assertEquals(first, coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle)))
        assertEquals(WorkerRecoveryOutcome.Recovered, coordinator.recoverFencedWorkers(lifecycle))
        assertEquals(WorkerRecoveryOutcome.NotFenced, coordinator.recoverFencedWorkers(lifecycle))
    }
}
