package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ComposerMarkdownTest {

    private fun markupOf(result: ComposedMarkdown): List<Triple<MarkupRefType, UInt, UInt>> =
        result.markup.map { Triple(it.type, it.begin, it.end) }

    @Test
    fun `plain text passes through untouched`() {
        val result = composeMarkdown("just words", emptyList())
        assertEquals("just words", result.body)
        assertTrue(result.markup.isEmpty())
    }

    @Test
    fun `inline styles strip markers and emit spans`() {
        val result = composeMarkdown("**b** *i* ~~s~~ `c`", emptyList())
        assertEquals("b i s c", result.body)
        assertEquals(
            listOf(
                Triple(MarkupRefType.BOLD, 0u, 1u),
                Triple(MarkupRefType.ITALIC, 2u, 3u),
                Triple(MarkupRefType.STRIKETHROUGH, 4u, 5u),
                Triple(MarkupRefType.CODE, 6u, 7u),
            ),
            markupOf(result),
        )
    }

    @Test
    fun `offsets count code points across emoji`() {
        val result = composeMarkdown("😀 **bold**", emptyList())
        assertEquals("😀 bold", result.body)
        assertEquals(listOf(Triple(MarkupRefType.BOLD, 2u, 6u)), markupOf(result))
    }

    @Test
    fun `fences become code blocks with fence lines removed`() {
        val result = composeMarkdown("a\n```\ncode\n```\nb", emptyList())
        assertEquals("a\ncode\nb", result.body)
        assertEquals(listOf(Triple(MarkupRefType.CODE_BLOCK, 2u, 6u)), markupOf(result))
    }

    @Test
    fun `fence language info string is dropped`() {
        val result = composeMarkdown("```rust\nfn main() {}\n```", emptyList())
        assertEquals("fn main() {}", result.body)
        assertEquals(listOf(Triple(MarkupRefType.CODE_BLOCK, 0u, 12u)), markupOf(result))
    }

    @Test
    fun `unclosed fence stays literal`() {
        val result = composeMarkdown("```\nno closer", emptyList())
        assertEquals("```\nno closer", result.body)
        assertTrue(result.markup.isEmpty())
    }

    @Test
    fun `inline markers inside fences are literal`() {
        val result = composeMarkdown("```\n**not bold**\n```", emptyList())
        assertEquals("**not bold**", result.body)
        assertEquals(listOf(Triple(MarkupRefType.CODE_BLOCK, 0u, 12u)), markupOf(result))
    }

    @Test
    fun `inline code protects its content from other styles`() {
        val result = composeMarkdown("`**x**`", emptyList())
        assertEquals("**x**", result.body)
        assertEquals(listOf(Triple(MarkupRefType.CODE, 0u, 5u)), markupOf(result))
    }

    @Test
    fun `quote lines emit a blockquote span keeping markers`() {
        val result = composeMarkdown("> one\n> two", emptyList())
        assertEquals("> one\n> two", result.body)
        assertEquals(listOf(Triple(MarkupRefType.BLOCKQUOTE, 0u, 11u)), markupOf(result))
    }

    @Test
    fun `separate quote groups emit separate spans`() {
        val result = composeMarkdown("> a\nplain\n> b", emptyList())
        assertEquals(
            listOf(
                Triple(MarkupRefType.BLOCKQUOTE, 0u, 3u),
                Triple(MarkupRefType.BLOCKQUOTE, 10u, 13u),
            ),
            markupOf(result),
        )
    }

    @Test
    fun `styled text inside a quote gets both spans`() {
        val result = composeMarkdown("> **b**", emptyList())
        assertEquals("> b", result.body)
        assertEquals(
            listOf(
                Triple(MarkupRefType.BLOCKQUOTE, 0u, 3u),
                Triple(MarkupRefType.BOLD, 2u, 3u),
            ),
            markupOf(result),
        )
    }

    @Test
    fun `lists are not markdown`() {
        val result = composeMarkdown("- item\n1. other", emptyList())
        assertEquals("- item\n1. other", result.body)
        assertTrue(result.markup.isEmpty())
    }

    @Test
    fun `lone asterisks around spaces stay literal`() {
        val result = composeMarkdown("a * b * c", emptyList())
        assertEquals("a * b * c", result.body)
        assertTrue(result.markup.isEmpty())
    }

    @Test
    fun `mentions rebase across removed markers`() {
        val mention = MentionRef(uri = "xmpp:alice@w.s", begin = 6u, end = 12u)
        val result = composeMarkdown("**b** @alice", listOf(mention))
        assertEquals("b @alice", result.body)
        assertEquals(listOf(mention.copy(begin = 2u, end = 8u)), result.mentions)
    }

    @Test
    fun `mentions rebase across removed fence lines`() {
        val body = "```\ncode\n```\n@alice hi"
        val mention = MentionRef(uri = "xmpp:alice@w.s", begin = 13u, end = 19u)
        val result = composeMarkdown(body, listOf(mention))
        assertEquals("code\n@alice hi", result.body)
        assertEquals(listOf(mention.copy(begin = 5u, end = 11u)), result.mentions)
    }

    @Test
    fun `mention untouched when no markdown present`() {
        val mention = MentionRef(uri = "xmpp:a@w.s", begin = 0u, end = 2u)
        val result = composeMarkdown("@a hi", listOf(mention))
        assertEquals(listOf(mention), result.mentions)
    }

    @Test
    fun `triple asterisks resolve to bold with literal inner markers`() {
        // Marker styles do not nest: the outer ** pair wins, the inner
        // asterisks stay literal (documented composeMarkdown edge).
        val result = composeMarkdown("***x***", emptyList())
        assertEquals("*x*", result.body)
        assertEquals(listOf(Triple(MarkupRefType.BOLD, 0u, 2u)), markupOf(result))
    }

    @Test
    fun `multiple bold runs keep independent offsets`() {
        val result = composeMarkdown("**a** mid **b**", emptyList())
        assertEquals("a mid b", result.body)
        assertEquals(
            listOf(
                Triple(MarkupRefType.BOLD, 0u, 1u),
                Triple(MarkupRefType.BOLD, 6u, 7u),
            ),
            markupOf(result),
        )
    }
}
