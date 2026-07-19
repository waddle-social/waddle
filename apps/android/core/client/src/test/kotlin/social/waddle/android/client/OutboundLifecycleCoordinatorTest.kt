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
class OutboundLifecycleCoordinatorTest {
    @Test
    fun `B cancellation before both workers are ready returns reachable lifecycle and replacement coordinator starts`() = runTest {
        val ownerJob = Job()
        val ownerScope = CoroutineScope(coroutineContext + ownerJob)
        val fixture = fixture(scope = ownerScope)
        val startup = async { fixture.messenger.start(ownerScope, OWNER) }

        ownerJob.cancel()
        runCurrent()

        val failed = startup.await() as LifecycleStartResult.Failed
        assertEquals(LifecycleStartFailure.CANCELLED, failed.cause)
        assertNotNull(failed.lifecycle)
        assertEquals(WorkerRecoveryOutcome.Recovered, fixture.messenger.recoverFencedWorkers(failed.lifecycle))
        val replacement = fixture.messenger.start(backgroundScope, OWNER).started()
        assertNotEquals(failed.lifecycle, replacement)
        fixture.stop(replacement)
    }

    @Test
    fun `B cancellation after first exact worker ready returns reachable lifecycle and replacement coordinator starts`() = runTest {
        val ownerJob = Job()
        val ownerScope = CoroutineScope(coroutineContext + ownerJob)
        val firstReady = CompletableDeferred<Unit>()
        val fixture = fixture(
            scope = ownerScope,
            phaseObserver = OutboundLifecyclePhaseObserver { phase ->
                if (phase == OutboundLifecyclePhase.TERMINAL_WORKER_READY) {
                    firstReady.complete(Unit)
                    ownerJob.cancel()
                }
            },
        )
        val startup = async { fixture.messenger.start(ownerScope, OWNER) }

        runCurrent()
        firstReady.await()
        runCurrent()

        val failed = startup.await() as LifecycleStartResult.Failed
        assertEquals(LifecycleStartFailure.CANCELLED, failed.cause)
        assertNotNull(failed.lifecycle)
        assertEquals(WorkerRecoveryOutcome.Recovered, fixture.messenger.recoverFencedWorkers(failed.lifecycle))
        val replacement = fixture.messenger.start(backgroundScope, OWNER).started()
        assertNotEquals(failed.lifecycle, replacement)
        fixture.stop(replacement)
    }

