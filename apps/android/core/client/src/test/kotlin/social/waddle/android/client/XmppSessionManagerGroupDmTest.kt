package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleException

/**
 * Group-DM lifecycle verbs through the manager: the
 * `urn:waddle:group-dm:*` create/rename/leave commands plus the
 * XEP-0045 §7.8.2 mediated add-member invite.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerGroupDmTest {
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

    private val room = "gdm-1@muc.waddle.test"

    @Test
    fun `createGroupDm sequences verb, topology refresh, and own-nick join`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val callsAfterReady = harness.client.topology.calls
        harness.client.groupDm.createdRoomJid = room

        val result = harness.manager.createGroupDm(
            name = "Alice, Bob",
            memberJids = listOf("icepuma@waddle.test", "alice@waddle.test", "bob@waddle.test"),
        )
        runCurrent()

        assertEquals(CreateRoomResult.Created(room), result)
        assertEquals(
            "Alice, Bob" to listOf("icepuma@waddle.test", "alice@waddle.test", "bob@waddle.test"),
            harness.client.groupDm.createCalls.single(),
        )
        // Sequencing: the topology refresh ran after the verb…
        assertEquals(callsAfterReady + 1, harness.client.topology.calls)
        // …and the join used the fresh room jid with the own localpart nick.
        assertEquals(room to "icepuma", harness.client.joinRoomCalls.single())
        assertTrue(room in harness.manager.roomStore.joinedRooms.value)
    }

    @Test
    fun `createGroupDm failure skips refresh and join`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val callsAfterReady = harness.client.topology.calls
        harness.client.groupDm.createFailure = WaddleException.Stanza("forbidden", null)

        val result = harness.manager.createGroupDm(
            name = "Alice, Bob",
            memberJids = listOf("icepuma@waddle.test", "alice@waddle.test"),
        )
        runCurrent()

        assertEquals(CreateRoomResult.NotPermitted, result)
        assertEquals(callsAfterReady, harness.client.topology.calls)
        assertTrue(harness.client.joinRoomCalls.isEmpty())
    }

    @Test
    fun `renameGroupDm forwards the trimmed name and refreshes topology`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val callsAfterReady = harness.client.topology.calls

        assertEquals(RoomAdminResult.Ok, harness.manager.renameGroupDm(room, "  Weekend crew  "))
        assertEquals(room to "Weekend crew", harness.client.groupDm.renameCalls.single())
        assertEquals(callsAfterReady + 1, harness.client.topology.calls)

        // Blank clears: the FFI verb receives null.
        assertEquals(RoomAdminResult.Ok, harness.manager.renameGroupDm(room, "   "))
        assertEquals(room to null, harness.client.groupDm.renameCalls.last())
    }

    @Test
    fun `leaveGroupDm unmarks the room and refreshes topology on Ok`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.manager.roomStore.markJoined(room)
        val callsAfterReady = harness.client.topology.calls

        val result = harness.manager.leaveGroupDm(room)
        runCurrent()

        assertEquals(RoomAdminResult.Ok, result)
        assertEquals(room, harness.client.groupDm.leaveCalls.single())
        assertFalse(room in harness.manager.roomStore.joinedRooms.value)
        assertEquals(callsAfterReady + 1, harness.client.topology.calls)
    }

    @Test
    fun `leaveGroupDm failure keeps the join mark and skips the refresh`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.manager.roomStore.markJoined(room)
        val callsAfterReady = harness.client.topology.calls
        harness.client.groupDm.leaveFailure = WaddleException.Stanza("item-not-found", null)

        assertEquals(RoomAdminResult.Rejected, harness.manager.leaveGroupDm(room))
        assertTrue(room in harness.manager.roomStore.joinedRooms.value)
        assertEquals(callsAfterReady, harness.client.topology.calls)
    }

    @Test
    fun `a topology refresh answering after a relogin never lands in the new session`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val seeded = harness.manager.roomStore.topology.value
        // The refresh parks mid-flight; the account signs out and back
        // in, so its answer belongs to a retired session.
        harness.client.topology.delayMillis = 1_000L
        harness.client.topology.result = harness.client.topology.result.copy(
            channels = listOf(testChannel(roomJid = "stale@muc.waddle.test")),
        )

        val pending = async { harness.manager.renameGroupDm(room, "Renamed") }
        runCurrent()
        harness.manager.logout()
        harness.loginReady(this)
        advanceTimeBy(1_100L)
        runCurrent()
        pending.await()

        assertEquals(seeded, harness.manager.roomStore.topology.value)
        harness.manager.logout()
    }

    @Test
    fun `inviteToGroupDm records the invitee and history choice`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        assertEquals(
            RoomAdminResult.Ok,
            harness.manager.inviteToGroupDm(room, "charlie@waddle.test", fullHistory = true),
        )
        assertEquals(
            Triple(room, "charlie@waddle.test", true),
            harness.client.groupDm.inviteCalls.single(),
        )

        harness.client.groupDm.inviteFailure = WaddleException.Stanza("forbidden", null)
        assertEquals(
            RoomAdminResult.NotPermitted,
            harness.manager.inviteToGroupDm(room, "charlie@waddle.test"),
        )
    }

    @Test
    fun `group dm verbs report NotConnected without a session`() = runTest {
        val harness = Harness(this)

        assertEquals(
            CreateRoomResult.NotConnected,
            harness.manager.createGroupDm("duo", listOf("a@waddle.test", "b@waddle.test")),
        )
        assertEquals(RoomAdminResult.NotConnected, harness.manager.renameGroupDm(room, "x"))
        assertEquals(RoomAdminResult.NotConnected, harness.manager.leaveGroupDm(room))
        assertEquals(
            RoomAdminResult.NotConnected,
            harness.manager.inviteToGroupDm(room, "charlie@waddle.test"),
        )
    }
}
