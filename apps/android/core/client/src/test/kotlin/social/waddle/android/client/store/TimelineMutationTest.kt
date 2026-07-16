package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMessage

class TimelineMutationTest {
    private val store = TimelineStore()

    @Before
    fun setUp() {
        store.setOwnBareJid("me@waddle.test")
    }

    private fun dmTimeline() = store.timeline("alice@waddle.test").value

    private fun mucTimeline() = store.timeline("room@muc.waddle.test").value

    // --- reactions (XEP-0444) ---

    @Test
    fun `reaction aggregates on the target and never inserts a row`() {
        store.onLiveMessage(testMessage(stanzaId = "s1", body = "hi"))
        val inserted = store.onLiveMessage(
            testMessage(
                id = "r1",
                stanzaId = null,
                body = null,
                reactionTargetId = "s1",
                reactionEmojis = listOf("👍"),
            ),
        )

        assertFalse("a reaction is not a new row", inserted)
        val items = dmTimeline()
        assertEquals(1, items.size)
        assertEquals(listOf(ReactionGroup("👍", 1, mine = false)), items[0].reactions)
    }

    @Test
    fun `a sender's new reaction set replaces their previous set`() {
        store.onLiveMessage(testMessage(stanzaId = "s1", body = "hi"))
        store.onLiveMessage(
            testMessage(id = "r1", body = null, reactionTargetId = "s1", reactionEmojis = listOf("👍", "🎉")),
        )
        store.onLiveMessage(
            testMessage(id = "r2", body = null, reactionTargetId = "s1", reactionEmojis = listOf("❤️")),
        )

        assertEquals(listOf(ReactionGroup("❤️", 1, mine = false)), dmTimeline()[0].reactions)
    }