    @Test
    fun `F2 owner scope cancellation while shutdown is outside gate preserves worker fence`() = runTest {
        val ownerJob = Job()
        val ownerScope = CoroutineScope(coroutineContext + ownerJob)
        val finalizerEntered = CompletableDeferred<Unit>()
        val releaseFinalizer = CompletableDeferred<Unit>()
        val fixture = fixture(
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
        prefs.activateSession(OWNER, SESSION_ID)
        val queue = OutboundQueue(prefs)
        val resume = ResumePersistence(prefs, queue)
        resume.start(backgroundScope)
        val activeSession = ActiveSession().also { it.ownBareJid = OWNER }
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
            transitionTimeoutMillis = TEST_TIMEOUT_MILLIS,
        )

        val lifecycle = coordinator.start(backgroundScope, OWNER).started()
        val activation = coordinator.activate(lifecycle)
        val terminalId = "f3-terminal"
        assertTrue(
            queue.enqueueAndClaimAbsoluteHead(
                QueuedOutboundDraft.create(
                    ownerBareJid = OWNER,
                    clientStanzaId = terminalId,
                    enqueuedAtMillis = 1_000,
                    payload = QueuedOutboundPayload(
                        target = QueuedOutboundTarget.Chat(PEER),
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
                OWNER,
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
    @Test
    fun `concurrent disconnect callers share one generation operation`() = runTest {
        val fixture = fixture()
        val lifecycle = fixture.start()
        val activation = fixture.messenger.activateAttempt(lifecycle)
        val client = FakeWaddleClient()
        assertTrue(fixture.messenger.attachTransport(activation.handle, client))

        val entered = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        client.beforeDisconnectReturns = {
            entered.complete(Unit)
            release.await()
        }

        val first = async {
            fixture.messenger.disconnectTransport(activation.handle)
        }
        runCurrent()
        assertTrue(entered.isCompleted)
        val second = async {
            fixture.messenger.disconnectTransport(activation.handle)
        }
        runCurrent()
        assertEquals(1, client.disconnectCalls)
        assertFalse(first.isCompleted)
        assertFalse(second.isCompleted)

        release.complete(Unit)
        runCurrent()
        assertTrue(first.await())
        assertTrue(second.await())
        assertEquals(1, client.disconnectCalls)

        assertEquals(
            AttemptCloseOutcome.Closed,
            fixture.messenger.closeAttempt(
                activation.handle,
                producerQuiesced = true,
            ),
        )
        assertFalse(fixture.messenger.disconnectTransport(activation.handle))
        assertEquals(1, client.disconnectCalls)
        fixture.stop(lifecycle)
    }

    @Test
    fun `same owner old lifecycle cannot stop replacement lifecycle`() = runTest {
        val fixture = fixture()
        val predecessor = fixture.start()
        fixture.stop(predecessor)

        val replacement = fixture.start()
        assertNotEquals(predecessor, replacement)
        assertTrue(fixture.messenger.beginShutdown(predecessor) is BeginShutdownDecision.Stale)
        assertEquals(
            LifecycleShutdownOutcome.Stale,
            fixture.messenger.shutdown(
                LifecycleShutdownTarget.CurrentOwner(predecessor),
            ),
        )

        val admitted = fixture.messenger.sendOrEnqueue(
            conversationJid = PEER,
            isGroupchat = false,
            body = "replacement remains authoritative",
        )
        assertEquals(WaddleSendMessageOutcome.NotConnected, admitted.outcome)
        assertNotNull(admitted.delivery)
        fixture.stop(replacement)
    }

    @Test
    fun `cancellation at every activation publication phase compensates exactly`() = runTest {
        val phases = listOf(
            OutboundLifecyclePhase.ATTEMPT_JOURNALING,
            OutboundLifecyclePhase.ATTEMPT_JOURNALED,
            OutboundLifecyclePhase.RESUME_REGISTERED,
            OutboundLifecyclePhase.DRAIN_BOUND,
            OutboundLifecyclePhase.ACTIVE_SESSION_PUBLISHED,
            OutboundLifecyclePhase.ATTEMPT_PUBLISHED,
        )
        phases.forEach { target ->
            val reached = CompletableDeferred<Unit>()
            val fixture = fixture(
                phaseObserver = OutboundLifecyclePhaseObserver { phase ->
                    if (phase == target) {
                        reached.complete(Unit)
                        awaitCancellation()
                    }
                },
            )
            val lifecycle = fixture.start()
            val activation = async {
                fixture.messenger.activateAttempt(lifecycle)
            }
            runCurrent()
            assertTrue("phase $target was not observed", reached.isCompleted)
            activation.cancelAndJoin()
            runCurrent()
            assertNull(fixture.queue.activeAttempt(OWNER))
            assertNull(fixture.activeSession.attemptRef)
            fixture.stop(lifecycle)
        }
    }

    @Test
    fun `I rotation cancellation after journal commit releases its exact lease`() = runTest {
        val committed = CompletableDeferred<Unit>()
        val fixture = fixture(
            phaseObserver = OutboundLifecyclePhaseObserver { phase ->
                if (phase == OutboundLifecyclePhase.ROTATION_JOURNALED) {
                    committed.complete(Unit)
                    awaitCancellation()
                }
            },
        )
        val lifecycle = fixture.start()
        val activation = fixture.messenger.activateAttempt(lifecycle)
        val old = activation.bootstrap.attempt
        val transition = DeliveryAttemptTransition(
            old = old,
            fresh = old.copy(
                attemptId = DeliveryAttemptId.random(),
                nativeGeneration = old.nativeGeneration.next(),
            ),
        )
        val rotation = async {
            fixture.messenger.rotateAndAwait(
                activation.handle,
                transition,
                emptySet(),
            )
        }
        runCurrent()
        assertTrue(committed.isCompleted)
        rotation.cancelAndJoin()
        runCurrent()

        assertNull(fixture.queue.activeAttempt(OWNER))
        assertNull(fixture.activeSession.attemptRef)
        assertEquals(
            AttemptCloseOutcome.OwnedBySessionShutdown,
            fixture.messenger.closeAttempt(
                activation.handle,
                producerQuiesced = true,
            ),
        )
        assertEquals(
            LifecycleShutdownOutcome.Stopped,
            fixture.messenger.shutdown(
                LifecycleShutdownTarget.CurrentOwner(lifecycle),
            ),
        )
        fixture.stop(fixture.start())
    }

    @Test
    fun `blocked live send fences shutdown and refuses restart`() = runTest {
        val fixture = fixture(transitionTimeoutMillis = TEST_TIMEOUT_MILLIS)
        val lifecycle = fixture.start()
        val activation = fixture.messenger.activateAttempt(lifecycle)
        val client = FakeWaddleClient()
        assertTrue(fixture.messenger.attachTransport(activation.handle, client))
        assertTrue(
            fixture.messenger.markReady(
                activation.handle,
                client,
                activation.bootstrap.attempt,
            ),
        )
        val sendEntered = CompletableDeferred<Unit>()
        val releaseSend = CompletableDeferred<Unit>()
        client.beforeSendReturns = {
            sendEntered.complete(Unit)
            releaseSend.await()
        }
        val send = async {
            fixture.messenger.sendOrEnqueue(PEER, false, "in flight")
        }
        runCurrent()
        assertTrue(sendEntered.isCompleted)

        assertTrue(fixture.messenger.beginShutdown(lifecycle) is BeginShutdownDecision.Begun)
        val shutdown = async {
            fixture.messenger.shutdown(
                LifecycleShutdownTarget.CurrentOwner(lifecycle),
            )
        }
        runCurrent()
        assertEquals(
            AttemptCloseOutcome.OwnedBySessionShutdown,
            fixture.messenger.closeAttempt(
                activation.handle,
                producerQuiesced = true,
            ),
        )
        runCurrent()
        advanceTimeBy(TEST_TIMEOUT_MILLIS + 1)
        runCurrent()
        val outcome = shutdown.await()
        assertTrue(outcome is LifecycleShutdownOutcome.FencedWithPending)
        outcome as LifecycleShutdownOutcome.FencedWithPending
        assertEquals(LifecyclePendingComponent.ATTEMPT_LEASES, outcome.component)
        assertTrue(
            runCatching {
                fixture.messenger.start(backgroundScope, OWNER)
            }.isFailure,
        )

        releaseSend.complete(Unit)
        runCurrent()
        assertEquals(WaddleSendMessageOutcome.Sent::class, send.await().outcome::class)
        fixture.retryShutdownAndStartReplacement(lifecycle)
    }

    @Test
    fun `post commit storage barrier fences shutdown and refuses restart`() = runTest {
        val dataStore = FailingPreferencesDataStore()
        val fixture = fixture(
            dataStore = dataStore,
            transitionTimeoutMillis = TEST_TIMEOUT_MILLIS,
        )
        val lifecycle = fixture.start()
        runCurrent()
        val committed = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        dataStore.afterCommitReturns = {
            committed.complete(Unit)
            release.await()
        }
        val send = async {
            fixture.messenger.sendOrEnqueue(PEER, false, "durably committed")
        }
        runCurrent()
        assertTrue(committed.isCompleted)

        assertTrue(fixture.messenger.beginShutdown(lifecycle) is BeginShutdownDecision.Begun)
        val shutdown = async {
            fixture.messenger.shutdown(
                LifecycleShutdownTarget.CurrentOwner(lifecycle),
            )
        }
        runCurrent()
        advanceTimeBy(TEST_TIMEOUT_MILLIS + 1)
        runCurrent()
        val outcome = shutdown.await()
        assertTrue(outcome is LifecycleShutdownOutcome.FencedWithPending)
        outcome as LifecycleShutdownOutcome.FencedWithPending
        assertEquals(LifecyclePendingComponent.ATTEMPT_LEASES, outcome.component)
        assertTrue(
            runCatching {
                fixture.messenger.start(backgroundScope, OWNER)
            }.isFailure,
        )

        release.complete(Unit)
        runCurrent()
        assertNotNull(send.await().delivery)
        fixture.retryShutdownAndStartReplacement(lifecycle)
    }

    @Test
    fun `I superseded construction retains then accepts its documented exact retry`() = runTest {
        val fixture = fixture(transitionTimeoutMillis = TEST_TIMEOUT_MILLIS)
        val lifecycle = fixture.start()
        val activation = fixture.messenger.activateAttempt(lifecycle)
        val construction = fixture.messenger.beginTransportConstruction(activation.handle)
        assertNotNull(construction)

        assertTrue(fixture.messenger.beginShutdown(lifecycle) is BeginShutdownDecision.Begun)
        assertEquals(
            AttemptCloseOutcome.OwnedBySessionShutdown,
            fixture.messenger.closeAttempt(activation.handle, producerQuiesced = true),
        )
        val fenced = fixture.messenger.shutdown(
            LifecycleShutdownTarget.CurrentOwner(lifecycle),
        )
        assertTrue(fenced is LifecycleShutdownOutcome.FencedWithPending)
        fenced as LifecycleShutdownOutcome.FencedWithPending
        assertEquals(LifecyclePendingComponent.ATTEMPT_LEASES, fenced.component)

        fixture.messenger.finishSupersededConstruction(construction!!)
        fixture.messenger.finishSupersededConstruction(construction)
        assertEquals(
            LifecycleShutdownOutcome.Stopped,
            fixture.messenger.shutdown(
                LifecycleShutdownTarget.CurrentOwner(lifecycle),
            ),
        )
    }

    @Test
    fun `I messenger offline live terminal and drain admissions release before shutdown`() = runTest {
        val fixture = fixture()
        val lifecycle = fixture.start()

        val offline = fixture.messenger.sendOrEnqueue(PEER, false, "offline")
        assertEquals(WaddleSendMessageOutcome.NotConnected, offline.outcome)

        val activation = fixture.messenger.activateAttempt(lifecycle)
        fixture.messenger.drainOutboundQueue()

        val client = FakeWaddleClient()
        assertTrue(fixture.messenger.attachTransport(activation.handle, client))
        assertTrue(
            fixture.messenger.markReady(
                activation.handle,
                client,
                activation.bootstrap.attempt,
            ),
        )
        val live = fixture.messenger.sendOrEnqueue(PEER, false, "live")
        val liveId = checkNotNull(live.delivery).identity.clientStanzaId
        assertTrue(
            !fixture.messenger.reconcileDeliveryEvent(
                XmppEvent.NativeDeliveryAcked(activation.bootstrap.attempt, liveId),
            ),
        )

        assertEquals(
            AttemptCloseOutcome.Closed,
            fixture.messenger.closeAttempt(activation.handle, producerQuiesced = true),
        )
        fixture.messenger.markTransportClosed(activation.handle, closed = true)
        fixture.stop(lifecycle)
    }

    @Test
    fun `I messenger finally rethrows cancellation with exact release violation suppressed`() = runTest {
        val primary = kotlinx.coroutines.CancellationException("live send cancelled")
        val fixture = fixture(
            admissionReleaseOperations = OutboundAdmissionReleaseOperations { lifecycle, lease ->
                assertEquals(LifecycleReleaseOutcome.Released, lifecycle.releaseAdmission(lease))
                LifecycleReleaseOutcome.NotOwned
            },
        )
        val lifecycle = fixture.start()
        val activation = fixture.messenger.activateAttempt(lifecycle)
        val client = FakeWaddleClient().also { fake ->
            fake.beforeSendReturns = { throw primary }
        }
        assertTrue(fixture.messenger.attachTransport(activation.handle, client))
        assertTrue(fixture.messenger.markReady(activation.handle, client, activation.bootstrap.attempt))

        try {
            fixture.messenger.sendOrEnqueue(PEER, false, "cancelled")
            throw AssertionError("expected cancellation")
        } catch (actual: kotlinx.coroutines.CancellationException) {
            assertTrue(actual === primary)
            val violation = actual.suppressed.single() as LifecycleReleaseViolation
            assertEquals(LifecycleReleaseOutcome.NotOwned, violation.outcome)
            assertEquals(LifecycleReleaseSite.LIVE_OUTBOUND, violation.site)
            assertEquals(lifecycle, violation.lifecycle)
            assertEquals(activation.bootstrap.attempt, violation.attempt)
        }

        assertEquals(
            AttemptCloseOutcome.Closed,
            fixture.messenger.closeAttempt(activation.handle, producerQuiesced = true),
        )
        fixture.messenger.markTransportClosed(activation.handle, closed = true)
        fixture.stop(lifecycle)
    }

    @Test
    fun `I attached construction releases claim but requires transport close proof`() = runTest {
        val fixture = fixture(transitionTimeoutMillis = TEST_TIMEOUT_MILLIS)
        val lifecycle = fixture.start()
        val activation = fixture.messenger.activateAttempt(lifecycle)
        val construction = fixture.messenger.beginTransportConstruction(activation.handle)
        assertNotNull(construction)
        assertEquals(
            TransportAttachOutcome.Attached,
            fixture.messenger.attachConstructedTransport(
                construction!!,
                FakeWaddleClient(),
            ),
        )

        assertTrue(fixture.messenger.beginShutdown(lifecycle) is BeginShutdownDecision.Begun)
        assertEquals(
            AttemptCloseOutcome.OwnedBySessionShutdown,
            fixture.messenger.closeAttempt(activation.handle, producerQuiesced = true),
        )
        val fenced = fixture.messenger.shutdown(
            LifecycleShutdownTarget.CurrentOwner(lifecycle),
        )
        assertTrue(fenced is LifecycleShutdownOutcome.FencedWithPending)
        fenced as LifecycleShutdownOutcome.FencedWithPending
        assertEquals(LifecyclePendingComponent.NATIVE_CLIENT_CLOSE, fenced.component)

        fixture.messenger.markTransportClosed(activation.handle, closed = true)
        assertEquals(
            LifecycleShutdownOutcome.Stopped,
            fixture.messenger.shutdown(
                LifecycleShutdownTarget.CurrentOwner(lifecycle),
            ),
        )
    }

    private suspend fun TestScope.fixture(
        dataStore: FailingPreferencesDataStore = FailingPreferencesDataStore(),
        transitionTimeoutMillis: Long = 5_000L,
        scope: CoroutineScope = backgroundScope,
        phaseObserver: OutboundLifecyclePhaseObserver =
            OutboundLifecyclePhaseObserver.NONE,
        ownerFinalizer: (suspend (OwnerWorkers, SessionLifecycleRef, AttemptRecord?) -> OwnerFinalizationResult)? = null,
        admissionReleaseOperations: OutboundAdmissionReleaseOperations =
            OutboundAdmissionReleaseOperations.COORDINATOR,
    ): Fixture {
        val prefs = SessionPrefs(dataStore)
        prefs.activateSession(OWNER, SESSION_ID)
        val queue = OutboundQueue(prefs)
        val resume = ResumePersistence(prefs, queue)
        resume.start(backgroundScope)
        val activeSession = ActiveSession().also {
            it.ownBareJid = OWNER
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
            admissionReleaseOperations = admissionReleaseOperations,
        )
        return Fixture(
            messenger,
            scope,
            queue,
            activeSession,
        )
    }

    private data class Fixture(
        val messenger: OutboundMessenger,
        val scope: CoroutineScope,
        val queue: OutboundQueue,
        val activeSession: ActiveSession,
    ) {
        suspend fun start(): SessionLifecycleRef =
            messenger.start(scope, OWNER).started()

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

    private companion object {
        const val OWNER = "icepuma@waddle.test"
        const val PEER = "alice@waddle.test"
        const val SESSION_ID = "session-1"
        const val TEST_TIMEOUT_MILLIS = 100L
    }
}

private fun LifecycleStartResult.started(): SessionLifecycleRef =
    (this as? LifecycleStartResult.Started)?.lifecycle
        ?: error("test lifecycle startup failed: $this")
