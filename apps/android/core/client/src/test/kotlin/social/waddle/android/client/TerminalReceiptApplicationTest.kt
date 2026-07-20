package social.waddle.android.client

import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryCallbackRef
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.NativeConnectionGeneration
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
import social.waddle.android.client.prefs.TerminalReceiptClaimant
import social.waddle.android.client.prefs.TerminalReceiptEffect
import social.waddle.android.client.prefs.TerminalReceiptId
import social.waddle.android.client.prefs.TerminalReceiptState

class TerminalReceiptApplicationTest {
    @Test
    fun `unclaimed receipt claims idempotently and reclaims only across process epochs`() {
        val journal = pendingJournal()
        val request = claimRequest(journal, "claim-a", "epoch-a")
        val claimed = journal.claimTerminalReceipt(request) as TerminalReceiptApplicationResult.Claimed

        assertEquals(claimed, claimed.journal.claimTerminalReceipt(request))
        val sameEpochOther = claimRequest(claimed.journal, "claim-b", "epoch-a")
        assertEquals(
            TerminalReceiptApplicationResult.Busy(claimed.journal, claimed.lease),
            claimed.journal.claimTerminalReceipt(sameEpochOther),
        )
        val nextEpoch = claimRequest(claimed.journal, "claim-c", "epoch-b")
        val reclaimed = claimed.journal.claimTerminalReceipt(nextEpoch) as TerminalReceiptApplicationResult.Claimed
        assertEquals(nextEpoch.claim, reclaimed.lease.claim)
        assertEquals(claimed.receipt.state.let { (it as TerminalReceiptState.Pending).effects }, reclaimed.effects)
    }

    @Test
    fun `exact acknowledge and release are idempotent and cannot reopen acknowledged receipt`() {
        val claimed = pendingJournal().claimTerminalReceipt(claimRequest(pendingJournal(), "claim", "epoch"))
            as TerminalReceiptApplicationResult.Claimed
        val released = claimed.journal.releaseTerminalReceipt(claimed.lease)
        assertTrue(released is TerminalReceiptApplicationResult.Released)
        val releasedJournal = released.journal
        assertEquals(
            TerminalReceiptApplicationResult.Stale(releasedJournal),
            releasedJournal.releaseTerminalReceipt(claimed.lease),
        )
        val next = releasedJournal.claimTerminalReceipt(claimRequest(releasedJournal, "claim-next", "epoch"))
            as TerminalReceiptApplicationResult.Claimed
        val acknowledged = next.journal.acknowledgeTerminalReceipt(next.lease)
            as TerminalReceiptApplicationResult.Acknowledged
        assertEquals(
            TerminalReceiptApplicationResult.AlreadyAcknowledged(acknowledged.journal, acknowledged.receipt),
            acknowledged.journal.acknowledgeTerminalReceipt(next.lease),
        )
        assertEquals(
            TerminalReceiptApplicationResult.AlreadyAcknowledged(acknowledged.journal, acknowledged.receipt),
            acknowledged.journal.releaseTerminalReceipt(next.lease),
        )
    }

    @Test
    fun `receipt validation fails closed for active attempt poison and foreign bucket preservation`() {
        val clean = pendingJournal()
        val ref = ref(clean)
        val poison = clean.copy(
            owners = clean.owners + (OWNER to clean.owners.getValue(OWNER).copy(activeAttempt = ref.attempt)),
        )
        assertEquals(
            TerminalReceiptApplicationResult.Corrupt(
                poison,
                TerminalReceiptCorruption.ACTIVE_ATTEMPT_REMAINS,
            ),
            poison.claimTerminalReceipt(claimRequest(poison, "claim", "epoch")),
        )
        val foreign = DeliveryOwnerJournal(activeAttempt = attempt(FOREIGN_OWNER, "foreign"))
        val withForeign = clean.copy(owners = clean.owners + (FOREIGN_OWNER to foreign))
        val claimed = withForeign.claimTerminalReceipt(claimRequest(withForeign, "claim", "epoch"))
            as TerminalReceiptApplicationResult.Claimed
        assertEquals(foreign, claimed.journal.owners[FOREIGN_OWNER])
    }

