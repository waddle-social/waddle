package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMessage

/** Timeline avatar gutters: sender groups and author JID resolution. */
class MessageAvatarTest {
    private val room = "room@muc.waddle.test"
    private val self = "me@waddle.test"

    private fun row(
        id: String,
        nick: String,
        timestamp: String? = "2026-07-15T10:00:00Z",
        mine: Boolean = false,
    ) = ConversationRow.Stored(
        TimelineItem(
            id = id,
            conversationJid = room,
            from = "$room/$nick",
            body = "hi",
            timestamp = timestamp,
            isMine = mine,
            source = TimelineSource.Live(
                testMessage(id = id, from = "$room/$nick", messageType = "groupchat", isMuc = true),
            ),
        ),
    )

    private fun avatarOf(avatars: Map<String, MessageAvatar>, row: ConversationRow.Stored) =
        avatars[avatarKeyOf(row.item)]

    @Test
    fun `consecutive rows of one author within five minutes share one avatar`() {
        val first = row("1", "alice", "2026-07-15T10:00:00Z")
        val second = row("2", "alice", "2026-07-15T10:04:59Z")
        val later = row("3", "alice", "2026-07-15T10:10:00Z")
        val other = row("4", "bob", "2026-07-15T10:10:30Z")
        val avatars = messageAvatarsOf(listOf(first, second, later, other), mapOf("alice" to "alice@waddle.test"), self)

        assertEquals(MessageAvatar("alice@waddle.test", visible = true), avatarOf(avatars, first))
        assertEquals(MessageAvatar("alice@waddle.test", visible = false), avatarOf(avatars, second))
        assertTrue(avatarOf(avatars, later)!!.visible)
        // Unknown room author: initials, never a nick-derived guess.
        assertEquals(MessageAvatar(null, visible = true), avatarOf(avatars, other))
    }

    @Test
    fun `own rows get no gutter and break the previous group`() {
        val a1 = row("1", "alice")
        val mine = row("2", "me", mine = true)
        val a2 = row("3", "alice")
        val avatars = messageAvatarsOf(listOf(a1, mine, a2), emptyMap(), self)

        assertNull(avatarOf(avatars, mine))
        assertTrue(avatarOf(avatars, a2)!!.visible)
    }

    @Test
    fun `a departed author resolves through the archived real JID`() {
        val archived = ConversationRow.Stored(
            TimelineItem(
                id = "m1",
                conversationJid = room,
                from = "$room/gone",
                body = "old",
                timestamp = "2026-07-14T09:00:00Z",
                isMine = false,
                source = TimelineSource.Archived(
                    testArchivedMessage(
                        from = "$room/gone",
                        messageType = "groupchat",
                        authorRealJid = "gone@waddle.test/web",
                    ),
                ),
            ),
        )
        val avatars = messageAvatarsOf(listOf(archived), emptyMap(), self)

        assertEquals("gone@waddle.test", avatarOf(avatars, archived)?.jid)
    }
}
