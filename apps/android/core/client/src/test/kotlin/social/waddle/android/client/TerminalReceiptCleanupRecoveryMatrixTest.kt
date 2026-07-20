package social.waddle.android.client

import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.DeliveryTerminalIntentId
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.TerminalClaimId
import social.waddle.android.client.prefs.TerminalReceipt
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptState
import java.io.IOException

@OptIn(ExperimentalCoroutinesApi::class)
class TerminalReceiptCleanupRecoveryMatrixTest {
    @Test
    fun `recovery releases the exact exhausted lease and does not redispatch`() = runTest {
        val fixture = exhaustedCleanup("exact-release")

        assertEquals(TerminalReceiptRecoveryCleanupResult.Released, fixture.run.recoverUnresolvedReceiptCleanup())
        val released = fixture.receipt().state as TerminalReceiptState.Pending
        assertEquals(TerminalReceiptClaimState.Unclaimed, released.claim)
        assertEquals(fixture.lease.claim, released.releasedClaim)
        assertEquals(TerminalReceiptRecoveryCleanupResult.NoPendingLease, fixture.run.recoverUnresolvedReceiptCleanup())
        fixture.assertNoRedispatch()
    }

    @Test
    fun `recovery reports every nonterminal durable outcome without a durable rewrite`() = runTest {
        val leaseMismatch = exhaustedCleanup("lease-mismatch")
        val current = leaseMismatch.lease.claim.copy(
            id = TerminalClaimId(terminalWorkerUuid("lease-mismatch-current")),
            processEpoch = ProcessEpoch(terminalWorkerUuid("lease-mismatch-current-process")),
        )
        leaseMismatch.replaceReceipt { receipt ->
            receipt.copy(state = (receipt.state as TerminalReceiptState.Pending).copy(claim = current))
        }
        leaseMismatch.assertUnresolved(TerminalReceiptCleanupReason.LeaseMismatch(current))

        val missing = exhaustedCleanup("receipt-missing")
        missing.replaceReceipt { null }
        missing.assertUnresolved(TerminalReceiptCleanupReason.ReceiptMissing)

        val replaced = exhaustedCleanup("receipt-replaced")
        val actual = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "receipt-replaced-actual")
        replaced.replaceReceipt { actual }
        replaced.assertUnresolved(
            TerminalReceiptCleanupReason.ReceiptReplaced(TerminalReceiptRef(actual.owner, actual.attempt, actual.id)),
        )

