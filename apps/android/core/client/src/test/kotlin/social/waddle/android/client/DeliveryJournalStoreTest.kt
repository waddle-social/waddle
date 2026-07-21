package social.waddle.android.client

import kotlinx.coroutines.async
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.DeliveryJournalStore.EnqueueResult
import social.waddle.android.client.DeliveryJournalStore.LiveAdmissionResult
import social.waddle.android.client.DeliveryJournalStore.ResumeTransitionResult
import social.waddle.android.client.prefs.CommittedResumeTransition
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.android.client.prefs.DeliveryIncarnation
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.NativeConnectionGeneration
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.SmResumeSnapshot
import java.util.UUID

class DeliveryJournalStoreTest {
    @Test
    fun `absolute lane head blocks later ready work until terminal apply`() = runTest {
        val (prefs, queue) = activeQueue()
        val attempt = queue.beginAttempt(OWNER_A).attempt
        val first = claimed(
            queue.enqueueAndClaimAbsoluteHead(draft(OWNER_A, "m-1"), attempt),
        )
        val second = stored(queue.enqueueReady(draft(OWNER_A, "m-2")))

        assertNull(queue.readyHead(OWNER_A))
        assertTrue(
            queue.recordTerminal(
                OWNER_A,
                first.clientStanzaId,
                attempt,
                DeliveryTerminalKind.ACK,
            ) is DeliveryJournalStore.TerminalRecordResult.Recorded,
        )
        assertNull(queue.readyHead(OWNER_A))

        assertTrue(queue.applyNextTerminal(OWNER_A) is DeliveryJournalStore.TerminalEffect.Acknowledged)
        assertEquals(second.identity, queue.readyHead(OWNER_A)?.identity)
        assertEquals(OWNER_A, prefs.ownerBareJid.first())
    }

    @Test
    fun `live admission stays queued behind ready native and terminal predecessors`() = runTest {
        val (_, queue) = activeQueue()
        val attempt = queue.beginAttempt(OWNER_A).attempt
        val first = stored(queue.enqueueReady(draft(OWNER_A, "m-1")))

        val behindReady = queue.enqueueAndClaimAbsoluteHead(
            draft(OWNER_A, "m-2"),
            attempt,
        ) as LiveAdmissionResult.Queued
        assertEquals(first.identity, behindReady.blocker.identity)
        assertEquals(OutboundOwnership.Ready, behindReady.blocker.ownership)
        assertEquals(OutboundOwnership.Ready, behindReady.row.ownership)

        val claimedFirst = checkNotNull(
            queue.claimAbsoluteReadyHead(OWNER_A, attempt),
        )
        val behindNative = queue.enqueueAndClaimAbsoluteHead(
            draft(OWNER_A, "m-3"),
            attempt,
        ) as LiveAdmissionResult.Queued
        assertEquals(claimedFirst.identity, behindNative.blocker.identity)
        assertTrue(behindNative.blocker.ownership is OutboundOwnership.NativeOwned)

        assertTrue(
            queue.recordTerminal(
                OWNER_A,
                claimedFirst.clientStanzaId,
                attempt,
                DeliveryTerminalKind.ACK,
            ) is DeliveryJournalStore.TerminalRecordResult.Recorded,
        )
        val behindTerminal = queue.enqueueAndClaimAbsoluteHead(
            draft(OWNER_A, "m-4"),
            attempt,
        ) as LiveAdmissionResult.Queued
        assertEquals(claimedFirst.identity, behindTerminal.blocker.identity)
        assertTrue(behindTerminal.blocker.ownership is OutboundOwnership.Terminal)

        assertTrue(
            queue.applyNextTerminal(OWNER_A) is
                DeliveryJournalStore.TerminalEffect.Acknowledged,
        )
        assertEquals(
            behindReady.row.identity,
            queue.claimAbsoluteReadyHead(OWNER_A, attempt)?.identity,
        )
    }

