package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.SerializationException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptState
import java.io.IOException
import java.util.logging.Handler
import java.util.logging.LogRecord
import java.util.logging.Logger

@OptIn(ExperimentalCoroutinesApi::class)
class DeliveryTerminalReceiptWorkerFailureTest {
    @Test
    fun `active owner mismatch fences the receipt worker with typed poison evidence`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "active-owner")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = "other@waddle.test",
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val run = terminalRun(
            DeliveryTerminalWorker(DeliveryJournalStore(prefs), dispatchEvent = {}, evidence = WorkerExitExceptionEvidence()),
            this,
        )

        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        val failure = (exit.reason as WorkerExitReason.UnexpectedFailure).kind
        assertEquals(
            WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION(
                TerminalReceiptApplicationFailure.DiscoveryOwnerFenced(
                    DeliveryOwnerBareJid(TERMINAL_WORKER_OWNER),
                    DeliveryOwnerBareJid("other@waddle.test"),
                ),
            ),
            failure,
        )
        assertFalse(run.ownership.lifecycle.ownerBareJid == "other@waddle.test")
    }

    @Test
    fun `corrupt native owned receipt state fences with its exact typed reason`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "native-poison")
        val row = (receipt.state as TerminalReceiptState.Pending).effects.single().row.copy(
            ownership = OutboundOwnership.NativeOwned(receipt.attempt, NativeOutboundPhase.FRESH),
        )
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (
                        TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(
                            outboundRows = listOf(row),
                            terminalReceipt = receipt,
                        )
                    ),
                ),
                Unit,
            )
        }
        val run = terminalRun(
            DeliveryTerminalWorker(DeliveryJournalStore(prefs), dispatchEvent = {}, evidence = WorkerExitExceptionEvidence()),
            this,
        )

        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertEquals(
            WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION(
                TerminalReceiptApplicationFailure.DiscoveryCorrupt(
                    DeliveryOwnerBareJid(TERMINAL_WORKER_OWNER),
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
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "release-failure")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
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
                    journal = DeliveryJournalStore(prefs),
                    dispatchEvent = {
                        store.failAllUpdates = true
                        throw primary
                    },
                    processEpoch = ProcessEpoch(terminalWorkerUuid("release-failure-process")),
                    evidence = WorkerExitExceptionEvidence(),
                ),
                this,
            )

            advanceTimeBy(8_750)
            runCurrent()
            val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
            val workerFailure = (exit.reason as WorkerExitReason.UnexpectedFailure).kind
                as WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION
            assertEquals(
                TerminalReceiptCleanupFailureCategory.IO_FAILURE,
                (workerFailure.failure as TerminalReceiptApplicationFailure.CleanupUnresolved)
                    .evidence.reason.let { it as TerminalReceiptCleanupReason.Persistence }.category,
            )
            assertEquals(primary, handler.observed)
            assertEquals(1, primary.suppressed.size)
            val cleanup = primary.suppressed.single() as TerminalReceiptCleanupException
            assertEquals(
                TerminalReceiptCleanupFailureCategory.IO_FAILURE,
                (cleanup.evidence.reason as TerminalReceiptCleanupReason.Persistence).category,
            )
            assertTrue(cleanup.cause is java.io.IOException)
            val pending = prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt?.state
                as TerminalReceiptState.Pending
            assertTrue(pending.claim is TerminalReceiptClaimState.Claimed)
        } finally {
            logger.removeHandler(handler)
        }
    }

    @Test
    fun `release cleanup exhausts every persistence category with the primary preserved`() = runTest {
        val cases = listOf(
            IOException("cleanup io") to TerminalReceiptCleanupFailureCategory.IO_FAILURE,
            SerializationException("cleanup codec") to TerminalReceiptCleanupFailureCategory.CODEC_FAILURE,
            IllegalArgumentException("cleanup runtime") to TerminalReceiptCleanupFailureCategory.RUNTIME_FAILURE,
            CancellationException("cleanup cancellation") to TerminalReceiptCleanupFailureCategory.CANCELLATION,
            AssertionError("cleanup error") to TerminalReceiptCleanupFailureCategory.ERROR_FAILURE,
        )

        cases.forEachIndexed { index, (storageFailure, category) ->
            val store = FailingPreferencesDataStore()
            val prefs = SessionPrefs(store)
            val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "cleanup-$index")
            prefs.updateDeliveryJournal { journal ->
                DeliveryJournalMutation(
                    journal.copy(
                        activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                        owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                    ),
                    Unit,
                )
            }
            val primary = IllegalStateException("dispatch primary $index")
            var observed: Throwable? = null
            val logger = Logger.getLogger(DeliveryTerminalWorker::class.java.name)
            val handler = object : Handler() {
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
                        journal = DeliveryJournalStore(prefs),
                        dispatchEvent = {
                            store.failAllUpdatesWith = storageFailure
                            throw primary
                        },
                        processEpoch = ProcessEpoch(terminalWorkerUuid("cleanup-process-$index")),
                        evidence = WorkerExitExceptionEvidence(),
                    ),
                    this,
                )
                advanceTimeBy(8_750)
                runCurrent()

                val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
                val workerFailure = (exit.reason as WorkerExitReason.UnexpectedFailure).kind
                    as WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION
                val evidence = (workerFailure.failure as TerminalReceiptApplicationFailure.CleanupUnresolved).evidence
                assertEquals(6, evidence.attempts)
                assertEquals(category, (evidence.reason as TerminalReceiptCleanupReason.Persistence).category)
                assertTrue(observed === primary)
                assertEquals(1, primary.suppressed.size)
                val cleanup = primary.suppressed.single() as TerminalReceiptCleanupException
                assertEquals(evidence, cleanup.evidence)
                assertTrue(cleanup.cause === storageFailure)
                val pending = prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt?.state
                    as TerminalReceiptState.Pending
                assertTrue(pending.claim is TerminalReceiptClaimState.Claimed)
            } finally {
                logger.removeHandler(handler)
            }
        }
    }

    @Test
    fun `release cleanup retry success preserves the dispatch failure without redispatch`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "cleanup-retry")
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val primary = IllegalStateException("dispatch primary")
        val events = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(
                journal = DeliveryJournalStore(prefs),
                dispatchEvent = {
                    events += it
                    store.failNextUpdate = true
                    throw primary
                },
                processEpoch = ProcessEpoch(terminalWorkerUuid("cleanup-retry-process")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )

        advanceTimeBy(250)
        runCurrent()
        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        assertEquals(WorkerFailureKind.DEPENDENCY_FAILURE, (exit.reason as WorkerExitReason.UnexpectedFailure).kind)
        assertTrue(primary.suppressed.isEmpty())
        assertEquals(1, events.size)
        assertTrue(
            (
                prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt?.state as
                TerminalReceiptState.Pending
            ).claim is TerminalReceiptClaimState.Unclaimed,
        )
    }
}
