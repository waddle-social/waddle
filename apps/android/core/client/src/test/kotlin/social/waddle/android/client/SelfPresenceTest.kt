package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleMucRole

/** XEP-0045 self-presence (status 110) gating helpers. */
class SelfPresenceTest {
    private val room = "general@muc.waddle.test"

    private fun occupants(vararg presences: Pair<String, social.waddle.client.ffi.WaddlePresence>) =
        presences.toMap()

    @Test
    fun `self occupant is identified by status code 110`() {
        val occupants = occupants(
            "alice" to testPresence(
                from = "$room/alice",
                mucAffiliation = WaddleMucAffiliation.OWNER,
            ),
            "me" to testPresence(
                from = "$room/me",
                mucAffiliation = WaddleMucAffiliation.MEMBER,
                mucStatusCodes = listOf(110u),
            ),
        )
        assertEquals(
            WaddleMucAffiliation.MEMBER,
            selfOccupantOf(occupants)?.mucAffiliation,
        )
    }

    @Test
    fun `management requires own owner or admin affiliation`() {
        // Another occupant's owner affiliation must never grant the
        // local account management UI (privilege-confusion guard).
        val notMine = occupants(
            "alice" to testPresence(
                from = "$room/alice",
                mucAffiliation = WaddleMucAffiliation.OWNER,
            ),
            "me" to testPresence(
                from = "$room/me",
                mucAffiliation = WaddleMucAffiliation.MEMBER,
                mucStatusCodes = listOf(110u),
            ),
        )
        assertFalse(canManageMembersOf(notMine))
        assertFalse(isRoomOwnerOf(notMine))
        assertFalse(canModerateOf(notMine))

        val admin = occupants(
            "me" to testPresence(
                from = "$room/me",
                mucAffiliation = WaddleMucAffiliation.ADMIN,
                mucStatusCodes = listOf(110u),
            ),
        )
        assertTrue(canManageMembersOf(admin))
        assertFalse(isRoomOwnerOf(admin))
        assertTrue(canModerateOf(admin))
    }

    @Test
    fun `moderator role grants moderation but not member management`() {
        val moderator = occupants(
            "me" to testPresence(
                from = "$room/me",
                mucAffiliation = WaddleMucAffiliation.MEMBER,
                mucRole = WaddleMucRole.MODERATOR,
                mucStatusCodes = listOf(110u),
            ),
        )
        assertTrue(canModerateOf(moderator))
        assertFalse(canManageMembersOf(moderator))
    }

    @Test
    fun `no self presence means no privileges`() {
        assertFalse(canManageMembersOf(emptyMap()))
        assertFalse(isRoomOwnerOf(emptyMap()))
        assertFalse(canModerateOf(emptyMap()))
    }
}
