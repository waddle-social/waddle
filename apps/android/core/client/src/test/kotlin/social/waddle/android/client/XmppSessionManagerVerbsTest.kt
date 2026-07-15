package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.store.MessageTombstone
import social.waddle.android.client.store.ReactionGroup
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddlePinAction
import social.waddle.client.ffi.WaddlePinEntry
import social.waddle.client.ffi.WaddlePinEvent
import social.waddle.client.ffi.WaddlePinPreview

/** Messaging action verbs (react/edit/retract/pin) through the manager. */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerVerbsTest {
    private class Harness(testScope: TestScope) {
        val factory = FakeClientFactory()
        val manager = XmppSessionManager(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
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

    private val room = "room@muc.waddle.test"

    private fun mucMessage(stanzaId: String, nick: String = "alice") = testMessage(
        id = "id-$stanzaId",
        stanzaId = stanzaId,
        stanzaIdBy = room,
        from = "$room/$nick",
        to = null,
        messageType = "groupchat",
        isMuc = true,
    )

    @Test
    fun `reaction applies optimistically and sends the complete set`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s1")))
        runCurrent()

        val sent = harness.manager.sendReaction(
            room,
            isGroupchat = true,
            targetStanzaId = "s1",
            emojis = listOf("👍"),
            previousEmojis = emptyList(),
        )

        assertTrue(sent)
        assertEquals(listOf(Triple(room, "s1", listOf("👍"))), harness.client.reactionCalls)
        assertEquals(
            listOf(ReactionGroup("👍", 1, mine = true)),
            harness.manager.timelineStore.timeline(room).value.single().reactions,
        )
        harness.manager.logout()
    }

    @Test
    fun `failed reaction send rolls the optimistic set back`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s1")))
        runCurrent()
        harness.client.reactionResult = false

        val sent = harness.manager.sendReaction(
            room,
            isGroupchat = true,
            targetStanzaId = "s1",
            emojis = listOf("👍"),
            previousEmojis = emptyList(),
        )

        assertFalse(sent)
        assertTrue(harness.manager.timelineStore.timeline(room).value.single().reactions.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `retraction tombstones the own row locally on success`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        // Own MUC echo: nick == account localpart.
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s1", nick = "icepuma")))
        runCurrent()

        val sent = harness.manager.sendRetraction(room, isGroupchat = true, targetStanzaId = "s1")

        assertTrue(sent)
        assertEquals(listOf(room to "s1"), harness.client.retractionCalls)
        assertEquals(
            MessageTombstone.Retracted,
            harness.manager.timelineStore.timeline(room).value.single().tombstone,
        )
        harness.manager.logout()
    }

    @Test
    fun `correction applies the new body locally on a sent outcome`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        // Own DM copy (no echo will ever arrive for the sending client).
        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(
                    id = "orig-1",
                    stanzaId = "s1",
                    from = "icepuma@waddle.test/phone",
                    to = "alice@waddle.test",
                    body = "helo",
                ),
            ),
        )
        runCurrent()

        val sent = harness.manager.sendCorrection(
            "alice@waddle.test",
            isGroupchat = false,
            targetId = "orig-1",
            newBody = "hello",
        )

        assertTrue(sent)
        val item = harness.manager.timelineStore.timeline("alice@waddle.test").value.single()
        assertEquals("hello", item.body)
        assertTrue(item.edited)
        harness.manager.logout()
    }

    @Test
    fun `pin events and pin fetches drive the pin store`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.roomPins = listOf(
            WaddlePinEntry(
                targetStanzaId = "s1",
                pinnerJid = "admin@waddle.test",
                pinnedAt = "2026-07-15T10:00:00Z",
                preview = WaddlePinPreview(
                    authorJid = "alice@waddle.test",
                    authorNick = "alice",
                    text = "hi",
                    messageTimestamp = "2026-07-15T09:00:00Z",
                ),
            ),
        )

        harness.manager.refreshRoomPins(room)
        assertEquals(setOf("s1"), harness.manager.pinStore.pinned.value[room])

        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(
                    id = "pin-evt",
                    from = room,
                    to = null,
                    messageType = "groupchat",
                    isMuc = true,
                    body = null,
                    pinEvent = WaddlePinEvent(
                        action = WaddlePinAction.UNPINNED,
                        targetStanzaId = "s1",
                        by = "admin@waddle.test",
                        reason = null,
                        preview = null,
                    ),
                ),
            ),
        )
        runCurrent()

        assertTrue(harness.manager.pinStore.pinned.value[room].orEmpty().isEmpty())
        assertNull(harness.manager.unreadStore.counts.value[room])
        harness.manager.logout()
    }

    @Test
    fun `pin and unpin route to the client`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        assertTrue(harness.manager.pinRoomMessage(room, "s1", pin = true))
        assertTrue(harness.manager.pinRoomMessage(room, "s1", pin = false))

        assertEquals(
            listOf(Triple(room, "s1", true), Triple(room, "s1", false)),
            harness.client.pinCalls,
        )
        harness.manager.logout()
    }
}
