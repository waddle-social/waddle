package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
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
import java.io.IOException

@OptIn(ExperimentalCoroutinesApi::class)
class OutboundLifecycleStateStoreRecoveryTest {
    @Test
    fun `B cancellation before workers are ready retains lifecycle and starts replacement store`() =
        runTest {
            val ownerJob = Job()
            val ownerScope = CoroutineScope(coroutineContext + ownerJob)
            val fixture = stateStoreFixture(scope = ownerScope)
            val startup = async { fixture.messenger.start(ownerScope, STATE_STORE_OWNER) }

            ownerJob.cancel()
            runCurrent()

            assertTrue(runCatching { startup.await() }.exceptionOrNull() is CancellationException)
            val replacement = fixture.messenger.start(backgroundScope, STATE_STORE_OWNER).startedStateStoreLifecycle()
            fixture.stop(replacement)
        }

    @Test
    fun `B cancellation after first exact worker ready compensates before replacement state store starts`() = runTest {
        val ownerJob = Job()
        val ownerScope = CoroutineScope(coroutineContext + ownerJob)
        val firstReady = CompletableDeferred<Unit>()
        val installedLifecycle = CompletableDeferred<SessionLifecycleRef>()
        val fixture = stateStoreFixture(
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
        val startup = async { fixture.messenger.start(ownerScope, STATE_STORE_OWNER) }

        runCurrent()
        firstReady.await()
        runCurrent()

        assertTrue(runCatching { startup.await() }.exceptionOrNull() is CancellationException)
        assertEquals(
            WorkerRecoveryOutcome.NotFenced,
            fixture.messenger.recoverFencedWorkers(installedLifecycle.await()),
        )
        val replacement = fixture.messenger.start(backgroundScope, STATE_STORE_OWNER).startedStateStoreLifecycle()
        fixture.stop(replacement)
    }

    @Test
    fun `B startup seams compensate ordinary failure cancellation and error`() = runTest {
        listOf(false, true).forEach { afterTerminal ->
            val exception = IllegalStateException("startup exception $afterTerminal")
            val ordinaryFixture = stateStoreFixture(
                workerStartHooks = failingStartHooks(afterTerminal, exception),
            )
            val failed =
                ordinaryFixture.messenger.start(backgroundScope, STATE_STORE_OWNER) as LifecycleStartResult.Failed
            assertEquals(
                LifecycleStartFailure.WORKER_CONSTRUCTION_FAILED,
                failed.cause,
            )
            ordinaryFixture.stop(ordinaryFixture.start())

            val cancellation = CancellationException("startup cancellation $afterTerminal")
            val cancellationFixture =
                stateStoreFixture(workerStartHooks = failingStartHooks(afterTerminal, cancellation))
            val cancelled = runCatching {
                cancellationFixture.messenger.start(backgroundScope, STATE_STORE_OWNER)
            }.exceptionOrNull()
            assertTrue(cancelled === cancellation)
            cancellationFixture.stop(cancellationFixture.start())

            val error = AssertionError("startup error $afterTerminal")
            val errorFixture = stateStoreFixture(workerStartHooks = failingStartHooks(afterTerminal, error))
            val thrown = runCatching {
                errorFixture.messenger.start(backgroundScope, STATE_STORE_OWNER)
            }.exceptionOrNull()
            assertTrue(thrown === error)
            errorFixture.stop(errorFixture.start())
        }
    }

    @Test
    fun `B drain startup error wins when terminal startup evidence already exists`() = runTest {
        val dataStore = FailingPreferencesDataStore()
        val terminalPrimary = AssertionError("terminal startup dependency failure")
        val drainPrimary = AssertionError("drain startup primary")
        val terminalReadyEntered = CompletableDeferred<Unit>()
        val releaseTerminalReady = CompletableDeferred<Unit>()
        val drainReadyEntered = CompletableDeferred<Unit>()
        val terminalExitObserved = CompletableDeferred<WorkerExit>()
        val terminalExitAwaiter = CompletableDeferred<Deferred<WorkerExit>>()
        val installed = CompletableDeferred<OwnerWorkers>()
        var exerciseFailure = true
        val fixture = stateStoreFixture(
            dataStore = dataStore,
            phaseObserver = OutboundLifecyclePhaseObserver { phase ->
                if (exerciseFailure) {
                    when (phase) {
                        OutboundLifecyclePhase.TERMINAL_WORKER_READY -> {
                            terminalReadyEntered.complete(Unit)
                            releaseTerminalReady.await()
                        }
                        OutboundLifecyclePhase.DRAIN_WORKER_READY -> {
                            drainReadyEntered.complete(Unit)
                            terminalExitObserved.await()
                            throw drainPrimary
                        }
                        else -> Unit
                    }
                }
            },
            workerStartHooks = object : WorkerStartHooks {
                override suspend fun beforeTerminal() = Unit

                override suspend fun afterTerminal(terminal: DeliveryTerminalWorker.Run) {
                    terminalExitAwaiter.complete(
                        backgroundScope.async {
                        val exit = when (val outcome = terminal.awaitExit(STATE_STORE_TEST_TIMEOUT_MILLIS)) {
                            is WorkerAwaitOutcome.Exited -> outcome.exit
                            WorkerAwaitOutcome.TimedOut -> error("terminal startup worker did not exit")
                        }
                        terminalExitObserved.complete(exit)
                        exit
                    }
                    )
                }

                override suspend fun afterInstall(workers: OwnerWorkers) {
                    installed.complete(workers)
                }
            },
        )
        val startupFailure = backgroundScope.async {
            runCatching { fixture.messenger.start(backgroundScope, STATE_STORE_OWNER) }.exceptionOrNull()
        }
        terminalReadyEntered.await()
        drainReadyEntered.await()
        val workers = installed.await()
        dataStore.failAllUpdatesWith = terminalPrimary
        releaseTerminalReady.complete(Unit)

        val terminalExit = terminalExitAwaiter.await().await()
        assertEquals(
            WorkerExitReason.UnexpectedFailure(WorkerFailureKind.DEPENDENCY_FAILURE),
            terminalExit.reason,
        )
        assertEquals(workers.terminalOwnership, terminalExit.ownership())

        assertSame(drainPrimary, startupFailure.await())
        val drainExit = workers.drain.awaitExit(STATE_STORE_TEST_TIMEOUT_MILLIS) as WorkerAwaitOutcome.Exited
        assertEquals(
            WorkerExitReason.UnexpectedFailure(WorkerFailureKind.DEPENDENCY_FAILURE),
            drainExit.exit.reason,
        )
        assertEquals(WorkerRecoveryOutcome.NotFenced, fixture.messenger.recoverFencedWorkers(workers.lifecycle))

        exerciseFailure = false
        dataStore.failAllUpdatesWith = null
        fixture.stop(fixture.start())
    }

    @Test
    fun `B after install ordinary failure stops both workers before typed result`() = runTest {
        val installed = CompletableDeferred<OwnerWorkers>()
        val failure = IllegalStateException("after install ordinary failure")
        val fixture = stateStoreFixture(
            workerStartHooks = failingAfterInstall(installed, failure),
        )

        val result = fixture.messenger.start(backgroundScope, STATE_STORE_OWNER) as LifecycleStartResult.Failed

        assertEquals(LifecycleStartFailure.WORKER_READINESS_FAILED, result.cause)
        assertRequestedStop(installed.await())
        fixture.stop(fixture.start())
    }

    @Test
    fun `B after install cancellation stops both workers before exact rethrow`() = runTest {
        val installed = CompletableDeferred<OwnerWorkers>()
        val cancellation = CancellationException("after install cancellation")
        val fixture = stateStoreFixture(
            workerStartHooks = failingAfterInstall(installed, cancellation),
        )

        assertSame(
            cancellation,
            runCatching {
            fixture.messenger.start(backgroundScope, STATE_STORE_OWNER)
        }.exceptionOrNull()
        )
        assertRequestedStop(installed.await())
        fixture.stop(fixture.start())
    }

    @Test
    fun `B after install error stops both workers before exact rethrow`() = runTest {
        val installed = CompletableDeferred<OwnerWorkers>()
        val error = AssertionError("after install error")
        val fixture = stateStoreFixture(
            workerStartHooks = failingAfterInstall(installed, error),
        )

        assertSame(
            error,
            runCatching {
            fixture.messenger.start(backgroundScope, STATE_STORE_OWNER)
        }.exceptionOrNull()
        )
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
            val fixture = stateStoreFixture(
                workerStartHooks = failingAfterInstall(installed, primary),
                workerExitEvidence = FailThirdDiscardEvidence(cleanup),
            )

            assertSame(
                primary,
                runCatching {
                fixture.messenger.start(backgroundScope, STATE_STORE_OWNER)
            }.exceptionOrNull()
            )
            assertRequestedStop(installed.await())
            assertEquals(1, primary.suppressed.size)
            assertSame(cleanup, primary.suppressed.single())
        }
    }

