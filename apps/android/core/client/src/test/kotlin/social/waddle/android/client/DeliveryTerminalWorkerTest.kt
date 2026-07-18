package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.OutboundQueue.EnqueueResult
import social.waddle.android.client.OutboundQueue.LiveAdmissionResult
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.SessionPrefs

@OptIn(ExperimentalCoroutinesApi::class)
class DeliveryTerminalWorkerTest {
    @Test
    fun `capacity 256 backpressures the next signal without losing admitted work`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        prefs.activateSession(OWNER, "sess-a")
        val queue = OutboundQueue(prefs, capacityPerOwner = SIGNAL_COUNT)
        val attempt = queue.beginAttempt(OWNER).attempt
        val rows = seedNativeOwnedRows(prefs, queue, attempt, SIGNAL_COUNT)
        val effects = mutableListOf<XmppEvent>()
        val worker = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = { effects += it },
            commandCapacity = 256,
            stopTimeoutMillis = 1_000,
        )
        worker.start(this, OWNER)
        runCurrent()
        worker.awaitStartupDrain(OWNER)
        store.failAllUpdates = true

        val submissions = rows.map { row ->
            async {
                worker.submitAndAwait(
                    OWNER,
                    row.clientStanzaId,
                    attempt,
                    DeliveryTerminalKind.ACK,
                )
            }
        }
        runCurrent()
        assertTrue(submissions.all { !it.isCompleted })

        // One command is executing and 256 fit in the bounded channel.
        // Cancelling command 258 while it is blocked on admission must leave
        // only that exact row untouched after every admitted command drains.
        submissions.last().cancelAndJoin()
        store.failAllUpdates = false
        advanceTimeBy(250)
        runCurrent()
        submissions.dropLast(1).forEach { it.await() }

        assertEquals(257, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(listOf("m-258"), queue.rows(OWNER).map { it.clientStanzaId })
        assertEquals(DeliveryTerminalWorker.StopResult.Drained, worker.stop(OWNER))
    }

    @Test
    fun `record failure retries at 250 500 1000 2000 then repeated 5000 milliseconds`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        prefs.activateSession(OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(OWNER).attempt
        val row = claimed(
            queue.enqueueAndClaimAbsoluteHead(draft("m-1"), attempt),
        )
        val worker = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = {},
        )
        worker.start(this, OWNER)
        runCurrent()
        worker.awaitStartupDrain(OWNER)
        val baselineAttempts = store.updateAttempts.get()
        store.failAllUpdates = true

        val submission = async {
            worker.submitAndAwait(
                OWNER,
                row.clientStanzaId,
                attempt,
                DeliveryTerminalKind.ACK,
            )
        }
        runCurrent()
        var expectedAttempts = baselineAttempts + 1
        assertEquals(expectedAttempts, store.updateAttempts.get())

        listOf(250L, 500L, 1_000L, 2_000L, 5_000L, 5_000L).forEach { delay ->
            advanceTimeBy(delay - 1)
            runCurrent()
            assertEquals(expectedAttempts, store.updateAttempts.get())
            advanceTimeBy(1)
            runCurrent()
            expectedAttempts += 1
            assertEquals(expectedAttempts, store.updateAttempts.get())
        }

        store.failAllUpdates = false
        advanceTimeBy(5_000)
        runCurrent()
        submission.await()
        assertTrue(queue.rows(OWNER).isEmpty())
        assertEquals(DeliveryTerminalWorker.StopResult.Drained, worker.stop(OWNER))
    }

    @Test
    fun `startup drain applies durable terminal intent before admissions`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        prefs.activateSession(OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(OWNER).attempt
        val row = claimed(
            queue.enqueueAndClaimAbsoluteHead(draft("m-1"), attempt),
        )
        queue.recordTerminal(OWNER, row.clientStanzaId, attempt, DeliveryTerminalKind.ACK)
        val effects = mutableListOf<XmppEvent>()
        val worker = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = { effects += it },
        )

        worker.start(this, OWNER)
        runCurrent()
        worker.awaitStartupDrain(OWNER)

        assertTrue(queue.rows(OWNER).isEmpty())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(DeliveryTerminalWorker.StopResult.Drained, worker.stop(OWNER))
    }

    @Test
    fun `apply failure parks durable intent until retry succeeds`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        prefs.activateSession(OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(OWNER).attempt
        val row = claimed(
            queue.enqueueAndClaimAbsoluteHead(draft("m-1"), attempt),
        )
        queue.recordTerminal(OWNER, row.clientStanzaId, attempt, DeliveryTerminalKind.ACK)
        val effects = mutableListOf<XmppEvent>()
        val worker = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = { effects += it },
        )
        store.failAllUpdates = true

        worker.start(this, OWNER)
        runCurrent()
        assertTrue(effects.isEmpty())
        assertTrue(queue.rows(OWNER).single().ownership is OutboundOwnership.Terminal)

        store.failAllUpdates = false
        advanceTimeBy(250)
        runCurrent()
        worker.awaitStartupDrain(OWNER)
        assertTrue(queue.rows(OWNER).isEmpty())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(DeliveryTerminalWorker.StopResult.Drained, worker.stop(OWNER))
    }

    @Test
    fun `two startup appliers emit one exact terminal effect`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        prefs.activateSession(OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(OWNER).attempt
        val row = claimed(
            queue.enqueueAndClaimAbsoluteHead(draft("m-1"), attempt),
        )
        queue.recordTerminal(OWNER, row.clientStanzaId, attempt, DeliveryTerminalKind.ACK)
        val effects = mutableListOf<XmppEvent>()
        val first = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = { effects += it },
        )
        val second = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = { effects += it },
        )

        first.start(this, OWNER)
        second.start(this, OWNER)
        runCurrent()
        first.awaitStartupDrain(OWNER)
        second.awaitStartupDrain(OWNER)

        assertTrue(queue.rows(OWNER).isEmpty())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(DeliveryTerminalWorker.StopResult.Drained, first.stop(OWNER))
        assertEquals(DeliveryTerminalWorker.StopResult.Drained, second.stop(OWNER))
    }

    @Test
    fun `bounded shutdown fences pending apply and restart drains it`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        prefs.activateSession(OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(OWNER).attempt
        val row = claimed(
            queue.enqueueAndClaimAbsoluteHead(draft("m-1"), attempt),
        )
        queue.recordTerminal(OWNER, row.clientStanzaId, attempt, DeliveryTerminalKind.ACK)
        val effects = mutableListOf<XmppEvent>()
        val worker = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = { effects += it },
            stopTimeoutMillis = 30_000,
        )
        store.failAllUpdates = true
        worker.start(this, OWNER)
        runCurrent()

        val stopping = async { worker.stop(OWNER) }
        advanceTimeBy(30_001)
        runCurrent()
        val stopped = stopping.await()
        assertTrue(stopped is DeliveryTerminalWorker.StopResult.FencedWithPending)
        stopped as DeliveryTerminalWorker.StopResult.FencedWithPending
        assertEquals(OWNER, stopped.ownerBareJid)
        assertEquals(1, stopped.pendingCommands)
        assertEquals(1, queue.terminalIntentCount(OWNER))
        assertTrue(queue.rows(OWNER).single().ownership is OutboundOwnership.Terminal)

        store.failAllUpdates = false
        worker.start(this, OWNER)
        runCurrent()
        worker.awaitStartupDrain(OWNER)
        assertTrue(queue.rows(OWNER).isEmpty())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(DeliveryTerminalWorker.StopResult.Drained, worker.stop(OWNER))
    }

    private fun draft(id: String): QueuedOutboundDraft = QueuedOutboundDraft.create(
        ownerBareJid = OWNER,
        clientStanzaId = id,
        enqueuedAtMillis = 1_000,
        payload = QueuedOutboundPayload(
            target = QueuedOutboundTarget.Chat("peer@waddle.test"),
            content = QueuedOutboundContent("body-$id"),
        ),
        source = DeliverySource.Composer,
    )

    private fun stored(result: EnqueueResult): QueuedOutboundMessage =
        (result as EnqueueResult.Stored).row

    private fun claimed(result: LiveAdmissionResult): QueuedOutboundMessage =
        (result as LiveAdmissionResult.Claimed).row

    private suspend fun seedNativeOwnedRows(
        prefs: SessionPrefs,
        queue: OutboundQueue,
        attempt: DeliveryAttemptRef,
        count: Int,
    ): List<QueuedOutboundMessage> {
        val readyRows = (1..count).map { index ->
            stored(queue.enqueueReady(draft("m-$index")))
        }
        return prefs.updateDeliveryJournal { journal ->
            val owner = checkNotNull(journal.owners[OWNER])
            val nativeRows = owner.outboundRows.map { row ->
                row.copy(
                    ownership = OutboundOwnership.NativeOwned(
                        attempt,
                        NativeOutboundPhase.FRESH,
                    ),
                )
            }
            DeliveryJournalMutation(
                journal = journal.copy(
                    owners = journal.owners + (
                        OWNER to owner.copy(outboundRows = nativeRows)
                    ),
                ),
                result = nativeRows,
            )
        }.also { nativeRows ->
            check(nativeRows.map { it.identity } == readyRows.map { it.identity })
        }
    }

    private companion object {
        const val OWNER = "alice@waddle.test"
        const val SIGNAL_COUNT = 258
    }
}
