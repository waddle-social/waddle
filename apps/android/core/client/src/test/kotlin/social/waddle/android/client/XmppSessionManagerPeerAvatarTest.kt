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
import social.waddle.android.client.store.authorBareJidOf
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleException
import social.waddle.client.ffi.WaddleMucRole

/** Peer avatars end to end: lazy fetch, AvatarChanged, reconnect, authors. */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerPeerAvatarTest {
    private class Harness(testScope: TestScope) {
        var now = 0L
        val factory = FakeClientFactory()
        val manager = XmppSessionManager(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
            clock = { now },
        )

        suspend fun loginReady(scope: TestScope) {
            manager.login(testSessionInfo())
            scope.runCurrent()
            factory.emit(WaddleClientEvent.Connected)
            scope.runCurrent()
        }

        val client get() = factory.clients.last()

        fun callsFor(jid: String) = client.requestAvatarCalls.filter { it.first == jid }
    }

    private val alice = "alice@waddle.test"
    private val room = "room@muc.waddle.test"

    @Test
    fun `ensure fetches a peer avatar once and dedupes repeat renders`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.avatar = testAvatar(jid = alice, id = "id-1")

        harness.manager.peerAvatars.ensure(alice)
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()

        assertEquals(1, harness.callsFor(alice).size)
        assertEquals("id-1", harness.manager.peerAvatars.avatars.value[alice]?.id)
        harness.manager.logout()
    }

    @Test
    fun `a peer without an avatar is re-queried only after the retry window`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        harness.manager.peerAvatars.ensure(alice)
        runCurrent()
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()
        assertEquals(1, harness.callsFor(alice).size)

        harness.now = PeerAvatarRepository.RETRY_TTL_MILLIS
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()
        assertEquals(2, harness.callsFor(alice).size)
        harness.manager.logout()
    }

    @Test
    fun `revalidation passes known ids so unchanged bytes are not re-downloaded`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.avatar = testAvatar(jid = alice, id = "id-1")
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()

        harness.now = PeerAvatarRepository.POSITIVE_TTL_MILLIS
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()

        assertEquals(listOf("id-1"), harness.callsFor(alice)[1].second)
        assertEquals("id-1", harness.manager.peerAvatars.avatars.value[alice]?.id)
        harness.manager.logout()
    }

    @Test
    fun `a failed lookup keeps the held avatar and retries after the retry window`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.avatar = testAvatar(jid = alice, id = "id-1")
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()

        // Revalidation hits a transient failure: the FFI throws, which is
        // NOT "no avatar" — the face must stay.
        harness.client.requestAvatarFailure = WaddleException.Stanza("remote-server-timeout", null)
        harness.now = PeerAvatarRepository.POSITIVE_TTL_MILLIS
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()
        assertEquals(2, harness.callsFor(alice).size)
        assertEquals("id-1", harness.manager.peerAvatars.avatars.value[alice]?.id)

        // Retried only once the 10-minute window has passed.
        harness.client.requestAvatarFailure = null
        harness.now += PeerAvatarRepository.RETRY_TTL_MILLIS - 1
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()
        assertEquals(2, harness.callsFor(alice).size)
        harness.now += 1
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()
        assertEquals(3, harness.callsFor(alice).size)
        assertEquals("id-1", harness.manager.peerAvatars.avatars.value[alice]?.id)
        harness.manager.logout()
    }

    @Test
    fun `an id-only answer whose bytes were evicted mid-flight refetches the data once`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.avatar = testAvatar(jid = alice, id = "id-1")
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()

        // Revalidation sends id-1 as known → the fake answers id-only, but
        // the cached bytes vanish while the IQ is in flight.
        var evictOnce = true
        harness.client.duringRequestAvatar = {
            if (evictOnce) {
                evictOnce = false
                harness.manager.profileStore.clear()
            }
        }
        harness.now = PeerAvatarRepository.POSITIVE_TTL_MILLIS
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()

        val calls = harness.callsFor(alice)
        assertEquals(listOf("id-1"), calls[1].second)
        assertEquals(emptyList<String>(), calls[2].second)
        assertEquals("id-1", harness.manager.peerAvatars.avatars.value[alice]?.id)
        harness.manager.logout()
    }

    @Test
    fun `a definitive no-avatar answer clears the held avatar`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.avatar = testAvatar(jid = alice, id = "id-1")
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()

        harness.client.avatar = null
        harness.now = PeerAvatarRepository.POSITIVE_TTL_MILLIS
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()
        assertNull(harness.manager.peerAvatars.avatars.value[alice])
        harness.manager.logout()
    }

    @Test
    fun `AvatarChanged refetches, a cached id skips the wire, and a disable clears`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.avatar = testAvatar(jid = alice, id = "id-1")
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()

        harness.client.avatar = testAvatar(jid = alice, id = "id-2")
        harness.factory.emit(WaddleClientEvent.AvatarChanged(alice, "id-2"))
        runCurrent()
        assertEquals(2, harness.callsFor(alice).size)
        assertEquals("id-2", harness.manager.peerAvatars.avatars.value[alice]?.id)

        // A flip back to an id whose bytes are cached never hits the wire.
        harness.factory.emit(WaddleClientEvent.AvatarChanged(alice, "id-1"))
        runCurrent()
        assertEquals(2, harness.callsFor(alice).size)
        assertEquals("id-1", harness.manager.peerAvatars.avatars.value[alice]?.id)

        harness.factory.emit(WaddleClientEvent.AvatarChanged(alice, null))
        runCurrent()
        assertNull(harness.manager.peerAvatars.avatars.value[alice])
        harness.manager.logout()
    }

    @Test
    fun `a reconnect makes settled avatars stale`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.avatar = testAvatar(jid = alice, id = "id-1")
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()

        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(RECONNECT_DELAY_MILLIS)
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        harness.manager.peerAvatars.ensure(alice)
        runCurrent()
        // The fresh attempt's client is asked again.
        assertEquals(1, harness.callsFor(alice).size)
        harness.manager.logout()
    }

    @Test
    fun `logout drops peer avatars and author mappings`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.avatar = testAvatar(jid = alice, id = "id-1")
        harness.manager.peerAvatars.ensure(alice)
        harness.factory.emit(
            WaddleClientEvent.Presence(testPresence(from = "$room/alice", mucJid = "$alice/phone")),
        )
        runCurrent()

        harness.manager.logout()

        assertTrue(harness.manager.peerAvatars.avatars.value.isEmpty())
        assertTrue(harness.manager.occupantJidStore.jids.value.isEmpty())
    }

    private fun roomMessage(id: String, nick: String, timestamp: String? = null) = WaddleClientEvent.Message(
        testMessage(
            id = id,
            stanzaId = id,
            from = "$room/$nick",
            to = "icepuma@waddle.test",
            messageType = "groupchat",
            isMuc = true,
            timestamp = timestamp,
        ),
    )

    private fun XmppSessionManager.authorOf(id: String): String? =
        authorBareJidOf(timelineStore.timeline(room).value.single { it.id == id }, "icepuma@waddle.test")

    @Test
    fun `a reused nick never re-attributes rows stored before it`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(WaddleClientEvent.Presence(testPresence(from = "$room/alice", mucJid = "$alice/phone")))
        harness.factory.emit(roomMessage("s1", "alice"))
        harness.factory.emit(
            WaddleClientEvent.Presence(
                testPresence(from = "$room/alice", presenceType = "unavailable", mucJid = "$alice/phone"),
            ),
        )
        // Bob joins under Alice's old nick.
        harness.factory.emit(
            WaddleClientEvent.Presence(testPresence(from = "$room/alice", mucJid = "bob@waddle.test/web")),
        )
        harness.factory.emit(roomMessage("s2", "alice"))
        runCurrent()

        assertEquals(alice, harness.manager.authorOf("s1"))
        assertEquals("bob@waddle.test", harness.manager.authorOf("s2"))
        harness.manager.logout()
    }

    private fun XmppSessionManager.row(id: String) = timelineStore.timeline(room).value.single { it.id == id }

    @Test
    fun `a room-assigned nick decides our live rows, not the configured one`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        // XEP-0045 210: the room renamed us; someone else holds our configured nick.
        harness.factory.emit(
            WaddleClientEvent.Presence(
                testPresence(
                    from = "$room/icepuma2",
                    mucJid = "icepuma@waddle.test/android",
                    mucRole = WaddleMucRole.PARTICIPANT,
                    mucStatusCodes = listOf(110u, 210u),
                ),
            ),
        )
        harness.factory.emit(
            WaddleClientEvent.Presence(
                testPresence(
                    from = "$room/icepuma",
                    mucJid = "peer@waddle.test/web",
                    mucRole = WaddleMucRole.PARTICIPANT,
                ),
            ),
        )
        harness.factory.emit(roomMessage("p1", "icepuma"))
        harness.factory.emit(roomMessage("o1", "icepuma2"))
        runCurrent()

        assertFalse(harness.manager.row("p1").isMine)
        assertEquals("peer@waddle.test", harness.manager.authorOf("p1"))
        assertTrue(harness.manager.row("o1").isMine)
        assertEquals("icepuma@waddle.test", harness.manager.authorOf("o1"))
        harness.manager.logout()
    }

    @Test
    fun `without disclosed JIDs the actual nick alone decides ownership`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.Presence(
                testPresence(
                    from = "$room/icepuma2",
                    mucRole = WaddleMucRole.PARTICIPANT,
                    mucStatusCodes = listOf(110u, 210u),
                ),
            ),
        )
        harness.factory.emit(roomMessage("p1", "icepuma"))
        harness.factory.emit(roomMessage("o1", "icepuma2"))
        runCurrent()

        assertFalse(harness.manager.row("p1").isMine)
        assertNull(harness.manager.authorOf("p1"))
        assertTrue(harness.manager.row("o1").isMine)
        assertEquals("icepuma@waddle.test", harness.manager.authorOf("o1"))

        // Our unavailable self-presence ends the assignment.
        harness.factory.emit(
            WaddleClientEvent.Presence(
                testPresence(
                    from = "$room/icepuma2",
                    presenceType = "unavailable",
                    mucRole = WaddleMucRole.NONE,
                    mucStatusCodes = listOf(110u),
                ),
            ),
        )
        runCurrent()
        assertNull(harness.manager.occupantJidStore.ownNickIn(room))
        harness.manager.logout()
    }

    @Test
    fun `a new session forgets nick holders but keeps stored stamps`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(WaddleClientEvent.Presence(testPresence(from = "$room/alice", mucJid = "$alice/phone")))
        harness.factory.emit(roomMessage("s1", "alice"))
        runCurrent()

        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(RECONNECT_DELAY_MILLIS)
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        // "alice" changed hands while we were away; a message races the
        // rejoin presence that would say who holds it now.
        harness.factory.emit(roomMessage("s2", "alice"))
        runCurrent()

        assertEquals(alice, harness.manager.authorOf("s1"))
        assertNull(harness.manager.authorOf("s2"))
        harness.manager.logout()
    }

    @Test
    fun `archive authors and delayed rows never label a nick's later holder`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.MamResult(
                testArchivedMessage(
                    mamId = "m1",
                    id = "a1",
                    stanzaId = "a1",
                    from = "$room/sam",
                    to = "icepuma@waddle.test",
                    messageType = "groupchat",
                    authorRealJid = "Sam.Old@waddle.test/web",
                ),
            ),
        )
        // No presence for "sam": the archived author must not leak onto it.
        harness.factory.emit(roomMessage("s3", "sam"))
        // Join history is delayed: it predates the current "alice" (Bob).
        harness.factory.emit(
            WaddleClientEvent.Presence(testPresence(from = "$room/alice", mucJid = "bob@waddle.test/web")),
        )
        harness.factory.emit(roomMessage("s4", "alice", timestamp = "2026-07-01T10:00:00Z"))
        runCurrent()

        assertEquals("sam.old@waddle.test", harness.manager.authorOf("a1"))
        assertNull(harness.manager.authorOf("s3"))
        assertNull(harness.manager.authorOf("s4"))
        assertNull(harness.manager.occupantJidStore.jids.value[room]?.get("sam"))
        harness.manager.logout()
    }

    @Test
    fun `a body-less PEP headline never becomes a timeline row`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(id = "pep-1", from = alice, body = null, messageType = "headline"),
            ),
        )
        harness.factory.emit(WaddleClientEvent.AvatarChanged(alice, "id-9"))
        runCurrent()

        assertTrue(harness.manager.timelineStore.timeline(alice).value.isEmpty())
        assertTrue(harness.manager.dmStore.peers.value.isEmpty())
        harness.manager.logout()
    }

    private companion object {
        /** First reconnect backoff at [PinnedRandom] 0.5, plus slack. */
        const val RECONNECT_DELAY_MILLIS = 1_001L
    }
}
