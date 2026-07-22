package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent

/**
 * XEP-0430 inbox sync through the session manager: session-ready
 * hydration, server-authoritative unread over the local overlay, DM
 * ordering from inbox recency, and mark-read co-firing on the
 * displayed path.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerInboxTest {
    private class Harness(testScope: TestScope) {
        val factory = FakeClientFactory()
        val network = FakeNetworkSignal()
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val userPrefs = UserPrefs(InMemoryPreferencesDataStore())
        val manager = XmppSessionManager(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = network,
            userPrefs = userPrefs,
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )

        suspend fun loginReady(scope: TestScope) {
            manager.login(testSessionInfo())
            scope.runCurrent()
            factory.emit(WaddleClientEvent.Connected)
            scope.runCurrent()
        }

        val client get() = factory.clients.last()
    }

    // ── Session-ready hydration ─────────────────────────────────────────

    @Test
    fun `session ready hydrates the inbox without message bodies`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        assertEquals(listOf(false to true), harness.client.inbox.fetchInboxCalls.toList())

        harness.manager.logout()
    }

    @Test
    fun `hydrated counts land as absolute unread state`() = runTest {
        val harness = Harness(this)
        harness.factory.onCreate = { client ->
            client.inbox.inboxResult = testInboxResult(
                conversations = listOf(
                    testInboxEntry(partner = "alice@waddle.test", lastStanzaId = "s-a", unread = 3u),
                    testInboxEntry(
                        partner = "room@muc.waddle.test",
                        kind = "muc",
                        lastStanzaId = "s-r",
                        unread = 2u,
                    ),
                ),
            )
        }
        harness.loginReady(this)

        assertEquals(3, harness.manager.unreadStore.counts.value["alice@waddle.test"])
        assertEquals(2, harness.manager.unreadStore.counts.value["room@muc.waddle.test"])
        assertEquals(3, harness.manager.inboxStore.direct.value["alice@waddle.test"]?.unread)
        assertEquals(2, harness.manager.inboxStore.muc.value["room@muc.waddle.test"]?.unread)

        harness.manager.logout()
    }

    @Test
    fun `a failed hydrate keeps the pipeline going`() = runTest {
        val harness = Harness(this)
        harness.factory.onCreate = { client ->
            client.inbox.fetchInboxFailure = IllegalStateException("inbox down")
        }
        harness.loginReady(this)

        // The MDS bootstrap after the failed hydrate still ran.
        assertEquals(1, harness.client.mdsSubscribeCalls)

        harness.manager.logout()
    }

    // ── DM ordering from inbox recency ──────────────────────────────────

    @Test
    fun `dm list orders by inbox last-updated recency`() = runTest {
        val harness = Harness(this)
        harness.prefs.setLastSeen("stale@waddle.test", "2026-07-01T10:00:00Z")
        harness.factory.onCreate = { client ->
            client.inbox.inboxResult = testInboxResult(
                conversations = listOf(
                    testInboxEntry(partner = "old@waddle.test", lastStanzaId = "s-o", lastUpdated = 1_000L),
                    testInboxEntry(partner = "new@waddle.test", lastStanzaId = "s-n", lastUpdated = 3_000L),
                    testInboxEntry(partner = "mid@waddle.test", lastStanzaId = "s-m", lastUpdated = 2_000L),
                ),
            )
        }
        harness.loginReady(this)

        assertEquals(
            listOf("new@waddle.test", "mid@waddle.test", "old@waddle.test", "stale@waddle.test"),
            harness.manager.dmStore.peers.value,
        )

        harness.manager.logout()
    }

    // ── Push reconciliation vs the local overlay ────────────────────────

    @Test
    fun `a push replaces the local unread overlay`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(id = "m1", stanzaId = "s1", from = "alice@waddle.test", to = "icepuma@waddle.test"),
            ),
        )
        runCurrent()
        assertEquals(1, harness.manager.unreadStore.counts.value["alice@waddle.test"])

        harness.factory.emit(
            WaddleClientEvent.InboxPush(
                testInboxEntry(partner = "alice@waddle.test", lastStanzaId = "s1", unread = 5u),
            ),
        )
        runCurrent()

        assertEquals(5, harness.manager.unreadStore.counts.value["alice@waddle.test"])

        harness.manager.logout()
    }

    @Test
    fun `a live message already accounted by a push does not double count`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        // Server push lands FIRST, naming s1 as the newest message.
        harness.factory.emit(
            WaddleClientEvent.InboxPush(
                testInboxEntry(partner = "alice@waddle.test", lastStanzaId = "s1", unread = 1u),
            ),
        )
        runCurrent()
        assertEquals(1, harness.manager.unreadStore.counts.value["alice@waddle.test"])

        // The same message then routes live: the push's absolute count
        // already covered it.
        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(id = "m1", stanzaId = "s1", from = "alice@waddle.test", to = "icepuma@waddle.test"),
            ),
        )
        runCurrent()

        assertEquals(1, harness.manager.unreadStore.counts.value["alice@waddle.test"])

        harness.manager.logout()
    }

    @Test
    fun `a message the inbox has not accounted still increments between pushes`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.InboxPush(
                testInboxEntry(partner = "alice@waddle.test", lastStanzaId = "s1", unread = 1u),
            ),
        )
        runCurrent()

        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(id = "m2", stanzaId = "s2", from = "alice@waddle.test", to = "icepuma@waddle.test"),
            ),
        )
        runCurrent()

        assertEquals(2, harness.manager.unreadStore.counts.value["alice@waddle.test"])

        harness.manager.logout()
    }

    @Test
    fun `a push never stamps over the on-screen conversation`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.manager.unreadStore.setActiveConversation("alice@waddle.test")

        harness.factory.emit(
            WaddleClientEvent.InboxPush(
                testInboxEntry(partner = "alice@waddle.test", lastStanzaId = "s1", unread = 5u),
            ),
        )
        runCurrent()

        assertNull(harness.manager.unreadStore.counts.value["alice@waddle.test"])
        // The inbox snapshot itself still reconciles.
        assertEquals(5, harness.manager.inboxStore.direct.value["alice@waddle.test"]?.unread)

        harness.manager.logout()
    }

    @Test
    fun `a stale push cannot regress a newer one`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.InboxPush(
                testInboxEntry(partner = "alice@waddle.test", lastStanzaId = "s2", lastUpdated = 2_000L, unread = 0u),
            ),
        )
        runCurrent()

        harness.factory.emit(
            WaddleClientEvent.InboxPush(
                testInboxEntry(partner = "alice@waddle.test", lastStanzaId = "s1", lastUpdated = 1_000L, unread = 4u),
            ),
        )
        runCurrent()

        assertNull(harness.manager.unreadStore.counts.value["alice@waddle.test"])

        harness.manager.logout()
    }

    // ── Mark-read co-firing ─────────────────────────────────────────────

    @Test
    fun `marking a conversation displayed co-fires the inbox mark-read`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(
                    id = "id-s1",
                    stanzaId = "s1",
                    stanzaIdBy = "room@muc.waddle.test",
                    from = "room@muc.waddle.test/alice",
                    to = null,
                    messageType = "groupchat",
                    isMuc = true,
                ),
            ),
        )
        runCurrent()

        harness.manager.markConversationDisplayed("room@muc.waddle.test", isGroupchat = true)

        assertEquals(
            listOf("room@muc.waddle.test" to null),
            harness.client.inbox.markInboxReadCalls.toList(),
        )

        harness.manager.logout()
    }

    @Test
    fun `the displayed mark-read arms the read-clear barrier against a racing push`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.InboxPush(
                testInboxEntry(partner = "alice@waddle.test", lastStanzaId = "s1", lastUpdated = 1_000L, unread = 2u),
            ),
        )
        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(
                    id = "m1",
                    stanzaId = "s1",
                    from = "alice@waddle.test",
                    to = "icepuma@waddle.test",
                    displayedMarkerRequested = true,
                ),
            ),
        )
        runCurrent()

        harness.manager.markConversationDisplayed("alice@waddle.test", isGroupchat = false)
        // A stale server push still naming the read stanza races in.
        harness.factory.emit(
            WaddleClientEvent.InboxPush(
                testInboxEntry(partner = "alice@waddle.test", lastStanzaId = "s1", lastUpdated = 2_000L, unread = 2u),
            ),
        )
        runCurrent()

        assertNull(harness.manager.unreadStore.counts.value["alice@waddle.test"])
        assertEquals(0, harness.manager.inboxStore.direct.value["alice@waddle.test"]?.unread)

        harness.manager.logout()
    }

    @Test
    fun `the mark-inbox-read verb reaches the client with the thread id`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        harness.manager.markInboxRead("room@muc.waddle.test", threadId = "t-1")

        assertEquals(
            listOf("room@muc.waddle.test" to "t-1"),
            harness.client.inbox.markInboxReadCalls.toList(),
        )

        harness.manager.logout()
    }

    @Test
    fun `logout wipes the inbox store`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.InboxPush(
                testInboxEntry(partner = "alice@waddle.test", lastStanzaId = "s1", unread = 5u),
            ),
        )
        runCurrent()

        harness.manager.logout()

        assertTrue(harness.manager.inboxStore.direct.value.isEmpty())
    }
}
