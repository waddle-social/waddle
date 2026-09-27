package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent

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
        val epoch = harness.manager.peerAvatars.epoch.value

        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(RECONNECT_DELAY_MILLIS)
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertTrue(harness.manager.peerAvatars.epoch.value > epoch)
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

    @Test
    fun `presence and archived authors feed the retained nick mapping`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.factory.emit(
            WaddleClientEvent.Presence(testPresence(from = "$room/alice", mucJid = "$alice/phone")),
        )
        harness.factory.emit(
            WaddleClientEvent.Presence(testPresence(from = "$room/alice", presenceType = "unavailable")),
        )
        harness.factory.emit(
            WaddleClientEvent.MamResult(
                testArchivedMessage(
                    from = "$room/bob",
                    to = "me@waddle.test",
                    messageType = "groupchat",
                    authorRealJid = "bob@waddle.test/web",
                ),
            ),
        )
        runCurrent()

        assertEquals(
            mapOf("alice" to alice, "bob" to "bob@waddle.test"),
            harness.manager.occupantJidStore.jids.value[room],
        )
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
