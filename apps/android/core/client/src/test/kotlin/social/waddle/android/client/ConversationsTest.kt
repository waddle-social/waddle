package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ConversationsTest {
    @Test
    fun `own muc echo is mine by occupant nick`() {
        val key = conversationKeyOf(
            ownBareJid = "icepuma@waddle.test",
            ownNick = "icepuma",
            from = "general@muc.waddle.test/icepuma",
            to = "icepuma@waddle.test/waddle-android-abcd1234",
            isGroupchat = true,
        )
        // The sender bare JID is the room, never the account — only the
        // occupant resource identifies an own echo (unread must not count
        // the user's own channel messages).
        assertTrue(key!!.isMine)
        assertEquals("general@muc.waddle.test", key.jid)
    }

    @Test
    fun `other occupants stay foreign even with the account bare room`() {
        val key = conversationKeyOf(
            ownBareJid = "icepuma@waddle.test",
            ownNick = "icepuma",
            from = "general@muc.waddle.test/alice",
            to = "icepuma@waddle.test",
            isGroupchat = true,
        )
        assertFalse(key!!.isMine)
        assertEquals("general@muc.waddle.test", key.jid)
    }

    @Test
    fun `dm identity still keys on the bare jid`() {
        val mine = conversationKeyOf(
            ownBareJid = "icepuma@waddle.test",
            ownNick = "icepuma",
            from = "icepuma@waddle.test/phone",
            to = "alice@waddle.test",
            isGroupchat = false,
        )
        assertTrue(mine!!.isMine)
        assertEquals("alice@waddle.test", mine.jid)

        val theirs = conversationKeyOf(
            ownBareJid = "icepuma@waddle.test",
            ownNick = "icepuma",
            from = "alice@waddle.test/laptop",
            to = "icepuma@waddle.test",
            isGroupchat = false,
        )
        assertFalse(theirs!!.isMine)
        assertEquals("alice@waddle.test", theirs.jid)
    }
}