    @Test
    fun `a cleared reaction stays cleared against an older mam replay`() {
        store.onArchivedMessage(
            testArchivedMessage(mamId = "m0", stanzaId = "s1", timestamp = "2026-07-15T09:00:00Z"),
        )
        // The conversation has seen wire stamps up to 10:30 when the
        // live clear applies — the clear anchors there.
        store.onArchivedMessage(
            testArchivedMessage(mamId = "m2", stanzaId = "s2", timestamp = "2026-07-15T10:30:00Z", body = "later"),
        )
        store.onLiveMessage(
            testMessage(id = "r2", body = null, reactionTargetId = "s1", reactionEmojis = emptyList()),
        )
        // A MAM page replays the reaction stamped BEFORE the clear's
        // anchor: it must not resurrect the chip.
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m1",
                id = "r1",
                body = null,
                timestamp = "2026-07-15T10:00:00Z",
                reactionTargetId = "s1",
                reactionEmojis = listOf("👍"),
            ),
        )

        assertTrue(dmTimeline().first { it.id == "s1" }.reactions.isEmpty())
    }

    @Test
    fun `an archived mutation stamped after everything seen beats a live apply`() {
        // Web appliedAfterWire parity: the live apply anchors at the
        // newest wire stamp SEEN (09:00); a reconnect replay stamped
        // later carries genuinely newer state and must win — an
        // absolute newest-rank for live applies would freeze stale
        // reactions forever after a stream drop.
        store.onArchivedMessage(
            testArchivedMessage(mamId = "m0", stanzaId = "s1", timestamp = "2026-07-15T09:00:00Z"),
        )
        store.onLiveMessage(
            testMessage(id = "r1", body = null, reactionTargetId = "s1", reactionEmojis = listOf("👍")),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m1",
                id = "r2",
                body = null,
                timestamp = "2026-07-15T10:05:00Z",
                reactionTargetId = "s1",
                reactionEmojis = listOf("❤️"),
            ),
        )

        assertEquals(
            listOf(ReactionGroup("❤️", 1, mine = false)),
            dmTimeline().first { it.id == "s1" }.reactions,
        )
    }

    @Test
    fun `a correction of a reply strips the quoted fallback prefix`() {
        store.onLiveMessage(testMessage(stanzaId = "s1", body = "original"))
        val prefix = "> quoted line\n\n"
        store.onLiveMessage(
            testMessage(
                id = "c1",
                body = prefix + "edited reply",
                replacesId = "s1",
                replyFallbackStart = 0u,
                replyFallbackEnd = prefix.codePointCount(0, prefix.length).toUInt(),
            ),
        )

        assertEquals("edited reply", dmTimeline().single().body)
    }

    @Test
    fun `an empty reaction set clears the sender's reactions`() {
        store.onLiveMessage(testMessage(stanzaId = "s1", body = "hi"))
        store.onLiveMessage(
            testMessage(id = "r1", body = null, reactionTargetId = "s1", reactionEmojis = listOf("👍")),
        )
        store.onLiveMessage(
            testMessage(id = "r2", body = null, reactionTargetId = "s1", reactionEmojis = emptyList()),
        )

        assertTrue(dmTimeline()[0].reactions.isEmpty())
    }

    @Test
    fun `own reactions mark the group mine and dm senders aggregate by bare jid`() {
        store.onLiveMessage(testMessage(stanzaId = "s1", body = "hi"))
        // Own reaction (from = me, any resource) and the peer's reaction.
        store.onLiveMessage(
            testMessage(
                id = "r1",
                from = "me@waddle.test/phone",
                to = "alice@waddle.test",
                body = null,
                reactionTargetId = "s1",
                reactionEmojis = listOf("👍"),
            ),
        )
        store.onLiveMessage(
            testMessage(id = "r2", body = null, reactionTargetId = "s1", reactionEmojis = listOf("👍")),
        )

        assertEquals(listOf(ReactionGroup("👍", 2, mine = true)), dmTimeline()[0].reactions)
    }

    @Test
    fun `muc reactions key senders by occupant jid`() {
        store.onLiveMessage(
            testMessage(
                stanzaId = "m1",
                from = "room@muc.waddle.test/alice",
                to = null,
                messageType = "groupchat",
                isMuc = true,
                body = "hi",
            ),
        )
        // Two distinct occupants of the same account still count separately
        // per XEP-0444 occupant identity.
        listOf("bob", "carol").forEachIndexed { index, nick ->
            store.onLiveMessage(
                testMessage(
                    id = "r$index",
                    from = "room@muc.waddle.test/$nick",
                    to = null,
                    messageType = "groupchat",
                    isMuc = true,
                    body = null,
                    reactionTargetId = "m1",
                    reactionEmojis = listOf("🔥"),
                ),
            )
        }

        assertEquals(listOf(ReactionGroup("🔥", 2, mine = false)), mucTimeline()[0].reactions)
    }

    @Test
    fun `stale archived reaction never downgrades a newer one`() {
        store.onArchivedMessage(
            testArchivedMessage(mamId = "m0", stanzaId = "s1", timestamp = "2026-07-15T09:00:00Z"),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m2",
                id = "r-new",
                body = null,
                timestamp = "2026-07-15T11:00:00Z",
                reactionTargetId = "s1",
                reactionEmojis = listOf("🎉"),
            ),
        )
        // Replayed OLDER reaction from the same sender must not win.
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m1",
                id = "r-old",
                body = null,
                timestamp = "2026-07-15T10:00:00Z",
                reactionTargetId = "s1",
                reactionEmojis = listOf("👍"),
            ),
        )

        assertEquals(listOf(ReactionGroup("🎉", 1, mine = false)), dmTimeline()[0].reactions)
    }

    @Test
    fun `reaction arriving before its target parks and applies on insert`() {
        // Backwards MAM paging: the newer page (with the reaction) loads
        // before the older page holding the target.
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m2",
                id = "r1",
                body = null,
                timestamp = "2026-07-15T11:00:00Z",
                reactionTargetId = "s1",
                reactionEmojis = listOf("👍"),
            ),
        )
        assertTrue(dmTimeline().isEmpty())

        store.onArchivedMessage(
            testArchivedMessage(mamId = "m1", stanzaId = "s1", timestamp = "2026-07-15T09:00:00Z"),
        )
        assertEquals(listOf(ReactionGroup("👍", 1, mine = false)), dmTimeline()[0].reactions)
    }

    // --- corrections (XEP-0308) ---

    @Test
    fun `correction swaps the body in place keeps the timestamp and marks edited`() {
        store.onArchivedMessage(
            testArchivedMessage(mamId = "m1", stanzaId = "s1", timestamp = "2026-07-15T09:00:00Z", body = "helo"),
        )
        val inserted = store.onLiveMessage(
            testMessage(id = "c1", body = "hello", replacesId = "s1"),
        )

        assertFalse(inserted)
        val item = dmTimeline().single()
        assertEquals("hello", item.body)
        assertTrue(item.edited)
        assertEquals("2026-07-15T09:00:00Z", item.timestamp)
    }

    @Test
    fun `correction from a different sender is rejected`() {
        store.onLiveMessage(testMessage(stanzaId = "s1", body = "original", from = "alice@waddle.test"))
        store.onLiveMessage(
            testMessage(
                id = "c1",
                from = "mallory@waddle.test",
                to = "me@waddle.test",
                body = "hacked",
                replacesId = "s1",
            ),
        )

        // Mallory's "correction" opens mallory's own conversation but must
        // not touch alice's row either way.
        val item = store.timeline("alice@waddle.test").value.single()
        assertEquals("original", item.body)
        assertFalse(item.edited)
    }

    @Test
    fun `muc correction requires the same occupant`() {
        store.onLiveMessage(
            testMessage(
                stanzaId = "m1",
                from = "room@muc.waddle.test/alice",
                to = null,
                messageType = "groupchat",
                isMuc = true,
                body = "original",
            ),
        )
        store.onLiveMessage(
            testMessage(
                id = "c1",
                from = "room@muc.waddle.test/mallory",
                to = null,
                messageType = "groupchat",
                isMuc = true,
                body = "hacked",
                replacesId = "m1",
            ),
        )
        assertEquals("original", mucTimeline().single().body)

        store.onLiveMessage(
            testMessage(
                id = "c2",
                from = "room@muc.waddle.test/alice",
                to = null,
                messageType = "groupchat",
                isMuc = true,
                body = "fixed",
                replacesId = "m1",
            ),
        )
        assertEquals("fixed", mucTimeline().single().body)
    }

    @Test
    fun `correction chain applies the newest correction`() {
        store.onArchivedMessage(
            testArchivedMessage(mamId = "m1", stanzaId = "s1", timestamp = "2026-07-15T09:00:00Z", body = "v1"),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m2", id = "c1", timestamp = "2026-07-15T09:01:00Z", body = "v2", replacesId = "s1",
            ),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m3", id = "c2", timestamp = "2026-07-15T09:02:00Z", body = "v3", replacesId = "s1",
            ),
        )

        assertEquals("v3", dmTimeline().single().body)
    }

    // --- retraction (XEP-0424) & moderation (XEP-0425) ---

    @Test
    fun `retraction by the sender tombstones the row`() {
        store.onLiveMessage(testMessage(stanzaId = "s1", body = "oops"))
        store.onLiveMessage(testMessage(id = "r1", body = null, retractsId = "s1"))

        val item = dmTimeline().single()
        assertEquals(MessageTombstone.Retracted, item.tombstone)
    }

    @Test
    fun `retraction by another sender is rejected`() {
        store.onLiveMessage(testMessage(stanzaId = "s1", body = "keep me", from = "alice@waddle.test"))
        store.onLiveMessage(
            testMessage(id = "r1", from = "mallory@waddle.test", to = "me@waddle.test", body = null, retractsId = "s1"),
        )

        assertNull(store.timeline("alice@waddle.test").value.single().tombstone)
    }

    @Test
    fun `tombstoned rows reject later corrections`() {
        store.onLiveMessage(testMessage(stanzaId = "s1", body = "oops"))
        store.onLiveMessage(testMessage(id = "r1", body = null, retractsId = "s1"))
        store.onLiveMessage(testMessage(id = "c1", body = "resurrected", replacesId = "s1"))

        val item = dmTimeline().single()
        assertEquals(MessageTombstone.Retracted, item.tombstone)
        assertEquals("oops", item.body)
    }

    @Test
    fun `moderation from the room service tombstones with actor and reason`() {
        store.onLiveMessage(
            testMessage(
                stanzaId = "m1",
                from = "room@muc.waddle.test/alice",
                to = null,
                messageType = "groupchat",
                isMuc = true,
                body = "spam",
            ),
        )
        store.onLiveMessage(
            testMessage(
                id = "mod1",
                from = "room@muc.waddle.test",
                to = null,
                messageType = "groupchat",
                isMuc = true,
                body = null,
                moderationTargetId = "m1",
                moderatedBy = "room@muc.waddle.test/admin",
                moderationReason = "spam",
            ),
        )

        assertEquals(
            MessageTombstone.Moderated(moderatedBy = "room@muc.waddle.test/admin", reason = "spam"),
            mucTimeline().single().tombstone,
        )
    }

    @Test
    fun `moderation claimed by an occupant is a spoof and is dropped`() {
        store.onLiveMessage(
            testMessage(
                stanzaId = "m1",
                from = "room@muc.waddle.test/alice",
                to = null,
                messageType = "groupchat",
                isMuc = true,
                body = "keep me",
            ),
        )
        store.onLiveMessage(
            testMessage(
                id = "mod1",
                from = "room@muc.waddle.test/mallory",
                to = null,
                messageType = "groupchat",
                isMuc = true,
                body = null,
                moderationTargetId = "m1",
                moderatedBy = "room@muc.waddle.test/mallory",
            ),
        )

        assertNull(mucTimeline().single().tombstone)
    }

    @Test
    fun `archived retracted original inserts as a tombstone`() {
        store.onArchivedMessage(
            testArchivedMessage(mamId = "m1", stanzaId = "s1", body = "gone", isRetracted = true),
        )

        assertEquals(MessageTombstone.Retracted, dmTimeline().single().tombstone)
    }

    // --- identity resolution ---

    @Test
    fun `sender-scoped retraction resolves its own row despite a cross-sender collision`() {
        // Two rows from DIFFERENT senders claiming the same origin id
        // (cross-sender collision — never merged). Alice's retraction is
        // sender-scoped (web parity): it lands on HER row only.
        store.onLiveMessage(testMessage(id = "a", stanzaId = "s1", originId = "dup", body = "one"))
        store.onLiveMessage(
            testMessage(
                id = "b",
                from = "me@waddle.test/phone",
                to = "alice@waddle.test",
                stanzaId = "s2",
                originId = "dup",
                body = "two",
            ),
        )
        store.onLiveMessage(testMessage(id = "r1", body = null, retractsId = "dup"))

        val items = dmTimeline()
        assertEquals(2, items.size)
        assertEquals(MessageTombstone.Retracted, items.first { it.id == "s1" }.tombstone)
        assertNull(items.first { it.id == "s2" }.tombstone)
    }

    @Test
    fun `reaction targeting an ambiguous cross-sender alias is dropped`() {
        // Reactions are NOT sender-scoped: an alias claimed by two rows
        // stays unresolvable and the reaction applies to neither.
        store.onLiveMessage(testMessage(id = "a", stanzaId = "s1", originId = "dup", body = "one"))
        store.onLiveMessage(
            testMessage(
                id = "b",
                from = "me@waddle.test/phone",
                to = "alice@waddle.test",
                stanzaId = "s2",
                originId = "dup",
                body = "two",
            ),
        )
        store.onLiveMessage(
            testMessage(id = "r1", body = null, reactionTargetId = "dup", reactionEmojis = listOf("👍")),
        )

        assertTrue(dmTimeline().all { it.reactions.isEmpty() })
    }

    @Test
    fun `mutation matching a primary id wins even when aliases collide`() {
        store.onLiveMessage(testMessage(id = "a", stanzaId = "s1", originId = "s2", body = "one"))
        store.onLiveMessage(
            testMessage(
                id = "b",
                from = "me@waddle.test/phone",
                to = "alice@waddle.test",
                stanzaId = "s2",
                body = "two",
            ),
        )
        // Own retraction of the own row whose primary id is "s2".
        store.onLiveMessage(
            testMessage(
                id = "r1",
                from = "me@waddle.test/phone",
                to = "alice@waddle.test",
                body = null,
                retractsId = "s2",
            ),
        )

        val items = dmTimeline()
        assertEquals(MessageTombstone.Retracted, items.first { it.id == "s2" }.tombstone)
        assertNull(items.first { it.id == "s1" }.tombstone)
    }

    @Test
    fun `same-sender wire-identity replay dedupes instead of duplicating`() {
        // The MAM copy of an own DM echo: echo keyed by origin id, the
        // archive copy by server stanza id, overlapping via origin id.
        store.onLiveMessage(
            testMessage(
                id = "client-id",
                from = "me@waddle.test/phone",
                to = "alice@waddle.test",
                originId = "client-id",
                body = "hi",
            ),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "mam-1",
                id = "client-id",
                stanzaId = "server-id",
                originId = "client-id",
                from = "me@waddle.test/phone",
                to = "alice@waddle.test",
                body = "hi",
            ),
        )

        val items = dmTimeline()
        assertEquals(1, items.size)
        assertEquals("the archive timestamp is adopted", "2026-07-15T10:00:00Z", items[0].timestamp)
    }

    @Test
    fun `mutations survive the live upgrade of an archived row`() {
        store.onArchivedMessage(
            testArchivedMessage(mamId = "m1", stanzaId = "s1", timestamp = "2026-07-15T09:00:00Z", body = "hi"),
        )
        store.onLiveMessage(
            testMessage(id = "r1", body = null, reactionTargetId = "s1", reactionEmojis = listOf("👍")),
        )
        // XEP-0198 replay upgrades the archived row to its live twin.
        store.onLiveMessage(testMessage(stanzaId = "s1", body = "hi"))

        val item = dmTimeline().single()
        assertTrue(item.source is TimelineSource.Live)
        assertEquals(listOf(ReactionGroup("👍", 1, mine = false)), item.reactions)
    }

    @Test
    fun `pending mutations are bounded per conversation`() {
        val bounded = TimelineStore(maxPendingMutationsPerConversation = 2)
        bounded.setOwnBareJid("me@waddle.test")
        // Three parked reactions; the first (👍 on s1) overflows and drops.
        listOf("s1" to "👍", "s2" to "🎉", "s3" to "❤️").forEachIndexed { index, (target, emoji) ->
            bounded.onLiveMessage(
                testMessage(id = "r$index", body = null, reactionTargetId = target, reactionEmojis = listOf(emoji)),
            )
        }
        bounded.onLiveMessage(testMessage(id = "a", stanzaId = "s1", body = "one"))
        bounded.onLiveMessage(testMessage(id = "b", stanzaId = "s2", body = "two"))

        val items = bounded.timeline("alice@waddle.test").value
        assertTrue(items.first { it.id == "s1" }.reactions.isEmpty())
        assertEquals(listOf(ReactionGroup("🎉", 1, mine = false)), items.first { it.id == "s2" }.reactions)
    }
}
