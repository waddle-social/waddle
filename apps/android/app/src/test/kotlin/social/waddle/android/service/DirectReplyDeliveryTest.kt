package social.waddle.android.service

import app.cash.turbine.test
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.FailingPreferencesDataStore
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.OutboundQueue
import social.waddle.android.client.PinnedRandom
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.DeliveryAttemptId
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.testSessionInfo
import social.waddle.client.ffi.WaddleSendMessageOutcome

@OptIn(ExperimentalCoroutinesApi::class)
class DirectReplyDeliveryTest {
    @Test
    fun `queued direct reply returns before predecessor drain and preserves exact delivery`() =
        runTest {
            val harness = ManagerHarness(this)
            harness.manager.login(
                testSessionInfo(username = "alice", jid = OWNER_A),
            )
            runCurrent()
            harness.factory.emitReady()
            runCurrent()
            val client = harness.factory.clients.single()
            client.sendOutcomes += WaddleSendMessageOutcome.NotConnected

            val predecessor = harness.manager.sendDirectReply(
                expectedOwnerBareJid = OWNER_A,
                conversationJid = PEER,
                isGroupchat = false,
                body = "predecessor",
            )
            val predecessorId =
                checkNotNull(predecessor.delivery).identity.clientStanzaId
            client.sendOutcomes += WaddleSendMessageOutcome.Error

            val target = harness.manager.sendDirectReply(
                expectedOwnerBareJid = OWNER_A,
                conversationJid = PEER,
                isGroupchat = false,
                body = "target",
            )
            val targetDelivery = checkNotNull(target.delivery)
            val targetId = targetDelivery.identity.clientStanzaId

            assertEquals(WaddleSendMessageOutcome.NotConnected, target.outcome)
            assertEquals(
                DeliverySource.DirectReply(PEER, false),
                targetDelivery.source,
            )
            assertEquals(
                listOf(predecessorId),
                client.sendOptions.map { it?.stanzaId },
            )

            runCurrent()
            assertEquals(
                listOf(predecessorId, predecessorId, targetId),
                client.sendOptions.map { it?.stanzaId },
            )
            assertEquals(1, client.sendOptions.count { it?.stanzaId == targetId })
            assertEquals(
                listOf(targetId),
                harness.prefs.deliveryJournal.first()
                    .owners[OWNER_A]
                    ?.outboundRows
                    .orEmpty()
                    .map { it.clientStanzaId },
            )
            harness.manager.logout()
        }

    @Test
    fun `direct reply context survives process reconstruction on the durable row`() = runTest {
        val harness = ManagerHarness(this)
        harness.manager.login(
            testSessionInfo(username = "alice", jid = OWNER_A),
        )
        runCurrent()

        val result = harness.manager.sendDirectReply(
            expectedOwnerBareJid = OWNER_A,
            conversationJid = PEER,
            isGroupchat = false,
            body = "reply after restart",
        )

        assertEquals(WaddleSendMessageOutcome.NotConnected, result.outcome)
        assertTrue(result.queued)
        val reconstructed = SessionPrefs(harness.dataStore)
        val row = reconstructed.deliveryJournal.first()
            .owners[OWNER_A]
            ?.outboundRows
            ?.single()
        assertEquals("reply after restart", row?.body)
        assertEquals(PEER, row?.conversationJid)
        assertEquals(DeliverySource.DirectReply(PEER, false), row?.source)
        harness.manager.logout()
    }

    @Test
    fun `owner scoped notification keys and intent identities never alias`() {
        val keyA = NotificationConversationKey(OWNER_A, PEER)
        val keyB = NotificationConversationKey(OWNER_B, PEER)

        assertNotEquals(keyA.notificationTag, keyB.notificationTag)
        assertNotEquals(keyA.notificationGroup, keyB.notificationGroup)
        NotificationIntentKind.entries.forEach { kind ->
            val intentA = notificationIntentIdentity(kind, keyA)
            val intentB = notificationIntentIdentity(kind, keyB)
            assertNotEquals(intentA, intentB)
            assertNotEquals(intentA.dataUri, intentB.dataUri)
        }
    }