    @Test
    fun `B unexpected terminal stop before install is retained as bootstrap failure and no pair is installed`() =
        runTest {
            val terminalExited = CompletableDeferred<Unit>()
            val fixture = stateStoreFixture(
                workerStartHooks = object : WorkerStartHooks {
                    private var stopped = false

                    override suspend fun beforeTerminal() = Unit

                    override suspend fun afterTerminal(terminal: DeliveryTerminalWorker.Run) {
                        if (!stopped) {
                            stopped = true
                            terminal.requestStop()
                            terminal.awaitExit(STATE_STORE_TEST_TIMEOUT_MILLIS)
                            terminalExited.complete(Unit)
                        }
                    }

                    override suspend fun afterInstall(workers: OwnerWorkers) = Unit
                },
            )

            val failed = fixture.messenger.start(backgroundScope, STATE_STORE_OWNER) as LifecycleStartResult.Failed

            terminalExited.await()
            assertEquals(LifecycleStartFailure.WORKER_CONSTRUCTION_FAILED, failed.cause)
            fixture.stop(fixture.start())
        }

    @Test
    fun `B explicit partial teardown requested stop remains record-only`() = runTest {
        val fixture = stateStoreFixture(
            workerStartHooks = failingStartHooks(true, IllegalStateException("after terminal")),
        )

        val failed = fixture.messenger.start(backgroundScope, STATE_STORE_OWNER) as LifecycleStartResult.Failed

        assertEquals(LifecycleStartFailure.WORKER_CONSTRUCTION_FAILED, failed.cause)
        fixture.stop(fixture.start())
    }

