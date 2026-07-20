package social.waddle.android.client

import androidx.datastore.preferences.core.stringPreferencesKey
import java.util.UUID
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.FinalizerGeneration
import social.waddle.android.client.prefs.LifecycleGeneration
import social.waddle.android.client.prefs.NativeConnectionGeneration
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.TerminalClaimId
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptClaimant
import social.waddle.android.client.prefs.TerminalReceiptId
import social.waddle.android.client.prefs.TerminalReceiptState
import social.waddle.android.client.prefs.TerminalReceiptWorkerKind
import social.waddle.android.client.prefs.WorkerGeneration as ReceiptWorkerGeneration

class TerminalReceiptLeasePersistenceTest {
    @Test
    fun `persisted pending acknowledgement denies each mismatched lease field without rewriting bytes`() = runTest {
        leaseMismatchCases().forEachIndexed { index, mismatch ->
            val fixture = claimedFixture("pending-ack-$index", mismatch.claimant)
            val before = fixture.snapshot()
            val result = fixture.prefs.acknowledgeTerminalReceipt(mismatch.mutate(fixture.lease))

            assertAcknowledgeDenial(mismatch, result)
            fixture.assertUnchanged(before, result.journal)
        }
    }

    @Test
    fun `persisted pending release denies each mismatched lease field without rewriting bytes`() = runTest {
        leaseMismatchCases().forEachIndexed { index, mismatch ->
            val fixture = claimedFixture("pending-release-$index", mismatch.claimant)
            val before = fixture.snapshot()
            val result = fixture.prefs.releaseTerminalReceipt(mismatch.mutate(fixture.lease))

            assertReleaseDenial(mismatch, result)
            fixture.assertUnchanged(before, result.journal)
        }
    }

    @Test
    fun `persisted acknowledged acknowledgement denies each mismatched lease field without rewriting bytes`() = runTest {
        leaseMismatchCases().forEachIndexed { index, mismatch ->
            val fixture = acknowledgedFixture("acknowledged-ack-$index", mismatch.claimant)
            val before = fixture.snapshot()
            val result = fixture.prefs.acknowledgeTerminalReceipt(mismatch.mutate(fixture.lease))

            assertAcknowledgeDenial(mismatch, result)
            fixture.assertUnchanged(before, result.journal)
        }
    }

    @Test
    fun `persisted acknowledged release denies each mismatched lease field without rewriting bytes`() = runTest {
        leaseMismatchCases().forEachIndexed { index, mismatch ->
            val fixture = acknowledgedFixture("acknowledged-release-$index", mismatch.claimant)
            val before = fixture.snapshot()
            val result = fixture.prefs.releaseTerminalReceipt(mismatch.mutate(fixture.lease))

            assertReleaseDenial(mismatch, result)
            fixture.assertUnchanged(before, result.journal)
        }
    }

    @Test
    fun `persisted exact pending leases survive active owner handoff for acknowledge and release`() = runTest {
        listOf("acknowledge", "release").forEach { operation ->
            val fixture = claimedFixture("handoff-$operation", workerClaimant("handoff-$operation"))
            fixture.prefs.updateDeliveryJournal { journal ->
                DeliveryJournalMutation(journal.copy(activeOwnerBareJid = OTHER_OWNER), Unit)
            }

            when (operation) {
                "acknowledge" -> assertTrue(
                    fixture.prefs.acknowledgeTerminalReceipt(fixture.lease) is TerminalReceiptAcknowledgeResult.Acknowledged,
                )
                "release" -> assertTrue(
                    fixture.prefs.releaseTerminalReceipt(fixture.lease) is TerminalReceiptReleaseResult.Released,
                )
            }
        }
    }

    @Test
    fun `persisted active owner handoff denies a valid different owner lease without rewriting bytes`() = runTest {
        listOf("acknowledge", "release").forEach { operation ->
            val fixture = claimedFixture("handoff-other-$operation", workerClaimant("handoff-other-$operation"))
            fixture.prefs.updateDeliveryJournal { journal ->
                DeliveryJournalMutation(journal.copy(activeOwnerBareJid = OTHER_OWNER), Unit)
            }
            val wrongOwnerLease = fixture.lease.copy(ref = fixture.lease.ref.copy(
                owner = social.waddle.android.client.prefs.DeliveryOwnerBareJid(OTHER_OWNER),
                attempt = fixture.lease.ref.attempt.copy(ownerBareJid = OTHER_OWNER),
            ))
            val before = fixture.snapshot()

            when (operation) {
                "acknowledge" -> {
                    val result = fixture.prefs.acknowledgeTerminalReceipt(wrongOwnerLease)
                    assertTrue(result is TerminalReceiptAcknowledgeResult.ReceiptMissing)
                    fixture.assertUnchanged(before, result.journal)
                }
                "release" -> {
                    val result = fixture.prefs.releaseTerminalReceipt(wrongOwnerLease)
                    assertTrue(result is TerminalReceiptReleaseResult.ReceiptMissing)
                    fixture.assertUnchanged(before, result.journal)
                }
            }
        }
    }

