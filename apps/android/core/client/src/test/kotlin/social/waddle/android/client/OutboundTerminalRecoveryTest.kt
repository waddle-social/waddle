package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class OutboundTerminalRecoveryTest {
    @Test
    fun `persistent terminal failure fences ordinary restart until explicit recovery`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            harness.factory.emitReady()
            runCurrent()
            val client = harness.factory.clients.single()

            harness.dataStore.failAllUpdates = true
            harness.factory.emitResumeStateChanged(testResumeState())
            runCurrent()
            advanceTimeBy(250)
            runCurrent()
            assertPulls(client, calls = 2, inFlight = 0)
            assertEquals(0, client.disconnectCalls)

            harness.dataStore.failAllUpdates = false
            advanceTimeBy(500)
            runCurrent()
            assertPulls(client, calls = 3, inFlight = 1)

            val sent = harness.messenger.sendOrEnqueue(PEER, false, "persist forever")
            val stanzaId = checkNotNull(sent.delivery).identity.clientStanzaId
            val activeAttempt = checkNotNull(
                harness.queue.activeAttempt(ConnectionLoopPullHarness.OWNER),
            )
            harness.dataStore.failAllUpdates = true
            harness.factory.emitAcked(stanzaId)
            runCurrent()
            assertEquals(0, client.disconnectCalls)

            val stopping = async { harness.stopTerminalWorker() }
            runCurrent()
            advanceTimeBy(30_000)
            harness.dataStore.failAllUpdates = false
            runCurrent()
            val result = stopping.await()
            assertTrue(result is LifecycleShutdownOutcome.FencedWithPending)
            result as LifecycleShutdownOutcome.FencedWithPending
            assertEquals(harness.lifecycle, result.lifecycle)
            assertEquals(LifecyclePendingComponent.TERMINAL_DRAIN, result.component)
            assertTrue(result.pending > 0)
            assertEquals(
                activeAttempt,
                harness.queue.activeAttempt(ConnectionLoopPullHarness.OWNER),
            )

            val fencedLifecycle = harness.lifecycle
            assertTrue(
                "ordinary restart must remain fenced while terminal intents are pending",
                runCatching { harness.startReplacementLifecycle() }.isFailure,
            )
            assertTrue(harness.recoverFencedWorkers(fencedLifecycle) is WorkerRecoveryOutcome.WorkerExitPending)
            advanceTimeBy(30_000)
            runCurrent()
            assertEquals(WorkerRecoveryOutcome.Recovered, harness.recoverFencedWorkers(fencedLifecycle))
            val replacement = harness.startReplacementLifecycle()
            assertTrue(replacement != fencedLifecycle)
            runCurrent()
            assertEquals(
                LifecycleShutdownOutcome.Stopped,
                harness.stopReplacementLifecycle(),
            )
        } finally {
            harness.shutdown()
        }
    }

    private fun assertPulls(
        client: FakeWaddleClient,
        calls: Int,
        inFlight: Int,
    ) {
        assertEquals(calls, client.nextEventCalls.get())
        assertEquals(inFlight, client.inFlightNextEvents.get())
        assertEquals(1, client.maxInFlightNextEvents.get())
    }

    private companion object {
        const val PEER = "alice@waddle.test"
    }
}
