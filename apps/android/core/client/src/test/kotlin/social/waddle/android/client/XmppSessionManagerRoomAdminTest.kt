package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.store.MemberListStatus
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleException
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleRoomMemberEntry

/**
 * Room lifecycle + member management verbs through the manager
 * (XEP-0045 §5/§8.2/§9.5/§10 and the `urn:waddle:admin` surface).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerRoomAdminTest {
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

    private val room = "general@muc.waddle.test"

    private fun entry(jid: String, affiliation: WaddleMucAffiliation) = WaddleRoomMemberEntry(
        jid = jid,
        affiliation = affiliation,
        nick = null,
        reason = null,
    )

    @Test
    fun `refreshRoomMembers fans out over the four tiers and merges`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.roomMembersByTier = mapOf(
            WaddleMucAffiliation.OWNER to listOf(entry("alice@waddle.test", WaddleMucAffiliation.OWNER)),
            WaddleMucAffiliation.MEMBER to listOf(entry("bob@waddle.test", WaddleMucAffiliation.MEMBER)),
        )

        harness.manager.refreshRoomMembers(room)
        runCurrent()

        // §9.5 fan-out: one query per affiliation tier.
        assertEquals(
            listOf(
                WaddleMucAffiliation.OWNER,
                WaddleMucAffiliation.ADMIN,
                WaddleMucAffiliation.MEMBER,
                WaddleMucAffiliation.OUTCAST,
            ),
            harness.client.listMembersCalls.map { it.second },
        )
        val state = harness.manager.roomMembersStore.rooms.value.getValue(room)
        assertEquals(MemberListStatus.LOADED, state.status)
        assertEquals(
            listOf("alice@waddle.test", "bob@waddle.test"),
            state.members.map { it.jid },
        )
    }

    @Test
    fun `refreshRoomMembers degrades to unavailable only when every tier fails`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.memberListFailure = WaddleException.Stanza("forbidden", null)

        harness.manager.refreshRoomMembers(room)
        runCurrent()

        assertEquals(
            MemberListStatus.UNAVAILABLE,
            harness.manager.roomMembersStore.rooms.value.getValue(room).status,
        )
    }

    @Test
    fun `setRoomAffiliation maps forbidden onto NotPermitted`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        val ok = harness.manager.setRoomAffiliation(
            room,
            "bob@waddle.test",
            WaddleMucAffiliation.ADMIN,
        )
        assertEquals(RoomAdminResult.Ok, ok)
        assertEquals(
            listOf<Any?>(room, "bob@waddle.test", WaddleMucAffiliation.ADMIN, null),
            harness.client.setAffiliationCalls.single(),
        )

        harness.client.setAffiliationFailure = WaddleException.Stanza("forbidden", null)
        assertEquals(
            RoomAdminResult.NotPermitted,
            harness.manager.setRoomAffiliation(room, "bob@waddle.test", WaddleMucAffiliation.OWNER),
        )
    }

    @Test
    fun `kickOccupant addresses the occupant by nick`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        val result = harness.manager.kickOccupant(room, "mallory", "spam")

        assertEquals(RoomAdminResult.Ok, result)
        assertEquals(Triple(room, "mallory", "spam"), harness.client.kickCalls.single())
    }

    @Test
    fun `createRoom slugs the name and reports the created jid`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        val result = harness.manager.createRoom(
            name = "Design Reviews",
            nick = "icepuma",
            description = "weekly crits",
            forum = true,
        )

        val created = result as CreateRoomResult.Created
        assertEquals("design-reviews@muc.waddle.test", created.roomJid)
        val (localpart, nick, patch) = harness.client.createRoomCalls.single()
        assertEquals("design-reviews", localpart)
        assertEquals("icepuma", nick)
        assertEquals("Design Reviews", patch.name)
        assertEquals("weekly crits", patch.description)
        assertEquals(true, patch.forum)
    }

    @Test
    fun `createRoom rejects an empty slug without touching the wire`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        assertEquals(CreateRoomResult.InvalidName, harness.manager.createRoom("   ", "icepuma"))
        assertTrue(harness.client.createRoomCalls.isEmpty())
    }

    @Test
    fun `destroyRoom forwards the reason and unmarks the room`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.manager.roomStore.markJoined(room)

        val result = harness.manager.destroyRoom(room, "archived")

        assertEquals(RoomAdminResult.Ok, result)
        assertEquals(room to "archived", harness.client.destroyRoomCalls.single())
        assertFalse(room in harness.manager.roomStore.joinedRooms.value)
    }

    @Test
    fun `owner probe and users list pass through`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.communityOwner = true

        assertTrue(harness.manager.isCommunityOwner())

        harness.manager.adminUsersList(prefix = "al", pageSize = 50u)
        assertEquals(
            Triple("al", 50u as UInt?, null as String?),
            harness.client.adminUsersListCalls.single(),
        )
    }

    @Test
    fun `sendModeration forwards to the moderation verb`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        val result = harness.manager.sendModeration(room, "stanza-1", "spam")

        assertEquals(VerbResult.Ok, result)
        assertEquals(Triple(room, "stanza-1", "spam"), harness.client.moderationCalls.single())
    }

    @Test
    fun `room admin verbs report NotConnected without a session`() = runTest {
        val harness = Harness(this)

        assertEquals(
            RoomAdminResult.NotConnected,
            harness.manager.setRoomAffiliation(room, "bob@waddle.test", WaddleMucAffiliation.MEMBER),
        )
        assertEquals(CreateRoomResult.NotConnected, harness.manager.createRoom("general", "icepuma"))
        assertFalse(harness.manager.isCommunityOwner())
    }
}