    @Test
    fun `persisted exact claimant leases acknowledge and release for every claimant subtype`() = runTest {
        val claimants = listOf(
            "worker" to workerClaimant("success-worker"),
            "finalizer" to finalizerClaimant("success-finalizer"),
            "bootstrap" to TerminalReceiptClaimant.BootstrapProcess,
        )
        claimants.forEach { (name, claimant) ->
            val acknowledged = claimedFixture("success-ack-$name", claimant)
            val acknowledgement = acknowledged.prefs.acknowledgeTerminalReceipt(acknowledged.lease)
            assertTrue("$name acknowledgement must succeed", acknowledgement is TerminalReceiptAcknowledgeResult.Acknowledged)
            val acknowledgedState = requireNotNull(
                acknowledged.prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt,
            ).state as TerminalReceiptState.Acknowledged
            assertEquals("$name acknowledgement must persist its exact claim", acknowledged.lease.claim, acknowledgedState.claim)

            val released = claimedFixture("success-release-$name", claimant)
            val release = released.prefs.releaseTerminalReceipt(released.lease)
            assertTrue("$name release must succeed", release is TerminalReceiptReleaseResult.Released)
            val releasedState = requireNotNull(
                released.prefs.deliveryJournal.first().owners.getValue(OWNER).terminalReceipt,
            ).state as TerminalReceiptState.Pending
            assertEquals("$name release must clear its durable claim", TerminalReceiptClaimState.Unclaimed, releasedState.claim)
            assertEquals("$name release must record its exact released claim", released.lease.claim, releasedState.releasedClaim)
        }
    }

    @Test
    fun `persisted exact terminal no ops preserve raw bytes for acknowledged and released receipts`() = runTest {
        val acknowledged = acknowledgedFixture("exact-acknowledged", workerClaimant("exact-acknowledged"))
        val acknowledgedBefore = acknowledged.snapshot()
        assertTrue(acknowledged.prefs.acknowledgeTerminalReceipt(acknowledged.lease) is TerminalReceiptAcknowledgeResult.AlreadyAcknowledged)
        acknowledged.assertUnchanged(acknowledgedBefore)
        assertTrue(acknowledged.prefs.releaseTerminalReceipt(acknowledged.lease) is TerminalReceiptReleaseResult.AlreadyAcknowledged)
        acknowledged.assertUnchanged(acknowledgedBefore)

        val released = claimedFixture("exact-released", workerClaimant("exact-released"))
        assertTrue(released.prefs.releaseTerminalReceipt(released.lease) is TerminalReceiptReleaseResult.Released)
        val releasedBefore = released.snapshot()
        assertTrue(released.prefs.releaseTerminalReceipt(released.lease) is TerminalReceiptReleaseResult.AlreadyReleased)
        released.assertUnchanged(releasedBefore)
    }

    private suspend fun claimedFixture(
        seed: String,
        claimant: TerminalReceiptClaimant,
    ): PersistedLeaseFixture {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val receipt = pendingTerminalReceipt(OWNER, seed)
        prefs.updateDeliveryJournal { journal ->
            DeliveryJournalMutation(
                journal.copy(
                    activeOwnerBareJid = OWNER,
                    owners = journal.owners + (OWNER to DeliveryOwnerJournal(terminalReceipt = receipt)),
                ),
                Unit,
            )
        }
        val request = TerminalReceiptClaimRequest(
            TerminalReceiptRef(receipt.owner, receipt.attempt, receipt.id),
            TerminalReceiptClaimState.Claimed(
                TerminalClaimId(uuid("$seed-claim")),
                claimant,
                ProcessEpoch(uuid("$seed-epoch")),
            ),
        )
        val claimed = prefs.claimTerminalReceipt(request) as TerminalReceiptClaimResult.Claimed
        return PersistedLeaseFixture(store, prefs, claimed.lease)
    }

    private suspend fun acknowledgedFixture(
        seed: String,
        claimant: TerminalReceiptClaimant,
    ): PersistedLeaseFixture = claimedFixture(seed, claimant).also { fixture ->
        assertTrue(fixture.prefs.acknowledgeTerminalReceipt(fixture.lease) is TerminalReceiptAcknowledgeResult.Acknowledged)
    }