    @Test
    fun `B terminal exit immediately after install enters full fenced recovery`() = runTest {
        val terminalExited = CompletableDeferred<Unit>()
        val fixture = stateStoreFixture(
            workerStartHooks = object : WorkerStartHooks {
                private var stopped = false

                override suspend fun beforeTerminal() = Unit
                override suspend fun afterTerminal(terminal: DeliveryTerminalWorker.Run) = Unit

                override suspend fun afterInstall(workers: OwnerWorkers) {
                    if (!stopped) {
                        stopped = true
                        workers.terminal.requestStop()
                        workers.terminal.awaitExit(STATE_STORE_TEST_TIMEOUT_MILLIS)
                        terminalExited.complete(Unit)
                    }
                }
            },
        )

        val failed = fixture.messenger.start(backgroundScope, STATE_STORE_OWNER) as LifecycleStartResult.Failed

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
        val terminal = workers.terminal.awaitExit(STATE_STORE_TEST_TIMEOUT_MILLIS)
        val drain = workers.drain.awaitExit(STATE_STORE_TEST_TIMEOUT_MILLIS)
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
        val fixture = stateStoreFixture(
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
        val fixture = f3Fixture(backgroundScope, siblingDependencyEntered, failSiblingDependency)
        val lifecycle = fixture.stateStore.start(backgroundScope, STATE_STORE_OWNER).startedStateStoreLifecycle()
        val activation = fixture.stateStore.activate(lifecycle)
        val terminal = fixture.startF3Terminal(
            this,
            activation,
            requestedDependencyEntered,
            releaseRequestedDependency,
        )

        requestedDependencyEntered.await()
        fixture.stateStore.signalDrain(activation.bootstrap.attempt)
        siblingDependencyEntered.await()

        val first = fixture.awaitF3UnexpectedDrainFence(this, lifecycle, failSiblingDependency)
        fixture.assertF3Recovery(
            this,
            lifecycle,
            first,
            terminal,
            releaseRequestedDependency,
        )
    }

    private suspend fun f3Fixture(
        scope: CoroutineScope,
        siblingDependencyEntered: CompletableDeferred<Unit>,
        failSiblingDependency: CompletableDeferred<Unit>,
    ): F3Fixture {
        val dataStore = FailingPreferencesDataStore()
        val prefs = SessionPrefs(dataStore)
        prefs.activateSession(STATE_STORE_OWNER, STATE_STORE_SESSION_ID)
        val queue = DeliveryJournalStore(prefs)
        val resume = ResumePersistence(prefs, queue)
        resume.start(scope)
        val activeSession = ActiveSession().also { it.ownBareJid = STATE_STORE_OWNER }
        val stateStore = OutboundLifecycleStateStore(
            activeSession = activeSession,
            journal = queue,
            resume = resume,
            dispatchEvent = {},
            drain = { _, _, _ ->
                siblingDependencyEntered.complete(Unit)
                failSiblingDependency.await()
                throw IOException("injected drain dependency failure")
            },
            transitionTimeoutMillis = STATE_STORE_TEST_TIMEOUT_MILLIS,
            workerExitEvidence = WorkerExitExceptionEvidence(),
        )
        return F3Fixture(dataStore, queue, stateStore)
    }

    private suspend fun F3Fixture.startF3Terminal(
        scope: TestScope,
        activation: AttemptActivation,
        requestedDependencyEntered: CompletableDeferred<Unit>,
        releaseRequestedDependency: CompletableDeferred<Unit>,
    ): Deferred<TerminalCommandOutcome> {
        val terminalId = "f3-terminal"
        assertTrue(
            queue.enqueueAndClaimAbsoluteHead(
                QueuedOutboundDraft.create(
                    ownerBareJid = STATE_STORE_OWNER,
                    clientStanzaId = terminalId,
                    enqueuedAtMillis = 1_000,
                    payload = QueuedOutboundPayload(
                        target = QueuedOutboundTarget.Chat(STATE_STORE_PEER),
                        content = QueuedOutboundContent("f3 terminal"),
                    ),
                    source = DeliverySource.Composer,
                ),
                activation.bootstrap.attempt,
            ) is DeliveryJournalStore.LiveAdmissionResult.Claimed,
        )
        dataStore.afterCommitReturns = {
            requestedDependencyEntered.complete(Unit)
            releaseRequestedDependency.await()
        }
        return scope.async {
            stateStore.submitTerminal(
                STATE_STORE_OWNER,
                terminalId,
                activation.bootstrap.attempt,
                DeliveryTerminalKind.ACK,
            )
        }
    }

    private suspend fun F3Fixture.awaitF3UnexpectedDrainFence(
        scope: TestScope,
        lifecycle: SessionLifecycleRef,
        failSiblingDependency: CompletableDeferred<Unit>,
    ): F3ShutdownFence {
        val shutdown = scope.async {
            stateStore.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))
        }
        scope.runCurrent()
        assertFalse(shutdown.isCompleted)

