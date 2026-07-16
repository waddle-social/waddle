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
import social.waddle.android.client.store.MessageTombstone
import social.waddle.android.client.store.ReactionGroup
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddlePinAction
import social.waddle.client.ffi.WaddlePinEntry
import social.waddle.client.ffi.WaddlePinEvent
import social.waddle.client.ffi.WaddlePinPreview
import social.waddle.client.ffi.WaddleSendMessageOutcome

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

        val result = harness.manager.toggleReaction(
            room,
            isGroupchat = true,
            targetStanzaId = "s1",
            emoji = "👍",
        )

        assertEquals(VerbResult.Ok, result)
        assertEquals(listOf(Triple(room, "s1", listOf("👍"))), harness.client.reactionCalls)
        assertEquals(
            listOf(ReactionGroup("👍", 1, mine = true)),
            harness.manager.timelineStore.timeline(room).value.single().reactions,
        )

        // Toggling again clears (the manager derives the set in-lock).
        harness.manager.toggleReaction(room, isGroupchat = true, targetStanzaId = "s1", emoji = "👍")
        assertEquals(listOf(room, room).size, harness.client.reactionCalls.size)
        assertEquals(emptyList<String>(), harness.client.reactionCalls.last().third)
        harness.manager.logout()
    }

    @Test
    fun `failed reaction send rolls the optimistic set back`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s1")))
        runCurrent()
        harness.client.reactionResult = false

        val result = harness.manager.toggleReaction(
            room,
            isGroupchat = true,
            targetStanzaId = "s1",
            emoji = "👍",
        )

        assertEquals(VerbResult.Rejected, result)
        assertTrue(harness.manager.timelineStore.timeline(room).value.single().reactions.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `muc retraction waits for the room reflection instead of tombstoning locally`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        // Own MUC echo: nick == account localpart.
        harness.factory.emit(WaddleClientEvent.Message(mucMessage("s1", nick = "icepuma")))
        runCurrent()

        val result = harness.manager.sendRetraction(room, isGroupchat = true, targetStanzaId = "s1")

        assertEquals(VerbResult.Ok, result)
        assertEquals(listOf(room to "s1"), harness.client.retractionCalls)
        // Stream-accept is not room-accept: the room may still reject,
        // so the tombstone waits for the reflected retraction.
        assertNull(harness.manager.timelineStore.timeline(room).value.single().tombstone)
        harness.manager.logout()
    }

    @Test
    fun `dm retraction tombstones locally on success (no reflection exists)`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(
                    id = "orig-1",
                    stanzaId = "s1",
                    from = "icepuma@waddle.test/phone",
                    to = "alice@waddle.test",
                    body = "oops",
                ),
            ),
        )
        runCurrent()

        val result = harness.manager.sendRetraction(
            "alice@waddle.test",
            isGroupchat = false,
            targetStanzaId = "orig-1",
        )

        assertEquals(VerbResult.Ok, result)
        assertEquals(
            MessageTombstone.Retracted,
            harness.manager.timelineStore.timeline("alice@waddle.test").value.single().tombstone,
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

        val result = harness.manager.sendCorrection(
            "alice@waddle.test",
            isGroupchat = false,
            targetId = "orig-1",
            newBody = "hello",
        )

        assertEquals(VerbResult.Ok, result)
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
        // The snapshot rides the serialized event stream.
        runCurrent()
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
    fun `queued sends replay with their attachments intact`() = runTest {
        val harness = Harness(this)
        // Login without ever reaching ready: sends queue.
        harness.manager.login(testSessionInfo())
        runCurrent()

        val result = harness.manager.sendChatMessage(
            "alice@waddle.test",
            body = "",
            extras = social.waddle.android.client.MessageSendExtras(
                sharedFiles = listOf(
                    social.waddle.android.client.prefs.SharedFileRef(
                        url = "https://files.waddle.test/cat.png",
                        name = "cat.png",
                        mediaType = "image/png",
                        sizeBytes = 42,
                        disposition = FileDisposition.INLINE,
                    ),
                ),
            ),
        )
        assertTrue(result.queued)

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val replayedOptions = harness.factory.clients.last().sendOptions.last()
        assertEquals(
            "https://files.waddle.test/cat.png",
            replayedOptions?.sharedFiles?.single()?.url,
        )
        harness.manager.logout()
    }

    @Test
    fun `dm typing indicators use the peer's localpart not their resource`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(
                    from = "alice@waddle.test/Conversations.x8f2",
                    body = null,
                    chatState = WaddleChatState.COMPOSING,
                ),
            ),
        )
        runCurrent()

        assertEquals(
            listOf("alice"),
            harness.manager.chatStateStore.composing.value["alice@waddle.test"],
        )
        harness.manager.logout()
    }

    @Test
    fun `pin and unpin route to the client`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        assertEquals(VerbResult.Ok, harness.manager.pinRoomMessage(room, "s1", pin = true))
        assertEquals(VerbResult.Ok, harness.manager.pinRoomMessage(room, "s1", pin = false))

        assertEquals(
            listOf(Triple(room, "s1", true), Triple(room, "s1", false)),
            harness.client.pinCalls,
        )
        harness.manager.logout()
    }

    @Test
    fun `verbs report not connected before session ready`() = runTest {
        val harness = Harness(this)
        // Login without ever reaching ready: no live client exists.
        harness.manager.login(testSessionInfo())
        runCurrent()

        assertEquals(VerbResult.NotConnected, harness.manager.joinRoom(room, "icepuma"))
        assertEquals(
            VerbResult.NotConnected,
            harness.manager.toggleReaction(room, isGroupchat = true, targetStanzaId = "s1", emoji = "👍"),
        )
        assertEquals(
            VerbResult.NotConnected,
            harness.manager.sendCorrection(room, isGroupchat = true, targetId = "s1", newBody = "x"),
        )
        assertEquals(
            VerbResult.NotConnected,
            harness.manager.sendRetraction(room, isGroupchat = true, targetStanzaId = "s1"),
        )
        assertEquals(VerbResult.NotConnected, harness.manager.pinRoomMessage(room, "s1", pin = true))
        assertEquals(
            VerbResult.NotConnected,
            harness.manager.sendChatState(room, isGroupchat = true, state = WaddleChatState.COMPOSING),
        )
        harness.manager.logout()
    }

    @Test
    fun `identity-gated verbs report not ready after logout`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.manager.logout()

        // Both resolve the account's mutation identity before touching
        // the client, so a signed-out account reports NotReady.
        assertEquals(
            VerbResult.NotReady,
            harness.manager.toggleReaction(room, isGroupchat = true, targetStanzaId = "s1", emoji = "👍"),
        )
        assertEquals(
            VerbResult.NotReady,
            harness.manager.sendRetraction(room, isGroupchat = true, targetStanzaId = "s1"),
        )
        // sendCorrection checks the client first: post-logout that is
        // gone too, so NotConnected wins; its NotReady branch guards the
        // signed-out-mid-flight race behind a still-live client.
        assertEquals(
            VerbResult.NotConnected,
            harness.manager.sendCorrection(room, isGroupchat = true, targetId = "s1", newBody = "x"),
        )
    }

    @Test
    fun `refused retraction and non-sent correction report rejected`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.retractionResult = false
        harness.client.correctionOutcome = WaddleSendMessageOutcome.StanzaError

        assertEquals(
            VerbResult.Rejected,
            harness.manager.sendRetraction(room, isGroupchat = true, targetStanzaId = "s1"),
        )
        assertEquals(
            VerbResult.Rejected,
            harness.manager.sendCorrection(room, isGroupchat = true, targetId = "s1", newBody = "x"),
        )
        harness.manager.logout()
    }
}