    @Test
    fun `owner buckets and capacity are isolated`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val queue = DeliveryJournalStore(prefs, capacityPerOwner = 2)
        prefs.activateSession(OWNER_A, "sess-a")
        val attemptA = queue.beginAttempt(OWNER_A).attempt
        val rowA1 = admitted(
            queue.enqueueAndClaimAbsoluteHead(draft(OWNER_A, "a-1"), attemptA),
        )
        val rowA2 = admitted(
            queue.enqueueAndClaimAbsoluteHead(draft(OWNER_A, "a-2"), attemptA),
        )

        prefs.activateSession(OWNER_B, "sess-b")
        val attemptB = queue.beginAttempt(OWNER_B).attempt
        val rowB1 = admitted(
            queue.enqueueAndClaimAbsoluteHead(draft(OWNER_B, "b-1"), attemptB),
        )
        val rowB2 = admitted(
            queue.enqueueAndClaimAbsoluteHead(draft(OWNER_B, "b-2"), attemptB),
        )
        assertEquals(
            LiveAdmissionResult.CapacityExhausted,
            queue.enqueueAndClaimAbsoluteHead(draft(OWNER_B, "b-3"), attemptB),
        )
        assertEquals(
            LiveAdmissionResult.StaleAttempt,
            queue.enqueueAndClaimAbsoluteHead(
                draft(OWNER_A, "a-stale"),
                attemptA,
            ),
        )
        assertEquals(
            DeliveryJournalStore.TerminalRecordResult.Stale,
            queue.recordTerminal(OWNER_A, "a-1", attemptA, DeliveryTerminalKind.ACK),
        )

        assertEquals(listOf(rowA1, rowA2), queue.rows(OWNER_A))
        assertEquals(listOf(rowB1, rowB2), queue.rows(OWNER_B))

