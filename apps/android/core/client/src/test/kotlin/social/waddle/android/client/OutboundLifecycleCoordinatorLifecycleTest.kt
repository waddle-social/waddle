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
class OutboundLifecycleCoordinatorLifecycleTest {
    @Test
    fun `concurrent disconnect callers share one generation operation`() = runTest {
        val fixture = coordinatorFixture()
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
        val fixture = coordinatorFixture()
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
            conversationJid = COORDINATOR_PEER,
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
            val fixture = coordinatorFixture(
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
            assertNull(fixture.queue.activeAttempt(COORDINATOR_OWNER))
            assertNull(fixture.activeSession.attemptRef)
            fixture.stop(lifecycle)
        }
    }

    @Test
    fun `I rotation cancellation after journal commit releases its exact lease`() = runTest {
        val committed = CompletableDeferred<Unit>()
        val fixture = coordinatorFixture(
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

        assertNull(fixture.queue.activeAttempt(COORDINATOR_OWNER))
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

}

