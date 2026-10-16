package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.HAT_URI_BOT
import social.waddle.android.client.testPresence
import social.waddle.client.ffi.WaddleMucRole
import social.waddle.client.ffi.WaddlePresence
import social.waddle.client.ffi.WaddlePresenceHat

class PeopleDirectoryTest {
    private val presence = PresenceStore().apply { setOwnBareJid(ME) }

    private fun occupant(
        room: String,
        nick: String,
        real: String?,
        type: String = "available",
        show: String? = null,
        self: Boolean = false,
        hats: List<WaddlePresenceHat> = emptyList(),
    ): WaddlePresence = testPresence(
        from = "$room/$nick",
        presenceType = type,
        show = show,
        mucRole = if (type == "unavailable") WaddleMucRole.NONE else WaddleMucRole.PARTICIPANT,
        mucJid = real,
        mucStatusCodes = if (self) listOf(110u) else emptyList(),
        hats = hats,
    )

    private fun snapshot(
        huddleNicks: Map<String, Set<String>> = emptyMap(),
        declaredBots: Set<String> = emptySet(),
    ): PeopleDirectorySnapshot = peopleDirectoryOf(
        PeopleDirectoryInputs(
            occupants = presence.occupants.value,
            contacts = presence.contacts.value,
            botJids = presence.botJids.value + declaredBots,
            huddleNicks = huddleNicks,
            ownBareJid = presence.ownBareJid.value,
        ),
    )

    @Test
    fun `one person per bare jid across rooms and contacts, any case`() {
        presence.onPresence(occupant(ROOM_A, "Alice", real = "Alice@Waddle.Test/phone", show = "away"))
        presence.onPresence(occupant("Room-B@Muc.Waddle.Test", "ally", real = "alice@waddle.test/web", show = "xa"))
        presence.onPresence(testPresence(from = "ALICE@waddle.test/laptop", show = "dnd"))

        val people = snapshot().people
        assertEquals(1, people.size)
        val alice = people.single()
        assertEquals("alice@waddle.test", alice.bareJid)
        assertEquals("Alice", alice.displayName)
        assertEquals(PresenceShow.DND, alice.show)
        assertEquals(listOf(ROOM_A, "room-b@muc.waddle.test"), alice.roomJids)
    }

    @Test
    fun `a contact-only person is named by localpart`() {
        presence.onPresence(testPresence(from = "zed@waddle.test/phone", show = "away"))
        val zed = snapshot().people.single()
        assertEquals("zed", zed.displayName)
        assertEquals(PresenceShow.AWAY, zed.show)
        assertTrue(zed.roomJids.isEmpty())
    }

    @Test
    fun `self is excluded by status 110 and by real jid but never counted here`() {
        presence.onPresence(occupant(ROOM_A, "me", real = "me@waddle.test/phone", self = true))
        presence.onPresence(occupant(ROOM_A, "me-tablet", real = "ME@waddle.test/tablet"))
        presence.onPresence(occupant(ROOM_A, "bob", real = "bob@waddle.test/web"))

        val snapshot = snapshot()
        assertEquals(listOf("bob@waddle.test"), snapshot.people.map { it.bareJid })
        assertEquals(1, snapshot.rooms[ROOM_A]?.here)
    }

    @Test
    fun `bots are excluded from people by hat and by declaration`() {
        val botHat = WaddlePresenceHat(uri = HAT_URI_BOT, title = "Bot")
        presence.onPresence(occupant(ROOM_A, "helper", real = "helper@waddle.test/bot", hats = listOf(botHat)))
        presence.onPresence(occupant(ROOM_A, "news", real = "news@waddle.test/bot"))
        presence.onPresence(testPresence(from = "helper@waddle.test/bot"))
        presence.onPresence(occupant(ROOM_A, "bob", real = "bob@waddle.test/web"))

        val snapshot = snapshot(declaredBots = setOf("news@waddle.test"))
        assertEquals(listOf("bob@waddle.test"), snapshot.people.map { it.bareJid })
        assertEquals("bots still count as here", 3, snapshot.rooms[ROOM_A]?.here)
    }

    @Test
    fun `occupants without a real jid are counted but are not people`() {
        presence.onPresence(occupant(ROOM_A, "anon", real = null))
        presence.onPresence(occupant(ROOM_A, "bob", real = "bob@waddle.test/web"))
        val snapshot = snapshot()
        assertEquals(listOf("bob@waddle.test"), snapshot.people.map { it.bareJid })
        assertEquals(2, snapshot.rooms[ROOM_A]?.here)
    }

    @Test
    fun `departures leave the directory and the headcount`() {
        presence.onPresence(occupant(ROOM_A, "bob", real = "bob@waddle.test/web"))
        presence.onPresence(occupant(ROOM_A, "cat", real = "cat@waddle.test/web"))
        presence.onPresence(occupant(ROOM_A, "bob", real = "bob@waddle.test/web", type = "unavailable"))

        val snapshot = snapshot()
        assertEquals(listOf("cat@waddle.test"), snapshot.people.map { it.bareJid })
        assertEquals(1, snapshot.rooms[ROOM_A]?.here)
    }

    @Test
    fun `huddle membership comes from present muji participants`() {
        presence.onPresence(occupant(ROOM_A, "me", real = "me@waddle.test/phone", self = true))
        presence.onPresence(occupant(ROOM_A, "bob", real = "bob@waddle.test/web"))
        presence.onPresence(occupant(ROOM_A, "cat", real = "cat@waddle.test/web"))
        presence.onPresence(occupant(ROOM_B, "dee", real = "dee@waddle.test/web"))

        val snapshot = snapshot(
            huddleNicks = mapOf(
                ROOM_A to setOf("me", "bob", "ghost"),
                ROOM_B to emptySet(),
            ),
        )
        val byJid = snapshot.people.associateBy { it.bareJid }
        assertEquals(ROOM_A, byJid.getValue("bob@waddle.test").huddleRoomJid)
        assertTrue(byJid.getValue("bob@waddle.test").inHuddle)
        assertFalse(byJid.getValue("cat@waddle.test").inHuddle)
        assertNull(byJid.getValue("dee@waddle.test").huddleRoomJid)
        // Self counts in the call; a participant nick no longer present does not.
        assertEquals(RoomHeadcount(here = 2, inHuddle = 2), snapshot.rooms[ROOM_A])
        assertEquals(RoomHeadcount(here = 1, inHuddle = 0), snapshot.rooms["ROOM-B@muc.waddle.test"])
    }

    @Test
    fun `people sort by name then jid`() {
        presence.onPresence(occupant(ROOM_A, "bob", real = "bob2@waddle.test/web"))
        presence.onPresence(occupant(ROOM_B, "Bob", real = "bob1@waddle.test/web"))
        presence.onPresence(occupant(ROOM_A, "amy", real = "amy@waddle.test/web"))
        assertEquals(
            listOf("amy@waddle.test", "bob1@waddle.test", "bob2@waddle.test"),
            snapshot().people.map { it.bareJid },
        )
    }

    private companion object {
        const val ME = "me@waddle.test"
        const val ROOM_A = "room-a@muc.waddle.test"
        const val ROOM_B = "room-b@muc.waddle.test"
    }
}