        val corrupt = exhaustedCleanup("decoded-invalid")
        corrupt.addTerminalRow()
        corrupt.assertUnresolved(TerminalReceiptCleanupReason.Corrupt(TerminalReceiptCorruption.TERMINAL_ROW_REMAINS))
    }

    @Test
    fun `already released cleanup clears the unresolved lease without redispatch`() = runTest {
        val fixture = exhaustedCleanup("already-released")
        fixture.replaceReceipt { receipt ->
            receipt.copy(
                state = (receipt.state as TerminalReceiptState.Pending).copy(
                    claim = TerminalReceiptClaimState.Unclaimed,
                    releasedClaim = fixture.lease.claim,
                ),
            )
        }
        val before = fixture.snapshot()

        assertEquals(TerminalReceiptRecoveryCleanupResult.Released, fixture.run.recoverUnresolvedReceiptCleanup())
        assertEquals(before, fixture.snapshot())
        assertEquals(TerminalReceiptRecoveryCleanupResult.NoPendingLease, fixture.run.recoverUnresolvedReceiptCleanup())
        fixture.assertNoRedispatch()
    }

    @Test
    fun `already acknowledged cleanup clears the unresolved lease without acknowledgement or redispatch`() = runTest {
        val fixture = exhaustedCleanup("already-acknowledged")
        fixture.replaceReceipt { receipt ->
            receipt.copy(state = TerminalReceiptState.Acknowledged(fixture.lease.claim))
        }
        val before = fixture.snapshot()

        assertEquals(TerminalReceiptRecoveryCleanupResult.Released, fixture.run.recoverUnresolvedReceiptCleanup())
        assertEquals(before, fixture.snapshot())
        assertTrue(fixture.receipt().state is TerminalReceiptState.Acknowledged)
        assertEquals(TerminalReceiptRecoveryCleanupResult.NoPendingLease, fixture.run.recoverUnresolvedReceiptCleanup())
        fixture.assertNoRedispatch()
    }

    private suspend fun TestScope.exhaustedCleanup(seed: String): Fixture {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
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
        val events = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(
                OutboundQueue(prefs),
                dispatchEvent = {
                    events += it
                    store.failAllUpdatesWith = IOException("cleanup exhaustion")
                    error("primary-$seed")
                },
                processEpoch = ProcessEpoch(terminalWorkerUuid("$seed-process")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )

        runCurrent()
        advanceTimeBy(8_750)
        runCurrent()
        val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
        val workerFailure = exit.reason as WorkerExitReason.UnexpectedFailure
        val terminalFailure = workerFailure.kind as WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION
        val cleanup = terminalFailure.failure as TerminalReceiptApplicationFailure.CleanupUnresolved
        assertEquals(6, cleanup.evidence.attempts)
        store.failAllUpdatesWith = null
        return Fixture(store, prefs, run, cleanup.evidence.lease, events)
    }

    private suspend fun Fixture.assertUnresolved(reason: TerminalReceiptCleanupReason) {
        val before = snapshot()
        val first = run.recoverUnresolvedReceiptCleanup() as TerminalReceiptRecoveryCleanupResult.Unresolved
        assertEquals(lease, first.evidence.lease)
        assertEquals(1, first.evidence.attempts)
        assertEquals(reason, first.evidence.reason)
        assertEquals(before, snapshot())
        val second = run.recoverUnresolvedReceiptCleanup() as TerminalReceiptRecoveryCleanupResult.Unresolved
        assertEquals(first.evidence, second.evidence)
        assertEquals(before, snapshot())
        assertNoRedispatch()
    }

    private suspend fun Fixture.replaceReceipt(transform: (TerminalReceipt) -> TerminalReceipt?) {
        prefs.updateDeliveryJournal { journal ->
            val owner = journal.owners.getValue(TERMINAL_WORKER_OWNER)
            DeliveryJournalMutation(
                journal.copy(
                    owners = journal.owners + (
                        TERMINAL_WORKER_OWNER to owner.copy(
                            terminalReceipt = transform(requireNotNull(owner.terminalReceipt)),
                        )
                    ),
                ),
                Unit,
            )
        }
    }

    private suspend fun Fixture.addTerminalRow() {
        prefs.updateDeliveryJournal { journal ->
            val owner = journal.owners.getValue(TERMINAL_WORKER_OWNER)
            val receipt = requireNotNull(owner.terminalReceipt)
            val effect = (receipt.state as TerminalReceiptState.Pending).effects.single()
            val invalidRow = effect.row.copy(
                ownership = OutboundOwnership.Terminal(
                    DeliveryTerminalIntentId(terminalWorkerUuid("decoded-invalid-terminal-row")),
                ),
            )
            DeliveryJournalMutation(
                journal.copy(
                    owners = journal.owners + (
                        TERMINAL_WORKER_OWNER to owner.copy(outboundRows = owner.outboundRows + invalidRow)
                    ),
                ),
                Unit,
            )
        }
    }

    private suspend fun Fixture.receipt(): TerminalReceipt = requireNotNull(
        prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt,
    )

    private suspend fun Fixture.snapshot() = DurableSnapshot(
        prefs.deliveryJournal.first(),
        requireNotNull(store.data.first()[DELIVERY_JOURNAL_KEY]),
    )

    private fun Fixture.assertNoRedispatch() {
        assertEquals(1, events.size)
    }

    private data class Fixture(
        val store: FailingPreferencesDataStore,
        val prefs: SessionPrefs,
        val run: DeliveryTerminalWorker.Run,
        val lease: TerminalReceiptLease,
        val events: List<XmppEvent>,
    )

    private data class DurableSnapshot(
        val journal: DeliveryJournal,
        val raw: String,
    )

    private companion object {
        val DELIVERY_JOURNAL_KEY = stringPreferencesKey("delivery_journal_v1")
    }
}
