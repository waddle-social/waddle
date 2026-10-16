package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Mention counts ride the unread store's lock and clear paths. */
class MentionStoreTest {
    private val unread = UnreadStore()
    private val mentions get() = unread.mentions.counts.value

    @Test
    fun `a mention counts with its unread and a plain message does not`() {
        unread.onLiveMessage(ROOM, isMine = false, mentionsSelf = true)
        unread.onLiveMessage(ROOM, isMine = false)
        assertEquals(2, unread.counts.value[ROOM])
        assertEquals(mapOf(ROOM to 1), mentions)
    }

    @Test
    fun `own and on-screen messages never count a mention`() {
        unread.onLiveMessage(ROOM, isMine = true, mentionsSelf = true)
        unread.setActiveConversation(ROOM)
        unread.onLiveMessage(ROOM, isMine = false, mentionsSelf = true)
        unread.onLiveMentionAccounted(ROOM, isMine = false)
        assertTrue(mentions.isEmpty())
    }

    @Test
    fun `an inbox-accounted message still counts its mention`() {
        unread.setUnlessActive(ROOM, 3)
        unread.onLiveMentionAccounted(ROOM, isMine = false)
        assertEquals(3, unread.counts.value[ROOM])
        assertEquals(mapOf(ROOM to 1), mentions)
    }

    @Test
    fun `an accounted mention after the unread was read adds no badge`() {
        unread.setUnlessActive(ROOM, 1)
        unread.setUnlessActive(ROOM, 0)
        unread.onLiveMentionAccounted(ROOM, isMine = false)
        assertTrue(mentions.isEmpty())
    }

    @Test
    fun `accounted mentions never outnumber the unread`() {
        unread.setUnlessActive(ROOM, 1)
        repeat(3) { unread.onLiveMentionAccounted(ROOM, isMine = false) }
        assertEquals(1, mentions[ROOM])
    }

    @Test
    fun `mentions clear exactly when the unread clears`() {
        unread.onLiveMessage(ROOM, isMine = false, mentionsSelf = true)
        unread.onLiveMessage(DM, isMine = false, mentionsSelf = true)
        unread.clear(ROOM)
        assertEquals(mapOf(DM to 1), mentions)

        // A sibling device read everything (XEP-0490) / the inbox says zero.
        unread.setUnlessActive(DM, 0)
        assertTrue(mentions.isEmpty())
    }

    @Test
    fun `an absolute recompute caps mentions at the unread left`() {
        repeat(3) { unread.onLiveMessage(ROOM, isMine = false, mentionsSelf = true) }
        unread.setUnlessActive(ROOM, 5)
        assertEquals(3, mentions[ROOM])
        unread.setUnlessActive(ROOM, 2)
        assertEquals(2, mentions[ROOM])
    }

    @Test
    fun `a recompute for the on-screen conversation leaves mentions alone`() {
        unread.onLiveMessage(ROOM, isMine = false, mentionsSelf = true)
        unread.setActiveConversation(ROOM)
        unread.setUnlessActive(ROOM, 0)
        assertEquals(1, mentions[ROOM])
    }

    @Test
    fun `a fresh stream resets mentions but not unread`() {
        unread.onLiveMessage(ROOM, isMine = false, mentionsSelf = true)
        unread.resetMentions()
        assertTrue(mentions.isEmpty())
        assertEquals(1, unread.counts.value[ROOM])

        unread.onLiveMessage(ROOM, isMine = false, mentionsSelf = true)
        unread.clearAll()
        assertTrue(mentions.isEmpty())
    }

    private companion object {
        const val ROOM = "room@muc.waddle.test"
        const val DM = "alice@waddle.test"
    }
}
