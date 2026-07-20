package social.waddle.android.client

import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryCallbackRef
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.LifecycleGeneration
import social.waddle.android.client.prefs.NativeConnectionGeneration
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.TerminalClaimId
import social.waddle.android.client.prefs.TerminalReceipt
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptClaimant
import social.waddle.android.client.prefs.TerminalReceiptEffect
import social.waddle.android.client.prefs.TerminalReceiptId
import social.waddle.android.client.prefs.TerminalReceiptState
import social.waddle.android.client.prefs.TerminalReceiptWorkerKind
import social.waddle.android.client.prefs.WorkerGeneration

class TerminalReceiptApplicationTest {
    @Test
    fun `claim is idempotent and same epoch competitors are busy`() {
        val journal = journal()
        val request = request(journal, "claim-a", "epoch-a")
        val claimed = journal.claimTerminalReceipt(request) as TerminalReceiptClaimResult.Claimed

        assertEquals(claimed, claimed.journal.claimTerminalReceipt(request))
        val competing = request(claimed.journal, "claim-b", "epoch-a")
        assertEquals(
            TerminalReceiptClaimResult.Busy(claimed.journal, competing, claimed.lease),
            claimed.journal.claimTerminalReceipt(competing),
        )
    }

    @Test
    fun `new epoch reclaims and displaced lease cannot mutate committed acknowledgement`() {
        val journal = journal()
        val first = journal.claimTerminalReceipt(request(journal, "first", "epoch-a"))
            as TerminalReceiptClaimResult.Claimed
        val second = first.journal.claimTerminalReceipt(request(first.journal, "second", "epoch-b"))
            as TerminalReceiptClaimResult.Claimed

        assertTrue(second.journal.acknowledgeTerminalReceipt(first.lease) is TerminalReceiptAcknowledgeResult.LeaseMismatch)
        val acknowledged = second.journal.acknowledgeTerminalReceipt(second.lease)
            as TerminalReceiptAcknowledgeResult.Acknowledged
        assertEquals(second.lease.claim, (acknowledged.receipt.state as TerminalReceiptState.Acknowledged).claim)
        assertTrue(acknowledged.journal.releaseTerminalReceipt(first.lease) is TerminalReceiptReleaseResult.LeaseMismatch)
        assertTrue(acknowledged.journal.releaseTerminalReceipt(second.lease) is TerminalReceiptReleaseResult.AlreadyAcknowledged)
    }

    @Test
    fun `claim owner fence and post claim owner change follow their exact boundaries`() {
        val initial = journal()
        val fenced = initial.copy(activeOwnerBareJid = "other@waddle.test")
        val request = request(initial, "claim", "epoch")
        assertEquals(
            TerminalReceiptClaimResult.OwnerFenced(
                fenced,
                request.ref.owner,
                DeliveryOwnerBareJid("other@waddle.test"),
            ),
            fenced.claimTerminalReceipt(request),
        )

        val claimed = initial.claimTerminalReceipt(request) as TerminalReceiptClaimResult.Claimed
        val changedOwner = claimed.journal.copy(activeOwnerBareJid = "other@waddle.test")
        assertTrue(changedOwner.acknowledgeTerminalReceipt(claimed.lease) is TerminalReceiptAcknowledgeResult.Acknowledged)
    }

    @Test
    fun `every wrong exact lease field is a no-op for pending and acknowledged receipts`() {
        val claimed = journal().claimTerminalReceipt(request(journal(), "claim", "epoch"))
            as TerminalReceiptClaimResult.Claimed
        val acknowledged = claimed.journal.acknowledgeTerminalReceipt(claimed.lease)
            as TerminalReceiptAcknowledgeResult.Acknowledged

        wrongLeases(claimed.lease).forEach { wrong ->
            assertEquals(claimed.journal, claimed.journal.acknowledgeTerminalReceipt(wrong).journal)
            assertEquals(claimed.journal, claimed.journal.releaseTerminalReceipt(wrong).journal)
            assertEquals(acknowledged.journal, acknowledged.journal.acknowledgeTerminalReceipt(wrong).journal)
            assertEquals(acknowledged.journal, acknowledged.journal.releaseTerminalReceipt(wrong).journal)
        }
    }

    private fun journal(): DeliveryJournal {
        val attempt = DeliveryAttemptRef(OWNER, DeliveryAttemptId(uuid("attempt")), NativeConnectionGeneration(1u))
        val row = QueuedOutboundDraft.create(
            OWNER,
            "row",
            1,
            QueuedOutboundPayload(QueuedOutboundTarget.Chat("peer@waddle.test"), QueuedOutboundContent("body")),
        ).persisted(1, OutboundOwnership.Ready)
        val receipt = TerminalReceipt(
            DeliveryOwnerBareJid(OWNER),
            attempt,
            TerminalReceiptId(uuid("receipt")),
            ProcessEpoch(uuid("origin")),
            1,
            TerminalReceiptState.Pending(
                TerminalReceiptClaimState.Unclaimed,
                listOf(TerminalReceiptEffect.Acknowledged(DeliveryCallbackRef(row.identity, attempt), row)),
            ),
        )
        return DeliveryJournal(
            activeOwnerBareJid = OWNER,
            owners = mapOf(OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
        )
    }

    private fun request(journal: DeliveryJournal, claim: String, epoch: String): TerminalReceiptClaimRequest {
        val receipt = journal.owners.getValue(OWNER).terminalReceipt!!
        return TerminalReceiptClaimRequest(
            TerminalReceiptRef(receipt.owner, receipt.attempt, receipt.id),
            TerminalReceiptClaimState.Claimed(
                TerminalClaimId(uuid(claim)),
                TerminalReceiptClaimant.BootstrapProcess,
                ProcessEpoch(uuid(epoch)),
            ),
        )
    }

    private fun wrongLeases(lease: TerminalReceiptLease): List<TerminalReceiptLease> = listOf(
        lease.copy(claim = lease.claim.copy(id = TerminalClaimId(uuid("wrong-id")))),
        lease.copy(claim = lease.claim.copy(claimant = TerminalReceiptClaimant.Worker(
            LifecycleGeneration(uuid("wrong-lifecycle")),
            TerminalReceiptWorkerKind.DELIVERY_TERMINAL,
            WorkerGeneration(uuid("wrong-worker")),
        ))),
        lease.copy(claim = lease.claim.copy(processEpoch = ProcessEpoch(uuid("wrong-epoch")))),
        lease.copy(ref = lease.ref.copy(
            owner = DeliveryOwnerBareJid("other@waddle.test"),
            attempt = lease.ref.attempt.copy(ownerBareJid = "other@waddle.test"),
        )),
        lease.copy(ref = lease.ref.copy(attempt = lease.ref.attempt.copy(attemptId = DeliveryAttemptId(uuid("wrong-attempt"))))),
        lease.copy(ref = lease.ref.copy(id = TerminalReceiptId(uuid("wrong-receipt")))),
    )

    private fun uuid(seed: String): String = UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

    private companion object {
        const val OWNER = "alice@waddle.test"
    }
}