    @Test
    fun `stale owner actions fail closed after account replacement`() = runTest {
        val harness = ManagerHarness(this)
        harness.manager.login(
            testSessionInfo(sessionId = "sess-b", username = "bob", jid = OWNER_B),
        )
        runCurrent()

        val reply = harness.manager.sendDirectReply(
            expectedOwnerBareJid = OWNER_A,
            conversationJid = PEER,
            isGroupchat = false,
            body = "stale reply",
        )
        val markedRead = harness.manager.markConversationDisplayedForOwner(
            expectedOwnerBareJid = OWNER_A,
            conversationJid = PEER,
            isGroupchat = false,
        )

        assertEquals(WaddleSendMessageOutcome.Error, reply.outcome)
        assertFalse(markedRead)
        assertFalse(notificationOwnerMatches(OWNER_B, OWNER_A))
        assertTrue(notificationOwnerMatches(OWNER_B, OWNER_B))
        assertTrue(
            harness.prefs.deliveryJournal.first()
                .owners[OWNER_B]
                ?.outboundRows
                .orEmpty()
                .isEmpty(),
        )
        harness.manager.logout()
    }

    @Test
    fun `identical stanza ids remain independent across owners`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val queue = OutboundQueue(prefs)
        val rowA = seedClaimed(prefs, queue, OWNER_A, "reply-a")
        val rowB = seedClaimed(prefs, queue, OWNER_B, "reply-b")

