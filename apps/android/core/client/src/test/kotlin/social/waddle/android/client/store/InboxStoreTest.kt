package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.testInboxEntry

/**
 * XEP-0430 inbox reconciliation (web `channels/inbox.ts` +
 * `dms/conversations.ts` parity): freshness guard, read-clear
 * barrier, kind routing, thread keying, accounted-message dedupe.
 */
class InboxStoreTest {
    private val store = InboxStore()

    // ── Kind routing + thread keying ────────────────────────────────────

    @Test
    fun `direct and muc entries land in their own maps`() {
        store.applyEntry(testInboxEntry(partner = "alice@waddle.test", kind = "direct"))
        store.applyEntry(testInboxEntry(partner = "room@muc.waddle.test", kind = "muc"))

        assertEquals(setOf("alice@waddle.test"), store.direct.value.keys)
        assertEquals(setOf("room@muc.waddle.test"), store.muc.value.keys)
        assertTrue(store.threads.value.isEmpty())
    }

    @Test
    fun `entries are keyed by bare jid`() {
        store.applyEntry(testInboxEntry(partner = "alice@waddle.test/phone", kind = "direct"))

        assertEquals(setOf("alice@waddle.test"), store.direct.value.keys)
    }

    @Test
    fun `thread entries are keyed room plus thread and never touch the room maps`() {
        store.applyEntry(
            testInboxEntry(
                partner = "room@muc.waddle.test",
                kind = "muc",
                threadId = "t-1",
                threadTitle = "Planning",
                unread = 4u,
            ),
        )

        val key = InboxThreadKey("room@muc.waddle.test", "t-1")
        assertEquals(4, store.threads.value[key]?.unread)
        assertEquals("Planning", store.threads.value[key]?.threadTitle)
        assertTrue(store.muc.value.isEmpty())
        assertTrue(store.direct.value.isEmpty())
    }

    @Test
    fun `a thread entry and its room entry coexist independently`() {
        store.applyEntry(testInboxEntry(partner = "room@muc.waddle.test", kind = "muc", unread = 2u))
        store.applyEntry(
            testInboxEntry(partner = "room@muc.waddle.test", kind = "muc", threadId = "t-1", unread = 9u),
        )

        assertEquals(2, store.muc.value["room@muc.waddle.test"]?.unread)
        assertEquals(9, store.threads.value[InboxThreadKey("room@muc.waddle.test", "t-1")]?.unread)
    }

    // ── Freshness guard ─────────────────────────────────────────────────

    @Test
    fun `an older entry is dropped as stale`() {
        store.applyEntry(testInboxEntry(lastStanzaId = "s2", lastUpdated = 2_000L, unread = 3u))

        val applied = store.applyEntry(
            testInboxEntry(lastStanzaId = "s1", lastUpdated = 1_000L, unread = 7u),
        )

        assertNull(applied)
        assertEquals(3, store.direct.value["alice@waddle.test"]?.unread)
    }

    @Test
    fun `a same-time entry naming a different stanza without more unread is dropped`() {
        store.applyEntry(testInboxEntry(lastStanzaId = "s2", lastUpdated = 2_000L, unread = 3u))

        val applied = store.applyEntry(
            testInboxEntry(lastStanzaId = "s1", lastUpdated = 2_000L, unread = 3u),
        )

        assertNull(applied)
        assertEquals("s2", store.direct.value["alice@waddle.test"]?.lastStanzaId)
    }

    @Test
    fun `a same-time entry with more unread applies`() {
        store.applyEntry(testInboxEntry(lastStanzaId = "s2", lastUpdated = 2_000L, unread = 3u))

        val applied = store.applyEntry(
            testInboxEntry(lastStanzaId = "s3", lastUpdated = 2_000L, unread = 4u),
        )

        assertNotNull(applied)
        assertEquals(4, store.direct.value["alice@waddle.test"]?.unread)
    }

    @Test
    fun `a newer entry replaces the stored snapshot outright`() {
        store.applyEntry(testInboxEntry(lastStanzaId = "s1", lastUpdated = 1_000L, unread = 5u))

        val applied = store.applyEntry(
            testInboxEntry(lastStanzaId = "s2", lastUpdated = 2_000L, unread = 0u),
        )

        assertEquals(0, applied?.unread)
        assertEquals("s2", store.direct.value["alice@waddle.test"]?.lastStanzaId)
    }