        prefs.activateSession(OWNER_A, "sess-a")
        assertEquals(
            EnqueueResult.CapacityExhausted,
            queue.enqueueReady(draft(OWNER_A, "a-3")),
        )
    }

    @Test
    fun `same stanza digest is idempotent conflict fails and reuse gets a new identity`() = runTest {
        val (_, queue) = activeQueue()
        val attempt = queue.beginAttempt(OWNER_A).attempt
        val originalDraft = draft(OWNER_A, "same-id", body = "first")
        val original = claimed(
            queue.enqueueAndClaimAbsoluteHead(originalDraft, attempt),
        )

        val duplicate = queue.enqueueAndClaimAbsoluteHead(
            draft(OWNER_A, "same-id", body = "first"),
            attempt,
        )
        assertTrue(duplicate is LiveAdmissionResult.Queued && duplicate.idempotent)
        duplicate as LiveAdmissionResult.Queued
        assertEquals(original.identity, duplicate.row.identity)
        assertEquals(original.identity, duplicate.blocker.identity)

        val conflict = queue.enqueueAndClaimAbsoluteHead(
            draft(OWNER_A, "same-id", body = "different"),
            attempt,
        )
        assertTrue(conflict is LiveAdmissionResult.Conflict)
        conflict as LiveAdmissionResult.Conflict
        assertEquals(original.identity, conflict.existing)
        assertNotEquals(original.payloadDigest, conflict.proposedDigest)

        queue.recordTerminal(
            OWNER_A,
            original.clientStanzaId,
            attempt,
            DeliveryTerminalKind.ACK,
        )
        queue.applyNextTerminal(OWNER_A)
        val reused = stored(
            queue.enqueueReady(draft(OWNER_A, "same-id", body = "different")),
        )
        assertEquals(original.clientStanzaId, reused.clientStanzaId)
        assertNotEquals(original.identity, reused.identity)
        assertTrue(reused.sequence > original.sequence)
    }

    @Test
    fun `absolute-head claim and release compare exact identity and ownership`() = runTest {
        val (_, queue) = activeQueue()
        val attempt = queue.beginAttempt(OWNER_A).attempt
        val first = stored(queue.enqueueReady(draft(OWNER_A, "m-1")))
        val second = stored(queue.enqueueReady(draft(OWNER_A, "m-2")))
        val wrongIdentity = first.identity.copy(incarnation = DeliveryIncarnation.random())

        val claimed = checkNotNull(queue.claimAbsoluteReadyHead(OWNER_A, attempt))
        assertEquals(first.identity, claimed.identity)
        assertEquals(OutboundOwnership.Ready, queue.rows(OWNER_A)[1].ownership)
        assertEquals(second.identity, queue.rows(OWNER_A)[1].identity)
        val exactOwnership = claimed.ownership as OutboundOwnership.NativeOwned
        val wrongAttempt = attempt.copy(
            attemptId = DeliveryAttemptId("00000000-0000-4000-8000-000000000099"),
        )
        assertFalse(
            queue.release(
                first.identity,
                OutboundOwnership.NativeOwned(wrongAttempt, NativeOutboundPhase.FRESH),
            ),
        )
        assertFalse(queue.release(wrongIdentity, exactOwnership))
        assertTrue(queue.release(first.identity, exactOwnership))
        assertEquals(
            listOf(OutboundOwnership.Ready, OutboundOwnership.Ready),
            queue.rows(OWNER_A).map { it.ownership },
        )
    }

    @Test
    fun `concurrent absolute-head claims produce exactly one native owner`() = runTest {
        val (_, queue) = activeQueue()
        val attempt = queue.beginAttempt(OWNER_A).attempt
        val ready = stored(queue.enqueueReady(draft(OWNER_A, "m-1")))

        val first = async { queue.claimAbsoluteReadyHead(OWNER_A, attempt) }
        val second = async { queue.claimAbsoluteReadyHead(OWNER_A, attempt) }
        val claims = listOf(first.await(), second.await()).filterNotNull()

        assertEquals(listOf(ready.identity), claims.map { it.identity })
        assertTrue(claims.single().ownership is OutboundOwnership.NativeOwned)
    }

    @Test
    fun `duplicate delivery sequences fail closed without mutation`() = runTest {
        val (prefs, queue) = activeQueue()
        val attempt = queue.beginAttempt(OWNER_A).attempt
        val first = stored(queue.enqueueReady(draft(OWNER_A, "m-1")))
        val second = stored(queue.enqueueReady(draft(OWNER_A, "m-2")))
        prefs.updateDeliveryJournal { journal ->
            val owner = checkNotNull(journal.owners[OWNER_A])
            DeliveryJournalMutation(
                journal = journal.copy(
                    owners = journal.owners + (
                        OWNER_A to owner.copy(
                            outboundRows = listOf(
                                first,
                                second.copy(sequence = first.sequence),
                            ),
                        )
                    ),
                ),
                result = Unit,
            )
        }
        val before = prefs.deliveryJournal.first()

        val failure = runCatching {
            queue.enqueueAndClaimAbsoluteHead(draft(OWNER_A, "m-3"), attempt)
        }.exceptionOrNull()

        assertNotNull(failure)
        assertEquals(before, prefs.deliveryJournal.first())
    }

    @Test
    fun `nonmonotonic next sequence fails closed without claiming`() = runTest {
        val (prefs, queue) = activeQueue()
        val attempt = queue.beginAttempt(OWNER_A).attempt
        val ready = stored(queue.enqueueReady(draft(OWNER_A, "m-1")))
        prefs.updateDeliveryJournal { journal ->
            val owner = checkNotNull(journal.owners[OWNER_A])
            DeliveryJournalMutation(
                journal = journal.copy(
                    owners = journal.owners + (
                        OWNER_A to owner.copy(nextSequence = ready.sequence)
                    ),
                ),
                result = Unit,
            )
        }
        val before = prefs.deliveryJournal.first()

        val failure = runCatching {
            queue.claimAbsoluteReadyHead(OWNER_A, attempt)
        }.exceptionOrNull()

        assertNotNull(failure)
        assertEquals(before, prefs.deliveryJournal.first())
    }

    @Test
    fun `exhausted delivery sequence fails closed without allocation`() = runTest {
        val (prefs, queue) = activeQueue()
        val attempt = queue.beginAttempt(OWNER_A).attempt
        prefs.updateDeliveryJournal { journal ->
            val owner = checkNotNull(journal.owners[OWNER_A])
            DeliveryJournalMutation(
                journal = journal.copy(
                    owners = journal.owners + (
                        OWNER_A to owner.copy(nextSequence = Long.MAX_VALUE)
                    ),
                ),
                result = Unit,
            )
        }
        val before = prefs.deliveryJournal.first()

        val failure = runCatching {
            queue.enqueueAndClaimAbsoluteHead(draft(OWNER_A, "m-1"), attempt)
        }.exceptionOrNull()

        assertNotNull(failure)
        assertEquals(before, prefs.deliveryJournal.first())
    }

    @Test
    fun `SM consumption and clear tombstones reject stale resurrection`() = runTest {
        val (prefs, queue) = activeQueue()
        val first = queue.beginAttempt(OWNER_A).attempt
        val snapshot = SmResumeSnapshot("previd-1", 3u, 4u)
        assertTrue(queue.saveSmResume(first, version = 1, snapshot = snapshot))

        val second = queue.beginAttempt(OWNER_A)
        assertEquals(snapshot, second.resumeSnapshot)
        var slot = prefs.deliveryJournal.first().owners[OWNER_A]?.sm
        assertEquals(1L, slot?.version)
        assertEquals(1L, slot?.tombstoneVersion)
        assertNull(slot?.snapshot)
        assertFalse(queue.saveSmResume(first, version = 2, snapshot = snapshot))

        assertTrue(queue.saveSmResume(second.attempt, version = 2, snapshot = snapshot))
        assertTrue(queue.saveSmResume(second.attempt, version = 3, snapshot = null))
        assertFalse(queue.saveSmResume(second.attempt, version = 2, snapshot = snapshot))
        slot = prefs.deliveryJournal.first().owners[OWNER_A]?.sm
        assertEquals(3L, slot?.version)
        assertEquals(3L, slot?.tombstoneVersion)
        assertNull(slot?.snapshot)
    }

    @Test
    fun `resume transition receipt is atomic idempotent and conflict detecting`() = runTest {
        val (prefs, queue) = activeQueue()
        val old = queue.beginAttempt(OWNER_A).attempt
        queue.saveSmResume(old, version = 1, snapshot = SmResumeSnapshot("previd-1", 3u, 4u))
        claimed(
            queue.enqueueAndClaimAbsoluteHead(
                draft(OWNER_A, "m-1"),
                old,
                NativeOutboundPhase.RESUME,
            ),
        )
        val fresh = old.next("00000000-0000-4000-8000-000000000002")
        val transition = DeliveryAttemptTransition(old, fresh)

        assertTrue(
            queue.rotateAfterResumeFailure(transition, setOf("wrong")) is
                ResumeTransitionResult.AffectedSetMismatch,
        )
        assertEquals(old, prefs.deliveryJournal.first().owners[OWNER_A]?.activeAttempt)

        val committed = queue.rotateAfterResumeFailure(transition, setOf("m-1"))
        assertTrue(committed is ResumeTransitionResult.Committed)
        val owner = prefs.deliveryJournal.first().owners[OWNER_A]
        assertEquals(fresh, owner?.activeAttempt)
        assertEquals(owner?.sm?.version, owner?.sm?.tombstoneVersion)
        assertNull(owner?.sm?.snapshot)
        assertEquals(
            OutboundOwnership.NativeOwned(fresh, NativeOutboundPhase.FRESH_FALLBACK),
            owner?.outboundRows?.single()?.ownership,
        )
        assertTrue(
            queue.rotateAfterResumeFailure(transition, setOf("m-1")) is
                ResumeTransitionResult.AlreadyCommitted,
        )
        assertEquals(
            ResumeTransitionResult.ReceiptConflict,
            queue.rotateAfterResumeFailure(
                DeliveryAttemptTransition(
                    old,
                    old.next("00000000-0000-4000-8000-000000000003"),
                ),
                setOf("m-1"),
            ),
        )
    }

    @Test
    fun `transition receipt capacity fails closed without eviction`() = runTest {
        val (prefs, queue) = activeQueue()
        val attempt = queue.beginAttempt(OWNER_A).attempt
        prefs.updateDeliveryJournal { journal ->
            val owner = checkNotNull(journal.owners[OWNER_A])
            DeliveryJournalMutation(
                journal = journal.copy(
                    owners = journal.owners + (
                        OWNER_A to owner.copy(
                            resumeTransitionReceipts = List(
                                DeliveryJournalStore.MAX_TRANSITION_RECEIPTS_PER_OWNER,
                                ::receipt,
                            ),
                        )
                    ),
                ),
                result = Unit,
            )
        }

        val fresh = attempt.next("00000000-0000-4000-8000-000000000099")
        assertEquals(
            ResumeTransitionResult.ReceiptCapacityExhausted,
            queue.rotateAfterResumeFailure(
                DeliveryAttemptTransition(attempt, fresh),
                affectedStanzaIds = emptySet(),
            ),
        )
        assertEquals(attempt, prefs.deliveryJournal.first().owners[OWNER_A]?.activeAttempt)
    }

    @Test
    fun `failed storage edit leaves exact journal unchanged`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        prefs.activateSession(OWNER_A, "sess-a")
        val queue = DeliveryJournalStore(prefs)
        val attempt = queue.beginAttempt(OWNER_A).attempt
        val before = prefs.deliveryJournal.first()

        store.failNextUpdate = true
        val failure = runCatching {
            queue.enqueueAndClaimAbsoluteHead(draft(OWNER_A, "m-1"), attempt)
        }.exceptionOrNull()

        assertNotNull(failure)
        assertEquals(before, prefs.deliveryJournal.first())
    }

    private suspend fun activeQueue(): Pair<SessionPrefs, DeliveryJournalStore> {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        prefs.activateSession(OWNER_A, "sess-a")
        return prefs to DeliveryJournalStore(prefs)
    }

    private fun draft(
        owner: String,
        id: String,
        body: String = "body-$id",
    ): QueuedOutboundDraft = QueuedOutboundDraft.create(
        ownerBareJid = owner,
        clientStanzaId = id,
        enqueuedAtMillis = 1_000,
        payload = QueuedOutboundPayload(
            target = QueuedOutboundTarget.Chat("peer@waddle.test"),
            content = QueuedOutboundContent(body),
        ),
        source = DeliverySource.Composer,
    )

    private fun stored(result: EnqueueResult): QueuedOutboundMessage =
        (result as EnqueueResult.Stored).row

    private fun claimed(result: LiveAdmissionResult): QueuedOutboundMessage =
        (result as LiveAdmissionResult.Claimed).row

    private fun admitted(result: LiveAdmissionResult): QueuedOutboundMessage =
        when (result) {
            is LiveAdmissionResult.Claimed -> result.row
            is LiveAdmissionResult.Queued -> result.row
            else -> error("expected admitted delivery row, received $result")
        }

    private fun DeliveryAttemptRef.next(id: String): DeliveryAttemptRef = copy(
        attemptId = DeliveryAttemptId(id),
        nativeGeneration = nativeGeneration.next(),
    )

    private fun receipt(index: Int): CommittedResumeTransition {
        val old = DeliveryAttemptRef(
            ownerBareJid = OWNER_A,
            attemptId = DeliveryAttemptId(
                UUID.nameUUIDFromBytes("old-$index".toByteArray()).toString(),
            ),
            nativeGeneration = NativeConnectionGeneration(1u),
        )
        return CommittedResumeTransition(
            transition = DeliveryAttemptTransition(
                old = old,
                fresh = old.copy(
                    attemptId = DeliveryAttemptId(
                        UUID.nameUUIDFromBytes("fresh-$index".toByteArray()).toString(),
                    ),
                    nativeGeneration = NativeConnectionGeneration(2u),
                ),
            ),
            affectedSetDigest = "digest-$index",
            smVersion = index.toLong() + 1,
            committedAtMillis = 1,
        )
    }

    private companion object {
        const val OWNER_A = "alice@waddle.test"
        const val OWNER_B = "bob@waddle.test"
    }
}
