package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.ConnectionLoopPullHarness.Companion.OWNER
import social.waddle.android.client.ConnectionLoopPullHarness.Companion.PEER
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.toDomain
import social.waddle.android.client.prefs.toFfi
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleDeliveryAttemptTransition
import social.waddle.client.ffi.WaddleSessionReadyKind

@OptIn(ExperimentalCoroutinesApi::class)
class ConnectionLoopPullTest {
    @Test
    fun `native pull is serialized with no prefetch`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            val client = harness.factory.clients.single()
            assertPulls(client, calls = 1, inFlight = 1)

            harness.factory.emitReady()
            runCurrent()
            assertPulls(client, calls = 2, inFlight = 1)

            harness.factory.emit(WaddleClientEvent.Error("keep consuming"))
            runCurrent()
            assertPulls(client, calls = 3, inFlight = 1)
            assertEquals(1, client.maxInFlightNextEvents.get())
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `resume transition commits before the next native pull`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.seedResumableRow(RESUME_STANZA_ID)
            harness.start()
            runCurrent()
            val client = harness.factory.clients.single()
            val old = harness.factory.configs.single().deliveryAttempt.toDomain(OWNER)
            val fresh = old.copy(
                attemptId = DeliveryAttemptId(FRESH_ATTEMPT_ID),
                nativeGeneration = old.nativeGeneration.next(),
            )

            harness.dataStore.failAllUpdates = true
            harness.factory.emitResumeFailed(
                WaddleDeliveryAttemptTransition(old.toFfi(), fresh.toFfi()),
                affectedStanzaIds = listOf(RESUME_STANZA_ID),
            )
            runCurrent()
            assertPulls(client, calls = 1, inFlight = 0)
            assertEquals(old, harness.prefs.deliveryJournal.first().owners[OWNER]?.activeAttempt)

            harness.dataStore.failAllUpdates = false
            advanceTimeBy(250)
            runCurrent()
            assertPulls(client, calls = 2, inFlight = 1)
            assertEquals(fresh, harness.prefs.deliveryJournal.first().owners[OWNER]?.activeAttempt)
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `ack and failure effects complete before the next native pull`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            harness.factory.emitReady()
            runCurrent()
            val client = harness.factory.clients.single()

            val acked = harness.messenger.sendOrEnqueue(PEER, false, "acked")
            val ackedId = checkNotNull(acked.delivery).identity.clientStanzaId
            harness.dataStore.failAllUpdates = true
            harness.factory.emitAcked(ackedId)
            runCurrent()
            assertPulls(client, calls = 2, inFlight = 0)
            harness.dataStore.failAllUpdates = false
            advanceTimeBy(250)
            runCurrent()
            assertPulls(client, calls = 3, inFlight = 1)

            val failed = harness.messenger.sendOrEnqueue(PEER, false, "failed")
            val failedId = checkNotNull(failed.delivery).identity.clientStanzaId
            harness.dataStore.failAllUpdates = true
            harness.factory.emitFailed(failedId)
            runCurrent()
            assertPulls(client, calls = 3, inFlight = 0)
            harness.dataStore.failAllUpdates = false
            advanceTimeBy(250)
            runCurrent()
            assertPulls(client, calls = 4, inFlight = 1)
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `stale attempts and failed persistence CAS self fence without another pull`() = runTest {
        val stale = ConnectionLoopPullHarness(this)
        try {
            stale.start()
            runCurrent()
            val client = stale.factory.clients.single()
            val expected = stale.factory.configs.single().deliveryAttempt.toDomain(OWNER)
            val wrong = expected.copy(attemptId = DeliveryAttemptId(STALE_ATTEMPT_ID))
            stale.loop.stopAdmissions()
            stale.factory.emit(
                WaddleClientEvent.SessionReady(
                    WaddleSessionReadyKind.FRESH,
                    wrong.toFfi(),
                ),
            )
            runCurrent()
            assertFenced(client, expectedCalls = 1)
        } finally {
            stale.shutdown()
        }

        val failedCas = ConnectionLoopPullHarness(this)
        try {
            failedCas.start()
            runCurrent()
            failedCas.factory.emitReady()
            runCurrent()
            val client = failedCas.factory.clients.single()
            failedCas.queue.beginAttempt(OWNER)
            failedCas.loop.stopAdmissions()
            failedCas.factory.emitResumeStateChanged(testResumeState())
            runCurrent()
            assertFenced(client, expectedCalls = 2)
        } finally {
            failedCas.shutdown()
        }
    }