        assertEquals(SHARED_STANZA_ID, rowA.row.clientStanzaId)
        assertEquals(SHARED_STANZA_ID, rowB.row.clientStanzaId)
        assertNotEquals(rowA.row.identity, rowB.row.identity)
        assertEquals(rowA.row, queue.rows(OWNER_A).single())
        assertEquals(rowB.row, queue.rows(OWNER_B).single())
    }

    @Test
    fun `native terminal effects require exact owner identity digest and attempt`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val queue = OutboundQueue(prefs)
        val rowA = seedClaimed(prefs, queue, OWNER_A, "reply-a")
        val rowB = seedClaimed(prefs, queue, OWNER_B, "reply-b")

        assertForeignOwnerTerminalIsRejected(queue, rowA, rowB)
        prefs.activateSession(OWNER_A, "session-a")
        assertExactAttemptIsRequired(queue, rowA, rowB)
        assertStaleIntentCannotDeleteReplacement()
    }

    private suspend fun assertForeignOwnerTerminalIsRejected(
        queue: OutboundQueue,
        rowA: SeededRow,
        rowB: SeededRow,
    ) {
        assertEquals(
            OutboundQueue.TerminalRecordResult.Stale,
            queue.recordTerminal(
                OWNER_A,
                SHARED_STANZA_ID,
                rowA.attempt,
                DeliveryTerminalKind.ACK,
            ),
        )
        queue.recordTerminal(
            OWNER_B,
            SHARED_STANZA_ID,
            rowB.attempt,
            DeliveryTerminalKind.NATIVE_FAILURE,
        )
        val failed = queue.applyNextTerminal(OWNER_B) as OutboundQueue.TerminalEffect.Failed
        assertEquals(rowB.row.identity, failed.callback.row)
        assertEquals(rowB.attempt, failed.callback.attempt)
        assertEquals(rowA.row, queue.rows(OWNER_A).single())
    }

    private suspend fun assertExactAttemptIsRequired(
        queue: OutboundQueue,
        rowA: SeededRow,
        rowB: SeededRow,
    ) {
        val wrongAttempt = rowA.attempt.copy(
            attemptId = DeliveryAttemptId(WRONG_ATTEMPT_ID),
        )
        assertEquals(
            OutboundQueue.TerminalRecordResult.Stale,
            queue.recordTerminal(
                OWNER_A,
                SHARED_STANZA_ID,
                wrongAttempt,
                DeliveryTerminalKind.ACK,
            ),
        )
        queue.recordTerminal(
            OWNER_A,
            SHARED_STANZA_ID,
            rowA.attempt,
            DeliveryTerminalKind.ACK,
        )
        val acknowledged =
            queue.applyNextTerminal(OWNER_A) as OutboundQueue.TerminalEffect.Acknowledged
        assertEquals(rowA.row.identity, acknowledged.callback.row)
        assertEquals(rowA.attempt, acknowledged.callback.attempt)
        assertTrue(queue.rows(OWNER_A).isEmpty())
        assertEquals(rowB.row.identity, queue.rows(OWNER_B).single().identity)
    }

    private suspend fun assertStaleIntentCannotDeleteReplacement() {
        val reusePrefs = SessionPrefs(InMemoryPreferencesDataStore())
        val reuseQueue = OutboundQueue(reusePrefs)
        val old = seedClaimed(reusePrefs, reuseQueue, OWNER_A, "old payload")
        reuseQueue.recordTerminal(
            OWNER_A,
            SHARED_STANZA_ID,
            old.attempt,
            DeliveryTerminalKind.ACK,
        )
        val replacement = directReplyDraft(OWNER_A, "new payload").persisted(
            sequence = old.row.sequence + 1,
            ownership = OutboundOwnership.NativeOwned(
                old.attempt,
                NativeOutboundPhase.FRESH,
            ),
        )
        reusePrefs.updateDeliveryJournal { journal ->
            val owner = checkNotNull(journal.owners[OWNER_A])
            DeliveryJournalMutation(
                journal = journal.copy(
                    owners = journal.owners + (
                        OWNER_A to owner.copy(outboundRows = listOf(replacement))
                    ),
                ),
                result = Unit,
            )
        }
        assertNull(reuseQueue.applyNextTerminal(OWNER_A))
        assertEquals(replacement.identity, reuseQueue.rows(OWNER_A).single().identity)
    }

    @Test
    fun `delivery event emits once and only after terminal commit`() = runTest {
        val harness = ManagerHarness(this)
        harness.manager.login(
            testSessionInfo(username = "alice", jid = OWNER_A),
        )
        runCurrent()

        harness.manager.events.test {
            harness.factory.emitReady()
            runCurrent()
            awaitItem()

            val result = harness.manager.sendDirectReply(
                expectedOwnerBareJid = OWNER_A,
                conversationJid = PEER,
                isGroupchat = false,
                body = "commit before UI",
            )
            val delivery = checkNotNull(result.delivery)
            harness.dataStore.failAllUpdates = true
            harness.factory.emitAcked(delivery.identity.clientStanzaId)
            runCurrent()
            expectNoEvents()

            harness.dataStore.failAllUpdates = false
            advanceTimeBy(250)
            runCurrent()
            assertEquals(XmppEvent.DeliveryAcked(delivery), awaitItem())

            harness.factory.emitAcked(delivery.identity.clientStanzaId)
            runCurrent()
            expectNoEvents()
            cancelAndIgnoreRemainingEvents()
        }
        harness.manager.logout()
    }

    private class ManagerHarness(testScope: TestScope) {
        val dataStore = FailingPreferencesDataStore()
        val prefs = SessionPrefs(dataStore)
        val factory = FakeClientFactory()
        val manager = XmppSessionManager(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )
    }

    private data class SeededRow(
        val attempt: DeliveryAttemptRef,
        val row: QueuedOutboundMessage,
    )

    private suspend fun seedClaimed(
        prefs: SessionPrefs,
        queue: OutboundQueue,
        owner: String,
        body: String,
    ): SeededRow {
        prefs.activateSession(owner, "session-$owner")
        val attempt = queue.beginAttempt(owner).attempt
        val row = (
            queue.enqueueAndClaimAbsoluteHead(
                directReplyDraft(owner, body),
                attempt,
            ) as OutboundQueue.LiveAdmissionResult.Claimed
        ).row
        return SeededRow(attempt, row)
    }

    private fun directReplyDraft(
        owner: String,
        body: String,
    ): QueuedOutboundDraft = QueuedOutboundDraft.create(
        ownerBareJid = owner,
        clientStanzaId = SHARED_STANZA_ID,
        enqueuedAtMillis = 1_000,
        payload = QueuedOutboundPayload(
            target = QueuedOutboundTarget.Chat(PEER),
            content = QueuedOutboundContent(body),
        ),
        source = DeliverySource.DirectReply(PEER, false),
    )

    private companion object {
        const val OWNER_A = "alice@waddle.test"
        const val OWNER_B = "bob@waddle.test"
        const val PEER = "carol@waddle.test"
        const val SHARED_STANZA_ID = "same-stanza-id"
        const val WRONG_ATTEMPT_ID = "00000000-0000-4000-8000-000000000099"
    }
}
