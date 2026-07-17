package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleMdsDisplayedEntry

/**
 * XEP-0333 displayed dispatch + XEP-0490 MDS read sync through the
 * session manager.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerReadStateTest {
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
            factory.emitReady()
            scope.runCurrent()
        }

        val client get() = factory.clients.last()
    }

    private fun mucMessage(stanzaId: String, nick: String = "alice", body: String = "hi") = testMessage(
        id = "id-$stanzaId",
        stanzaId = stanzaId,
        stanzaIdBy = "room@muc.waddle.test",
        from = "room@muc.waddle.test/$nick",
        to = null,
        messageType = "groupchat",
        isMuc = true,
        body = body,
    )

    @Test
    fun `marking a muc conversation displayed sends the marker and publishes mds`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s1")))
        runCurrent()

        harness.manager.markConversationDisplayed("room@muc.waddle.test", isGroupchat = true)

        assertEquals(
            listOf(Triple("room@muc.waddle.test", "s1", true)),
            harness.client.displayedCalls,
        )
        assertEquals(
            listOf(Triple("room@muc.waddle.test", "s1", "room@muc.waddle.test")),
            harness.client.mdsPublishCalls,
        )
        assertNull(harness.manager.unreadStore.counts.value["room@muc.waddle.test"])

        // Second call for the same newest row is a no-op.
        harness.manager.markConversationDisplayed("room@muc.waddle.test", isGroupchat = true)
        assertEquals(1, harness.client.displayedCalls.size)
        assertEquals(1, harness.client.mdsPublishCalls.size)

        harness.manager.logout()
    }

    @Test
    fun `notification mark read is atomically fenced to its account`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s1")))
        runCurrent()

        assertFalse(
            harness.manager.markConversationDisplayedForOwner(
                expectedOwnerBareJid = "bob@waddle.test",
                conversationJid = "room@muc.waddle.test",
                isGroupchat = true,
            ),
        )
        assertTrue(harness.client.displayedCalls.isEmpty())
        assertEquals(1, harness.manager.unreadStore.counts.value["room@muc.waddle.test"])

        assertTrue(
            harness.manager.markConversationDisplayedForOwner(
                expectedOwnerBareJid = "icepuma@waddle.test",
                conversationJid = "room@muc.waddle.test",
                isGroupchat = true,
            ),
        )
        assertEquals(1, harness.client.displayedCalls.size)
        harness.manager.logout()
    }

    @Test
    fun `read receipts off suppresses the marker but still publishes mds`() = runTest {
        val harness = Harness(this)
        harness.userPrefs.setReadReceiptsEnabled(false)
        harness.loginReady(this)
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s1")))
        runCurrent()

        harness.manager.markConversationDisplayed("room@muc.waddle.test", isGroupchat = true)

        assertTrue(harness.client.displayedCalls.isEmpty())
        assertEquals(1, harness.client.mdsPublishCalls.size)
        harness.manager.logout()
    }

    @Test
    fun `a muc row whose stanza id authority is not the room sends no marker`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.Message(
                mucMessage("s1").copy(stanzaIdBy = "other.authority"),
            ),
        )
        runCurrent()

        harness.manager.markConversationDisplayed("room@muc.waddle.test", isGroupchat = true)

        assertTrue(harness.client.displayedCalls.isEmpty())
        // XEP-0490 group chats require the MUC-ASSIGNED id: an
        // occupant-injected foreign-authority stanza-id must not become
        // a published room read cursor either.
        assertTrue(harness.client.mdsPublishCalls.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `a dm marker requires the sender's request and mds publishes regardless`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(stanzaId = "s1", stanzaIdBy = "waddle.test", displayedMarkerRequested = false),
            ),
        )
        runCurrent()
        harness.manager.markConversationDisplayed("alice@waddle.test", isGroupchat = false)
        assertTrue(harness.client.displayedCalls.isEmpty())
        assertEquals(1, harness.client.mdsPublishCalls.size)

        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(
                    id = "id-2",
                    stanzaId = "s2",
                    stanzaIdBy = "waddle.test",
                    displayedMarkerRequested = true,
                ),
            ),
        )
        runCurrent()
        harness.manager.markConversationDisplayed("alice@waddle.test", isGroupchat = false)
        // XEP-0333 id-class rule: the DM marker carries the AUTHOR's id
        // ("id-2"), never the local archive stanza id — the peer never
        // saw "s2". The MDS publish still uses the stanza-id pair.
        assertEquals(listOf(Triple("alice@waddle.test", "id-2", false)), harness.client.displayedCalls)
        assertEquals("s2", harness.client.mdsPublishCalls.last().second)
        harness.manager.logout()
    }

    @Test
    fun `reading while disconnected does not consume the marker dispatch`() = runTest {
        val harness = Harness(this)
        // Login but never reach ready: no live client.
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.manager.timelineStore.onLiveMessage(mucMessage("s1"))

        harness.manager.markConversationDisplayed("room@muc.waddle.test", isGroupchat = true)
        assertNull(
            "the cursor dedupe must not swallow an offline read",
            harness.manager.readCursorStore.cursor("room@muc.waddle.test"),
        )

        harness.factory.emitReady()
        runCurrent()
        harness.manager.markConversationDisplayed("room@muc.waddle.test", isGroupchat = true)
        assertEquals(
            listOf(Triple("room@muc.waddle.test", "s1", true)),
            harness.client.displayedCalls,
        )
        harness.manager.logout()
    }

    @Test
    fun `thread replies never advance the read cursor from the main feed`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s1")))
        // Newer thread reply, hidden from the feed.
        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(
                    id = "id-s2",
                    stanzaId = "s2",
                    stanzaIdBy = "room@muc.waddle.test",
                    from = "room@muc.waddle.test/alice",
                    to = null,
                    messageType = "groupchat",
                    isMuc = true,
                    body = "thread reply",
                    thread = "s1",
                ),
            ),
        )
        runCurrent()

        harness.manager.markConversationDisplayed("room@muc.waddle.test", isGroupchat = true)

        assertEquals(
            "the marker targets the newest FEED row, not the unseen thread reply",
            listOf(Triple("room@muc.waddle.test", "s1", true)),
            harness.client.displayedCalls,
        )
        harness.manager.logout()
    }

    @Test
    fun `own and tombstoned rows are never displayed targets`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        // Own MUC echo (nick == localpart) then a retracted remote row.
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s1", nick = "icepuma")))
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s2")))
        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(
                    id = "r1",
                    from = "room@muc.waddle.test/alice",
                    to = null,
                    messageType = "groupchat",
                    isMuc = true,
                    body = null,
                    retractsId = "s2",
                ),
            ),
        )
        runCurrent()

        harness.manager.markConversationDisplayed("room@muc.waddle.test", isGroupchat = true)

        assertTrue("nothing displayable remains", harness.client.displayedCalls.isEmpty())
        assertTrue(harness.client.mdsPublishCalls.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `a live mds event from a sibling device retires the unread badge`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s1")))
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s2", body = "again")))
        runCurrent()
        assertEquals(2, harness.manager.unreadStore.counts.value["room@muc.waddle.test"])

        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(
                    id = "mds-1",
                    body = null,
                    mdsDisplayed = listOf(
                        WaddleMdsDisplayedEntry(
                            chatId = "room@muc.waddle.test",
                            stanzaId = "s1",
                            stanzaIdBy = "room@muc.waddle.test",
                        ),
                    ),
                ),
            ),
        )
        runCurrent()

        assertEquals(
            "one message after the displayed cursor stays unread",
            1,
            harness.manager.unreadStore.counts.value["room@muc.waddle.test"],
        )
        assertEquals("s1", harness.manager.readCursorStore.cursor("room@muc.waddle.test"))
        harness.manager.logout()
    }

    @Test
    fun `mds bootstrap seeds cursors and subscribes after catch-up`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s1")))
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s2", body = "again")))
        runCurrent()

        // A fresh attempt whose fetch returns a cursor at the newest row.
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(2_000)
        runCurrent()
        // The retry created a NEW client; its fetch serves the entries.
        harness.client.mdsEntries = listOf(
            WaddleMdsDisplayedEntry(
                chatId = "room@muc.waddle.test",
                stanzaId = "s2",
                stanzaIdBy = "room@muc.waddle.test",
            ),
        )
        harness.factory.emitReady()
        runCurrent()

        assertEquals("s2", harness.manager.readCursorStore.cursor("room@muc.waddle.test"))
        assertNull(harness.manager.unreadStore.counts.value["room@muc.waddle.test"])
        assertTrue(harness.factory.clients.last().mdsSubscribeCalls >= 1)
        harness.manager.logout()
    }
}