        failSiblingDependency.complete(Unit)
        scope.runCurrent()
        val first = stateStore.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle))
            as LifecycleShutdownOutcome.WorkerFenced
        val firstFence = first.cause as LifecycleFenceCause.WorkerExited
        assertEquals(lifecycle, first.lifecycle)
        assertEquals(WorkerKind.OUTBOUND_DRAIN, firstFence.fence.exit.kind)
        assertEquals(
            WorkerExitReason.UnexpectedFailure(WorkerFailureKind.DEPENDENCY_FAILURE),
            firstFence.fence.exit.reason,
        )
        return F3ShutdownFence(first, firstFence, shutdown)
    }

    private suspend fun F3Fixture.assertF3Recovery(
        scope: TestScope,
        lifecycle: SessionLifecycleRef,
        first: F3ShutdownFence,
        terminal: Deferred<TerminalCommandOutcome>,
        releaseRequestedDependency: CompletableDeferred<Unit>,
    ) {
        val recovering = scope.async { stateStore.recoverFencedWorkers(lifecycle) }
        scope.runCurrent()
        val losingRecovery = stateStore.recoverFencedWorkers(lifecycle)
            as WorkerRecoveryOutcome.RecoveryInProgress
        assertEquals(first.fence.fence, losingRecovery.claim.fence)
        assertEquals(lifecycle, losingRecovery.claim.lifecycle)
        recovering.cancelAndJoin()

        releaseRequestedDependency.complete(Unit)
        assertEquals(TerminalCommandOutcome.Committed, terminal.await())
        scope.runCurrent()
        assertEquals(first.first, first.shutdown.await())
        assertEquals(first.first, stateStore.shutdown(LifecycleShutdownTarget.CurrentOwner(lifecycle)))
        assertEquals(WorkerRecoveryOutcome.Recovered, stateStore.recoverFencedWorkers(lifecycle))
        assertEquals(WorkerRecoveryOutcome.NotFenced, stateStore.recoverFencedWorkers(lifecycle))
    }

    private data class F3Fixture(
        val dataStore: FailingPreferencesDataStore,
        val queue: DeliveryJournalStore,
        val stateStore: OutboundLifecycleStateStore,
    )

    private data class F3ShutdownFence(
        val first: LifecycleShutdownOutcome.WorkerFenced,
        val fence: LifecycleFenceCause.WorkerExited,
        val shutdown: Deferred<LifecycleShutdownOutcome>,
    )
}
