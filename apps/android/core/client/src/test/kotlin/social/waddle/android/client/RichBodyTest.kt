package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddleMarkupSpan
import social.waddle.client.ffi.WaddleMarkupSpanType
import social.waddle.client.ffi.WaddleReference
import social.waddle.client.ffi.WaddleReferenceType

class RichBodyTest {

    private fun span(type: WaddleMarkupSpanType, start: UInt, end: UInt, uri: String? = null) =
        WaddleMarkupSpan(spanType = type, start = start, end = end, uri = uri)

    private fun mention(uri: String, begin: UInt, end: UInt) = WaddleReference(
        refType = WaddleReferenceType.Mention,
        uri = uri,
        begin = begin,
        end = end,
        anchor = null,
    )

    private fun dataRef(uri: String, begin: UInt, end: UInt) = WaddleReference(
        refType = WaddleReferenceType.Data,
        uri = uri,
        begin = begin,
        end = end,
        anchor = null,
    )

    private fun blocks(
        body: String,
        markup: List<WaddleMarkupSpan> = emptyList(),
        references: List<WaddleReference> = emptyList(),
        fallbackStart: UInt? = null,
        fallbackEnd: UInt? = null,
    ) = richBlocksOf(body, markup, references, fallbackStart, fallbackEnd)

    private fun paragraphText(block: RichBlock): String =
        (block as RichBlock.Paragraph).runs.joinToString("") { it.text }

    @Test
    fun `plain body is a single unstyled paragraph`() {
        val result = blocks("hello world")
        assertEquals(1, result.size)
        val paragraph = result[0] as RichBlock.Paragraph
        assertEquals(listOf(RichInlineRun("hello world")), paragraph.runs)
    }

    @Test
    fun `bold span styles the covered run only`() {
        val result = blocks(
            "make it bold",
            markup = listOf(span(WaddleMarkupSpanType.BOLD, 8u, 12u)),
        )
        val runs = (result[0] as RichBlock.Paragraph).runs
        assertEquals(listOf("make it ", "bold"), runs.map { it.text })
        assertTrue(runs[1].bold)
        assertEquals(false, runs[0].bold)
    }

    @Test
    fun `offsets are code points not utf16 units`() {
        // 😀 is one code point but two UTF-16 units.
        val result = blocks(
            "😀 bold",
            markup = listOf(span(WaddleMarkupSpanType.BOLD, 2u, 6u)),
        )
        val runs = (result[0] as RichBlock.Paragraph).runs
        assertEquals(listOf("😀 ", "bold"), runs.map { it.text })
        assertTrue(runs[1].bold)
    }

    @Test
    fun `code block partitions the body`() {
        val body = "before\ncode line\nafter"
        val result = blocks(
            body,
            markup = listOf(span(WaddleMarkupSpanType.CODE_BLOCK, 7u, 16u)),
        )
        assertEquals(3, result.size)
        assertEquals("before", paragraphText(result[0]))
        assertEquals("code line", (result[1] as RichBlock.CodeBlock).text)
        assertEquals("after", paragraphText(result[2]))
    }

    @Test
    fun `blockquote strips quote markers per line`() {
        val body = "> one\n> two"
        val result = blocks(
            body,
            markup = listOf(span(WaddleMarkupSpanType.BLOCKQUOTE, 0u, 11u)),
        )
        val quote = result[0] as RichBlock.Blockquote
        assertEquals(1, quote.paragraphs.size)
        assertEquals(listOf("one", "\n", "two"), quote.paragraphs[0].runs.map { it.text })
    }

    @Test
    fun `nested block inside another is not a top level partition`() {
        val body = "> quoted code"
        val result = blocks(
            body,
            markup = listOf(
                span(WaddleMarkupSpanType.BLOCKQUOTE, 0u, 13u),
                span(WaddleMarkupSpanType.CODE_BLOCK, 2u, 8u),
            ),
        )
        assertEquals(1, result.size)
        assertTrue(result[0] is RichBlock.Blockquote)
    }

    @Test
    fun `hostile span past body end is dropped`() {
        val result = blocks(
            "short",
            markup = listOf(
                span(WaddleMarkupSpanType.BOLD, 0u, 4_000_000_000u),
                span(WaddleMarkupSpanType.BOLD, 3u, 2u),
            ),
        )
        val runs = (result[0] as RichBlock.Paragraph).runs
        assertEquals(listOf(RichInlineRun("short")), runs)
    }

