package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.store.TimelineItem
import social.waddle.client.ffi.WaddleClientEvent

/**
 * XEP-0045 `muc#user` traffic (room private messages, invites, declines)
 * has no native conversation surface: through the session manager it
 * reaches no DM list, timeline, unread count or recency write, while
 * ordinary DM and room traffic on the same paths still does.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerMucUserTest {
    private class Harness(private val scope: TestScope) {
        val factory = FakeClientFactory()
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val manager = XmppSessionManager(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(scope.testScheduler),
        )

        suspend fun loginReady() {
            manager.login(testSessionInfo())
            scope.runCurrent()
            factory.emit(WaddleClientEvent.Connected)
            scope.runCurrent()
        }

        fun emit(event: WaddleClientEvent) {
            factory.emit(event)
            scope.runCurrent()
        }
    }

    @Test
    fun `muc user traffic is shown nowhere`() = runTest {
        val harness = Harness(this)
        harness.loginReady()

        // XEP-0045 private message: type=chat from the occupant JID.
        harness.emit(WaddleClientEvent.Message(testMessage(stanzaId = "pm-1", from = "$ROOM/alice", mucUser = true)))
        // Mediated invite: type=normal from the bare room, with a body.
        harness.emit(
            WaddleClientEvent.Message(
                testMessage(stanzaId = "inv-1", from = ROOM, messageType = "normal", body = "join", mucUser = true),
            ),
        )
        // Archived twin of a private message.
        harness.emit(
            WaddleClientEvent.MamResult(testArchivedMessage(mamId = "mam-1", from = "$ROOM/alice", mucUser = true)),
        )

        assertEquals(emptyList<String>(), harness.manager.dmStore.peers.value)
        assertEquals(emptyMap<String, String>(), harness.prefs.lastSeen.first())
        assertEquals(emptyList<TimelineItem>(), harness.manager.timelineStore.timeline(ROOM).value)
        assertEquals(emptyMap<String, Int>(), harness.manager.unreadStore.counts.value)

        harness.manager.logout()
    }

    @Test
    fun `ordinary dm and room traffic are unaffected`() = runTest {
        val harness = Harness(this)
        harness.loginReady()

        harness.emit(WaddleClientEvent.Message(testMessage(stanzaId = "dm-1", from = "alice@waddle.test/phone")))
        harness.emit(
            WaddleClientEvent.Message(
                testMessage(stanzaId = "gc-1", from = "$ROOM/bob", messageType = "groupchat", isMuc = true),
            ),
        )

        assertEquals(listOf("alice@waddle.test"), harness.manager.dmStore.peers.value)
        assertEquals(setOf("alice@waddle.test"), harness.prefs.lastSeen.first().keys)
        assertEquals(1, harness.manager.timelineStore.timeline(ROOM).value.size)
        assertEquals(mapOf("alice@waddle.test" to 1, ROOM to 1), harness.manager.unreadStore.counts.value)

        harness.manager.logout()
    }

    private companion object {
        const val ROOM = "room@muc.waddle.test"
    }
}
