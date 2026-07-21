package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import java.io.IOException

@OptIn(ExperimentalCoroutinesApi::class)
class OutboundDrainRecoveryTest {
    @Test
    fun `K fatal drain exit retains exact outbound lease until recovery can proceed`() = runTest {
        val prefs = SessionPrefs(FailingPreferencesDataStore())
        prefs.activateSession(OWNER, "drain-fatal")
        val queue = DeliveryJournalStore(prefs)
        val resume = ResumePersistence(prefs, queue)
        resume.start(backgroundScope)
        val ownerJob = Job()
        val ownerScope = CoroutineScope(coroutineContext + ownerJob)
        val drainEntered = CompletableDeferred<Unit>()
        val failDrain = CompletableDeferred<Unit>()
        val replacementDrain = CompletableDeferred<Unit>()
        val fatalDrain = FatalDrainControl()
        val coordinator = OutboundLifecycleStateStore(
            activeSession = ActiveSession().also { it.ownBareJid = OWNER },
            journal = queue,
            resume = resume,
            dispatchEvent = {},
            drain = { _, _, _ ->
                if (fatalDrain.enabled) {
                    drainEntered.complete(Unit)
                    failDrain.await()
                    throw IOException("injected drain dependency failure")
                }
                replacementDrain.complete(Unit)
            },
            transitionTimeoutMillis = TEST_TIMEOUT_MILLIS,
            workerExitEvidence = WorkerExitExceptionEvidence(),
        )
        val lifecycle = (coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Started).lifecycle
        val activation = coordinator.activate(lifecycle)
        val attempt = activation.bootstrap.attempt
        val held = (coordinator.acquireOutbound(DeliverySource.Composer) as OutboundAdmissionResult.Granted).lease
        assertFatalDrainRecovery(this, coordinator, lifecycle, attempt, held, drainEntered, failDrain)
        assertReplacementDrainRecovery(coordinator, ownerScope, ownerJob, lifecycle, replacementDrain, fatalDrain)
    }

    private suspend fun assertFatalDrainRecovery(
        scope: TestScope,
        coordinator: OutboundLifecycleStateStore,
        lifecycle: SessionLifecycleRef,
        attempt: DeliveryAttemptRef,
        held: OutboundAdmissionLease,
        drainEntered: CompletableDeferred<Unit>,
        failDrain: CompletableDeferred<Unit>,
    ) {
        assertEquals(DrainSignalOutcome.Accepted, coordinator.signalDrain(attempt))
        drainEntered.await()
        failDrain.complete(Unit)
        scope.runCurrent()

        val fenced = coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))
            as LifecycleShutdownOutcome.WorkerFenced
        val cause = fenced.cause as LifecycleFenceCause.WorkerExited
        val exit = cause.fence.exit
        assertEquals(lifecycle, fenced.lifecycle)
        assertEquals(lifecycle, exit.lifecycle)
        assertEquals(WorkerKind.OUTBOUND_DRAIN, exit.kind)
        assertEquals(
            WorkerExitReason.UnexpectedFailure(WorkerFailureKind.DEPENDENCY_FAILURE),
            exit.reason,
        )
        assertEquals(DrainSignalOutcome.WorkerUnavailable, coordinator.signalDrain(attempt))
        assertTrue(coordinator.acquireOutbound(DeliverySource.Composer) is OutboundAdmissionResult.LifecycleUnavailable)

        val recovery = scope.async { coordinator.recoverFencedWorkers(lifecycle) }
        scope.runCurrent()
        val losingRecovery = coordinator.recoverFencedWorkers(lifecycle)
            as WorkerRecoveryOutcome.RecoveryInProgress
        assertEquals(lifecycle, losingRecovery.claim.lifecycle)
        assertEquals(cause.fence, losingRecovery.claim.fence)
        assertEquals(exit.ownership(), losingRecovery.claim.fence.exit.ownership())

        scope.advanceTimeBy(TEST_TIMEOUT_MILLIS + 1)
        scope.runCurrent()
        val retained = recovery.await() as WorkerRecoveryOutcome.RetainedOperationsPending
        assertEquals(lifecycle, retained.lifecycle)
        assertEquals(1, retained.count)
        assertEquals(cause.fence, retained.claim.fence)
        requireLifecycleRelease(
            coordinator.releaseAdmission(held),
            held.capability,
            LifecycleReleaseSite.OFFLINE_OUTBOUND,
        )
    }

    private suspend fun assertReplacementDrainRecovery(
        coordinator: OutboundLifecycleStateStore,
        ownerScope: CoroutineScope,
        ownerJob: Job,
        lifecycle: SessionLifecycleRef,
        replacementDrain: CompletableDeferred<Unit>,
        fatalDrain: FatalDrainControl,
    ) {
        assertEquals(WorkerRecoveryOutcome.Recovered, coordinator.recoverFencedWorkers(lifecycle))
        assertEquals(WorkerRecoveryOutcome.NotFenced, coordinator.recoverFencedWorkers(lifecycle))
        assertTrue(ownerJob.isActive)

        fatalDrain.enabled = false
        val replacement = (coordinator.start(ownerScope, OWNER) as LifecycleStartResult.Started).lifecycle
        assertTrue(replacement != lifecycle)
        val replacementAttempt = coordinator.activate(replacement).bootstrap.attempt
        val replacementLease =
            (coordinator.acquireOutbound(DeliverySource.Composer) as OutboundAdmissionResult.Granted).lease
        assertEquals(DrainSignalOutcome.Accepted, coordinator.signalDrain(replacementAttempt))
        replacementDrain.await()
        requireLifecycleRelease(
            coordinator.releaseAdmission(replacementLease),
            replacementLease.capability,
            LifecycleReleaseSite.OFFLINE_OUTBOUND,
        )
        assertTrue(ownerJob.isActive)
        ownerJob.cancelAndJoin()
    }

    private class FatalDrainControl {
        var enabled = true
    }

    private companion object {
        const val OWNER = "drain@waddle.test"
        const val TEST_TIMEOUT_MILLIS = 100L
    }
}
