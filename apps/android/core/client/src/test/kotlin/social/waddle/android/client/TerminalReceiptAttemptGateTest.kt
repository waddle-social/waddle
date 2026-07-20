package social.waddle.android.client

import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.DeliveryTerminalIntentId
import social.waddle.android.client.prefs.NativeConnectionGeneration
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.TerminalClaimId
import social.waddle.android.client.prefs.TerminalReceipt
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptClaimant
import social.waddle.android.client.prefs.TerminalReceiptId
import social.waddle.android.client.prefs.TerminalReceiptState

class TerminalReceiptAttemptGateTest {
    @Test
    fun `pending receipt blocks replacement without changing the journal`() {
        val previous = attempt("previous")
        val journal = journal(previous, receipt(previous, TerminalReceiptState.Pending(
            TerminalReceiptClaimState.Claimed(
                TerminalClaimId(id("claim")),
                TerminalReceiptClaimant.BootstrapProcess,
                ProcessEpoch(id("epoch")),
            ),
            listOf(effect(previous)),
        )))

        val result = journal.beginDeliveryAttempt(OWNER, attempt("replacement"), 1)

        assertTrue(result.result is DeliveryJournalStore.BeginAttemptResult.PendingReceipt)
        assertEquals(journal, result.journal)
    }

    @Test
    fun `post fence acknowledged and preacknowledged tombstones are collected before replacement`() {
        listOf(
            TerminalReceiptState.Acknowledged(
                TerminalReceiptClaimState.Claimed(
                    TerminalClaimId(id("ack-claim")),
                    TerminalReceiptClaimant.BootstrapProcess,
                    ProcessEpoch(id("ack-epoch")),
                ),
            ),
            TerminalReceiptState.PreAcknowledged,
        ).forEachIndexed { index, state ->
            val previous = attempt("previous-$index")
            val result = journal(previous, receipt(previous, state)).beginDeliveryAttempt(
                OWNER,
                attempt("replacement-$index"),
                1,
            )

            assertTrue(result.result is DeliveryJournalStore.BeginAttemptResult.Started)
            assertEquals(null, result.journal.owners.getValue(OWNER).terminalReceipt)
        }
    }

    @Test
    fun `acknowledged and preacknowledged tombstones retain native and terminal projections`() {
        val states = listOf(
            TerminalReceiptState.Acknowledged(
                TerminalReceiptClaimState.Claimed(
                    TerminalClaimId(id("ack-claim")),
                    TerminalReceiptClaimant.BootstrapProcess,
                    ProcessEpoch(id("ack-epoch")),
                ),
            ),
            TerminalReceiptState.PreAcknowledged,
        )
        val projections = listOf(
            OutboundOwnership.NativeOwned(attempt("native-projection"), NativeOutboundPhase.FRESH),
            OutboundOwnership.Terminal(DeliveryTerminalIntentId(id("terminal-projection"))),
        )

        states.forEachIndexed { stateIndex, state ->
            projections.forEachIndexed { projectionIndex, projection ->
                val previous = attempt("previous-${stateIndex}-${projectionIndex}")
                val owner = DeliveryOwnerJournal(
                    activeAttempt = null,
                    terminalReceipt = receipt(previous, state),
                    outboundRows = listOf(row(previous, projection)),
                )
                val journal = DeliveryJournal(activeOwnerBareJid = OWNER, owners = mapOf(OWNER to owner))

                val result = journal.beginDeliveryAttempt(OWNER, attempt("replacement-${stateIndex}-${projectionIndex}"), 1)

                assertTrue(result.result is DeliveryJournalStore.BeginAttemptResult.TombstoneNotPostFence)
                assertEquals(journal, result.journal)
            }
        }
    }

    private fun journal(attempt: DeliveryAttemptRef, receipt: TerminalReceipt): DeliveryJournal = DeliveryJournal(
        activeOwnerBareJid = OWNER,
        owners = mapOf(OWNER to DeliveryOwnerJournal(activeAttempt = null, terminalReceipt = receipt)),
    )

    private fun receipt(attempt: DeliveryAttemptRef, state: TerminalReceiptState): TerminalReceipt = TerminalReceipt(
        owner = DeliveryOwnerBareJid(OWNER),
        attempt = attempt,
        id = TerminalReceiptId(id("receipt-${attempt.attemptId.value}")),
        originProcessEpoch = ProcessEpoch(id("origin-${attempt.attemptId.value}")),
        preparedAtMillis = 1,
        state = state,
    )

    private fun effect(attempt: DeliveryAttemptRef): social.waddle.android.client.prefs.TerminalReceiptEffect {
        val row = social.waddle.android.client.prefs.QueuedOutboundDraft.create(
            ownerBareJid = OWNER,
            clientStanzaId = "row",
            enqueuedAtMillis = 1,
            payload = social.waddle.android.client.prefs.QueuedOutboundPayload(
                social.waddle.android.client.prefs.QueuedOutboundTarget.Chat("peer@waddle.test"),
                social.waddle.android.client.prefs.QueuedOutboundContent("body"),
            ),
        ).persisted(1, social.waddle.android.client.prefs.OutboundOwnership.Ready)
        return social.waddle.android.client.prefs.TerminalReceiptEffect.Acknowledged(
            social.waddle.android.client.prefs.DeliveryCallbackRef(row.identity, attempt), row,
        )
    }

    private fun row(
        attempt: DeliveryAttemptRef,
        ownership: OutboundOwnership,
    ) = social.waddle.android.client.prefs.QueuedOutboundDraft.create(
        ownerBareJid = OWNER,
        clientStanzaId = "residual-${attempt.attemptId.value}",
        enqueuedAtMillis = 1,
        payload = social.waddle.android.client.prefs.QueuedOutboundPayload(
            social.waddle.android.client.prefs.QueuedOutboundTarget.Chat("peer@waddle.test"),
            social.waddle.android.client.prefs.QueuedOutboundContent("body"),
        ),
    ).persisted(1, ownership)

    private fun attempt(seed: String): DeliveryAttemptRef = DeliveryAttemptRef(
        OWNER,
        DeliveryAttemptId(id(seed)),
        NativeConnectionGeneration(1u),
    )

    private fun id(seed: String): String = UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

    private companion object { const val OWNER = "alice@waddle.test" }
}
