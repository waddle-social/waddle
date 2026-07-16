package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddleReference
import social.waddle.client.ffi.WaddleReferenceType

class MentionSpansTest {
    private fun mention(begin: UInt, end: UInt, uri: String = "xmpp:bob@waddle.test") =
        WaddleReference(
            refType = WaddleReferenceType.Mention,
            uri = uri,
            begin = begin,
            end = end,
            anchor = null,
        )

    @Test
    fun `code point offsets map to char indices`() {
        val body = "hi @bob"

        val spans = mentionSpansIn(body, listOf(mention(3u, 7u)), null, null)

        assertEquals(1, spans.size)
        assertEquals("@bob", body.substring(spans[0].startIndex, spans[0].endIndex))
        assertEquals("xmpp:bob@waddle.test", spans[0].uri)
    }

    @Test
    fun `an emoji before the mention widens char indices past the surrogate pair`() {
        // "😀" is one code point but two UTF-16 units: code points 2..6
        // land on chars 3..7.
        val body = "😀 @bob"

        val spans = mentionSpansIn(body, listOf(mention(2u, 6u)), null, null)

        assertEquals(1, spans.size)
        assertEquals(3, spans[0].startIndex)
        assertEquals(7, spans[0].endIndex)
        assertEquals("@bob", body.substring(spans[0].startIndex, spans[0].endIndex))
    }

    @Test
    fun `hostile out-of-range offsets are dropped without crashing`() {
        val body = "short"

        assertTrue(mentionSpansIn(body, listOf(mention(0u, 4_000_000_000u)), null, null).isEmpty())
        assertTrue(mentionSpansIn(body, listOf(mention(3u, 2u)), null, null).isEmpty())
        assertTrue(mentionSpansIn(body, listOf(mention(90u, 95u)), null, null).isEmpty())
    }

    @Test
    fun `the anchor-only sentinel and non-mention references are ignored`() {
        val body = "hi @bob"
        val data = WaddleReference(
            refType = WaddleReferenceType.Data,
            uri = "https://example.com",
            begin = 0u,
            end = 2u,
            anchor = null,
        )

        assertTrue(mentionSpansIn(body, listOf(mention(0u, 0u), data), null, null).isEmpty())
    }

    @Test
    fun `offsets rebase past a stripped reply fallback`() {
        // Wire body "> quote\n\nhi @bob": the store strips the fallback
        // range [0, 9) (code points), so the display body starts at "hi".
        val display = "hi @bob"

        val spans = mentionSpansIn(display, listOf(mention(12u, 16u)), 0u, 9u)

        assertEquals(1, spans.size)
        assertEquals("@bob", display.substring(spans[0].startIndex, spans[0].endIndex))
    }

    @Test
    fun `mentions inside the stripped fallback are dropped`() {
        val display = "hi"

        assertTrue(mentionSpansIn(display, listOf(mention(2u, 6u)), 0u, 9u).isEmpty())
    }

    @Test
    fun `a fallback range the store rejected never rebases the offsets`() {
        // stripReplyFallback leaves the body untouched when the range is
        // out of bounds — a fallback starting past the display body can
        // never have been stripped, so the offsets stay wire offsets.
        val display = "hi @bob"

        val spans = mentionSpansIn(display, listOf(mention(3u, 7u)), 40u, 49u)

        assertEquals(1, spans.size)
        assertEquals("@bob", display.substring(spans[0].startIndex, spans[0].endIndex))
    }
}
