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
    fun `startup applies and acknowledges a persisted terminal receipt before legacy intent drain`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val row = QueuedOutboundDraft.create(
            ownerBareJid = OWNER,
            clientStanzaId = "receipt-row",
            enqueuedAtMillis = 1,
            payload = QueuedOutboundPayload(
                target = QueuedOutboundTarget.Chat("peer@waddle.test"),
                content = QueuedOutboundContent("receipt"),
            ),
        ).persisted(1, OutboundOwnership.Ready)
        val attempt = DeliveryAttemptRef(
            ownerBareJid = OWNER,
            attemptId = DeliveryAttemptId(uuid("receipt-attempt")),
            nativeGeneration = social.waddle.android.client.prefs.NativeConnectionGeneration(1u),
        )
        val receipt = TerminalReceipt(
            owner = DeliveryOwnerBareJid(OWNER),
            attempt = attempt,
            id = TerminalReceiptId(uuid("receipt-id")),
            originProcessEpoch = ProcessEpoch(uuid("receipt-origin")),
            preparedAtMillis = 1,
            state = TerminalReceiptState.Pending(
                TerminalReceiptClaimState.Unclaimed,
                listOf(TerminalReceiptEffect.Acknowledged(DeliveryCallbackRef(row.identity, attempt), row)),
            ),
        )
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = OWNER,
                    owners = journal.owners + (OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val effects = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(
                journal = OutboundQueue(prefs),
                dispatchEvent = { effects += it },
                processEpoch = ProcessEpoch(uuid("worker-process")),
            ),
            this,
        )

        run.awaitStartupDrain()

        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(
            TerminalReceiptState.Acknowledged,
            prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt?.state,
        )
        assertRequested(run)
    }

    @Test
    fun `receipt acknowledgement io retry does not redispatch persisted effects`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingReceipt(OWNER, "retry")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = OWNER,
                    owners = journal.owners + (OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val effects = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(
                journal = OutboundQueue(prefs),
                dispatchEvent = {
                    effects += it
                    store.failNextUpdate = true
                },
                processEpoch = ProcessEpoch(uuid("retry-process")),
            ),
            this,
        )

        runCurrent()
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        advanceTimeBy(250)
        runCurrent()
        run.awaitStartupDrain()
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(
            TerminalReceiptState.Acknowledged,
            prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt?.state,
        )
        assertRequested(run)
    }

    @Test
    fun `same process receipt claim is busy and does not dispatch`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val epoch = ProcessEpoch(uuid("busy-epoch"))
        val receipt = pendingReceipt(OWNER, "busy").copy(
            state = (pendingReceipt(OWNER, "busy").state as TerminalReceiptState.Pending).copy(
                claim = TerminalReceiptClaimState.Claimed(
                    social.waddle.android.client.prefs.TerminalClaimId(uuid("busy-claim")),
                    social.waddle.android.client.prefs.TerminalReceiptClaimant.BootstrapProcess,
                    epoch,
                ),
            ),
        )
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = OWNER,
                    owners = journal.owners + (OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val effects = mutableListOf<XmppEvent>()
        val run = terminalRun(DeliveryTerminalWorker(OutboundQueue(prefs), { effects += it }, epoch), this)
        run.awaitStartupDrain()
        assertTrue(effects.isEmpty())
        assertEquals(receipt, prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt)
        assertRequested(run)
    }

    @Test
    fun `new process epoch reclaims receipt and typed active owner mismatch fences`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val receipt = pendingReceipt(OWNER, "reclaim").copy(
            state = (pendingReceipt(OWNER, "reclaim").state as TerminalReceiptState.Pending).copy(
                claim = TerminalReceiptClaimState.Claimed(
                    social.waddle.android.client.prefs.TerminalClaimId(uuid("old-claim")),
                    social.waddle.android.client.prefs.TerminalReceiptClaimant.BootstrapProcess,
                    ProcessEpoch(uuid("old-epoch")),
                ),
            ),
        )
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = OWNER,
                    owners = journal.owners + (OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val effects = mutableListOf<XmppEvent>()
        val run = terminalRun(DeliveryTerminalWorker(OutboundQueue(prefs), { effects += it }, ProcessEpoch(uuid("new-epoch"))), this)
        run.awaitStartupDrain()
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(TerminalReceiptState.Acknowledged, prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt?.state)
        assertRequested(run)
    }

    @Test
    fun `claim commit uncertainty retries the same receipt before dispatch`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingReceipt(OWNER, "claim-uncertain")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = OWNER,
                    owners = journal.owners + (OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        store.installAfterCommitReturnsOnce { throw java.io.IOException("claim returned uncertain") }
        val effects = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(
                journal = OutboundQueue(prefs),
                dispatchEvent = { effects += it },
                processEpoch = ProcessEpoch(uuid("claim-uncertain-process")),
            ),
            this,
        )

        advanceTimeBy(250)
        runCurrent()
        run.awaitStartupDrain()

        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(
            TerminalReceiptState.Acknowledged,
            prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt?.state,
        )
        assertRequested(run)
    }

    @Test
    fun `dispatch prefix failure releases then a new process replays the whole receipt`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val receipt = pendingReceipt(OWNER, "prefix", effectCount = 2)
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = OWNER,
                    owners = journal.owners + (OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val firstEvents = mutableListOf<XmppEvent>()
        val first = terminalRun(
            DeliveryTerminalWorker(
                journal = OutboundQueue(prefs),
                dispatchEvent = {
                    firstEvents += it
                    throw IllegalStateException("dispatch prefix failed")
                },
                processEpoch = ProcessEpoch(uuid("prefix-first-process")),
            ),
            this,
        )

        val firstExit = (first.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertEquals(
            WorkerFailureKind.DEPENDENCY_FAILURE,
            (firstExit.reason as WorkerExitReason.UnexpectedFailure).kind,
        )
        assertEquals(1, firstEvents.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        val released = prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt
        assertEquals(
            TerminalReceiptClaimState.Unclaimed,
            (released?.state as TerminalReceiptState.Pending).claim,
        )

        val replayed = mutableListOf<XmppEvent>()
        val second = terminalRun(
            DeliveryTerminalWorker(
                journal = OutboundQueue(prefs),
                dispatchEvent = { replayed += it },
                processEpoch = ProcessEpoch(uuid("prefix-second-process")),
            ),
            this,
        )
        second.awaitStartupDrain()

        assertEquals(2, replayed.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(
            TerminalReceiptState.Acknowledged,
            prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt?.state,
        )
        assertRequested(second)
    }

    @Test
    fun `acknowledge commit cancellation does not replay after restart`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingReceipt(OWNER, "ack-cancel")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = OWNER,
                    owners = journal.owners + (OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val firstEvents = mutableListOf<XmppEvent>()
        val cancellation = kotlinx.coroutines.CancellationException("acknowledge returned cancelled")
        val first = terminalRun(
            DeliveryTerminalWorker(
                journal = OutboundQueue(prefs),
                dispatchEvent = {
                    firstEvents += it
                    store.afterCommitReturns = {
                        store.afterCommitReturns = null
                        throw cancellation
                    }
                },
                processEpoch = ProcessEpoch(uuid("ack-cancel-first-process")),
            ),
            this,
        )

        val firstExit = (first.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertEquals(WorkerExitReason.OwnerScopeCancelled, firstExit.reason)
        assertEquals(1, firstEvents.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(
            TerminalReceiptState.Acknowledged,
            prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt?.state,
        )

        val restartedEvents = mutableListOf<XmppEvent>()
        val second = terminalRun(
            DeliveryTerminalWorker(
                journal = OutboundQueue(prefs),
                dispatchEvent = { restartedEvents += it },
                processEpoch = ProcessEpoch(uuid("ack-cancel-second-process")),
            ),
            this,
        )
        second.awaitStartupDrain()
        assertTrue(restartedEvents.isEmpty())
        assertRequested(second)
    }

    @Test
    fun `active owner mismatch fences the receipt worker with typed poison evidence`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val receipt = pendingReceipt(OWNER, "active-owner")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = "other@waddle.test",
                    owners = journal.owners + (OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val run = terminalRun(
            DeliveryTerminalWorker(OutboundQueue(prefs), dispatchEvent = {}),
            this,
        )

        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        val failure = (exit.reason as WorkerExitReason.UnexpectedFailure).kind
        assertEquals(
            WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION(
                TerminalReceiptApplicationFailure(
                    TerminalReceiptOperation.DISCOVERY,
                    TerminalReceiptCorruption.ACTIVE_OWNER_MISMATCH,
                ),
            ),
            failure,
        )
        assertFalse(run.ownership.lifecycle.ownerBareJid == "other@waddle.test")
    }

    @Test
    fun `corrupt native owned receipt state fences with its exact typed reason`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val receipt = pendingReceipt(OWNER, "native-poison")
        val row = (receipt.state as TerminalReceiptState.Pending).effects.single().row.copy(
            ownership = OutboundOwnership.NativeOwned(receipt.attempt, NativeOutboundPhase.FRESH),
        )
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = OWNER,
                    owners = journal.owners + (
                        OWNER to DeliveryOwnerJournal(
                            outboundRows = listOf(row),
                            terminalReceipt = receipt,
                        )
                    ),
                ),
                Unit,
            )
        }
        val run = terminalRun(
            DeliveryTerminalWorker(OutboundQueue(prefs), dispatchEvent = {}),
            this,
        )

        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertEquals(
            WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION(
                TerminalReceiptApplicationFailure(
                    TerminalReceiptOperation.DISCOVERY,
                    TerminalReceiptCorruption.NATIVE_OWNED_ROW_REMAINS,
                ),
            ),
            (exit.reason as WorkerExitReason.UnexpectedFailure).kind,
        )
    }

    @Test
    fun `release failure is suppressed on the original dispatch failure`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingReceipt(OWNER, "release-failure")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = OWNER,
                    owners = journal.owners + (OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val primary = IllegalStateException("dispatch failed")
        val logger = Logger.getLogger(DeliveryTerminalWorker::class.java.name)
        val handler = object : Handler() {
            var observed: Throwable? = null

            override fun publish(record: LogRecord) {
                if (record.thrown === primary) observed = record.thrown
            }

            override fun flush() = Unit

            override fun close() = Unit
        }
        logger.addHandler(handler)
        try {
            val run = terminalRun(
                DeliveryTerminalWorker(
                    journal = OutboundQueue(prefs),
                    dispatchEvent = {
                        store.failNextUpdate = true
                        throw primary
                    },
                    processEpoch = ProcessEpoch(uuid("release-failure-process")),
                ),
                this,
            )

            val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
            assertEquals(
                WorkerFailureKind.DEPENDENCY_FAILURE,
                (exit.reason as WorkerExitReason.UnexpectedFailure).kind,
            )
            assertEquals(primary, handler.observed)
            assertEquals(1, primary.suppressed.size)
            assertEquals(
                TerminalReceiptApplicationFailure(TerminalReceiptOperation.RELEASE, null),
                (primary.suppressed.single() as TerminalReceiptApplicationException).failure,
            )
            val pending = prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt?.state
                as TerminalReceiptState.Pending
            assertTrue(pending.claim is TerminalReceiptClaimState.Claimed)
        } finally {
            logger.removeHandler(handler)
        }
    }

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
        assertEquals(listOf("m-258"), queue.rows(OWNER).map { it.clientStanzaId })
        assertRequested(run)
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
        assertTrue(queue.rows(OWNER).isEmpty())
        assertRequested(run)
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

        val run = terminalRun(worker, this)
        runCurrent()
        run.awaitStartupDrain()

        assertTrue(queue.rows(OWNER).isEmpty())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertRequested(run)
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

        val run = terminalRun(worker, this)
        runCurrent()
        assertTrue(effects.isEmpty())
        assertTrue(queue.rows(OWNER).single().ownership is OutboundOwnership.Terminal)

        store.failAllUpdates = false
        advanceTimeBy(250)
        runCurrent()
        run.awaitStartupDrain()
        assertTrue(queue.rows(OWNER).isEmpty())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertRequested(run)
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

        val firstRun = terminalRun(first, this)
        val secondRun = terminalRun(second, this)
        runCurrent()
        firstRun.awaitStartupDrain()
        secondRun.awaitStartupDrain()

        assertTrue(queue.rows(OWNER).isEmpty())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertRequested(firstRun)
        assertRequested(secondRun)
    }

    @Test
    fun `requested stop times out without cancelling durable terminal work`() = runTest {
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
        val run = terminalRun(worker, this)
        runCurrent()

        run.requestStop()
        assertEquals(WorkerAwaitOutcome.TimedOut, run.awaitExit(1))
        assertEquals(1, queue.terminalIntentCount(OWNER))
        assertTrue(queue.rows(OWNER).single().ownership is OutboundOwnership.Terminal)

        store.failAllUpdates = false
        advanceTimeBy(250)
        runCurrent()
        assertTrue(queue.rows(OWNER).isEmpty())
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertEquals(
            WorkerExitReason.RequestedStop,
            (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit.reason,
        )
    }

    @Test
    fun `fatal terminal dependency failure exits with exact ownership`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        prefs.activateSession(OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(OWNER).attempt
        val row = claimed(queue.enqueueAndClaimAbsoluteHead(draft("m-1"), attempt))
        queue.recordTerminal(OWNER, row.clientStanzaId, attempt, DeliveryTerminalKind.ACK)
        val worker = DeliveryTerminalWorker(queue, dispatchEvent = { error("terminal exploded") })
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
        prefs.activateSession(OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(OWNER).attempt
        val row = claimed(queue.enqueueAndClaimAbsoluteHead(draft("m-1"), attempt))
        val effects = mutableListOf<XmppEvent>()
        val run = terminalRun(DeliveryTerminalWorker(queue, { effects += it }), this)
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
        prefs.activateSession(OWNER, "sess-a")
        val exits = mutableListOf<WorkerExit>()
        val run = terminalRun(DeliveryTerminalWorker(OutboundQueue(prefs), {}), this) { exits += it }

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
        prefs.activateSession(OWNER, "sess-a")
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(OWNER).attempt
        val row = claimed(queue.enqueueAndClaimAbsoluteHead(draft("m-1"), attempt))
        val effects = mutableListOf<XmppEvent>()
        val worker = DeliveryTerminalWorker(queue, dispatchEvent = { effects += it })
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

    private fun uuid(seed: String): String = UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

    private fun pendingReceipt(
        owner: String,
        seed: String,
        effectCount: Int = 1,
    ): TerminalReceipt {
        val attempt = DeliveryAttemptRef(
            ownerBareJid = owner,
            attemptId = DeliveryAttemptId(uuid("$seed-attempt")),
            nativeGeneration = social.waddle.android.client.prefs.NativeConnectionGeneration(1u),
        )
        val rows = (0 until effectCount).map { index ->
            QueuedOutboundDraft.create(
                ownerBareJid = owner,
                clientStanzaId = "$seed-row-$index",
                enqueuedAtMillis = (index + 1).toLong(),
                payload = QueuedOutboundPayload(
                    target = QueuedOutboundTarget.Chat("peer@waddle.test"),
                    content = QueuedOutboundContent("$seed-$index"),
                ),
            ).persisted(index.toLong() + 1, OutboundOwnership.Ready)
        }
        return TerminalReceipt(
            owner = DeliveryOwnerBareJid(owner),
            attempt = attempt,
            id = TerminalReceiptId(uuid("$seed-receipt")),
            originProcessEpoch = ProcessEpoch(uuid("$seed-origin")),
            preparedAtMillis = 1,
            state = TerminalReceiptState.Pending(
                TerminalReceiptClaimState.Unclaimed,
                rows.map { row ->
                    TerminalReceiptEffect.Acknowledged(DeliveryCallbackRef(row.identity, attempt), row)
                },
            ),
        )
    }

    private fun terminalRun(
        worker: DeliveryTerminalWorker,
        scope: CoroutineScope,
        onExit: suspend (WorkerExit) -> Unit = {},
    ): DeliveryTerminalWorker.Run = worker.start(
        scope,
        WorkerOwnership(
            SessionLifecycleRef.create(OWNER),
            WorkerKind.DELIVERY_TERMINAL,
            WorkerGeneration.random(),
        ),
        {},
        onExit,
    )

    private suspend fun assertRequested(run: DeliveryTerminalWorker.Run) {
        run.requestStop()
        val outcome = run.awaitExit(1_000)
        assertTrue(outcome is WorkerAwaitOutcome.Exited)
        assertEquals(WorkerExitReason.RequestedStop, (outcome as WorkerAwaitOutcome.Exited).exit.reason)
    }
}
