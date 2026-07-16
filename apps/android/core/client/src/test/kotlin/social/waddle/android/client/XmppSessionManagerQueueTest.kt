package social.waddle.android.client

import app.cash.turbine.test
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * Persisted outbound queue behavior through the session manager:
 * enqueue on session-shaped failures, in-order drain on `SessionReady`,
 * drop-oldest at the cap, and permanent-failure drops (web
 * `waddle.chat.outbound-queue` parity).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerQueueTest {
    private class Harness(testScope: TestScope) {
        val factory = FakeClientFactory()
        val network = FakeNetworkSignal()
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
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
        assertTrue("drained entries leave the queue", harness.prefs.outboundQueue.first().isEmpty())

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

        harness.manager.logout()
    }

    @Test
    fun `permanent replay failure drops the entry and surfaces it`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        val queuedSend = harness.manager.sendChatMessage("nobody@waddle.test", "doomed")
        harness.manager.sendChatMessage("alice@waddle.test", "fine")

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
        assertFalse(
            "drained entry leaves the persisted queue",
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
}