    private fun leaseMismatchCases(): List<LeaseMismatchCase> = listOf(
        LeaseMismatchCase("attempt owner", workerClaimant("attempt-owner"), LeaseDenial.MISSING) { lease ->
            lease.copy(ref = lease.ref.copy(
                owner = social.waddle.android.client.prefs.DeliveryOwnerBareJid(OTHER_OWNER),
                attempt = lease.ref.attempt.copy(ownerBareJid = OTHER_OWNER),
            ))
        },
        LeaseMismatchCase("attempt id", workerClaimant("attempt-id"), LeaseDenial.REPLACED) { lease ->
            lease.copy(ref = lease.ref.copy(attempt = lease.ref.attempt.copy(
                attemptId = DeliveryAttemptId(uuid("wrong-attempt-id")),
            )))
        },
        LeaseMismatchCase("native generation", workerClaimant("native-generation"), LeaseDenial.REPLACED) { lease ->
            lease.copy(ref = lease.ref.copy(attempt = lease.ref.attempt.copy(
                nativeGeneration = NativeConnectionGeneration(lease.ref.attempt.nativeGeneration.value + 1u),
            )))
        },
        LeaseMismatchCase("receipt id", workerClaimant("receipt-id"), LeaseDenial.REPLACED) { lease ->
            lease.copy(ref = lease.ref.copy(id = TerminalReceiptId(uuid("wrong-receipt-id"))))
        },
        LeaseMismatchCase("claim id", workerClaimant("claim-id"), LeaseDenial.LEASE_MISMATCH) { lease ->
            lease.copy(claim = lease.claim.copy(id = TerminalClaimId(uuid("wrong-claim-id"))))
        },
        LeaseMismatchCase("claimant worker to finalizer", workerClaimant("worker-finalizer"), LeaseDenial.LEASE_MISMATCH) { lease ->
            lease.copy(claim = lease.claim.copy(claimant = finalizerClaimant("wrong-finalizer")))
        },
        LeaseMismatchCase("claimant worker to bootstrap", workerClaimant("worker-bootstrap"), LeaseDenial.LEASE_MISMATCH) { lease ->
            lease.copy(claim = lease.claim.copy(claimant = TerminalReceiptClaimant.BootstrapProcess))
        },
        LeaseMismatchCase("claimant finalizer to worker", finalizerClaimant("finalizer-worker"), LeaseDenial.LEASE_MISMATCH) { lease ->
            lease.copy(claim = lease.claim.copy(claimant = workerClaimant("wrong-worker")))
        },
        LeaseMismatchCase("claimant finalizer to bootstrap", finalizerClaimant("finalizer-bootstrap"), LeaseDenial.LEASE_MISMATCH) { lease ->
            lease.copy(claim = lease.claim.copy(claimant = TerminalReceiptClaimant.BootstrapProcess))
        },
        LeaseMismatchCase("claimant bootstrap to worker", TerminalReceiptClaimant.BootstrapProcess, LeaseDenial.LEASE_MISMATCH) { lease ->
            lease.copy(claim = lease.claim.copy(claimant = workerClaimant("bootstrap-worker")))
        },
        LeaseMismatchCase("claimant bootstrap to finalizer", TerminalReceiptClaimant.BootstrapProcess, LeaseDenial.LEASE_MISMATCH) { lease ->
            lease.copy(claim = lease.claim.copy(claimant = finalizerClaimant("bootstrap-finalizer")))
        },
        LeaseMismatchCase("worker kind", workerClaimant("worker-kind"), LeaseDenial.LEASE_MISMATCH) { lease ->
            val claimant = lease.claim.claimant as TerminalReceiptClaimant.Worker
            lease.copy(claim = lease.claim.copy(claimant = claimant.copy(kind = TerminalReceiptWorkerKind.OUTBOUND_DRAIN)))
        },
        LeaseMismatchCase("worker lifecycle generation", workerClaimant("worker-lifecycle"), LeaseDenial.LEASE_MISMATCH) { lease ->
            val claimant = lease.claim.claimant as TerminalReceiptClaimant.Worker
            lease.copy(claim = lease.claim.copy(claimant = claimant.copy(
                lifecycleGeneration = LifecycleGeneration(uuid("wrong-worker-lifecycle")),
            )))
        },
        LeaseMismatchCase("worker generation", workerClaimant("worker-generation"), LeaseDenial.LEASE_MISMATCH) { lease ->
            val claimant = lease.claim.claimant as TerminalReceiptClaimant.Worker
            lease.copy(claim = lease.claim.copy(claimant = claimant.copy(
                workerGeneration = ReceiptWorkerGeneration(uuid("wrong-worker-generation")),
            )))
        },
        LeaseMismatchCase("finalizer lifecycle generation", finalizerClaimant("finalizer-lifecycle"), LeaseDenial.LEASE_MISMATCH) { lease ->
            val claimant = lease.claim.claimant as TerminalReceiptClaimant.Finalizer
            lease.copy(claim = lease.claim.copy(claimant = claimant.copy(
                lifecycleGeneration = LifecycleGeneration(uuid("wrong-finalizer-lifecycle")),
            )))
        },
        LeaseMismatchCase("finalizer generation", finalizerClaimant("finalizer-generation"), LeaseDenial.LEASE_MISMATCH) { lease ->
            val claimant = lease.claim.claimant as TerminalReceiptClaimant.Finalizer
            lease.copy(claim = lease.claim.copy(claimant = claimant.copy(
                finalizerGeneration = FinalizerGeneration(uuid("wrong-finalizer-generation")),
            )))
        },
        LeaseMismatchCase("claim process epoch", workerClaimant("process-epoch"), LeaseDenial.LEASE_MISMATCH) { lease ->
            lease.copy(claim = lease.claim.copy(processEpoch = ProcessEpoch(uuid("wrong-process-epoch"))))
        },
    )

