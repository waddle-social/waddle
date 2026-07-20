package social.waddle.android.client

import java.util.UUID
import java.util.logging.Handler
import java.util.logging.LogRecord
import java.util.logging.Logger
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.OutboundQueue.EnqueueResult
import social.waddle.android.client.OutboundQueue.LiveAdmissionResult
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryCallbackRef
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliveryOwnerJournal
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
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.TerminalReceipt
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptEffect
import social.waddle.android.client.prefs.TerminalReceiptId
import social.waddle.android.client.prefs.TerminalReceiptState

@OptIn(ExperimentalCoroutinesApi::class)
class DeliveryTerminalWorkerTest {
    @Test
    fun `capacity 256 backpressures the next signal without losing admitted work`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        prefs.activateSession(TERMINAL_WORKER_OWNER, "sess-a")
        val queue = OutboundQueue(prefs, capacityPerOwner = TERMINAL_WORKER_SIGNAL_COUNT)
        val attempt = queue.beginAttempt(TERMINAL_WORKER_OWNER).attempt
        val rows = seedNativeOwnedTerminalRows(prefs, queue, attempt, TERMINAL_WORKER_SIGNAL_COUNT)
        val effects = mutableListOf<XmppEvent>()
        val worker = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = { effects += it },
            commandCapacity = 256,
            evidence = WorkerExitExceptionEvidence(),
        )
        val run = terminalRun(worker, this)
        runCurrent()
        run.awaitStartupDrain()
        store.failAllUpdates = true

        val submissions = rows.map { row ->
            async {
                run.submitAndAwait(
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
        assertEquals(listOf("m-258"), queue.rows(TERMINAL_WORKER_OWNER).map { it.clientStanzaId })
        assertRequested(run)
    }

    @Test
    fun `record failure retries at 250 500 1000 2000 then repeated 5000 milliseconds`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        prefs.activateSession(TERMINAL_WORKER_OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(TERMINAL_WORKER_OWNER).attempt
        val row = terminalWorkerClaimed(
            queue.enqueueAndClaimAbsoluteHead(terminalWorkerDraft("m-1"), attempt),
        )
        val worker = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = {},
            evidence = WorkerExitExceptionEvidence(),
        )
        val run = terminalRun(worker, this)
        runCurrent()
        run.awaitStartupDrain()
        val baselineAttempts = store.updateAttempts.get()
        store.failAllUpdates = true

        val submission = async {
            run.submitAndAwait(
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
        assertTrue(queue.rows(TERMINAL_WORKER_OWNER).isEmpty())
        assertRequested(run)
    }

    @Test
    fun `startup drain applies durable terminal intent before admissions`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        prefs.activateSession(TERMINAL_WORKER_OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(TERMINAL_WORKER_OWNER).attempt
        val row = terminalWorkerClaimed(
            queue.enqueueAndClaimAbsoluteHead(terminalWorkerDraft("m-1"), attempt),
        )
        queue.recordTerminal(TERMINAL_WORKER_OWNER, row.clientStanzaId, attempt, DeliveryTerminalKind.ACK)
        val effects = mutableListOf<XmppEvent>()
        val worker = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = { effects += it },
            evidence = WorkerExitExceptionEvidence(),
        )

        val run = terminalRun(worker, this)
        runCurrent()
        run.awaitStartupDrain()

        assertTrue(queue.rows(TERMINAL_WORKER_OWNER).isEmpty())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertRequested(run)
    }

    @Test
    fun `apply failure parks durable intent until retry succeeds`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        prefs.activateSession(TERMINAL_WORKER_OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(TERMINAL_WORKER_OWNER).attempt
        val row = terminalWorkerClaimed(
            queue.enqueueAndClaimAbsoluteHead(terminalWorkerDraft("m-1"), attempt),
        )
        queue.recordTerminal(TERMINAL_WORKER_OWNER, row.clientStanzaId, attempt, DeliveryTerminalKind.ACK)
        val effects = mutableListOf<XmppEvent>()
        val worker = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = { effects += it },
            evidence = WorkerExitExceptionEvidence(),
        )
        store.failAllUpdates = true

        val run = terminalRun(worker, this)
        runCurrent()
        assertTrue(effects.isEmpty())
        assertTrue(queue.rows(TERMINAL_WORKER_OWNER).single().ownership is OutboundOwnership.Terminal)

        store.failAllUpdates = false
        advanceTimeBy(250)
        runCurrent()
        run.awaitStartupDrain()
        assertTrue(queue.rows(TERMINAL_WORKER_OWNER).isEmpty())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertRequested(run)
    }

    @Test
    fun `two startup appliers emit one exact terminal effect`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        prefs.activateSession(TERMINAL_WORKER_OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(TERMINAL_WORKER_OWNER).attempt
        val row = terminalWorkerClaimed(
            queue.enqueueAndClaimAbsoluteHead(terminalWorkerDraft("m-1"), attempt),
        )
        queue.recordTerminal(TERMINAL_WORKER_OWNER, row.clientStanzaId, attempt, DeliveryTerminalKind.ACK)
        val effects = mutableListOf<XmppEvent>()
        val first = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = { effects += it },
            evidence = WorkerExitExceptionEvidence(),
        )
        val second = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = { effects += it },
            evidence = WorkerExitExceptionEvidence(),
        )

        val firstRun = terminalRun(first, this)
        val secondRun = terminalRun(second, this)
        runCurrent()
        firstRun.awaitStartupDrain()
        secondRun.awaitStartupDrain()

        assertTrue(queue.rows(TERMINAL_WORKER_OWNER).isEmpty())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertRequested(firstRun)
        assertRequested(secondRun)
    }

    @Test
    fun `requested stop times out without cancelling durable terminal work`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        prefs.activateSession(TERMINAL_WORKER_OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(TERMINAL_WORKER_OWNER).attempt
        val row = terminalWorkerClaimed(
            queue.enqueueAndClaimAbsoluteHead(terminalWorkerDraft("m-1"), attempt),
        )
        queue.recordTerminal(TERMINAL_WORKER_OWNER, row.clientStanzaId, attempt, DeliveryTerminalKind.ACK)
        val effects = mutableListOf<XmppEvent>()
        val worker = DeliveryTerminalWorker(
            journal = queue,
            dispatchEvent = { effects += it },
            evidence = WorkerExitExceptionEvidence(),
        )
        store.failAllUpdates = true
        val run = terminalRun(worker, this)
        runCurrent()

        run.requestStop()
        assertEquals(WorkerAwaitOutcome.TimedOut, run.awaitExit(1))
        assertEquals(1, queue.terminalIntentCount(TERMINAL_WORKER_OWNER))
        assertTrue(queue.rows(TERMINAL_WORKER_OWNER).single().ownership is OutboundOwnership.Terminal)

        store.failAllUpdates = false
        advanceTimeBy(250)
        runCurrent()
        assertTrue(queue.rows(TERMINAL_WORKER_OWNER).isEmpty())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(
            WorkerExitReason.RequestedStop,
            (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit.reason,
        )
    }

    @Test
    fun `fatal terminal dependency failure exits with exact ownership`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        prefs.activateSession(TERMINAL_WORKER_OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(TERMINAL_WORKER_OWNER).attempt
        val row = terminalWorkerClaimed(queue.enqueueAndClaimAbsoluteHead(terminalWorkerDraft("m-1"), attempt))
        queue.recordTerminal(TERMINAL_WORKER_OWNER, row.clientStanzaId, attempt, DeliveryTerminalKind.ACK)
        val worker = DeliveryTerminalWorker(
            queue,
            dispatchEvent = { error("terminal exploded") },
            evidence = WorkerExitExceptionEvidence(),
        )
        val run = terminalRun(worker, this)

        runCurrent()

        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertEquals(run.ownership.lifecycle, exit.lifecycle)
        assertEquals(run.ownership.generation, exit.generation)
        assertEquals(WorkerKind.DELIVERY_TERMINAL, exit.kind)
        assertEquals(
            WorkerFailureKind.DEPENDENCY_FAILURE,
            (exit.reason as WorkerExitReason.UnexpectedFailure).kind,
        )
    }

    @Test
    fun `requested stop drains an admitted terminal command before its exit`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        prefs.activateSession(TERMINAL_WORKER_OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(TERMINAL_WORKER_OWNER).attempt
        val row = terminalWorkerClaimed(queue.enqueueAndClaimAbsoluteHead(terminalWorkerDraft("m-1"), attempt))
        val effects = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(queue, { effects += it }, evidence = WorkerExitExceptionEvidence()),
            this,
        )
        run.awaitStartupDrain()
        store.failAllUpdates = true

        val admitted = async {
            run.submitAndAwait(row.clientStanzaId, attempt, DeliveryTerminalKind.ACK)
        }
        runCurrent()
        assertTrue(!admitted.isCompleted)
        run.requestStop()

        store.failAllUpdates = false
        advanceTimeBy(250)
        runCurrent()

        assertEquals(TerminalCommandOutcome.Committed, admitted.await())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(
            WorkerExitReason.RequestedStop,
            (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit.reason,
        )
    }

    @Test
    fun `terminal requested stop emits one matching exit`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        prefs.activateSession(TERMINAL_WORKER_OWNER, "sess-a")
        val exits = mutableListOf<WorkerExit>()
        val run = terminalRun(
            DeliveryTerminalWorker(OutboundQueue(prefs), {}, evidence = WorkerExitExceptionEvidence()),
            this,
        ) { exits += it }

        runCurrent()
        run.requestStop()

        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertEquals(run.ownership.lifecycle, exit.lifecycle)
        assertEquals(run.ownership.generation, exit.generation)
        assertEquals(WorkerKind.DELIVERY_TERMINAL, exit.kind)
        assertEquals(WorkerExitReason.RequestedStop, exit.reason)
        assertEquals(listOf(exit), exits)
    }

    @Test
    fun `sequential terminal runs isolate old submission and exit`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        prefs.activateSession(TERMINAL_WORKER_OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(TERMINAL_WORKER_OWNER).attempt
        val row = terminalWorkerClaimed(queue.enqueueAndClaimAbsoluteHead(terminalWorkerDraft("m-1"), attempt))
        val effects = mutableListOf<XmppEvent>()
        val worker = DeliveryTerminalWorker(
            queue,
            dispatchEvent = { effects += it },
            evidence = WorkerExitExceptionEvidence(),
        )
        val firstExits = mutableListOf<WorkerExit>()
        val first = terminalRun(worker, this) { firstExits += it }
        first.awaitStartupDrain()
        first.requestStop()
        val firstExit = (first.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit

        val secondExits = mutableListOf<WorkerExit>()
        val second = terminalRun(worker, this) { secondExits += it }
        second.awaitStartupDrain()
        assertEquals(
            TerminalCommandOutcome.WorkerUnavailable,
            first.submitAndAwait(row.clientStanzaId, attempt, DeliveryTerminalKind.ACK),
        )
        assertEquals(
            TerminalCommandOutcome.Committed,
            second.submitAndAwait(row.clientStanzaId, attempt, DeliveryTerminalKind.ACK),
        )
        second.requestStop()
        val secondExit = (second.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit

        assertTrue(first.ownership.generation != second.ownership.generation)
        assertEquals(1, firstExits.size)
        assertEquals(firstExit, firstExits.single())
        assertEquals(1, secondExits.size)
        assertEquals(secondExit, secondExits.single())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
    }

}