    @Test
    fun `overlapping spans resolve to the first covering span`() {
        // Sorted start-asc/end-desc: bold [0,8) sorts before italic [4,12).
        val result = blocks(
            "aaaabbbbcccc",
            markup = listOf(
                span(WaddleMarkupSpanType.ITALIC, 4u, 12u),
                span(WaddleMarkupSpanType.BOLD, 0u, 8u),
            ),
        )
        val runs = (result[0] as RichBlock.Paragraph).runs
        assertEquals(listOf("aaaabbbb", "cccc"), runs.map { it.text })
        assertTrue(runs[0].bold)
        assertEquals(false, runs[0].italic)
        assertTrue(runs[1].italic)
    }

    @Test
    fun `link markup spans are dropped and links come from data references`() {
        val result = blocks(
            "see docs here",
            markup = listOf(span(WaddleMarkupSpanType.LINK, 0u, 3u, uri = "https://evil.example/")),
            references = listOf(dataRef("https://waddle.social/docs", 4u, 8u)),
        )
        val runs = (result[0] as RichBlock.Paragraph).runs
        assertEquals(listOf("see ", "docs", " here"), runs.map { it.text })
        assertNull(runs[0].linkUri)
        assertEquals("https://waddle.social/docs", runs[1].linkUri)
    }

    @Test
    fun `unsafe reference schemes never become links`() {
        val result = blocks(
            "click me",
            references = listOf(dataRef("javascript:alert(1)", 0u, 5u)),
        )
        val runs = (result[0] as RichBlock.Paragraph).runs
        assertEquals(listOf(RichInlineRun("click me")), runs)
    }

    @Test
    fun `bare urls autolink outside code ranges`() {
        val body = "go to https://example.com/x now"
        val result = blocks(body)
        val runs = (result[0] as RichBlock.Paragraph).runs
        assertEquals("https://example.com/x", runs[1].text)
        assertEquals("https://example.com/x", runs[1].linkUri)
    }

    @Test
    fun `code spans suppress autolink`() {
        val body = "https://example.com/x"
        val result = blocks(
            body,
            markup = listOf(span(WaddleMarkupSpanType.CODE, 0u, 21u)),
        )
        val runs = (result[0] as RichBlock.Paragraph).runs
        assertEquals(1, runs.size)
        assertTrue(runs[0].code)
        assertNull(runs[0].linkUri)
    }

    @Test
    fun `mention references compose with markup styling`() {
        val body = "hey @alice look"
        val result = blocks(
            body,
            markup = listOf(span(WaddleMarkupSpanType.BOLD, 0u, 15u)),
            references = listOf(mention("xmpp:alice@waddle.social", 4u, 10u)),
        )
        val runs = (result[0] as RichBlock.Paragraph).runs
        assertEquals(listOf("hey ", "@alice", " look"), runs.map { it.text })
        assertTrue(runs.all { it.bold })
        assertEquals("xmpp:alice@waddle.social", runs[1].mentionUri)
        assertNull(runs[0].mentionUri)
    }

    @Test
    fun `markup rebases past a stripped reply fallback`() {
        // Wire body: "> quoted\n\nbold" — fallback [0,10) stripped.
        val display = "bold"
        val result = blocks(
            display,
            markup = listOf(span(WaddleMarkupSpanType.BOLD, 10u, 14u)),
            fallbackStart = 0u,
            fallbackEnd = 10u,
        )
        val runs = (result[0] as RichBlock.Paragraph).runs
        assertEquals(listOf("bold"), runs.map { it.text })
        assertTrue(runs[0].bold)
    }

    @Test
    fun `blank lines split paragraphs`() {
        val result = blocks("one\n\ntwo")
        assertEquals(2, result.size)
        assertEquals("one", paragraphText(result[0]))
        assertEquals("two", paragraphText(result[1]))
    }

    @Test
    fun `empty body yields no blocks`() {
        assertTrue(blocks("").isEmpty())
    }

    @Test
    fun `hasRichContent is false for plain paragraphs and mentions`() {
        val plain = blocks("hi @a", references = listOf(mention("xmpp:a@w.s", 3u, 5u)))
        assertEquals(false, plain.hasRichContent())
        val styled = blocks("hi", markup = listOf(span(WaddleMarkupSpanType.ITALIC, 0u, 2u)))
        assertTrue(styled.hasRichContent())
    }
}
