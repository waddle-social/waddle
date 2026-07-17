package social.waddle.android.client

import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.OutboundQueue.EnqueueResult
import social.waddle.android.client.OutboundQueue.ResumeTransitionResult
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
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.SmResumeSnapshot
import java.util.UUID

class OutboundQueueTest {
    @Test
    fun `absolute lane head blocks later ready work until terminal apply`() = runTest {
        val (prefs, queue) = activeQueue()
        val attempt = queue.beginAttempt(OWNER_A).attempt
        val first = stored(queue.enqueueClaimed(draft(OWNER_A, "m-1"), attempt))
        val second = stored(queue.enqueueReady(draft(OWNER_A, "m-2")))

        assertNull(queue.readyHead(OWNER_A))
        assertTrue(
            queue.recordTerminal(
                OWNER_A,
                first.clientStanzaId,
                attempt,
                DeliveryTerminalKind.ACK,
            ) is OutboundQueue.TerminalRecordResult.Recorded,
        )
        assertNull(queue.readyHead(OWNER_A))

        assertTrue(queue.applyNextTerminal(OWNER_A) is OutboundQueue.TerminalEffect.Acknowledged)
        assertEquals(second.identity, queue.readyHead(OWNER_A)?.identity)
        assertEquals(OWNER_A, prefs.ownerBareJid.first())
    }

    @Test
    fun `owner buckets and capacity are isolated`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val queue = OutboundQueue(prefs, capacityPerOwner = 2)
        prefs.activateSession(OWNER_A, "sess-a")
        val attemptA = queue.beginAttempt(OWNER_A).attempt
        val rowA1 = stored(queue.enqueueClaimed(draft(OWNER_A, "a-1"), attemptA))
        val rowA2 = stored(queue.enqueueClaimed(draft(OWNER_A, "a-2"), attemptA))

        prefs.activateSession(OWNER_B, "sess-b")
        val attemptB = queue.beginAttempt(OWNER_B).attempt
        val rowB1 = stored(queue.enqueueClaimed(draft(OWNER_B, "b-1"), attemptB))
        val rowB2 = stored(queue.enqueueClaimed(draft(OWNER_B, "b-2"), attemptB))
        assertEquals(
            EnqueueResult.CapacityExhausted,
            queue.enqueueClaimed(draft(OWNER_B, "b-3"), attemptB),
        )
        assertEquals(
            EnqueueResult.StaleAttempt,
            queue.enqueueClaimed(draft(OWNER_A, "a-stale"), attemptA),
        )
        assertEquals(
            OutboundQueue.TerminalRecordResult.Stale,
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
        val original = stored(queue.enqueueClaimed(originalDraft, attempt))

        val duplicate = queue.enqueueClaimed(
            draft(OWNER_A, "same-id", body = "first"),
            attempt,
        )
        assertTrue(duplicate is EnqueueResult.Stored && duplicate.idempotent)
        assertEquals(original.identity, (duplicate as EnqueueResult.Stored).row.identity)

        val conflict = queue.enqueueClaimed(
            draft(OWNER_A, "same-id", body = "different"),
            attempt,
        )
        assertTrue(conflict is EnqueueResult.Conflict)
        conflict as EnqueueResult.Conflict
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
    fun `exact CAS compares identity and expected ownership`() = runTest {
        val (_, queue) = activeQueue()
        val attempt = queue.beginAttempt(OWNER_A).attempt
        val ready = stored(queue.enqueueReady(draft(OWNER_A, "m-1")))
        val wrongIdentity = ready.identity.copy(incarnation = DeliveryIncarnation.random())

        assertNull(queue.claimReady(wrongIdentity, attempt))
        val claimed = checkNotNull(queue.claimReady(ready.identity, attempt))
        val exactOwnership = claimed.ownership as OutboundOwnership.NativeOwned
        val wrongAttempt = attempt.copy(
            attemptId = DeliveryAttemptId("00000000-0000-4000-8000-000000000099"),
        )
        assertFalse(
            queue.release(
                ready.identity,
                OutboundOwnership.NativeOwned(wrongAttempt, NativeOutboundPhase.FRESH),
            ),
        )
        assertFalse(queue.transition(wrongIdentity, exactOwnership, OutboundOwnership.Ready))
        assertTrue(queue.release(ready.identity, exactOwnership))
        assertEquals(OutboundOwnership.Ready, queue.rows(OWNER_A).single().ownership)
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
        stored(
            queue.enqueueClaimed(
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
                                OutboundQueue.MAX_TRANSITION_RECEIPTS_PER_OWNER,
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
        val queue = OutboundQueue(prefs)
        val attempt = queue.beginAttempt(OWNER_A).attempt
        val before = prefs.deliveryJournal.first()

        store.failNextUpdate = true
        val failure = runCatching {
            queue.enqueueClaimed(draft(OWNER_A, "m-1"), attempt)
        }.exceptionOrNull()

        assertNotNull(failure)
        assertEquals(before, prefs.deliveryJournal.first())
    }

    private suspend fun activeQueue(): Pair<SessionPrefs, OutboundQueue> {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        prefs.activateSession(OWNER_A, "sess-a")
        return prefs to OutboundQueue(prefs)
    }

    private fun draft(
        owner: String,
        id: String,
        body: String = "body-$id",
    ): QueuedOutboundDraft = QueuedOutboundDraft.create(
        ownerBareJid = owner,
        conversationJid = "peer@waddle.test",
        isGroupchat = false,
        body = body,
        clientStanzaId = id,
        enqueuedAtMillis = 1_000,
        source = DeliverySource.Composer,
    )

    private fun stored(result: EnqueueResult): QueuedOutboundMessage =
        (result as EnqueueResult.Stored).row

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