    @Test
    fun `session facade preserves raw bytes for identical claim retry`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val journal = pendingJournal()
        prefs.updateDeliveryJournal { DeliveryJournalMutation(journal, Unit) }
        val request = claimRequest(journal, "claim", "epoch")
        val first = prefs.claimTerminalReceipt(request) as TerminalReceiptApplicationResult.Claimed
        val rawAfterClaim = store.data.first()[DELIVERY_JOURNAL_KEY]

        assertEquals(first, prefs.claimTerminalReceipt(request))
        assertEquals(rawAfterClaim, store.data.first()[DELIVERY_JOURNAL_KEY])
    }

    @Test
    fun `wrong lease and acknowledged receipt no ops preserve durable raw bytes`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val journal = pendingJournal()
        prefs.updateDeliveryJournal { DeliveryJournalMutation(journal, Unit) }
        val claim = prefs.claimTerminalReceipt(claimRequest(journal, "claim", "epoch"))
            as TerminalReceiptApplicationResult.Claimed
        val rawClaimed = store.data.first()[DELIVERY_JOURNAL_KEY]
        val wrongLease = claim.lease.copy(
            claim = claimRequest(claim.journal, "wrong", "epoch").claim,
        )

        assertEquals(
            TerminalReceiptApplicationResult.Busy(claim.journal, claim.lease),
            prefs.releaseTerminalReceipt(wrongLease),
        )
        assertEquals(rawClaimed, store.data.first()[DELIVERY_JOURNAL_KEY])

        val acknowledged = prefs.acknowledgeTerminalReceipt(claim.lease)
            as TerminalReceiptApplicationResult.Acknowledged
        val rawAcknowledged = store.data.first()[DELIVERY_JOURNAL_KEY]
        assertEquals(
            TerminalReceiptApplicationResult.AlreadyAcknowledged(
                acknowledged.journal,
                acknowledged.receipt,
            ),
            prefs.releaseTerminalReceipt(claim.lease),
        )
        assertEquals(rawAcknowledged, store.data.first()[DELIVERY_JOURNAL_KEY])
    }

    @Test
    fun `wrong leases are typed no ops and acknowledged receipt cannot reopen`() {
        val journal = pendingJournal()
        val claimed = journal.claimTerminalReceipt(claimRequest(journal, "claim", "epoch"))
            as TerminalReceiptApplicationResult.Claimed
        val wrongClaim = claimRequest(claimed.journal, "wrong-claim", "epoch").claim
        val wrongLease = claimed.lease.copy(claim = wrongClaim)

        assertEquals(
            TerminalReceiptApplicationResult.Busy(claimed.journal, claimed.lease),
            claimed.journal.acknowledgeTerminalReceipt(wrongLease),
        )
        assertEquals(
            TerminalReceiptApplicationResult.Busy(claimed.journal, claimed.lease),
            claimed.journal.releaseTerminalReceipt(wrongLease),
        )
        val wrongId = claimed.lease.copy(ref = claimed.lease.ref.copy(id = TerminalReceiptId(uuid("wrong-id"))))
        assertEquals(
            TerminalReceiptApplicationResult.Stale(claimed.journal),
            claimed.journal.acknowledgeTerminalReceipt(wrongId),
        )
        val wrongAttempt = claimed.lease.copy(
            ref = claimed.lease.ref.copy(attempt = attempt(OWNER, "wrong-attempt")),
        )
        assertEquals(
            TerminalReceiptApplicationResult.Stale(claimed.journal),
            claimed.journal.releaseTerminalReceipt(wrongAttempt),
        )
        val foreignRef = TerminalReceiptRef(
            DeliveryOwnerBareJid(FOREIGN_OWNER),
            attempt(FOREIGN_OWNER, "foreign-attempt"),
            claimed.lease.ref.id,
        )
        assertEquals(
            TerminalReceiptApplicationResult.Stale(claimed.journal),
            claimed.journal.releaseTerminalReceipt(claimed.lease.copy(ref = foreignRef)),
        )
        val acknowledged = claimed.journal.acknowledgeTerminalReceipt(claimed.lease)
            as TerminalReceiptApplicationResult.Acknowledged
        assertEquals(
            TerminalReceiptApplicationResult.AlreadyAcknowledged(acknowledged.journal, acknowledged.receipt),
            acknowledged.journal.releaseTerminalReceipt(claimed.lease),
        )
    }

    @Test
    @OptIn(ExperimentalCoroutinesApi::class)
    fun `serialized competing claims have one winner and one same epoch busy lease`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val journal = pendingJournal()
        prefs.updateDeliveryJournal { DeliveryJournalMutation(journal, Unit) }
        val entered = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        store.installBeforeCommitReturnsOnce {
            entered.complete(Unit)
            release.await()
        }
        val firstRequest = claimRequest(journal, "first", "epoch")
        val secondRequest = claimRequest(journal, "second", "epoch")
        val first = async { prefs.claimTerminalReceipt(firstRequest) }
        entered.await()
        val second = async { prefs.claimTerminalReceipt(secondRequest) }
        try {
            runCurrent()
            assertTrue("second caller must remain serialized behind the first commit", !second.isCompleted)
        } finally {
            release.complete(Unit)
        }

        val winner = first.await() as TerminalReceiptApplicationResult.Claimed
        assertEquals(
            TerminalReceiptApplicationResult.Busy(winner.journal, winner.lease),
            second.await(),
        )
        assertEquals(winner.receipt, prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt)
    }

    private fun pendingJournal(): DeliveryJournal {
        val attempt = attempt(OWNER, "attempt")
        val row = QueuedOutboundDraft.create(
            ownerBareJid = OWNER,
            clientStanzaId = "row",
            enqueuedAtMillis = 1,
            payload = QueuedOutboundPayload(
                target = QueuedOutboundTarget.Chat("peer@waddle.test"),
                content = QueuedOutboundContent("body"),
            ),
        ).persisted(1, OutboundOwnership.Ready)
        val receipt = TerminalReceipt(
            owner = DeliveryOwnerBareJid(OWNER),
            attempt = attempt,
            id = TerminalReceiptId(uuid("receipt")),
            originProcessEpoch = ProcessEpoch(uuid("origin")),
            preparedAtMillis = 1,
            state = TerminalReceiptState.Pending(
                TerminalReceiptClaimState.Unclaimed,
                listOf(
                    TerminalReceiptEffect.Acknowledged(
                        DeliveryCallbackRef(row.identity, attempt),
                        row,
                    ),
                ),
            ),
        )
        return DeliveryJournal(
            activeOwnerBareJid = OWNER,
            owners = mapOf(OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
        )
    }

    private fun ref(journal: DeliveryJournal): TerminalReceiptRef {
        val receipt = journal.owners.getValue(OWNER).terminalReceipt!!
        return TerminalReceiptRef(receipt.owner, receipt.attempt, receipt.id)
    }

    private fun claimRequest(
        journal: DeliveryJournal,
        claimSeed: String,
        epochSeed: String,
    ): TerminalReceiptClaimRequest = TerminalReceiptClaimRequest(
        ref(journal),
        TerminalReceiptClaimState.Claimed(
            TerminalClaimId(uuid(claimSeed)),
            TerminalReceiptClaimant.BootstrapProcess,
            ProcessEpoch(uuid(epochSeed)),
        ),
    )

    private fun attempt(owner: String, seed: String): DeliveryAttemptRef = DeliveryAttemptRef(
        ownerBareJid = owner,
        attemptId = DeliveryAttemptId(uuid(seed)),
        nativeGeneration = NativeConnectionGeneration(1u),
    )

    private fun uuid(seed: String): String = UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

    private companion object {
        const val OWNER = "alice@waddle.test"
        const val FOREIGN_OWNER = "foreign@waddle.test"
        val DELIVERY_JOURNAL_KEY = androidx.datastore.preferences.core.stringPreferencesKey("delivery_journal_v1")
    }
}
