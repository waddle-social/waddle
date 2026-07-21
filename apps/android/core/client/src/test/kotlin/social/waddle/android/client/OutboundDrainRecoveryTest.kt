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
        val fixture = DrainRecoveryFixture.create(this)
        fixture.assertFatalDrainRecovery()
        fixture.assertReplacementDrainRecovery()
    }

    private class DrainRecoveryFixture private constructor(
        private val state: DrainRecoveryState,
    ) {
        suspend fun assertFatalDrainRecovery() {
            assertEquals(DrainSignalOutcome.Accepted, state.coordinator.signalDrain(state.active.attempt))
            state.control.signals.entered.await()
            state.control.signals.failure.complete(Unit)
            state.scope.runCurrent()

            val fenced = state.coordinator.shutdown(LifecycleShutdownTarget.CurrentOwner(state.active.lifecycle))
                as LifecycleShutdownOutcome.WorkerFenced
            val cause = fenced.cause as LifecycleFenceCause.WorkerExited
            val exit = cause.fence.exit
            assertEquals(state.active.lifecycle, fenced.lifecycle)
            assertEquals(state.active.lifecycle, exit.lifecycle)
            assertEquals(WorkerKind.OUTBOUND_DRAIN, exit.kind)
            assertEquals(
                WorkerExitReason.UnexpectedFailure(WorkerFailureKind.DEPENDENCY_FAILURE),
                exit.reason,
            )
            assertEquals(DrainSignalOutcome.WorkerUnavailable, state.coordinator.signalDrain(state.active.attempt))
            assertTrue(
                state.coordinator.acquireOutbound(DeliverySource.Composer)
                    is OutboundAdmissionResult.LifecycleUnavailable,
            )

            val recovery = state.scope.async { state.coordinator.recoverFencedWorkers(state.active.lifecycle) }
            state.scope.runCurrent()
            val losingRecovery = state.coordinator.recoverFencedWorkers(state.active.lifecycle)
                as WorkerRecoveryOutcome.RecoveryInProgress
            assertEquals(state.active.lifecycle, losingRecovery.claim.lifecycle)
            assertEquals(cause.fence, losingRecovery.claim.fence)
            assertEquals(exit.ownership(), losingRecovery.claim.fence.exit.ownership())

            state.scope.advanceTimeBy(TEST_TIMEOUT_MILLIS + 1)
            state.scope.runCurrent()
            val retained = recovery.await() as WorkerRecoveryOutcome.RetainedOperationsPending
            assertEquals(state.active.lifecycle, retained.lifecycle)
            assertEquals(1, retained.count)
            assertEquals(cause.fence, retained.claim.fence)
            requireLifecycleRelease(
                state.coordinator.releaseAdmission(state.active.held),
                state.active.held.capability,
                LifecycleReleaseSite.OFFLINE_OUTBOUND,
            )
        }

        suspend fun assertReplacementDrainRecovery() {
            assertEquals(
                WorkerRecoveryOutcome.Recovered,
                state.coordinator.recoverFencedWorkers(state.active.lifecycle),
            )
            assertEquals(
                WorkerRecoveryOutcome.NotFenced,
                state.coordinator.recoverFencedWorkers(state.active.lifecycle),
            )
            assertTrue(state.owner.job.isActive)

            state.control.fatalDrain.enabled = false
            val replacement =
                (state.coordinator.start(state.owner.scope, OWNER) as LifecycleStartResult.Started).lifecycle
            assertTrue(replacement != state.active.lifecycle)
            val replacementAttempt = state.coordinator.activate(replacement).bootstrap.attempt
            val replacementLease =
                (state.coordinator.acquireOutbound(DeliverySource.Composer) as OutboundAdmissionResult.Granted).lease
            assertEquals(DrainSignalOutcome.Accepted, state.coordinator.signalDrain(replacementAttempt))
            state.control.signals.replacement.await()
            requireLifecycleRelease(
                state.coordinator.releaseAdmission(replacementLease),
                replacementLease.capability,
                LifecycleReleaseSite.OFFLINE_OUTBOUND,
            )
            assertTrue(state.owner.job.isActive)
            state.owner.job.cancelAndJoin()
        }

        private companion object {
            suspend fun create(scope: TestScope): DrainRecoveryFixture {
                val prefs = SessionPrefs(FailingPreferencesDataStore())
                prefs.activateSession(OWNER, "drain-fatal")
                val queue = DeliveryJournalStore(prefs)
                val resume = ResumePersistence(prefs, queue)
                resume.start(scope.backgroundScope)
                val owner = OwnerScope(scope)
                val control = DrainControl()
                val coordinator = OutboundLifecycleStateStore(
                    activeSession = ActiveSession().also { it.ownBareJid = OWNER },
                    journal = queue,
                    resume = resume,
                    dispatchEvent = {},
                    drain = { _, _, _ ->
                        if (control.fatalDrain.enabled) {
                            control.signals.entered.complete(Unit)
                            control.signals.failure.await()
                            throw IOException("injected drain dependency failure")
                        }
                        control.signals.replacement.complete(Unit)
                    },
                    transitionTimeoutMillis = TEST_TIMEOUT_MILLIS,
                    workerExitEvidence = WorkerExitExceptionEvidence(),
                )
                val lifecycle =
                    (coordinator.start(owner.scope, OWNER) as LifecycleStartResult.Started).lifecycle
                val attempt = coordinator.activate(lifecycle).bootstrap.attempt
                val held =
                    (coordinator.acquireOutbound(DeliverySource.Composer) as OutboundAdmissionResult.Granted).lease
                return DrainRecoveryFixture(
                    DrainRecoveryState(
                        scope = scope,
                        coordinator = coordinator,
                        owner = owner,
                        active = ActiveDrain(lifecycle, attempt, held),
                        control = control,
                    ),
                )
            }
        }
    }

    private class DrainRecoveryState(
        val scope: TestScope,
        val coordinator: OutboundLifecycleStateStore,
        val owner: OwnerScope,
        val active: ActiveDrain,
        val control: DrainControl,
    )

    private class OwnerScope(scope: TestScope) {
        val job = Job()
        val scope = CoroutineScope(scope.coroutineContext + job)
    }

    private class ActiveDrain(
        val lifecycle: SessionLifecycleRef,
        val attempt: DeliveryAttemptRef,
        val held: OutboundAdmissionLease,
    )

    private class DrainSignals {
        val entered = CompletableDeferred<Unit>()
        val failure = CompletableDeferred<Unit>()
        val replacement = CompletableDeferred<Unit>()
    }

    private class DrainControl {
        val fatalDrain = FatalDrainControl()
        val signals = DrainSignals()
    }

    private class FatalDrainControl {
        var enabled = true
    }

    private companion object {
        const val OWNER = "drain@waddle.test"
        const val TEST_TIMEOUT_MILLIS = 100L
    }
}
