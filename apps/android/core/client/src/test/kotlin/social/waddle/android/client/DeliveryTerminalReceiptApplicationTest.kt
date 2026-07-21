package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
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
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryCallbackRef
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.TerminalClaimId
import social.waddle.android.client.prefs.TerminalReceipt
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptEffect
import social.waddle.android.client.prefs.TerminalReceiptId
import social.waddle.android.client.prefs.TerminalReceiptState

@OptIn(ExperimentalCoroutinesApi::class)
class DeliveryTerminalReceiptApplicationTest {
    @Test
    fun `startup applies and acknowledges a persisted terminal receipt before legacy intent drain`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val row = QueuedOutboundDraft.create(
            ownerBareJid = TERMINAL_WORKER_OWNER,
            clientStanzaId = "receipt-row",
            enqueuedAtMillis = 1,
            payload = QueuedOutboundPayload(
                target = QueuedOutboundTarget.Chat("peer@waddle.test"),
                content = QueuedOutboundContent("receipt"),
            ),
        ).persisted(1, OutboundOwnership.Ready)
        val attempt = DeliveryAttemptRef(
            ownerBareJid = TERMINAL_WORKER_OWNER,
            attemptId = DeliveryAttemptId(terminalWorkerUuid("receipt-attempt")),
            nativeGeneration = social.waddle.android.client.prefs.NativeConnectionGeneration(1u),
        )
        val receipt = TerminalReceipt(
            owner = DeliveryOwnerBareJid(TERMINAL_WORKER_OWNER),
            attempt = attempt,
            id = TerminalReceiptId(terminalWorkerUuid("receipt-id")),
            originProcessEpoch = ProcessEpoch(terminalWorkerUuid("receipt-origin")),
            preparedAtMillis = 1,
            state = TerminalReceiptState.Pending(
                TerminalReceiptClaimState.Unclaimed,
                listOf(TerminalReceiptEffect.Acknowledged(DeliveryCallbackRef(row.identity, attempt), row)),
            ),
        )
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val effects = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(
                journal = DeliveryJournalStore(prefs),
                dispatchEvent = { effects += it },
                processEpoch = ProcessEpoch(terminalWorkerUuid("worker-process")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )

        run.awaitStartupDrain()

        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertTrue(prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt?.state is TerminalReceiptState.Acknowledged)
        assertRequested(run)
    }

