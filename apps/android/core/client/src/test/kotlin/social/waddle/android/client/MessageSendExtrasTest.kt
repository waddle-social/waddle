package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class MessageSendExtrasTest {
    @Test
    fun `reply prefixes the quoted fallback and marks its codepoint range`() {
        val (body, options) = preparedSend(
            stanzaId = "sid-1",
            body = "sounds good",
            extras = MessageSendExtras(
                replyToId = "s1",
                replyToAuthorJid = "room@muc.waddle.test/alice",
                replyParentBody = "line one\nline two",
            ),
        )

        assertEquals("> line one\n> line two\n\nsounds good", body)
        assertEquals("s1", options.reply?.messageId)
        assertEquals("room@muc.waddle.test/alice", options.reply?.authorJid)
        assertEquals(0u, options.fallback?.start)
        assertEquals("> line one\n> line two\n\n".length.toUInt(), options.fallback?.end)
        assertEquals("sid-1", options.stanzaId)
    }

    @Test
    fun `fallback range counts codepoints not utf-16 units`() {
        val parent = "🎉🎉" // two supplementary-plane codepoints, four UTF-16 units
        val (body, options) = preparedSend(
            stanzaId = "sid-1",
            body = "nice",
            extras = MessageSendExtras(
                replyToId = "s1",
                replyToAuthorJid = "alice@waddle.test",
                replyParentBody = parent,
            ),
        )

        // "> 🎉🎉\n\n" = 1('>')+1(' ')+2(emoji)+2(newlines) = 6 codepoints.
        assertEquals(6u, options.fallback?.end)
        assertEquals("> $parent\n\n", body.substringBefore("nice"))
    }

    @Test
    fun `thread target rides along without touching the body`() {
        val (body, options) = preparedSend(
            stanzaId = "sid-1",
            body = "in thread",
            extras = MessageSendExtras(threadId = "t1", threadParent = null),
        )

        assertEquals("in thread", body)
        assertEquals("t1", options.thread?.id)
        assertNull(options.reply)
        assertNull(options.fallback)
    }

    @Test
    fun `strip round-trips the built fallback`() {
        val parent = "hello 🎉 world"
        val prefix = buildReplyFallbackPrefix(parent)
        val wire = prefix + "the reply"

        val stripped = stripReplyFallback(
            wire,
            start = 0u,
            end = prefix.codePointCount(0, prefix.length).toUInt(),
        )

        assertEquals("the reply", stripped)
    }

    @Test
    fun `invalid fallback ranges leave the body untouched`() {
        assertEquals("abc", stripReplyFallback("abc", 2u, 1u))
        assertEquals("abc", stripReplyFallback("abc", 0u, 99u))
        assertEquals("abc", stripReplyFallback("abc", null, 2u))
    }
}
