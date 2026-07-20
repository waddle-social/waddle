package social.waddle.android.client

import androidx.datastore.preferences.core.mutablePreferencesOf
import androidx.datastore.preferences.core.stringPreferencesKey
import java.io.IOException
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.TerminalClaimId
import social.waddle.android.client.prefs.TerminalReceipt
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptClaimant
import social.waddle.android.client.prefs.TerminalReceiptEffect
import social.waddle.android.client.prefs.TerminalReceiptState

@OptIn(ExperimentalCoroutinesApi::class)
class TerminalReceiptReplayPersistenceTest {
    @Test
    fun `claim precommit cancellation preserves bytes and a later worker applies the receipt`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "claim-precommit")
        seed(prefs, receipt)
        val before = snapshot(store, prefs)
        val cancellation = CancellationException("claim cancelled before commit")
        store.installBeforeCommitReturnsOnce { throw cancellation }

        val failure = try {
            prefs.claimTerminalReceipt(request(receipt, "cancelled-claim", "cancelled-epoch"))
            null
        } catch (actual: Throwable) {
            actual
        }

        assertSame(cancellation, failure)
        assertEquals(before, snapshot(store, prefs))

        val events = mutableListOf<XmppEvent>()
        val run = terminalRun(
            DeliveryTerminalWorker(
                OutboundQueue(prefs),
                events::add,
                ProcessEpoch(uuid("claim-later-epoch")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )
        run.awaitStartupDrain()

        assertEquals(1, events.size)
        assertTrue(receiptState(prefs) is TerminalReceiptState.Acknowledged)
        assertRequested(run)
    }

    @Test
    fun `persisted two effect prefixes replay in canonical order after a distinct process reclaims`() = runTest {
        listOf(0, 1, 2).forEach { prefix ->
            val store = FailingPreferencesDataStore()
            val prefs = SessionPrefs(store)
            val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "replay-prefix-$prefix", effectCount = 2)
            seed(prefs, receipt)
            val canonical = (receipt.state as TerminalReceiptState.Pending).effects.map(::eventFor)
            val firstEvents = mutableListOf<XmppEvent>()
            val crash = IllegalStateException("process crashed after prefix $prefix")
            val first = terminalRun(
                DeliveryTerminalWorker(
                    OutboundQueue(prefs),
                    dispatchEvent = { event ->
                        if (firstEvents.size == prefix) throw crash
                        firstEvents += event
                        if (firstEvents.size == prefix) throw crash
                    },
                    processEpoch = ProcessEpoch(uuid("replay-first-$prefix")),
                    evidence = WorkerExitExceptionEvidence(),
                ),
                this,
            )

            val firstExit = (first.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
            assertEquals(WorkerFailureKind.DEPENDENCY_FAILURE, (firstExit.reason as WorkerExitReason.UnexpectedFailure).kind)
            assertEquals(canonical.take(prefix), firstEvents)
            val released = receiptState(prefs) as TerminalReceiptState.Pending
            assertEquals(TerminalReceiptClaimState.Unclaimed, released.claim)
            assertTrue("prefix $prefix must durably retain its released lease", released.releasedClaim != null)

            val replayed = mutableListOf<XmppEvent>()
            val replacementEpoch = ProcessEpoch(uuid("replay-replacement-$prefix"))
            val replacement = terminalRun(
                DeliveryTerminalWorker(
                    OutboundQueue(prefs),
                    replayed::add,
                    replacementEpoch,
                    evidence = WorkerExitExceptionEvidence(),
                ),
                this,
            )
            replacement.awaitStartupDrain()

            assertEquals(canonical.take(prefix) + canonical, firstEvents + replayed)
            val acknowledged = receiptState(prefs) as TerminalReceiptState.Acknowledged
            assertEquals(replacementEpoch, acknowledged.claim.processEpoch)
            assertRequested(replacement)
        }
    }

    @Test
    fun `replacement claim fences the stale lease without rewriting persisted bytes`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "replacement")
        seed(prefs, receipt)
        val stale = prefs.claimTerminalReceipt(request(receipt, "stale-claim", "stale-epoch")) as TerminalReceiptClaimResult.Claimed
        val replacement = prefs.claimTerminalReceipt(request(receipt, "replacement-claim", "replacement-epoch"))
            as TerminalReceiptClaimResult.Claimed
        val beforeStaleMutation = snapshot(store, prefs)

        val staleAcknowledge = prefs.acknowledgeTerminalReceipt(stale.lease)
        val staleRelease = prefs.releaseTerminalReceipt(stale.lease)

        assertTrue(staleAcknowledge is TerminalReceiptAcknowledgeResult.LeaseMismatch)
        assertTrue(staleRelease is TerminalReceiptReleaseResult.LeaseMismatch)
        assertEquals(beforeStaleMutation, snapshot(store, prefs))

        assertTrue(prefs.acknowledgeTerminalReceipt(replacement.lease) is TerminalReceiptAcknowledgeResult.Acknowledged)
        val acknowledged = receiptState(prefs) as TerminalReceiptState.Acknowledged
        assertEquals(replacement.lease.claim, acknowledged.claim)
    }

    @Test
    fun `acknowledgement postcommit uncertainty retries only acknowledgement and restart dispatches nothing`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "ack-postcommit")
        seed(prefs, receipt)
        val firstEvents = mutableListOf<XmppEvent>()
        val first = terminalRun(
            DeliveryTerminalWorker(
                OutboundQueue(prefs),
                dispatchEvent = { event ->
                    firstEvents += event
                    store.afterCommitReturns = {
                        store.afterCommitReturns = null
                        throw IOException("acknowledgement committed before return")
                    }
                },
                processEpoch = ProcessEpoch(uuid("ack-postcommit-first")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )

        runCurrent()
        advanceTimeBy(250)
        runCurrent()
        first.awaitStartupDrain()

        assertEquals(1, firstEvents.size)
        assertTrue(receiptState(prefs) is TerminalReceiptState.Acknowledged)
        assertRequested(first)

        val restartedEvents = mutableListOf<XmppEvent>()
        val restarted = terminalRun(
            DeliveryTerminalWorker(
                OutboundQueue(prefs),
                restartedEvents::add,
                ProcessEpoch(uuid("ack-postcommit-restart")),
                evidence = WorkerExitExceptionEvidence(),
            ),
            this,
        )
        restarted.awaitStartupDrain()

        assertTrue(restartedEvents.isEmpty())
        assertRequested(restarted)
    }

    @Test
    fun `malformed persisted effect lists fence before callbacks and retain their raw bytes`() = runTest {
        listOf("duplicate", "reversed", "empty", "binding-invalid").forEach { corruption ->
            val store = FailingPreferencesDataStore()
            val prefs = SessionPrefs(store)
            val receipt = pendingTerminalReceipt(TERMINAL_WORKER_OWNER, "corrupt-$corruption", effectCount = 2)
            seed(prefs, receipt)
            val raw = requireNotNull(store.data.first()[DELIVERY_JOURNAL_KEY])
            val malformed = malformedEffects(raw, receipt, corruption)
            store.updateData { mutablePreferencesOf(DELIVERY_JOURNAL_KEY to malformed) }
            val events = mutableListOf<XmppEvent>()
            val run = terminalRun(
                DeliveryTerminalWorker(
                    OutboundQueue(prefs),
                    events::add,
                    evidence = WorkerExitExceptionEvidence(),
                ),
                this,
            )

            val exit = (run.awaitExit(1_000) as WorkerAwaitOutcome.Exited).exit
            assertEquals(
                TerminalReceiptApplicationFailure.DiscoveryCorrupt(
                    DeliveryOwnerBareJid(TERMINAL_WORKER_OWNER),
                    TerminalReceiptCorruption.PERSISTED_DECODE_FAILURE,
                ),
                ((exit.reason as WorkerExitReason.UnexpectedFailure).kind as WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION).failure,
            )
            assertTrue("$corruption must not dispatch callbacks", events.isEmpty())
            assertEquals("$corruption must retain raw persistence", malformed, store.data.first()[DELIVERY_JOURNAL_KEY])
        }
    }

    private suspend fun seed(prefs: SessionPrefs, receipt: TerminalReceipt) {
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = TERMINAL_WORKER_OWNER,
                    owners = journal.owners + (TERMINAL_WORKER_OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
    }

    private fun request(receipt: TerminalReceipt, claim: String, epoch: String) = TerminalReceiptClaimRequest(
        TerminalReceiptRef(receipt.owner, receipt.attempt, receipt.id),
        TerminalReceiptClaimState.Claimed(
            TerminalClaimId(uuid(claim)),
            TerminalReceiptClaimant.BootstrapProcess,
            ProcessEpoch(uuid(epoch)),
        ),
    )

    private suspend fun snapshot(store: FailingPreferencesDataStore, prefs: SessionPrefs) = ReceiptSnapshot(
        prefs.deliveryJournal.first(),
        requireNotNull(store.data.first()[DELIVERY_JOURNAL_KEY]),
    )

    private suspend fun receiptState(prefs: SessionPrefs) = requireNotNull(
        prefs.deliveryJournal.first().owners.getValue(TERMINAL_WORKER_OWNER).terminalReceipt,
    ).state

    private fun eventFor(effect: TerminalReceiptEffect): XmppEvent {
        val acknowledged = effect as TerminalReceiptEffect.Acknowledged
        return XmppEvent.DeliveryAcked(DeliveryOutcomeRef(acknowledged.row.identity, acknowledged.row.source))
    }

    private fun malformedEffects(raw: String, receipt: TerminalReceipt, corruption: String): String {
        val effects = (receipt.state as TerminalReceiptState.Pending).effects
        val first = (effects[0] as TerminalReceiptEffect.Acknowledged).row.identity
        val second = (effects[1] as TerminalReceiptEffect.Acknowledged).row.identity
        val malformed = when (corruption) {
            "duplicate" -> raw
                .replace(second.clientStanzaId, first.clientStanzaId)
                .replace(second.incarnation.value, first.incarnation.value)
                .replace(second.payloadDigest.value, first.payloadDigest.value)
            "reversed" -> raw.replaceFirst("\"sequence\":1", "\"sequence\":3")
            "empty" -> raw.replaceFirst(Regex("\"effects\":\\[.*?\\](?=})"), "\"effects\":[]")
            "binding-invalid" -> raw.replaceFirst(first.clientStanzaId, "wrong-${first.clientStanzaId}")
            else -> error("unknown corruption $corruption")
        }
        check(malformed != raw) { "$corruption must change persisted effects" }
        return malformed
    }

    private data class ReceiptSnapshot(
        val journal: social.waddle.android.client.prefs.DeliveryJournal,
        val raw: String,
    )

    private companion object {
        val DELIVERY_JOURNAL_KEY = stringPreferencesKey("delivery_journal_v1")

        fun uuid(seed: String): String = UUID.nameUUIDFromBytes(seed.toByteArray()).toString()
    }
}
