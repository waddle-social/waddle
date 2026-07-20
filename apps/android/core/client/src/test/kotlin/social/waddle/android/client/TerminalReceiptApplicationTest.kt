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
import social.waddle.android.client.prefs.FinalizerGeneration
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
    fun `two discovery snapshots admit one exact same-process claimant and preserve the busy journal`() {
        val initial = journal()
        assertTrue(initial.discoverTerminalReceipt(DeliveryOwnerBareJid(OWNER)) is TerminalReceiptDiscovery.Pending)
        assertTrue(initial.discoverTerminalReceipt(DeliveryOwnerBareJid(OWNER)) is TerminalReceiptDiscovery.Pending)

        val winner = initial.claimTerminalReceipt(request(initial, "winner", "epoch"))
            as TerminalReceiptClaimResult.Claimed
        val losingRequest = request(winner.journal, "loser", "epoch")
        val loser = winner.journal.claimTerminalReceipt(losingRequest)

        assertEquals(
            TerminalReceiptClaimResult.Busy(winner.journal, losingRequest, winner.lease),
            loser,
        )
        assertEquals(winner.journal, loser.journal)
    }

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
    fun `only the exact released lease is idempotent after a later epoch reclaims`() {
        val initial = journal()
        val first = initial.claimTerminalReceipt(request(initial, "released-first", "released-epoch-a"))
            as TerminalReceiptClaimResult.Claimed
        val released = first.journal.releaseTerminalReceipt(first.lease)
            as TerminalReceiptReleaseResult.Released
        assertTrue(released.journal.releaseTerminalReceipt(first.lease) is TerminalReceiptReleaseResult.AlreadyReleased)
        val second = released.journal.claimTerminalReceipt(request(released.journal, "released-second", "released-epoch-b"))
            as TerminalReceiptClaimResult.Claimed
        val releasedSecond = second.journal.releaseTerminalReceipt(second.lease)
            as TerminalReceiptReleaseResult.Released
        assertTrue(releasedSecond.journal.releaseTerminalReceipt(first.lease) is TerminalReceiptReleaseResult.LeaseMismatch)
        assertTrue(releasedSecond.journal.releaseTerminalReceipt(second.lease) is TerminalReceiptReleaseResult.AlreadyReleased)
    }

    @Test
    fun `released receipt fences every differing durable worker lease field without mutation`() {
        val claimant = TerminalReceiptClaimant.Worker(
            LifecycleGeneration(uuid("released-lifecycle")),
            TerminalReceiptWorkerKind.DELIVERY_TERMINAL,
            WorkerGeneration(uuid("released-worker")),
        )
        val initial = journal()
        val claimed = initial.claimTerminalReceipt(request(initial, "released-claim", "released-epoch", claimant))
            as TerminalReceiptClaimResult.Claimed
        val released = claimed.journal.releaseTerminalReceipt(claimed.lease)
            as TerminalReceiptReleaseResult.Released
        val exactWorker = claimed.lease.claim.claimant as TerminalReceiptClaimant.Worker
        val wrongLeases = listOf(
            claimed.lease.copy(claim = claimed.lease.claim.copy(id = TerminalClaimId(uuid("released-other-claim")))),
            claimed.lease.copy(claim = claimed.lease.claim.copy(claimant = TerminalReceiptClaimant.BootstrapProcess)),
            claimed.lease.copy(claim = claimed.lease.claim.copy(
                claimant = exactWorker.copy(lifecycleGeneration = LifecycleGeneration(uuid("released-other-lifecycle"))),
            )),
            claimed.lease.copy(claim = claimed.lease.claim.copy(
                claimant = exactWorker.copy(workerGeneration = WorkerGeneration(uuid("released-other-worker"))),
            )),
            claimed.lease.copy(claim = claimed.lease.claim.copy(processEpoch = ProcessEpoch(uuid("released-other-epoch")))),
        )

        wrongLeases.forEach { wrong ->
            val result = released.journal.releaseTerminalReceipt(wrong)
            assertTrue(result is TerminalReceiptReleaseResult.LeaseMismatch)
            assertEquals(released.journal, result.journal)
        }
        assertTrue(released.journal.releaseTerminalReceipt(claimed.lease) is TerminalReceiptReleaseResult.AlreadyReleased)
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

    @Test
    fun `worker and finalizer generation fields are exact lease identity`() {
        val worker = TerminalReceiptClaimant.Worker(
            LifecycleGeneration(uuid("worker-lifecycle")),
            TerminalReceiptWorkerKind.DELIVERY_TERMINAL,
            WorkerGeneration(uuid("worker-generation")),
        )
        val workerJournal = journal()
        val workerClaim = workerJournal.claimTerminalReceipt(request(workerJournal, "worker", "epoch", worker))
            as TerminalReceiptClaimResult.Claimed
        val workerIdentity = workerClaim.lease.claim.claimant as TerminalReceiptClaimant.Worker
        val workerWrongGenerations = listOf(
            workerClaim.lease.copy(claim = workerClaim.lease.claim.copy(
                claimant = workerIdentity.copy(lifecycleGeneration = LifecycleGeneration(uuid("other-lifecycle"))),
            )),
            workerClaim.lease.copy(claim = workerClaim.lease.claim.copy(
                claimant = workerIdentity.copy(workerGeneration = WorkerGeneration(uuid("other-worker"))),
            )),
        )

        val finalizer = TerminalReceiptClaimant.Finalizer(
            LifecycleGeneration(uuid("finalizer-lifecycle")),
            FinalizerGeneration(uuid("finalizer-generation")),
        )
        val finalizerJournal = journal()
        val finalizerClaim = finalizerJournal.claimTerminalReceipt(
            request(finalizerJournal, "finalizer", "epoch", finalizer),
        )
            as TerminalReceiptClaimResult.Claimed
        val finalizerIdentity = finalizerClaim.lease.claim.claimant as TerminalReceiptClaimant.Finalizer
        val finalizerWrongGeneration = finalizerClaim.lease.copy(claim = finalizerClaim.lease.claim.copy(
            claimant = finalizerIdentity.copy(finalizerGeneration = FinalizerGeneration(uuid("other-finalizer"))),
        ))

        (workerWrongGenerations + finalizerWrongGeneration).forEach { wrong ->
            val source = if (wrong.ref == workerClaim.lease.ref) workerClaim else finalizerClaim
            assertEquals(source.journal, source.journal.releaseTerminalReceipt(wrong).journal)
            assertEquals(source.journal, source.journal.acknowledgeTerminalReceipt(wrong).journal)
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

    private fun request(
        journal: DeliveryJournal,
        claim: String,
        epoch: String,
        claimant: TerminalReceiptClaimant = TerminalReceiptClaimant.BootstrapProcess,
    ): TerminalReceiptClaimRequest {
        val receipt = journal.owners.getValue(OWNER).terminalReceipt!!
        return TerminalReceiptClaimRequest(
            TerminalReceiptRef(receipt.owner, receipt.attempt, receipt.id),
            TerminalReceiptClaimState.Claimed(
                TerminalClaimId(uuid(claim)),
                claimant,
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
