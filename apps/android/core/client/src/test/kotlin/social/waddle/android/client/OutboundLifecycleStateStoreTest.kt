package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddleSendMessageOutcome

@OptIn(ExperimentalCoroutinesApi::class)
class OutboundLifecycleStateStoreTest {
    @Test
    fun `blocked live send fences shutdown and refuses restart`() = runTest {
        val fixture = stateStoreFixture(transitionTimeoutMillis = STATE_STORE_TEST_TIMEOUT_MILLIS)
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
            fixture.messenger.sendOrEnqueue(STATE_STORE_PEER, false, "in flight")
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
        advanceTimeBy(STATE_STORE_TEST_TIMEOUT_MILLIS + 1)
        runCurrent()
        val outcome = shutdown.await()
        assertTrue(outcome is LifecycleShutdownOutcome.FencedWithPending)
        outcome as LifecycleShutdownOutcome.FencedWithPending
        assertEquals(LifecyclePendingComponent.ATTEMPT_LEASES, outcome.component)
        assertTrue(
            runCatching {
                fixture.messenger.start(backgroundScope, STATE_STORE_OWNER)
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
        val fixture = stateStoreFixture(
            dataStore = dataStore,
            transitionTimeoutMillis = STATE_STORE_TEST_TIMEOUT_MILLIS,
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
            fixture.messenger.sendOrEnqueue(STATE_STORE_PEER, false, "durably committed")
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
        advanceTimeBy(STATE_STORE_TEST_TIMEOUT_MILLIS + 1)
        runCurrent()
        val outcome = shutdown.await()
        assertTrue(outcome is LifecycleShutdownOutcome.FencedWithPending)
        outcome as LifecycleShutdownOutcome.FencedWithPending
        assertEquals(LifecyclePendingComponent.ATTEMPT_LEASES, outcome.component)
        assertTrue(
            runCatching {
                fixture.messenger.start(backgroundScope, STATE_STORE_OWNER)
            }.isFailure,
        )

        release.complete(Unit)
        runCurrent()
        assertNotNull(send.await().delivery)
        fixture.retryShutdownAndStartReplacement(lifecycle)
    }

    @Test
    fun `I superseded construction retains then accepts its documented exact retry`() = runTest {
        val fixture = stateStoreFixture(transitionTimeoutMillis = STATE_STORE_TEST_TIMEOUT_MILLIS)
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
        val fixture = stateStoreFixture()
        val lifecycle = fixture.start()

        val offline = fixture.messenger.sendOrEnqueue(STATE_STORE_PEER, false, "offline")
        assertEquals(WaddleSendMessageOutcome.NotConnected, offline.outcome)

        val activation = fixture.messenger.activateAttempt(lifecycle)
        fixture.messenger.drainDeliveryJournal()

        val client = FakeWaddleClient()
        assertTrue(fixture.messenger.attachTransport(activation.handle, client))
        assertTrue(
            fixture.messenger.markReady(
                activation.handle,
                client,
                activation.bootstrap.attempt,
            ),
        )
        val live = fixture.messenger.sendOrEnqueue(STATE_STORE_PEER, false, "live")
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
        val fixture = stateStoreFixture(
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
            fixture.messenger.sendOrEnqueue(STATE_STORE_PEER, false, "cancelled")
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
        val fixture = stateStoreFixture(transitionTimeoutMillis = STATE_STORE_TEST_TIMEOUT_MILLIS)
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
}
