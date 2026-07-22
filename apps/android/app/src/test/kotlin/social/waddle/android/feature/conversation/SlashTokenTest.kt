package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.android.client.ExtensionCommand
import social.waddle.android.client.ExtensionCommandScope

/** Port parity with the wasm client's slash-trigger/slash-match tests. */
class SlashTokenTest {
    private fun command(
        prefix: String?,
        scope: ExtensionCommandScope = ExtensionCommandScope.GLOBAL,
        node: String = "urn:waddle:extension:1:${prefix ?: "nameless"}",
    ) = ExtensionCommand(
        serviceJid = "extensions.waddle.test",
        node = node,
        name = prefix ?: "nameless",
        scope = scope,
        composerPrefix = prefix,
    )

    // ── parseSlashTrigger ────────────────────────────────────────────

    @Test
    fun `a bare slash arms with an empty prefix`() {
        assertEquals(SlashTrigger(prefix = "", trailing = ""), parseSlashTrigger("/"))
    }

    @Test
    fun `a prefix without trailing text arms`() {
        assertEquals(SlashTrigger(prefix = "poll", trailing = ""), parseSlashTrigger("/poll"))
    }

    @Test
    fun `trailing text after the separator is captured trimmed at the start`() {
        assertEquals(
            SlashTrigger(prefix = "ai", trailing = "what is xmpp?"),
            parseSlashTrigger("/ai   what is xmpp?"),
        )
    }

    @Test
    fun `a separator without trailing text keeps the empty trailing`() {
        assertEquals(SlashTrigger(prefix = "poll", trailing = ""), parseSlashTrigger("/poll "))
    }

    @Test
    fun `trailing text may span multiple lines`() {
        assertEquals(
            SlashTrigger(prefix = "ai", trailing = "first\nsecond"),
            parseSlashTrigger("/ai first\nsecond"),
        )
    }

    @Test
    fun `plain text never arms`() {
        assertNull(parseSlashTrigger("hello /poll"))
        assertNull(parseSlashTrigger("poll"))
        assertNull(parseSlashTrigger(""))
    }

    @Test
    fun `a prefix must start with a letter`() {
        assertNull(parseSlashTrigger("/1poll"))
        assertNull(parseSlashTrigger("/-poll"))
    }

    // ── filterSlashCandidates ────────────────────────────────────────

    @Test
    fun `filtering matches by case-insensitive starts-with`() {
        val poll = command("poll")
        val ai = command("ai")
        assertEquals(
            listOf(poll),
            filterSlashCandidates("PO", listOf(poll, ai), inMuc = false),
        )
    }

    @Test
    fun `an empty prefix lists every command with a composer prefix`() {
        val poll = command("poll")
        val nameless = command(null)
        assertEquals(
            listOf(poll),
            filterSlashCandidates("", listOf(poll, nameless), inMuc = false),
        )
    }

    @Test
    fun `channel-scoped commands are filtered outside MUCs`() {
        val poll = command("poll", scope = ExtensionCommandScope.CHANNEL)
        assertEquals(emptyList<ExtensionCommand>(), filterSlashCandidates("p", listOf(poll), inMuc = false))
        assertEquals(listOf(poll), filterSlashCandidates("p", listOf(poll), inMuc = true))
    }

    // ── resolveSlashCommand ──────────────────────────────────────────

    @Test
    fun `resolution requires an exact case-insensitive prefix`() {
        val poll = command("poll")
        assertEquals(poll, resolveSlashCommand("POLL", listOf(poll), inMuc = true))
        assertNull(resolveSlashCommand("pol", listOf(poll), inMuc = true))
    }

    @Test
    fun `an empty prefix never resolves`() {
        assertNull(resolveSlashCommand("", listOf(command("poll")), inMuc = true))
    }

    @Test
    fun `an ambiguous prefix refuses to auto-resolve`() {
        val first = command("poll", node = "urn:waddle:extension:1:poll-a")
        val second = command("poll", node = "urn:waddle:extension:1:poll-b")
        assertNull(resolveSlashCommand("poll", listOf(first, second), inMuc = true))
    }

    @Test
    fun `channel-scoped commands never resolve outside MUCs`() {
        val poll = command("poll", scope = ExtensionCommandScope.CHANNEL)
        assertNull(resolveSlashCommand("poll", listOf(poll), inMuc = false))
        assertEquals(poll, resolveSlashCommand("poll", listOf(poll), inMuc = true))
    }
}
