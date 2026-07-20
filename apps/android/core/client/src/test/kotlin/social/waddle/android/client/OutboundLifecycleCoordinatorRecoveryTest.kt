package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
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

        assertTrue(runCatching { startup.await() }.exceptionOrNull() is CancellationException)
        val replacement = fixture.messenger.start(backgroundScope, COORDINATOR_OWNER).startedCoordinatorLifecycle()
        fixture.stop(replacement)
    }

    @Test
    fun `B cancellation after first exact worker ready compensates before replacement coordinator starts`() = runTest {
        val ownerJob = Job()
        val ownerScope = CoroutineScope(coroutineContext + ownerJob)
        val firstReady = CompletableDeferred<Unit>()
        val installedLifecycle = CompletableDeferred<SessionLifecycleRef>()
        val fixture = coordinatorFixture(
            scope = ownerScope,
            phaseObserver = OutboundLifecyclePhaseObserver { phase ->
                if (phase == OutboundLifecyclePhase.TERMINAL_WORKER_READY) {
                    firstReady.complete(Unit)
                    ownerJob.cancel()
                }
            },
            workerStartHooks = object : WorkerStartHooks {
                override suspend fun beforeTerminal() = Unit
                override suspend fun afterTerminal(terminal: DeliveryTerminalWorker.Run) = Unit
                override suspend fun afterInstall(workers: OwnerWorkers) {
                    installedLifecycle.complete(workers.lifecycle)
                }
            },
        )
        val startup = async { fixture.messenger.start(ownerScope, COORDINATOR_OWNER) }

        runCurrent()
        firstReady.await()
        runCurrent()

        assertTrue(runCatching { startup.await() }.exceptionOrNull() is CancellationException)
        assertEquals(
            WorkerRecoveryOutcome.NotFenced,
            fixture.messenger.recoverFencedWorkers(installedLifecycle.await()),
        )
        val replacement = fixture.messenger.start(backgroundScope, COORDINATOR_OWNER).startedCoordinatorLifecycle()
        fixture.stop(replacement)
    }

    @Test
    fun `B startup seams compensate ordinary failure cancellation and error`() = runTest {
        listOf(false, true).forEach { afterTerminal ->
            val exception = IllegalStateException("startup exception $afterTerminal")
            val ordinaryFixture = coordinatorFixture(
                workerStartHooks = failingStartHooks(afterTerminal, exception),
            )
            val failed = ordinaryFixture.messenger.start(backgroundScope, COORDINATOR_OWNER) as LifecycleStartResult.Failed
            assertEquals(
                LifecycleStartFailure.WORKER_CONSTRUCTION_FAILED,
                failed.cause,
            )
            ordinaryFixture.stop(ordinaryFixture.start())

            val cancellation = CancellationException("startup cancellation $afterTerminal")
            val cancellationFixture = coordinatorFixture(workerStartHooks = failingStartHooks(afterTerminal, cancellation))
            val cancelled = runCatching {
                cancellationFixture.messenger.start(backgroundScope, COORDINATOR_OWNER)
            }.exceptionOrNull()
            assertTrue(cancelled === cancellation)
            cancellationFixture.stop(cancellationFixture.start())

            val error = AssertionError("startup error $afterTerminal")
            val errorFixture = coordinatorFixture(workerStartHooks = failingStartHooks(afterTerminal, error))
            val thrown = runCatching {
                errorFixture.messenger.start(backgroundScope, COORDINATOR_OWNER)
            }.exceptionOrNull()
            assertTrue(thrown === error)
            errorFixture.stop(errorFixture.start())
        }
    }

    @Test
    fun `B terminal startup evidence wins over distinct drain startup evidence`() = runTest {
        val terminalPrimary = AssertionError("terminal startup primary")
        val drainPrimary = AssertionError("drain startup secondary")
        val drainPublished = CompletableDeferred<Unit>()
        var failStartup = true
        val fixture = coordinatorFixture(
            phaseObserver = OutboundLifecyclePhaseObserver { phase ->
                if (failStartup) {
                    when (phase) {
                        OutboundLifecyclePhase.TERMINAL_WORKER_READY -> throw terminalPrimary
                        OutboundLifecyclePhase.DRAIN_WORKER_READY -> {
                            drainPublished.complete(Unit)
                            throw drainPrimary
                        }
                        else -> Unit
                    }
                }
            },
        )

        assertSame(terminalPrimary, runCatching {
            fixture.messenger.start(backgroundScope, COORDINATOR_OWNER)
        }.exceptionOrNull())
        drainPublished.await()
        failStartup = false
        fixture.stop(fixture.start())
    }

    @Test
    fun `B drain startup evidence wins after terminal readiness`() = runTest {
        val drainPrimary = CancellationException("drain startup primary")
        val terminalReady = CompletableDeferred<Unit>()
        var failStartup = true
        val fixture = coordinatorFixture(
            phaseObserver = OutboundLifecyclePhaseObserver { phase ->
                if (failStartup) {
                    when (phase) {
                        OutboundLifecyclePhase.TERMINAL_WORKER_READY -> terminalReady.complete(Unit)
                        OutboundLifecyclePhase.DRAIN_WORKER_READY -> throw drainPrimary
                        else -> Unit
                    }
                }
            },
        )

        assertSame(drainPrimary, runCatching {
            fixture.messenger.start(backgroundScope, COORDINATOR_OWNER)
        }.exceptionOrNull())
        terminalReady.await()
        failStartup = false
        fixture.stop(fixture.start())
    }

    @Test
    fun `B after install ordinary failure stops both workers before typed result`() = runTest {
        val installed = CompletableDeferred<OwnerWorkers>()
        val failure = IllegalStateException("after install ordinary failure")
        val fixture = coordinatorFixture(
            workerStartHooks = failingAfterInstall(installed, failure),
        )

        val result = fixture.messenger.start(backgroundScope, COORDINATOR_OWNER) as LifecycleStartResult.Failed

        assertEquals(LifecycleStartFailure.WORKER_READINESS_FAILED, result.cause)
        assertRequestedStop(installed.await())
        fixture.stop(fixture.start())
    }

    @Test
    fun `B after install cancellation stops both workers before exact rethrow`() = runTest {
        val installed = CompletableDeferred<OwnerWorkers>()
        val cancellation = CancellationException("after install cancellation")
        val fixture = coordinatorFixture(
            workerStartHooks = failingAfterInstall(installed, cancellation),
        )

        assertSame(cancellation, runCatching {
            fixture.messenger.start(backgroundScope, COORDINATOR_OWNER)
        }.exceptionOrNull())
        assertRequestedStop(installed.await())
        fixture.stop(fixture.start())
    }

    @Test
    fun `B after install error stops both workers before exact rethrow`() = runTest {
        val installed = CompletableDeferred<OwnerWorkers>()
        val error = AssertionError("after install error")
        val fixture = coordinatorFixture(
            workerStartHooks = failingAfterInstall(installed, error),
        )

        assertSame(error, runCatching {
            fixture.messenger.start(backgroundScope, COORDINATOR_OWNER)
        }.exceptionOrNull())
        assertRequestedStop(installed.await())
        fixture.stop(fixture.start())
    }

    @Test
    fun `B after install cancellation and error suppress one cleanup failure in order`() = runTest {
        listOf<Throwable>(
            CancellationException("after install cancellation primary"),
            AssertionError("after install error primary"),
        ).forEach { primary ->
            val installed = CompletableDeferred<OwnerWorkers>()
            val cleanup = IllegalStateException("startup cleanup failure ${primary.message}")
            val fixture = coordinatorFixture(
                workerStartHooks = failingAfterInstall(installed, primary),
                workerExitEvidence = FailThirdDiscardEvidence(cleanup),
            )

            assertSame(primary, runCatching {
                fixture.messenger.start(backgroundScope, COORDINATOR_OWNER)
            }.exceptionOrNull())
            assertRequestedStop(installed.await())
            assertEquals(1, primary.suppressed.size)
            assertSame(cleanup, primary.suppressed.single())
        }
    }

    @Test
    fun `B unexpected terminal stop before install is retained as bootstrap failure and no pair is installed`() = runTest {
        val terminalExited = CompletableDeferred<Unit>()
        val fixture = coordinatorFixture(
            workerStartHooks = object : WorkerStartHooks {
                private var stopped = false

                override suspend fun beforeTerminal() = Unit

                override suspend fun afterTerminal(terminal: DeliveryTerminalWorker.Run) {
                    if (!stopped) {
                        stopped = true
                        terminal.requestStop()
                        terminal.awaitExit(COORDINATOR_TEST_TIMEOUT_MILLIS)
                        terminalExited.complete(Unit)
                    }
                }

                override suspend fun afterInstall(workers: OwnerWorkers) = Unit
            },
        )

        val failed = fixture.messenger.start(backgroundScope, COORDINATOR_OWNER) as LifecycleStartResult.Failed

        terminalExited.await()
        assertEquals(LifecycleStartFailure.WORKER_CONSTRUCTION_FAILED, failed.cause)
        fixture.stop(fixture.start())
    }

    @Test
    fun `B explicit partial teardown requested stop remains record-only`() = runTest {
        val fixture = coordinatorFixture(
            workerStartHooks = failingStartHooks(true, IllegalStateException("after terminal")),
        )

        val failed = fixture.messenger.start(backgroundScope, COORDINATOR_OWNER) as LifecycleStartResult.Failed

        assertEquals(LifecycleStartFailure.WORKER_CONSTRUCTION_FAILED, failed.cause)
        fixture.stop(fixture.start())
    }

    @Test
    fun `B terminal exit immediately after install enters full fenced recovery`() = runTest {
        val terminalExited = CompletableDeferred<Unit>()
        val fixture = coordinatorFixture(
            workerStartHooks = object : WorkerStartHooks {
                private var stopped = false

                override suspend fun beforeTerminal() = Unit
                override suspend fun afterTerminal(terminal: DeliveryTerminalWorker.Run) = Unit

                override suspend fun afterInstall(workers: OwnerWorkers) {
                    if (!stopped) {
                        stopped = true
                        workers.terminal.requestStop()
                        workers.terminal.awaitExit(COORDINATOR_TEST_TIMEOUT_MILLIS)
                        terminalExited.complete(Unit)
                    }
                }
            },
        )

        val failed = fixture.messenger.start(backgroundScope, COORDINATOR_OWNER) as LifecycleStartResult.Failed

        terminalExited.await()
        assertEquals(LifecycleStartFailure.WORKER_READINESS_FAILED, failed.cause)
        val fence = fixture.messenger.beginShutdown(failed.lifecycle) as BeginShutdownDecision.WorkerFenced
        assertEquals(WorkerKind.DELIVERY_TERMINAL, (fence.cause as LifecycleFenceCause.WorkerExited).fence.exit.kind)
        assertEquals(WorkerRecoveryOutcome.Recovered, fixture.messenger.recoverFencedWorkers(failed.lifecycle))
        fixture.stop(fixture.start())
    }

    private fun failingStartHooks(afterTerminal: Boolean, failure: Throwable): WorkerStartHooks =
        object : WorkerStartHooks {
            private var fired = false

            override suspend fun beforeTerminal() {
                if (!afterTerminal && !fired) {
                    fired = true
                    throw failure
                }
            }

            override suspend fun afterTerminal(terminal: DeliveryTerminalWorker.Run) {
                if (afterTerminal && !fired) {
                    fired = true
                    throw failure
                }
            }

            override suspend fun afterInstall(workers: OwnerWorkers) = Unit
        }

    private fun failingAfterInstall(
        installed: CompletableDeferred<OwnerWorkers>,
        failure: Throwable,
    ): WorkerStartHooks = object : WorkerStartHooks {
        private var fired = false

        override suspend fun beforeTerminal() = Unit

        override suspend fun afterTerminal(terminal: DeliveryTerminalWorker.Run) = Unit

        override suspend fun afterInstall(workers: OwnerWorkers) {
            installed.complete(workers)
            if (!fired) {
                fired = true
                throw failure
            }
        }
    }

    private suspend fun assertRequestedStop(workers: OwnerWorkers) {
        val terminal = workers.terminal.awaitExit(COORDINATOR_TEST_TIMEOUT_MILLIS)
        val drain = workers.drain.awaitExit(COORDINATOR_TEST_TIMEOUT_MILLIS)
        assertEquals(
            WorkerExitReason.RequestedStop,
            (terminal as WorkerAwaitOutcome.Exited).exit.reason,
        )
        assertEquals(
            WorkerExitReason.RequestedStop,
            (drain as WorkerAwaitOutcome.Exited).exit.reason,
        )
    }

    private class FailThirdDiscardEvidence(
        private val cleanup: Throwable,
    ) : WorkerExitEvidence {
        private var discardCount = 0

        override fun record(ownership: WorkerOwnership, failure: Throwable) = Unit

        override fun lookup(outcome: WorkerRecoveryOutcome): Throwable? = null

        override fun discard(ownership: WorkerOwnership) {
            discardCount += 1
            if (discardCount == 3) throw cleanup
        }
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
            workerExitEvidence = WorkerExitExceptionEvidence(),
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
