package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.store.ContactPresence
import social.waddle.android.client.store.DirectoryPerson
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.client.testPresence
import social.waddle.android.feature.members.PeerProfile
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddlePresenceHat

/** What a message author's profile shows, and when it offers "Message". */
class MessageAuthorTest {
    private val room = "general@muc.waddle.test"
    private val alice = MessageAuthor(name = "alice", jid = "alice@waddle.test", nick = "alice")

    @Test
    fun `room role, hats, presence and status come from the occupant`() {
        val occupant = testPresence(
            from = "$room/alice",
            mucAffiliation = WaddleMucAffiliation.ADMIN,
            mucJid = "Alice@waddle.test/phone",
            show = "away",
            hats = listOf(WaddlePresenceHat(uri = "urn:hat:host", title = "Host")),
        ).copy(status = "  Reviewing the MAM PR  ")

        assertEquals(
            PeerProfile(
                displayName = "alice",
                jid = "alice@waddle.test",
                presence = PresenceShow.AWAY,
                statusText = "Reviewing the MAM PR",
                affiliation = WaddleMucAffiliation.ADMIN,
                hats = listOf("Host"),
            ),
            authorProfileOf(alice, occupant, person = null, contact = null),
        )
    }

    @Test
    fun `a nick now held by someone else says nothing about the author`() {
        val impostor = testPresence(
            from = "$room/alice",
            mucAffiliation = WaddleMucAffiliation.OWNER,
            mucJid = "mallory@waddle.test/laptop",
        )
        val profile = authorProfileOf(alice, impostor, person = null, contact = null)

        assertNull(profile.affiliation)
        assertNull(profile.presence)
    }

    @Test
    fun `an anonymous occupant shows the nick and what the room says`() {
        val anonymous = MessageAuthor(name = "guest", jid = null, nick = "guest")
        val profile = authorProfileOf(anonymous, testPresence(from = "$room/guest"), person = null, contact = null)

        assertEquals("guest", profile.displayName)
        assertNull(profile.jid)
        assertEquals(PresenceShow.AVAILABLE, profile.presence)
    }

    @Test
    fun `a DM author takes the stream's presence for their bare JID`() {
        val dmAuthor = alice.copy(nick = null)
        val contact = ContactPresence(
            bareJid = "alice@waddle.test",
            show = PresenceShow.DND,
            presence = testPresence(from = "alice@waddle.test/phone", show = "dnd").copy(status = "Heads down"),
        )
        val person = DirectoryPerson(
            bareJid = "alice@waddle.test",
            displayName = "alice",
            show = PresenceShow.DND,
            huddleRoomJid = null,
            roomJids = emptyList(),
        )
        val profile = authorProfileOf(dmAuthor, occupant = null, person = person, contact = contact)

        assertEquals(PresenceShow.DND, profile.presence)
        assertEquals("Heads down", profile.statusText)
        assertNull(profile.affiliation)
    }

    @Test
    fun `message needs a real JID that is neither you nor the open DM`() {
        assertTrue(canMessage("alice@waddle.test", selfBareJid = "me@waddle.test", dmPeerJid = null))
        assertFalse(canMessage(null, selfBareJid = "me@waddle.test", dmPeerJid = null))
        assertFalse(canMessage("Me@waddle.test", selfBareJid = "me@waddle.test", dmPeerJid = null))
        assertFalse(canMessage("alice@waddle.test", selfBareJid = "me@waddle.test", dmPeerJid = "ALICE@waddle.test"))
    }
}