    // ── Read-clear barrier ──────────────────────────────────────────────

    @Test
    fun `mark read locally zeroes the stored entry`() {
        store.applyEntry(testInboxEntry(unread = 6u))

        store.markReadLocally("alice@waddle.test")

        assertEquals(0, store.direct.value["alice@waddle.test"]?.unread)
    }

    @Test
    fun `a racing push naming the read stanza is clamped to zero`() {
        store.applyEntry(testInboxEntry(lastStanzaId = "s1", lastUpdated = 1_000L, unread = 6u))
        store.markReadLocally("alice@waddle.test")

        val applied = store.applyEntry(
            testInboxEntry(lastStanzaId = "s1", lastUpdated = 3_000L, unread = 6u),
        )

        assertEquals(0, applied?.unread)
        assertEquals(0, store.direct.value["alice@waddle.test"]?.unread)
    }

    @Test
    fun `a push with a genuinely newer stanza voids the barrier`() {
        store.applyEntry(testInboxEntry(lastStanzaId = "s1", lastUpdated = 1_000L, unread = 6u))
        store.markReadLocally("alice@waddle.test")

        val newer = store.applyEntry(
            testInboxEntry(lastStanzaId = "s2", lastUpdated = 3_000L, unread = 1u),
        )
        assertEquals(1, newer?.unread)

        // The barrier is gone: a later push for s2 applies untouched.
        val later = store.applyEntry(
            testInboxEntry(lastStanzaId = "s2", lastUpdated = 4_000L, unread = 2u),
        )
        assertEquals(2, later?.unread)
    }

    @Test
    fun `thread barriers are scoped to the thread`() {
        store.applyEntry(
            testInboxEntry(partner = "room@muc.waddle.test", kind = "muc", threadId = "t-1", unread = 3u),
        )
        store.applyEntry(
            testInboxEntry(partner = "room@muc.waddle.test", kind = "muc", lastStanzaId = "s9", unread = 5u),
        )

        store.markReadLocally("room@muc.waddle.test", threadId = "t-1")

        assertEquals(0, store.threads.value[InboxThreadKey("room@muc.waddle.test", "t-1")]?.unread)
        assertEquals(5, store.muc.value["room@muc.waddle.test"]?.unread)
    }

    @Test
    fun `mark read for an unknown conversation is a no-op`() {
        store.markReadLocally("ghost@waddle.test")

        assertTrue(store.direct.value.isEmpty())
    }

    // ── Accounted-message dedupe ────────────────────────────────────────

    @Test
    fun `applied entries account their newest stanza id`() {
        store.applyEntry(testInboxEntry(lastStanzaId = "s1"))

        assertTrue(store.wasUnreadAccounted("alice@waddle.test", listOf("s1")))
        assertTrue(store.wasUnreadAccounted("alice@waddle.test/phone", listOf("other", "s1")))
        assertFalse(store.wasUnreadAccounted("alice@waddle.test", listOf("s2")))
        assertFalse(store.wasUnreadAccounted("bob@waddle.test", listOf("s1")))
    }

    @Test
    fun `stale entries do not account their stanza id`() {
        store.applyEntry(testInboxEntry(lastStanzaId = "s2", lastUpdated = 2_000L))
        store.applyEntry(testInboxEntry(lastStanzaId = "s1", lastUpdated = 1_000L, unread = 9u))

        assertFalse(store.wasUnreadAccounted("alice@waddle.test", listOf("s1")))
    }

    // ── Clear ───────────────────────────────────────────────────────────

    @Test
    fun `clear wipes maps barriers and accounted ids`() {
        store.applyEntry(testInboxEntry(lastStanzaId = "s1", unread = 3u))
        store.markReadLocally("alice@waddle.test")

        store.clear()

        assertTrue(store.direct.value.isEmpty())
        assertFalse(store.wasUnreadAccounted("alice@waddle.test", listOf("s1")))
        // No stale barrier survives the wipe: a fresh entry naming the
        // pre-clear stanza id applies with its own unread.
        val applied = store.applyEntry(testInboxEntry(lastStanzaId = "s1", unread = 3u))
        assertEquals(3, applied?.unread)
    }
}
