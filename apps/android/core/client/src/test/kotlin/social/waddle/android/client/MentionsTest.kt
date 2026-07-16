package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddleMucRole

class MentionsTest {
    @Test
    fun `mention uri round-trips through its bare jid`() {
        assertEquals("xmpp:bob@waddle.test", mentionUriFor("bob@waddle.test"))
        assertEquals("bob@waddle.test", bareJidOfMentionUri("xmpp:bob@waddle.test"))
    }

    @Test
    fun `mention uri canonicalizes case and resource`() {
        assertEquals("xmpp:bob@waddle.test", mentionUriFor("Bob@Waddle.Test"))
        assertEquals("bob@waddle.test", bareJidOfMentionUri("XMPP:Bob@Waddle.Test/phone"))
        // RFC 5122 query suffixes never leak into the JID.
        assertEquals("bob@waddle.test", bareJidOfMentionUri("xmpp:bob@waddle.test?message"))
    }

    @Test
    fun `broadcast uris resolve to no bare jid`() {
        assertNull(bareJidOfMentionUri(MENTION_URI_EVERYONE))
        assertNull(bareJidOfMentionUri(MENTION_URI_HERE))
        assertNull(bareJidOfMentionUri("not a uri"))
    }

    @Test
    fun `broadcast uris satisfy the rust parser's classification`() {
        // parsing/mod.rs derives broadcast_mention from `xmpp:` URIs
        // containing `@everyone`/`@here` — the emitted constants MUST
        // round-trip through that check (mirrored here verbatim).
        fun rustClassifiesAsBroadcast(uri: String): Boolean =
            uri.startsWith("xmpp:") && (uri.contains("@everyone") || uri.contains("@here"))
        listOf(MENTION_URI_EVERYONE, MENTION_URI_HERE).forEach { uri ->
            assertTrue(rustClassifiesAsBroadcast(uri))
        }
        assertFalse(rustClassifiesAsBroadcast("xmpp:bob@waddle.test"))
        assertFalse(rustClassifiesAsBroadcast("@everyone"))
    }

    @Test
    fun `broadcast mention addresses everyone`() {
        assertTrue(messageMentionsBareJid(MENTION_URI_EVERYONE, emptyList(), "bob@waddle.test"))
        assertTrue(messageMentionsBareJid(MENTION_URI_HERE, emptyList(), null))
    }

    @Test
    fun `self mention matches case-insensitively across uri shapes`() {
        assertTrue(
            messageMentionsBareJid(null, listOf("xmpp:Bob@Waddle.Test"), "bob@waddle.test"),
        )
        assertTrue(
            messageMentionsBareJid(null, listOf("xmpp:bob@waddle.test"), "Bob@Waddle.Test/phone"),
        )
    }

    @Test
    fun `foreign mentions and missing self never match`() {
        assertFalse(messageMentionsBareJid(null, listOf("xmpp:alice@waddle.test"), "bob@waddle.test"))
        assertFalse(messageMentionsBareJid(null, listOf("xmpp:bob@waddle.test"), null))
        assertFalse(messageMentionsBareJid(null, emptyList(), "bob@waddle.test"))
    }

    @Test
    fun `candidates list broadcasts first then occupants with real jids`() {
        val candidates = mentionCandidatesOf(
            mapOf(
                "bob" to testPresence(from = "room@muc.waddle.test/bob", mucJid = "bob@waddle.test/phone"),
                "alice" to testPresence(from = "room@muc.waddle.test/alice", mucJid = "alice@waddle.test"),
            ),
        )

        assertEquals(listOf("everyone", "here", "alice", "bob"), candidates.map { it.display })
        assertEquals(
            listOf(MENTION_URI_EVERYONE, MENTION_URI_HERE, "xmpp:alice@waddle.test", "xmpp:bob@waddle.test"),
            candidates.map { it.uri },
        )
        assertEquals(listOf(true, true, false, false), candidates.map { it.isBroadcast })
    }

    @Test
    fun `occupants without a real jid are omitted`() {
        val candidates = mentionCandidatesOf(
            mapOf(
                "ghost" to testPresence(
                    from = "room@muc.waddle.test/ghost",
                    mucRole = WaddleMucRole.PARTICIPANT,
                ),
            ),
        )

        assertEquals(listOf("everyone", "here"), candidates.map { it.display })
    }

    @Test
    fun `nicks colliding with broadcast identifiers are filtered`() {
        val candidates = mentionCandidatesOf(
            mapOf(
                "Everyone" to testPresence(
                    from = "room@muc.waddle.test/Everyone",
                    mucJid = "everyone@waddle.test",
                ),
            ),
        )

        assertEquals(listOf("everyone", "here"), candidates.map { it.display })
    }
}
