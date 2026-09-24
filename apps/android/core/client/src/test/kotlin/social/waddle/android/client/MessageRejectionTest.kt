package social.waddle.android.client

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
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent

@OptIn(ExperimentalCoroutinesApi::class)
class MessageRejectionTest {
    private class Harness(scope: TestScope) {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val factory = FakeClientFactory()
        val manager = XmppSessionManager(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(scope.testScheduler),
        )
    }

    @Test
    fun `explicit rejection before or after ack removes durable retry and reaches UI`() = runTest {
        for (ackFirst in listOf(false, true)) {
            val h = Harness(this)
            h.manager.login(testSessionInfo())
            runCurrent()
            h.factory.emit(WaddleClientEvent.Connected)
            runCurrent()
            val id = checkNotNull(h.manager.sendChatMessage("alice@waddle.test", "hello").queuedId)
            if (ackFirst) {
                h.factory.emit(WaddleClientEvent.DeliveryAcked(id))
                runCurrent()
            }
            h.manager.events.test {
                h.factory.emit(WaddleClientEvent.MessageRejected(id, "alice@waddle.test/p", "icepuma@waddle.test/p"))
                runCurrent()
                assertEquals(XmppEvent.MessageRejected(id, "alice@waddle.test/p", "icepuma@waddle.test/p"), awaitItem())
                assertTrue(h.prefs.outboundQueue.first().isEmpty())
                assertTrue(h.manager.timelineStore.timeline("alice@waddle.test").value.single().rejected)
                h.factory.emit(WaddleClientEvent.DeliveryAcked(id))
                runCurrent()
                assertEquals(XmppEvent.DeliveryAcked(id), awaitItem())
                assertTrue(h.manager.timelineStore.timeline("alice@waddle.test").value.single().rejected)
            }
            h.factory.emit(WaddleClientEvent.Disconnected)
            runCurrent()
            advanceTimeBy(2_000)
            runCurrent()
            h.factory.emit(WaddleClientEvent.Connected)
            runCurrent()
            assertTrue("fresh stream must not replay a rejected send", h.factory.clients.last().sendCalls.isEmpty())
            h.manager.logout()
        }
    }

    @Test
    fun `forged rejection cannot remove intent or reach app consumers`() = runTest {
        val h = Harness(this)
        h.manager.login(testSessionInfo())
        runCurrent()
        h.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        val id = checkNotNull(h.manager.sendChatMessage("alice@waddle.test", "hello").queuedId)
        h.manager.events.test {
            h.factory.emit(WaddleClientEvent.MessageRejected(id, "eve@waddle.test", null))
            h.factory.emit(WaddleClientEvent.MessageRejected(id, "icepuma@waddle.test", null))
            h.factory.emit(WaddleClientEvent.MessageRejected(id, "alice@waddle.test", "other@waddle.test"))
            h.factory.emit(WaddleClientEvent.MessageRejected("unknown", "alice@waddle.test", null))
            runCurrent()
            expectNoEvents()
            assertEquals(id, h.prefs.outboundQueue.first().single().clientStanzaId)
            assertFalse(h.manager.timelineStore.timeline("alice@waddle.test").value.single().rejected)
        }
        h.manager.logout()
    }

    @Test
    fun `rejection received during send settles after its continuation without retry`() = runTest {
        val h = Harness(this)
        h.manager.login(testSessionInfo())
        runCurrent()
        h.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        val client = h.factory.clients.single()
        val release = CompletableDeferred<Unit>()
        client.sendMessageStall = release
        val sending = async { h.manager.sendChatMessage("alice@waddle.test", "hello") }
        runCurrent()
        val id = h.prefs.outboundQueue.first().single().clientStanzaId
        h.factory.emit(WaddleClientEvent.MessageRejected(id, "alice@waddle.test", null))
        runCurrent()
        assertFalse(h.prefs.outboundQueue.first().isEmpty())
        release.complete(Unit)
        runCurrent()
        assertEquals(id, sending.await().queuedId)
        assertTrue(h.prefs.outboundQueue.first().isEmpty())
        assertTrue(h.manager.timelineStore.timeline("alice@waddle.test").value.single().rejected)
        h.manager.logout()
    }

    @Test
    fun `acknowledged send from previous login cannot be rejected in new login`() = runTest {
        val h = Harness(this)
        h.manager.login(testSessionInfo())
        runCurrent()
        h.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        val id = checkNotNull(h.manager.sendChatMessage("alice@waddle.test", "hello").queuedId)
        h.factory.emit(WaddleClientEvent.DeliveryAcked(id))
        runCurrent()
        h.manager.logout()
        h.manager.login(testSessionInfo())
        runCurrent()
        h.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        h.manager.events.test {
            h.factory.emit(WaddleClientEvent.MessageRejected(id, "alice@waddle.test", null))
            runCurrent()
            expectNoEvents()
        }
        h.manager.logout()
    }

    @Test
    fun `address matching preserves account room occupant and service identity`() {
        val account = queued("alice@remote.test")
        assertTrue(rejection("alice@remote.test/phone").matches(account))
        assertTrue(rejection("waddle.test").matches(account))
        assertTrue(rejection("remote.test").matches(account))
        assertFalse(rejection("remote.test/forged").matches(account))
        assertFalse(rejection("icepuma@waddle.test").matches(account))
        assertTrue(rejection("icepuma@waddle.test").matches(queued("icepuma@waddle.test")))
        val room = queued("chat@muc.waddle.test", groupchat = true)
        assertTrue(rejection("chat@muc.waddle.test").matches(room))
        assertFalse(rejection("chat@muc.waddle.test/eve").matches(room))
        val occupant = queued("chat@muc.waddle.test/Bob")
        assertTrue(rejection("chat@muc.waddle.test/Bob").matches(occupant))
        assertTrue(rejection("chat@muc.waddle.test").matches(occupant))
        assertFalse(rejection("chat@muc.waddle.test/bob").matches(occupant))
        assertFalse(rejection("chat@muc.waddle.test/eve").matches(occupant))
    }

    private fun queued(peer: String, groupchat: Boolean = false) = QueuedOutboundMessage(
        ownerBareJid = "icepuma@waddle.test",
        conversationJid = peer,
        isGroupchat = groupchat,
        body = "hello",
        clientStanzaId = "send-1",
        enqueuedAtMillis = 0,
    )

    private fun rejection(from: String) = XmppEvent.MessageRejected("send-1", from, "icepuma@waddle.test/p")
}
