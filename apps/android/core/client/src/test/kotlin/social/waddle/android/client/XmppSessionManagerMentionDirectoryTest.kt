package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleMentionTarget
import social.waddle.client.ffi.WaddleUserSearchEntry

/**
 * XEP-0055 directory hits from a room's `@` mention search become
 * typed-`@username` targets of that room's send-time mention directory,
 * so people with no MUC presence can be mentioned.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerMentionDirectoryTest {
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

    @Test
    fun `searched directory users resolve typed mentions in that room only`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.directory.userSearchResults = listOf(
            WaddleUserSearchEntry(jid = "Carol@Waddle.Test", username = "carol", displayName = "Carol"),
        )

        val hits = harness.manager.searchRoomMentionUsers(ROOM, "car")
        assertEquals(listOf("car"), harness.client.directory.searchUsersCalls.toList())
        assertEquals("carol", hits?.single()?.username)

        harness.manager.sendGroupchatMessage(ROOM, "hi @carol")
        harness.manager.sendGroupchatMessage(OTHER_ROOM, "hi @carol")
        runCurrent()

        val (roomOptions, otherOptions) = harness.client.sendOptions.toList()
        assertEquals(
            listOf(WaddleMentionTarget(label = "carol", jid = "carol@waddle.test")),
            roomOptions?.mentionDirectory?.targets,
        )
        assertEquals(emptyList<WaddleMentionTarget>(), otherOptions?.mentionDirectory?.targets)

        harness.manager.logout()
    }

    @Test
    fun `search before the session is ready answers null and records nothing`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        assertNull(harness.manager.searchRoomMentionUsers(ROOM, "car"))

        harness.manager.logout()
    }

    private companion object {
        const val ROOM = "general@muc.waddle.test"
        const val OTHER_ROOM = "random@muc.waddle.test"
    }
}
