package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.SerializationException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptState
import java.io.IOException

@OptIn(ExperimentalCoroutinesApi::class)
class TerminalReceiptCleanupExecutorTest {
    @Test
    fun `cleanup retries each persistence failure category once then releases without redispatch`() = runTest {
        val failures = listOf<Throwable>(
            IOException("io"),
            SerializationException("codec"),
            IllegalArgumentException("runtime"),
            CancellationException("cancelled"),
            AssertionError("error"),
        )
        failures.forEachIndexed { index, storageFailure ->
            val store = FailingPreferencesDataStore()
            val prefs = SessionPrefs(store)
            seed(prefs, "retry-$index")
            val primary = IllegalStateException("primary-$index")
            val events = mutableListOf<XmppEvent>()
            var attemptsBeforeFirstCleanup: Int? = null
            val run = terminalRun(
                DeliveryTerminalWorker(
                    DeliveryJournalStore(prefs),
                    dispatchEvent = {
                        events += it
                        attemptsBeforeFirstCleanup = store.updateAttempts.get()
                        store.failAllUpdatesWith = storageFailure
                        throw primary
                    },
                    processEpoch = ProcessEpoch(terminalWorkerUuid("retry-process-$index")),
                    evidence = WorkerExitExceptionEvidence(),
                ),
                this,
            )

            runCurrent()
            val attemptsAfterFirstCleanup = store.updateAttempts.get()
            assertEquals(requireNotNull(attemptsBeforeFirstCleanup) + 1, attemptsAfterFirstCleanup)
            advanceTimeBy(249)
            runCurrent()
            assertEquals(attemptsAfterFirstCleanup, store.updateAttempts.get())
            store.failAllUpdatesWith = null
            advanceTimeBy(1)
            runCurrent()
            assertEquals(attemptsAfterFirstCleanup + 1, store.updateAttempts.get())

            val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
            assertEquals(WorkerFailureKind.DEPENDENCY_FAILURE, (exit.reason as WorkerExitReason.UnexpectedFailure).kind)
            assertEquals(1, events.size)
            assertTrue(primary.suppressed.isEmpty())
            assertEquals(TerminalReceiptClaimState.Unclaimed, pending(prefs).claim)
        }
    }

    @Test
    fun `exhausted cleanup retains its exact lease for recovery without redispatch`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        seed(prefs, "repair")
        val events = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(
                DeliveryJournalStore(prefs),
                dispatchEvent = {
                    events += it
                    store.failAllUpdatesWith = IOException("cleanup exhausted")
                    error("primary")
                },
                processEpoch = ProcessEpoch(terminalWorkerUuid("repair-process")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )

        advanceTimeBy(8_750)
        runCurrent()
        run.awaitExit(1_000)
        val retained = pending(prefs).claim as TerminalReceiptClaimState.Claimed
        store.failAllUpdatesWith = null

        assertEquals(TerminalReceiptRecoveryCleanupResult.Released, run.recoverUnresolvedReceiptCleanup())
        assertEquals(TerminalReceiptClaimState.Unclaimed, pending(prefs).claim)
        assertEquals(retained, pending(prefs).releasedClaim)
        assertEquals(1, events.size)
    }

    @Test
    fun `cleanup exhaustion uses the six deterministic virtual-time attempts`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        seed(prefs, "schedule")
        val run = terminalRun(
            DeliveryTerminalWorker(
                DeliveryJournalStore(prefs),
                dispatchEvent = {
                    store.failAllUpdatesWith = IOException("schedule failure")
                    error("primary")
                },
                processEpoch = ProcessEpoch(terminalWorkerUuid("schedule-process")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )

        runCurrent()
        var attempts = store.updateAttempts.get()
        listOf(250L, 500L, 1_000L, 2_000L, 5_000L).forEach { delay ->
            advanceTimeBy(delay - 1)
            runCurrent()
            assertEquals(attempts, store.updateAttempts.get())
            advanceTimeBy(1)
            runCurrent()
            attempts += 1
            assertEquals(attempts, store.updateAttempts.get())
        }

        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        val cleanup = (
            (exit.reason as WorkerExitReason.UnexpectedFailure).kind as
            WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION
        ).failure as TerminalReceiptApplicationFailure.CleanupUnresolved
        assertEquals(6, cleanup.evidence.attempts)
        advanceTimeBy(60_000)
        runCurrent()
        assertEquals(attempts, store.updateAttempts.get())
    }

    private suspend fun seed(prefs: SessionPrefs, seed: String) {
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, seed)
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (
                        TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)
                    ),
                ),
                Unit,
            )
        }
    }

    private suspend fun pending(prefs: SessionPrefs) = requireNotNull(
        prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt,
    ).state as TerminalReceiptState.Pending
}
