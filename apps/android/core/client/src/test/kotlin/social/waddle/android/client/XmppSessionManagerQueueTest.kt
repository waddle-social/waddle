package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleSendMessageOutcome

@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerQueueTest {
    @Test
    fun `native pull waits for resume snapshot durability`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val factory = FakeClientFactory()
        val manager = manager(prefs, factory)
        manager.login(testSessionInfo(jid = OWNER_A, username = "alice"))
        runCurrent()
        val client = factory.clients.single()

        factory.emitReady()
        runCurrent()
        assertEquals(2, client.nextEventCalls.get())

        store.failAllUpdates = true
        factory.emitResumeStateChanged(testResumeState())
        runCurrent()
        assertEquals(
            "the pull lane must remain parked at the failing persistence barrier",
            2,
            client.nextEventCalls.get(),
        )

        store.failAllUpdates = false
        advanceTimeBy(250)
        runCurrent()
        assertEquals(3, client.nextEventCalls.get())
        assertEquals(
            testResumeState().previd,
            prefs.deliveryJournal.first().owners[OWNER_A]?.sm?.snapshot?.previd,
        )
        manager.logout()
    }

    @Test
    fun `native pull waits for terminal ack durability and application`() = runTest {
        val store = FailingPreferencesDataStore()
        val prefs = SessionPrefs(store)
        val factory = FakeClientFactory()
        val manager = manager(prefs, factory)
        manager.login(testSessionInfo(jid = OWNER_A, username = "alice"))
        runCurrent()
        factory.emitReady()
        runCurrent()
        val client = factory.clients.single()
        val sent = manager.sendChatMessage("carol@waddle.test", "hello")
        val stanzaId = checkNotNull(sent.delivery).identity.clientStanzaId
        assertEquals(2, client.nextEventCalls.get())

        store.failAllUpdates = true
        factory.emitAcked(stanzaId)
        runCurrent()
        assertEquals(
            "the pull lane must not request another event before ACK commit/apply",
            2,
            client.nextEventCalls.get(),
        )
        assertEquals(1, prefs.deliveryJournal.first().owners[OWNER_A]?.outboundRows?.size)

        store.failAllUpdates = false
        advanceTimeBy(250)
        runCurrent()
        assertEquals(3, client.nextEventCalls.get())
        assertTrue(prefs.deliveryJournal.first().owners[OWNER_A]?.outboundRows.orEmpty().isEmpty())
        manager.logout()
    }

    @Test
    fun `direct reply is owner fenced and preserves durable source provenance`() = runTest {
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val manager = manager(prefs, FakeClientFactory())
        manager.login(testSessionInfo(jid = OWNER_A, username = "alice"))
        runCurrent()

        val stale = manager.sendDirectReply(
            expectedOwnerBareJid = OWNER_B,
            conversationJid = "carol@waddle.test",
            isGroupchat = false,
            body = "wrong account",
        )
        assertEquals(WaddleSendMessageOutcome.Error, stale.outcome)
        assertEquals(0, prefs.deliveryJournal.first().owners[OWNER_A]?.outboundRows?.size)

        val accepted = manager.sendDirectReply(
            expectedOwnerBareJid = OWNER_A,
            conversationJid = "carol@waddle.test",
            isGroupchat = false,
            body = "hello",
        )
        assertEquals(WaddleSendMessageOutcome.NotConnected, accepted.outcome)
        assertTrue(accepted.queued)
        val delivery = checkNotNull(accepted.delivery)
        assertEquals(OWNER_A, delivery.identity.ownerBareJid)
        assertEquals(
            DeliverySource.DirectReply("carol@waddle.test", false),
            delivery.source,
        )

        manager.login(testSessionInfo(sessionId = "sess-b", username = "bob", jid = OWNER_B))
        runCurrent()
        val oldPendingIntent = manager.sendDirectReply(
            expectedOwnerBareJid = OWNER_A,
            conversationJid = "carol@waddle.test",
            isGroupchat = false,
            body = "stale action",
        )
        assertEquals(WaddleSendMessageOutcome.Error, oldPendingIntent.outcome)
        assertTrue(
            prefs.deliveryJournal.first().owners[OWNER_B]?.outboundRows?.isEmpty() != false,
        )
        manager.logout()
    }

    private fun TestScope.manager(
        prefs: SessionPrefs,
        factory: FakeClientFactory,
    ): XmppSessionManager = XmppSessionManager(
        sessionPrefs = prefs,
        clientFactory = factory,
        networkSignal = FakeNetworkSignal(),
        userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
        dispatcher = StandardTestDispatcher(testScheduler),
    )

    private companion object {
        const val OWNER_A = "alice@waddle.test"
        const val OWNER_B = "bob@waddle.test"
    }
}
