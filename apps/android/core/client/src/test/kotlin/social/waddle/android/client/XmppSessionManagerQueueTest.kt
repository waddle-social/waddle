package social.waddle.android.client

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import app.cash.turbine.test
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.prefs.toSnapshot
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * Durable outbound journal behavior through the session manager: exact
 * generation ownership, in-order Ready-row drain, XEP-0198 reconciliation,
 * fail-closed capacity, and permanent-failure drops.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerQueueTest {
    private class Harness(
        testScope: TestScope,
        dataStore: DataStore<Preferences> = InMemoryPreferencesDataStore(),
    ) {
        val factory = FakeClientFactory()
        val network = FakeNetworkSignal()
        val prefs = SessionPrefs(dataStore)
        val manager = XmppSessionManager(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = network,
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )
    }

    @Test
    fun `offline send returns the raw outcome and persists the message`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        val result = harness.manager.sendChatMessage("alice@waddle.test", "hi from the subway")

        assertEquals(WaddleSendMessageOutcome.NotConnected, result.outcome)
        assertNotNull("queued id hands the replay identity to the caller", result.queuedId)
        val queued = harness.prefs.outboundQueue.first().single()
        assertEquals("alice@waddle.test", queued.conversationJid)
        assertEquals(false, queued.isGroupchat)
        assertEquals("hi from the subway", queued.body)
        assertEquals(result.queuedId, queued.clientStanzaId)
        assertEquals(OutboundOwnership.Ready, queued.ownership)

        harness.manager.logout()
    }

    @Test
    fun `queue drains in order on session ready and replays the persisted stanza id`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        val first = harness.manager.sendChatMessage("alice@waddle.test", "one")
        val second = harness.manager.sendGroupchatMessage("general@muc.waddle.test", "two")
        assertEquals(2, harness.prefs.outboundQueue.first().size)

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.single()
        assertEquals(
            listOf("alice@waddle.test" to "one", "general@muc.waddle.test" to "two"),
            client.sendCalls,
        )
        assertEquals(
            "replay reuses the persisted client stanza ids",
            listOf(first.queuedId, second.queuedId),
            client.sendOptions.map { it?.stanzaId },
        )
        assertTrue(
            "sent entries remain native-owned until their exact ack",
            harness.prefs.outboundQueue.first().all { it.ownership is OutboundOwnership.NativeOwned },
        )

        harness.factory.emit(WaddleClientEvent.DeliveryAcked(first.queuedId!!))
        harness.factory.emit(WaddleClientEvent.DeliveryAcked(second.queuedId!!))
        runCurrent()
        assertTrue("acked entries leave the durable queue", harness.prefs.outboundQueue.first().isEmpty())

        harness.manager.logout()
    }

    @Test
    fun `drain stops on a session-shaped failure and keeps the remainder`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.manager.sendChatMessage("alice@waddle.test", "one")
        harness.manager.sendChatMessage("alice@waddle.test", "two")

        val client = harness.factory.clients.single()
        client.sendOutcome = WaddleSendMessageOutcome.NotConnected
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertEquals("drain stops after the first failed replay", 1, client.sendCalls.size)
        assertEquals(
            "both messages stay queued for the next session",
            listOf("one", "two"),
            harness.prefs.outboundQueue.first().map { it.body },
        )
        assertTrue(
            "failed head and untouched tail are replayable",
            harness.prefs.outboundQueue.first().all { it.ownership == OutboundOwnership.Ready },
        )

        harness.manager.logout()
    }

    @Test
    fun `permanent replay failure drops the entry and surfaces it`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        val queuedSend = harness.manager.sendChatMessage("nobody@waddle.test", "doomed")
        val acceptedSend = harness.manager.sendChatMessage("alice@waddle.test", "fine")

        val client = harness.factory.clients.single()
        client.sendOutcomes += WaddleSendMessageOutcome.InvalidRecipient
        client.sendOutcomes += WaddleSendMessageOutcome.Sent("ok-2")

        harness.manager.events.test {
            harness.factory.emit(WaddleClientEvent.Connected)
            runCurrent()

            assertEquals(XmppEvent.SessionReady, awaitItem())
            assertEquals(
                "drop flips the optimistic row to failed",
                XmppEvent.DeliveryFailed(queuedSend.queuedId!!),
                awaitItem(),
            )
            val error = awaitItem()
            assertTrue("drop is surfaced as a diagnostic: $error", error is XmppEvent.Error)

            assertEquals("the drain continues past the drop", 2, client.sendCalls.size)
            assertEquals(
                "the accepted replay remains durable until native ack",
                listOf(acceptedSend.queuedId),
                harness.prefs.outboundQueue.first().map { it.clientStanzaId },
            )

            harness.factory.emit(WaddleClientEvent.DeliveryAcked(acceptedSend.queuedId!!))
            runCurrent()
            assertEquals(XmppEvent.DeliveryAcked(acceptedSend.queuedId), awaitItem())
            assertTrue(harness.prefs.outboundQueue.first().isEmpty())
        }

        harness.manager.logout()
    }

    @Test
    fun `queue caps at capacity by evicting the oldest and surfacing it`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        val firstResult = harness.manager.sendChatMessage("alice@waddle.test", "m-0")
        repeat(OutboundQueue.DEFAULT_CAPACITY - 1) { index ->
            harness.manager.sendChatMessage("alice@waddle.test", "m-${index + 1}")
        }
        assertEquals(OutboundQueue.DEFAULT_CAPACITY, harness.prefs.outboundQueue.first().size)

        harness.manager.events.test {
            val overflow = harness.manager.sendChatMessage("alice@waddle.test", "m-overflow")
            assertTrue(overflow.queued)

            assertEquals(
                "evicted oldest is reported undeliverable",
                XmppEvent.DeliveryFailed(firstResult.queuedId!!),
                awaitItem(),
            )
            assertTrue(awaitItem() is XmppEvent.Error)
        }

        val queue = harness.prefs.outboundQueue.first()
        assertEquals(OutboundQueue.DEFAULT_CAPACITY, queue.size)
        assertEquals("oldest evicted", "m-1", queue.first().body)
        assertEquals("m-overflow", queue.last().body)

        harness.manager.logout()
    }

    @Test
    fun `a queue persisted by a previous process drains on the next session`() = runTest {
        val harness = Harness(this)
        // Simulate a prior process that enqueued offline and then died:
        // login() never ran in THIS manager, only the prefs blob exists.
        harness.prefs.updateOutboundQueue {
            listOf(
                QueuedOutboundMessage(
                    ownerBareJid = "icepuma@waddle.test",
                    conversationJid = "alice@waddle.test",
                    isGroupchat = false,
                    body = "written before the crash",
                    clientStanzaId = "q-persisted",
                    enqueuedAtMillis = 0L,
                ),
            )
        }

        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.single()
        assertEquals(listOf("alice@waddle.test" to "written before the crash"), client.sendCalls)
        assertEquals("q-persisted", client.sendOptions.single()?.stanzaId)
        assertTrue(
            "accepted replay remains native-owned until its exact ack",
            harness.prefs.outboundQueue.first().single().ownership is OutboundOwnership.NativeOwned,
        )

        harness.factory.emit(WaddleClientEvent.DeliveryAcked("q-persisted"))
        runCurrent()
        assertFalse(
            "acked entry leaves the persisted queue",
            harness.prefs.outboundQueue.first().any { it.clientStanzaId == "q-persisted" },
        )

        harness.manager.logout()
    }

    @Test
    fun `another account's persisted queue entries are pruned, never replayed`() = runTest {
        val harness = Harness(this)
        harness.prefs.updateOutboundQueue {
            listOf(
                QueuedOutboundMessage(
                    ownerBareJid = "someone-else@waddle.test",
                    conversationJid = "peer@waddle.test",
                    isGroupchat = false,
                    body = "secret written by the previous account",
                    clientStanzaId = "q-foreign",
                    enqueuedAtMillis = 0L,
                ),
            )
        }

        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        // Cross-account misdelivery guard: the foreign entry must be
        // dropped before the drain, not sent under this account.
        val client = harness.factory.clients.last()
        assertTrue(
            "foreign queued message must never be sent",
            client.sendCalls.none { it.second.contains("secret") },
        )
        assertEquals(emptyList<QueuedOutboundMessage>(), harness.prefs.outboundQueue.first())

        harness.manager.logout()
    }

    @Test
    fun `live send is durably native-owned before FFI and retained until ack`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.single()
        var rowObservedInsideFfi: QueuedOutboundMessage? = null
        client.beforeSendReturns = {
            rowObservedInsideFfi = harness.prefs.outboundQueue.first().single()
        }

        val result = harness.manager.sendChatMessage("alice@waddle.test", "persist first")

        assertTrue(result.outcome is WaddleSendMessageOutcome.Sent)
        val stanzaId = client.sendOptions.single()?.stanzaId!!
        val observedOwnership = rowObservedInsideFfi?.ownership as? OutboundOwnership.NativeOwned
        assertNotNull("the row must be committed before entering FFI", observedOwnership)
        assertEquals(NativeOutboundPhase.FRESH, observedOwnership?.phase)
        assertEquals(
            "successful socket submission is not durable completion",
            stanzaId,
            harness.prefs.outboundQueue.first().single().clientStanzaId,
        )

        harness.factory.emit(WaddleClientEvent.DeliveryAcked(stanzaId))
        runCurrent()
        assertTrue(harness.prefs.outboundQueue.first().isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `persistence failure prevents any FFI send`() = runTest {
        val dataStore = FailingPreferencesDataStore()
        val harness = Harness(this, dataStore)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        dataStore.failNextUpdate = true
        val result = harness.manager.sendChatMessage("alice@waddle.test", "must not escape")

        assertEquals(WaddleSendMessageOutcome.Error, result.outcome)
        assertTrue(
            "wire call is forbidden when the durable insert fails",
            harness.factory.clients.single().sendCalls.isEmpty(),
        )
        assertTrue(harness.prefs.outboundQueue.first().isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `ack deletion commits before the ack reaches observers`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        harness.manager.sendChatMessage("alice@waddle.test", "commit the ack")
        val stanzaId = harness.factory.clients.single().sendOptions.single()?.stanzaId!!

        harness.manager.events.test {
            harness.factory.emit(WaddleClientEvent.DeliveryAcked(stanzaId))
            assertEquals(XmppEvent.DeliveryAcked(stanzaId), awaitItem())
            assertTrue(
                "an observer must never see the ack before its durable delete",
                harness.prefs.outboundQueue.first().isEmpty(),
            )
        }
        harness.manager.logout()
    }

    @Test
    fun `failed ack deletion suppresses the router ack and retains the row`() = runTest {
        val dataStore = FailingPreferencesDataStore()
        val harness = Harness(this, dataStore)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        harness.manager.sendChatMessage("alice@waddle.test", "retain on failure")
        val stanzaId = harness.factory.clients.single().sendOptions.single()?.stanzaId!!

        harness.manager.events.test {
            dataStore.failNextUpdate = true
            harness.factory.emit(WaddleClientEvent.DeliveryAcked(stanzaId))
            runCurrent()

            expectNoEvents()
            assertEquals(
                "a failed durable delete keeps the native-owned row",
                stanzaId,
                harness.prefs.outboundQueue.first().single().clientStanzaId,
            )
        }
        harness.manager.logout()
    }

    @Test
    fun `restart releases stale claims without a snapshot and transfers matching snapshot rows`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val queueBeforeCrash = OutboundQueue(prefs)
        val row = QueuedOutboundMessage(
            ownerBareJid = "icepuma@waddle.test",
            conversationJid = "alice@waddle.test",
            isGroupchat = false,
            body = "survive process death",
            clientStanzaId = "q-crash",
            enqueuedAtMillis = 1L,
        )
        assertTrue(queueBeforeCrash.enqueue(row).stored)
        assertNotNull(
            queueBeforeCrash.claimReady(
                row.clientStanzaId,
                OutboundOwnership.NativeOwned(7L, NativeOutboundPhase.FRESH),
            ),
        )

        val restartedWithoutSnapshot = OutboundQueue(prefs)
        restartedWithoutSnapshot.reconcileAttempt(row.ownerBareJid, 8L, emptySet())
        assertEquals(
            "a dead generation with no SM evidence becomes replayable",
            OutboundOwnership.Ready,
            prefs.outboundQueue.first().single().ownership,
        )

        assertNotNull(
            restartedWithoutSnapshot.claimReady(
                row.clientStanzaId,
                OutboundOwnership.NativeOwned(8L, NativeOutboundPhase.FRESH),
            ),
        )
        val restartedWithSnapshot = OutboundQueue(prefs)
        restartedWithSnapshot.reconcileAttempt(row.ownerBareJid, 9L, setOf(row.clientStanzaId))
        assertEquals(
            OutboundOwnership.NativeOwned(9L, NativeOutboundPhase.RESUME),
            prefs.outboundQueue.first().single().ownership,
        )
    }

    @Test
    fun `stale generation ack and failure cannot mutate a replacement claim`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val queue = OutboundQueue(prefs)
        val row = QueuedOutboundMessage(
            ownerBareJid = "icepuma@waddle.test",
            conversationJid = "alice@waddle.test",
            isGroupchat = false,
            body = "fenced",
            clientStanzaId = "q-fenced",
            enqueuedAtMillis = 1L,
        )
        queue.enqueue(row)
        queue.claimReady(
            row.clientStanzaId,
            OutboundOwnership.NativeOwned(11L, NativeOutboundPhase.RESUME),
        )

        assertFalse(queue.acknowledge(row.clientStanzaId, 10L))
        assertEquals(OutboundQueue.FailureResolution.STALE, queue.failNative(row.clientStanzaId, 10L))
        assertEquals(
            OutboundOwnership.NativeOwned(11L, NativeOutboundPhase.RESUME),
            prefs.outboundQueue.first().single().ownership,
        )
        assertTrue(queue.acknowledge(row.clientStanzaId, 11L))
        assertTrue(prefs.outboundQueue.first().isEmpty())
    }

    @Test
    fun `first resume failure transfers silently and second native failure releases`() = runTest {
        val harness = Harness(this)
        val stanzaId = "q-resume-fallback"
        harness.prefs.updateOutboundQueue {
            listOf(
                QueuedOutboundMessage(
                    ownerBareJid = "icepuma@waddle.test",
                    conversationJid = "alice@waddle.test",
                    isGroupchat = false,
                    body = "resume me",
                    clientStanzaId = stanzaId,
                    enqueuedAtMillis = 1L,
                ),
            )
        }
        harness.prefs.setSmResume(testResumeState(queuedStanzaId = stanzaId).toSnapshot())
        harness.manager.login(testSessionInfo())
        runCurrent()
        assertEquals(
            OutboundOwnership.NativeOwned(1L, NativeOutboundPhase.RESUME),
            harness.prefs.outboundQueue.first().single().ownership,
        )

        harness.manager.events.test {
            harness.factory.emit(WaddleClientEvent.DeliveryFailed(stanzaId))
            runCurrent()
            expectNoEvents()
            assertEquals(
                OutboundOwnership.NativeOwned(1L, NativeOutboundPhase.FALLBACK),
                harness.prefs.outboundQueue.first().single().ownership,
            )

            harness.factory.emit(WaddleClientEvent.Connected)
            assertEquals(XmppEvent.SessionReady, awaitItem())

            harness.factory.emit(WaddleClientEvent.DeliveryFailed(stanzaId))
            assertEquals(XmppEvent.DeliveryFailed(stanzaId), awaitItem())
            assertEquals(OutboundOwnership.Ready, harness.prefs.outboundQueue.first().single().ownership)
        }
        harness.manager.logout()
    }

    @Test
    fun `capacity never evicts native-owned rows and rejects when every row is owned`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val queue = OutboundQueue(prefs, capacity = 2)
        fun row(id: String) = QueuedOutboundMessage(
            ownerBareJid = "icepuma@waddle.test",
            conversationJid = "alice@waddle.test",
            isGroupchat = false,
            body = id,
            clientStanzaId = id,
            enqueuedAtMillis = 1L,
        )

        queue.enqueue(row("native-head"))
        queue.claimReady(
            "native-head",
            OutboundOwnership.NativeOwned(1L, NativeOutboundPhase.FRESH),
        )
        queue.enqueue(row("ready-oldest"))
        val replacement = queue.enqueue(row("ready-new"))
        assertEquals("ready-oldest", replacement.evicted?.clientStanzaId)
        assertEquals(listOf("native-head", "ready-new"), prefs.outboundQueue.first().map { it.clientStanzaId })

        queue.claimReady(
            "ready-new",
            OutboundOwnership.NativeOwned(1L, NativeOutboundPhase.FRESH),
        )
        val rejected = queue.enqueue(row("must-not-evict-native"))
        assertFalse(rejected.stored)
        assertEquals(listOf("native-head", "ready-new"), prefs.outboundQueue.first().map { it.clientStanzaId })
    }

    @Test
    fun `attempt replacement waits until the old generation exits its FFI send`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val enteredFfi = CompletableDeferred<Unit>()
        val releaseFfi = CompletableDeferred<Unit>()
        val firstClient = harness.factory.clients.single()
        firstClient.beforeSendReturns = {
            enteredFfi.complete(Unit)
            releaseFfi.await()
        }
        val send = async {
            harness.manager.sendChatMessage("alice@waddle.test", "old generation in flight")
        }
        runCurrent()
        assertTrue(enteredFfi.isCompleted)

        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(1_000L)
        runCurrent()
        assertEquals(
            "reconciliation cannot replace ownership while the old write is in flight",
            1,
            harness.factory.clients.size,
        )

        releaseFfi.complete(Unit)
        assertTrue(send.await().outcome is WaddleSendMessageOutcome.Sent)
        runCurrent()
        advanceTimeBy(1_000L)
        runCurrent()
        assertEquals(2, harness.factory.clients.size)

        val stanzaId = firstClient.sendOptions.single()?.stanzaId!!
        assertEquals(
            OutboundOwnership.Ready,
            harness.prefs.outboundQueue.first().single().ownership,
        )
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertEquals(stanzaId, harness.factory.clients[1].sendOptions.single()?.stanzaId)
        harness.factory.emit(WaddleClientEvent.DeliveryAcked(stanzaId))
        runCurrent()
        assertTrue(harness.prefs.outboundQueue.first().isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `session-ready drain cannot steal a concurrently persisted live send`() = runTest {
        val harness = Harness(this)
        harness.prefs.updateOutboundQueue {
            listOf(
                QueuedOutboundMessage(
                    ownerBareJid = "icepuma@waddle.test",
                    conversationJid = "alice@waddle.test",
                    isGroupchat = false,
                    body = "older ready row",
                    clientStanzaId = "q-drain-head",
                    enqueuedAtMillis = 1L,
                ),
            )
        }
        harness.manager.login(testSessionInfo())
        runCurrent()
        val client = harness.factory.clients.single()
        val drainEnteredFfi = CompletableDeferred<Unit>()
        val releaseDrain = CompletableDeferred<Unit>()
        var blockFirstSend = true
        client.beforeSendReturns = {
            if (blockFirstSend) {
                blockFirstSend = false
                drainEnteredFfi.complete(Unit)
                releaseDrain.await()
            }
        }

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertTrue(drainEnteredFfi.isCompleted)
        val liveSend = async {
            harness.manager.sendChatMessage("bob@waddle.test", "new live row")
        }
        runCurrent()

        val rowsWhileBlocked = harness.prefs.outboundQueue.first()
        assertEquals(2, rowsWhileBlocked.size)
        assertTrue(rowsWhileBlocked.all { it.ownership is OutboundOwnership.NativeOwned })
        assertEquals(
            "only the drain head may enter FFI while the live row is atomically claimed",
            1,
            client.sendCalls.size,
        )

        releaseDrain.complete(Unit)
        val liveResult = liveSend.await()
        runCurrent()
        assertTrue(liveResult.outcome is WaddleSendMessageOutcome.Sent)
        assertEquals("each durable row reaches FFI exactly once", 2, client.sendCalls.size)
        assertEquals(
            2,
            client.sendOptions.mapNotNull { it?.stanzaId }.distinct().size,
        )

        for (stanzaId in client.sendOptions.mapNotNull { it?.stanzaId }) {
            harness.factory.emit(WaddleClientEvent.DeliveryAcked(stanzaId))
        }
        runCurrent()
        assertTrue(harness.prefs.outboundQueue.first().isEmpty())
        harness.manager.logout()
    }
}
