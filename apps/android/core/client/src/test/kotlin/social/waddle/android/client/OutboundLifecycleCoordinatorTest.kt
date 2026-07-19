package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitCancellation
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
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleSendMessageOutcome

@OptIn(ExperimentalCoroutinesApi::class)
class OutboundLifecycleCoordinatorTest {
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
        assertFalse(fixture.messenger.beginShutdown(predecessor))
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
    fun `rotation cancellation after journal commit releases its exact lease`() = runTest {
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

        assertTrue(fixture.messenger.beginShutdown(lifecycle))
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

        assertTrue(fixture.messenger.beginShutdown(lifecycle))
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
    fun `shutdown retains construction until the superseded client is closed`() = runTest {
        val fixture = fixture(transitionTimeoutMillis = TEST_TIMEOUT_MILLIS)
        val lifecycle = fixture.start()
        val activation = fixture.messenger.activateAttempt(lifecycle)
        val construction = fixture.messenger.beginTransportConstruction(activation.handle)
        assertNotNull(construction)

        assertTrue(fixture.messenger.beginShutdown(lifecycle))
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
        assertEquals(
            LifecycleShutdownOutcome.Stopped,
            fixture.messenger.shutdown(
                LifecycleShutdownTarget.CurrentOwner(lifecycle),
            ),
        )
    }

    @Test
    fun `shutdown requires the attached transport close proof`() = runTest {
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

        assertTrue(fixture.messenger.beginShutdown(lifecycle))
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
        phaseObserver: OutboundLifecyclePhaseObserver =
            OutboundLifecyclePhaseObserver.NONE,
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
        )
        return Fixture(
            messenger,
            backgroundScope,
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
            messenger.start(scope, OWNER)

        suspend fun stop(lifecycle: SessionLifecycleRef) {
            assertTrue(messenger.beginShutdown(lifecycle))
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
