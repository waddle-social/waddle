package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.android.client.MentionCandidate

class MentionTokenizerTest {
    @Test
    fun `an at sign at the start of the text arms the token`() {
        val token = activeMentionToken("@", cursor = 1)

        assertEquals(MentionToken(start = 0, end = 1, query = ""), token)
    }

    @Test
    fun `a partial query after whitespace is captured`() {
        val token = activeMentionToken("hi @bo", cursor = 6)

        assertEquals(MentionToken(start = 3, end = 6, query = "bo"), token)
    }

    @Test
    fun `an embedded at sign never arms the popover`() {
        assertNull(activeMentionToken("mail a@b", cursor = 8))
    }

    @Test
    fun `a completed token followed by a space disarms`() {
        assertNull(activeMentionToken("hi @bob ", cursor = 8))
    }

    @Test
    fun `the token tracks the cursor not the text end`() {
        // Cursor inside "@bo|b tail": the active query is what precedes it.
        val token = activeMentionToken("@bob tail", cursor = 3)

        assertEquals(MentionToken(start = 0, end = 3, query = "bo"), token)
    }

    @Test
    fun `out-of-range cursors are rejected`() {
        assertNull(activeMentionToken("@bob", cursor = -1))
        assertNull(activeMentionToken("@bob", cursor = 5))
    }

    @Test
    fun `an empty query lists the first candidates`() {
        val candidates = (1..10).map { candidate("user$it") }

        assertEquals(8, filterMentionCandidates(candidates, "").size)
    }

    @Test
    fun `filtering is case-insensitive contains`() {
        val candidates = listOf(candidate("Alice"), candidate("bob"), candidate("carol"))

        assertEquals(listOf("Alice"), filterMentionCandidates(candidates, "LIC").map { it.display })
    }

    @Test
    fun `diacritics fold for matching`() {
        val candidates = listOf(candidate("José"), candidate("bob"))

        assertEquals(listOf("José"), filterMentionCandidates(candidates, "jose").map { it.display })
    }

    @Test
    fun `directory hits follow matching room candidates without duplicates`() {
        val room = listOf(
            MentionCandidate("everyone", "xmpp:@everyone", isBroadcast = true),
            person("carl", "carl@waddle.test"),
            person("bob", "bob@waddle.test"),
        )
        val hits = listOf(
            person("carl", "Carl@Waddle.Test"),
            person("carol", "carol@waddle.test"),
            person("caro", "caro@waddle.test"),
        )

        assertEquals(
            listOf("carl", "carol", "caro"),
            mentionPopoverResults(room, "car", hits).map { it.display },
        )
    }

    @Test
    fun `directory hits are ignored for an empty query and respect the row limit`() {
        val room = (1..6).map { person("car$it", "car$it@waddle.test") }
        val hits = (1..6).map { person("carx$it", "carx$it@waddle.test") }

        assertEquals(room, mentionPopoverResults(room, "", hits).take(6))
        assertEquals(6, mentionPopoverResults(room, "", hits).size)
        val results = mentionPopoverResults(room, "car", hits)
        assertEquals(8, results.size)
        assertEquals(room + hits.take(2), results)
    }

    private fun person(display: String, jid: String) = MentionCandidate(
        display = display,
        uri = "xmpp:${jid.lowercase()}",
        isBroadcast = false,
        jid = jid,
    )

    private fun candidate(display: String) = MentionCandidate(
        display = display,
        uri = "xmpp:${display.lowercase()}@waddle.test",
        isBroadcast = false,
    )
}