    @Test
    fun `disconnect and logout join pending pulls with one disconnect`() = runTest {
        val disconnected = ConnectionLoopPullHarness(this)
        var disconnectedClient: FakeWaddleClient? = null
        try {
            disconnected.start()
            runCurrent()
            disconnected.factory.emitReady()
            runCurrent()
            val client = disconnected.factory.clients.single()
            disconnectedClient = client
            disconnected.loop.stopAdmissions()
            disconnected.factory.emit(WaddleClientEvent.Disconnected)
            runCurrent()
            assertFenced(client, expectedCalls = 2)
        } finally {
            disconnected.shutdown()
        }
        val finalDisconnectedClient = checkNotNull(disconnectedClient)
        assertTrue(disconnected.loopJob.isCompleted)
        assertEquals(1, finalDisconnectedClient.disconnectCalls)

        val loggedOut = ConnectionLoopPullHarness(this)
        val client = try {
            loggedOut.start()
            runCurrent()
            loggedOut.factory.emitReady()
            runCurrent()
            loggedOut.factory.clients.single().also {
                assertEquals(1, it.inFlightNextEvents.get())
            }
        } finally {
            loggedOut.shutdown()
        }
        assertFenced(client, expectedCalls = 2)
        assertTrue(loggedOut.loopJob.isCompleted)
    }

    @Test
    fun `storage retry keeps socket live and persistent failure returns typed fence`() = runTest {
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
            assertTrue(result is DeliveryTerminalWorker.StopResult.FencedWithPending)
            result as DeliveryTerminalWorker.StopResult.FencedWithPending
            assertEquals(OWNER, result.ownerBareJid)
            assertTrue(result.pendingCommands > 0)
        } finally {
            harness.shutdown()
        }
    }

    @Test
    fun `old generation event after replacement is dropped`() = runTest {
        val harness = ConnectionLoopPullHarness(this)
        try {
            harness.start()
            runCurrent()
            harness.factory.emitReady()
            runCurrent()
            val oldClient = harness.factory.clients.single()

            harness.factory.emit(WaddleClientEvent.Disconnected)
            runCurrent()
            advanceTimeBy(1_000)
            runCurrent()
            harness.factory.emitReady(attemptIndex = 1)
            runCurrent()
            val newClient = harness.factory.clients[1]

            val message = WaddleClientEvent.Message(
                testMessage(
                    id = "replacement-message",
                    from = "$PEER/phone",
                    to = OWNER,
                ),
            )
            harness.factory.emitAt(0, message)
            runCurrent()
            assertTrue(harness.stores.timelineStore.timeline(PEER).value.isEmpty())
            assertEquals(2, oldClient.nextEventCalls.get())

            harness.factory.emitAt(1, message)
            runCurrent()
            assertEquals(1, harness.stores.timelineStore.timeline(PEER).value.size)
            assertEquals(3, newClient.nextEventCalls.get())
        } finally {
            harness.shutdown()
        }
    }

    private fun assertPulls(client: FakeWaddleClient, calls: Int, inFlight: Int) {
        assertEquals(calls, client.nextEventCalls.get())
        assertEquals(inFlight, client.inFlightNextEvents.get())
        assertEquals(1, client.maxInFlightNextEvents.get())
    }

    private fun assertFenced(client: FakeWaddleClient, expectedCalls: Int) {
        assertEquals(expectedCalls, client.nextEventCalls.get())
        assertEquals(0, client.inFlightNextEvents.get())
        assertEquals(1, client.disconnectCalls)
        assertFalse(client.maxInFlightNextEvents.get() > 1)
    }

    private companion object {
        const val RESUME_STANZA_ID = "resume-stanza-1"
        const val FRESH_ATTEMPT_ID = "00000000-0000-4000-8000-000000000002"
        const val STALE_ATTEMPT_ID = "00000000-0000-4000-8000-000000000099"
    }
}