    @Test
    fun `receipt acknowledgement io retry does not redispatch persisted effects`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "retry")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val effects = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(
                journal = DeliveryJournalStore(prefs),
                dispatchEvent = {
                    effects += it
                    store.failNextUpdate = true
                },
                processEpoch = ProcessEpoch(terminalWorkerUuid("retry-process")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )

        runCurrent()
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        advanceTimeBy(250)
        runCurrent()
        run.awaitStartupDrain()
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertTrue(prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt?.state is TerminalReceiptState.Acknowledged)
        assertRequested(run)
    }

    @Test
    fun `same process receipt claim is busy and does not dispatch`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val epoch = ProcessEpoch(terminalWorkerUuid("busy-epoch"))
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "busy").copy(
            state = (pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "busy").state as TerminalReceiptState.Pending).copy(
                claim = TerminalReceiptClaimState.Claimed(
                    social.waddle.android.client.prefs.TerminalClaimId(terminalWorkerUuid("busy-claim")),
                    social.waddle.android.client.prefs.TerminalReceiptClaimant.BootstrapProcess,
                    epoch,
                ),
            ),
        )
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val effects = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(DeliveryJournalStore(prefs), { effects += it }, epoch, evidence = WorkerExitExceptionEvidence()),
            this,
        )
        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertTrue(effects.isEmpty())
        assertEquals(receipt, prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt)
        assertTrue(
            (exit.reason as WorkerExitReason.UnexpectedFailure).kind is
                WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION,
        )
    }

    @Test
    fun `persisted same epoch claims serialize one winner and a busy loser before a new epoch reclaims`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "persisted-race")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val ref = TerminalReceiptRef(receipt.owner, receipt.attempt, receipt.id)
        val epoch = ProcessEpoch(terminalWorkerUuid("persisted-race-epoch"))
        fun request(id: String, process: ProcessEpoch) = TerminalReceiptClaimRequest(
            ref,
            TerminalReceiptClaimState.Claimed(
                TerminalClaimId(terminalWorkerUuid(id)),
                social.waddle.android.client.prefs.TerminalReceiptClaimant.BootstrapProcess,
                process,
            ),
        )
        val entered = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        store.installBeforeCommitReturnsOnce {
            entered.complete(Unit)
            release.await()
        }
        val winner = async { prefs.claimTerminalReceipt(request("persisted-race-winner", epoch)) }
        entered.await()
        val loser = async { prefs.claimTerminalReceipt(request("persisted-race-loser", epoch)) }
        runCurrent()
        assertFalse("second claim must wait behind the first DataStore commit", loser.isCompleted)
        release.complete(Unit)
        val claimed = winner.await() as TerminalReceiptClaimResult.Claimed
        val busy = loser.await() as TerminalReceiptClaimResult.Busy
        assertEquals(claimed.lease, busy.current)
        assertEquals(claimed.lease.claim, ((prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt?.state as TerminalReceiptState.Pending).claim))
        val reclaimed = prefs.claimTerminalReceipt(request("persisted-race-reclaim", ProcessEpoch(terminalWorkerUuid("persisted-race-new-epoch"))))
            as TerminalReceiptClaimResult.Claimed
        assertEquals(ProcessEpoch(terminalWorkerUuid("persisted-race-new-epoch")), reclaimed.lease.claim.processEpoch)
    }

    @Test
    fun `receipt discovery io exhausts after the bounded six attempts`() = runTest {
        val store = FailingPreferencesDataStore().also { it.failAllUpdates = true }
        val run = terminalRun(
            DeliveryTerminalWorker(
                DeliveryJournalStore(SessionPrefs(store)),
                dispatchEvent = {},
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )

        advanceTimeBy(8_750)
        runCurrent()

        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        val failure = (
            (exit.reason as WorkerExitReason.UnexpectedFailure).kind as
            WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION
        ).failure
        assertEquals(
            TerminalReceiptApplicationFailure.PersistenceExhausted(
                TerminalReceiptPersistenceOperation.DISCOVERY,
                DeliveryOwnerBareJid(TERMINAL_WORKER_OWNER),
                null,
                6,
            ),
            failure,
        )
        assertEquals(6, store.updateAttempts.get())
    }

    @Test
    fun `new process epoch reclaims receipt and typed active owner mismatch fences`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "reclaim").copy(
            state = (pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "reclaim").state as TerminalReceiptState.Pending).copy(
                claim = TerminalReceiptClaimState.Claimed(
                    social.waddle.android.client.prefs.TerminalClaimId(terminalWorkerUuid("old-claim")),
                    social.waddle.android.client.prefs.TerminalReceiptClaimant.BootstrapProcess,
                    ProcessEpoch(terminalWorkerUuid("old-epoch")),
                ),
            ),
        )
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val effects = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(
                DeliveryJournalStore(prefs),
                { effects += it },
                ProcessEpoch(terminalWorkerUuid("new-epoch")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )
        run.awaitStartupDrain()
        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertTrue(prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt?.state is TerminalReceiptState.Acknowledged)
        assertRequested(run)
    }

    @Test
    fun `claim commit uncertainty retries the same receipt before dispatch`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "claim-uncertain")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        store.installAfterCommitReturnsOnce { throw java.io.IOException("claim returned uncertain") }
        val effects = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(
                journal = DeliveryJournalStore(prefs),
                dispatchEvent = { effects += it },
                processEpoch = ProcessEpoch(terminalWorkerUuid("claim-uncertain-process")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )

        advanceTimeBy(250)
        runCurrent()
        run.awaitStartupDrain()

        assertEquals(1, effects.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertTrue(prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt?.state is TerminalReceiptState.Acknowledged)
        assertRequested(run)
    }

    @Test
    fun `post commit release uncertainty is idempotent before a new process replays the receipt`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "prefix", effectCount = 2)
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val firstEvents = mutableListOf<XmppEvent>()
        val first = terminalRun(
            DeliveryTerminalWorker(
                journal = DeliveryJournalStore(prefs),
                dispatchEvent = {
                    firstEvents += it
                    store.afterCommitReturns = {
                        store.afterCommitReturns = null
                        throw java.io.IOException("release returned uncertain")
                    }
                    throw IllegalStateException("dispatch prefix failed")
                },
                processEpoch = ProcessEpoch(terminalWorkerUuid("prefix-first-process")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )

        val firstExit = (first.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertEquals(
            WorkerFailureKind.DEPENDENCY_FAILURE,
            (firstExit.reason as WorkerExitReason.UnexpectedFailure).kind,
        )
        assertEquals(1, firstEvents.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        val released = prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt
        assertEquals(
            TerminalReceiptClaimState.Unclaimed,
            (released?.state as TerminalReceiptState.Pending).claim,
        )

        val replayed = mutableListOf<XmppEvent>()
        val second = terminalRun(
            DeliveryTerminalWorker(
                journal = DeliveryJournalStore(prefs),
                dispatchEvent = { replayed += it },
                processEpoch = ProcessEpoch(terminalWorkerUuid("prefix-second-process")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )
        second.awaitStartupDrain()

        assertEquals(2, replayed.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertTrue(prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt?.state is TerminalReceiptState.Acknowledged)
        assertRequested(second)
    }

    @Test
    fun `post commit release cancellation preserves the original cancellation and exact retry is idempotent`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "release-cancel", effectCount = 2)
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val cancellation = CancellationException("release committed then cancelled")
        val events = mutableListOf<XmppEvent>()
        val first = terminalRun(
            DeliveryTerminalWorker(
                journal = DeliveryJournalStore(prefs),
                dispatchEvent = {
                    events += it
                    store.afterCommitReturns = {
                        store.afterCommitReturns = null
                        throw cancellation
                    }
                    throw cancellation
                },
                processEpoch = ProcessEpoch(terminalWorkerUuid("release-cancel-process")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )

        advanceTimeBy(250)
        runCurrent()
        val exit = (first.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertEquals(WorkerExitReason.OwnerScopeCancelled, exit.reason)
        assertEquals(1, events.size)
        val pending = prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt?.state
            as TerminalReceiptState.Pending
        assertEquals(TerminalReceiptClaimState.Unclaimed, pending.claim)
        val durableReceipt = prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt!!
        val lease = TerminalReceiptLease(
            TerminalReceiptRef(durableReceipt.owner, durableReceipt.attempt, durableReceipt.id),
            requireNotNull(pending.releasedClaim),
        )
        assertTrue(prefs.releaseTerminalReceipt(lease) is TerminalReceiptReleaseResult.AlreadyReleased)
    }

    @Test
    fun `acknowledge commit cancellation does not replay after restart`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "ack-cancel")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val firstEvents = mutableListOf<XmppEvent>()
        val cancellation = kotlinx.coroutines.CancellationException("acknowledge returned cancelled")
        val first = terminalRun(
            DeliveryTerminalWorker(
                journal = DeliveryJournalStore(prefs),
                dispatchEvent = {
                    firstEvents += it
                    store.afterCommitReturns = {
                        store.afterCommitReturns = null
                        throw cancellation
                    }
                },
                processEpoch = ProcessEpoch(terminalWorkerUuid("ack-cancel-first-process")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )

        val firstExit = (first.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertEquals(WorkerExitReason.OwnerScopeCancelled, firstExit.reason)
        assertEquals(1, firstEvents.filterIsInstance<XmppEvent.DeliveryAcked>().size)
        assertTrue(prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt?.state is TerminalReceiptState.Acknowledged)

        val restartedEvents = mutableListOf<XmppEvent>()
        val second = terminalRun(
            DeliveryTerminalWorker(
                journal = DeliveryJournalStore(prefs),
                dispatchEvent = { restartedEvents += it },
                processEpoch = ProcessEpoch(terminalWorkerUuid("ack-cancel-second-process")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )
        second.awaitStartupDrain()
        assertTrue(restartedEvents.isEmpty())
        assertRequested(second)
    }
}
