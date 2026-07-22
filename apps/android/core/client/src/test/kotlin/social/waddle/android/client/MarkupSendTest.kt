package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.client.ffi.WaddleMarkupSpanType

class MarkupSendTest {

    @Test
    fun `markup spans reach the send options`() {
        val extras = MessageSendExtras(
            markup = listOf(MarkupRef(MarkupRefType.BOLD, 0u, 4u)),
        )
        val (body, options) = preparedSend("sid-1", "bold text", extras)
        assertEquals("bold text", body)
        assertEquals(1, options.markupSpans.size)
        val span = options.markupSpans[0]
        assertEquals(WaddleMarkupSpanType.BOLD, span.spanType)
        assertEquals(0u, span.start)
        assertEquals(4u, span.end)
    }

    @Test
    fun `markup offsets shift right past the reply fallback prefix`() {
        val extras = MessageSendExtras(
            replyToId = "orig-1",
            replyToAuthorJid = "room@muc.waddle.test/alice",
            replyParentBody = "parent",
            markup = listOf(MarkupRef(MarkupRefType.ITALIC, 0u, 4u)),
        )
        val (body, options) = preparedSend("sid-1", "ital reply", extras)
        val fallback = checkNotNull(options.fallback)
        assertTrue(body.startsWith("> parent\n\n"))
        val span = options.markupSpans.single()
        assertEquals(fallback.end, span.start)
        assertEquals(fallback.end + 4u, span.end)
    }

    @Test
    fun `inverted markup spans are dropped at the boundary`() {
        val extras = MessageSendExtras(
            markup = listOf(MarkupRef(MarkupRefType.CODE, 5u, 5u)),
        )
        val (_, options) = preparedSend("sid-1", "body", extras)
        assertTrue(options.markupSpans.isEmpty())
    }

    @Test
    fun `every markup type maps to its wire span type`() {
        val extras = MessageSendExtras(
            markup = MarkupRefType.entries.map { MarkupRef(it, 0u, 1u) },
        )
        val (_, options) = preparedSend("sid-1", "x", extras)
        assertEquals(
            listOf(
                WaddleMarkupSpanType.BOLD,
                WaddleMarkupSpanType.ITALIC,
                WaddleMarkupSpanType.STRIKETHROUGH,
                WaddleMarkupSpanType.CODE,
                WaddleMarkupSpanType.CODE_BLOCK,
                WaddleMarkupSpanType.BLOCKQUOTE,
            ),
            options.markupSpans.map { it.spanType },
        )
    }

    @Test
    fun `queued sends replay with their markup intact`() {
        val queued = QueuedOutboundMessage(
            ownerBareJid = "me@waddle.test",
            conversationJid = "room@muc.waddle.test",
            isGroupchat = true,
            body = "bold text",
            clientStanzaId = "sid-1",
            enqueuedAtMillis = 0L,
            markup = listOf(MarkupRef(MarkupRefType.BOLD, 0u, 4u)),
        )
        val extras = checkNotNull(queued.sendExtras())
        assertEquals(queued.markup, extras.markup)
    }
}