    private fun assertAcknowledgeDenial(
        mismatch: LeaseMismatchCase,
        result: TerminalReceiptAcknowledgeResult,
    ) = when (mismatch.denial) {
        LeaseDenial.MISSING -> assertTrue("${mismatch.field} must be ReceiptMissing", result is TerminalReceiptAcknowledgeResult.ReceiptMissing)
        LeaseDenial.REPLACED -> assertTrue("${mismatch.field} must be ReceiptReplaced", result is TerminalReceiptAcknowledgeResult.ReceiptReplaced)
        LeaseDenial.LEASE_MISMATCH -> assertTrue("${mismatch.field} must be LeaseMismatch", result is TerminalReceiptAcknowledgeResult.LeaseMismatch)
    }

    private fun assertReleaseDenial(
        mismatch: LeaseMismatchCase,
        result: TerminalReceiptReleaseResult,
    ) = when (mismatch.denial) {
        LeaseDenial.MISSING -> assertTrue("${mismatch.field} must be ReceiptMissing", result is TerminalReceiptReleaseResult.ReceiptMissing)
        LeaseDenial.REPLACED -> assertTrue("${mismatch.field} must be ReceiptReplaced", result is TerminalReceiptReleaseResult.ReceiptReplaced)
        LeaseDenial.LEASE_MISMATCH -> assertTrue("${mismatch.field} must be LeaseMismatch", result is TerminalReceiptReleaseResult.LeaseMismatch)
    }

    private fun workerClaimant(seed: String) = TerminalReceiptClaimant.Worker(
        LifecycleGeneration(uuid("$seed-lifecycle")),
        TerminalReceiptWorkerKind.DELIVERY_TERMINAL,
        ReceiptWorkerGeneration(uuid("$seed-worker")),
    )

    private fun finalizerClaimant(seed: String) = TerminalReceiptClaimant.Finalizer(
        LifecycleGeneration(uuid("$seed-lifecycle")),
        FinalizerGeneration(uuid("$seed-finalizer")),
    )

    private data class LeaseMismatchCase(
        val field: String,
        val claimant: TerminalReceiptClaimant,
        val denial: LeaseDenial,
        val mutate: (TerminalReceiptLease) -> TerminalReceiptLease,
    )

    private enum class LeaseDenial { MISSING, REPLACED, LEASE_MISMATCH }

    private data class PersistedLeaseFixture(
        val store: FailingPreferencesDataStore,
        val prefs: SessionPrefs,
        val lease: TerminalReceiptLease,
    ) {
        suspend fun snapshot(): DeliveryJournalSnapshot = DeliveryJournalSnapshot(
            journal = prefs.deliveryJournal.first(),
            raw = requireNotNull(store.data.first()[DELIVERY_JOURNAL_KEY]),
        )

        suspend fun assertUnchanged(
            before: DeliveryJournalSnapshot,
            resultJournal: social.waddle.android.client.prefs.DeliveryJournal = before.journal,
        ) {
            assertEquals(before.journal, resultJournal)
            assertEquals(before.journal, prefs.deliveryJournal.first())
            assertEquals(before.raw, requireNotNull(store.data.first()[DELIVERY_JOURNAL_KEY]))
        }
    }

    private data class DeliveryJournalSnapshot(
        val journal: social.waddle.android.client.prefs.DeliveryJournal,
        val raw: String,
    )

    private fun uuid(seed: String): String = UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

    private companion object {
        const val OWNER = "alice@waddle.test"
        const val OTHER_OWNER = "other@waddle.test"
        val DELIVERY_JOURNAL_KEY = stringPreferencesKey("delivery_journal_v1")
    }
}
