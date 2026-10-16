package social.waddle.android.service

import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.testMessage
import social.waddle.client.ffi.WaddleStanzaId

/** One banner per message, whichever of push and the live stream comes first. */
class AnnouncedItemsTest {
    private val prefs = SessionPrefs(InMemoryPreferencesDataStore())
    private val announced = AnnouncedItems(SessionPrefsPushedItems(prefs))

    /** A DM to [OWN] whose archive id [stanzaId] was stamped by [by]. */
    private fun dm(stanzaId: String, delayed: Boolean = true, by: String = OWN) = testMessage(
        from = "alice@waddle.test/phone",
        timestamp = if (delayed) "2026-10-16T09:00:00Z" else null,
    ).copy(stanzaIds = listOf(WaddleStanzaId(id = stanzaId, by = by)))

    private fun roomMessage(stanzaId: String, by: String) = testMessage(
        from = "$ROOM/alice",
        messageType = "groupchat",
        isMuc = true,
    ).copy(stanzaIds = listOf(WaddleStanzaId(id = stanzaId, by = by)))

    @Test
    fun `the offline flush of a pushed message does not announce it again`() = runTest {
        assertTrue(announced.claimPush("s-pushed"))

        assertFalse(announced.claimLive(dm("s-pushed"), OWN))
    }

    @Test
    fun `a push posted by a process that died still silences the flush in the next one`() = runTest {
        assertTrue(announced.claimPush("s-pushed"))

        // Process B: a fresh dedupe over the same persisted store.
        val nextProcess = AnnouncedItems(SessionPrefsPushedItems(prefs))
        assertFalse(nextProcess.claimLive(dm("s-pushed"), OWN))
        assertTrue(nextProcess.claimLive(dm("s-new"), OWN))
    }

    @Test
    fun `a delayed redelivery push never announced is announced live`() = runTest {
        // XEP-0198 redelivery to a sibling resource: delayed, never pushed.
        assertTrue(announced.claimPush("s-other"))

        assertTrue(announced.claimLive(dm("s-redelivered"), OWN))
    }

    @Test
    fun `a push after the live message stays silent`() = runTest {
        assertTrue(announced.claimLive(dm("s-live", delayed = false), OWN))

        assertTrue(announced.handledLive("s-live"))
        assertFalse(announced.claimPush("s-live"))
    }

    @Test
    fun `a stanza-id planted by the sender cannot silence the room push`() = runTest {
        // XEP-0359 §4: a DM's id is the account's, a room message's the
        // room's. Mallory's own <stanza-id by='mallory@…'/> proves nothing.
        assertTrue(announced.claimLive(dm("s-room", by = "mallory@waddle.test"), OWN))
        assertTrue(announced.claimLive(roomMessage("s-room", by = "mallory@waddle.test"), OWN))

        assertFalse(announced.handledLive("s-room"))
        assertTrue("the room push still posts", announced.claimPush("s-room"))
    }

    @Test
    fun `only the archiving authority's id matches the pushed item`() = runTest {
        assertTrue(announced.claimPush("s-room"))
        assertFalse(
            "the room's own id matches",
            announced.claimLive(roomMessage("s-room", by = ROOM.uppercase()), OWN),
        )

        assertTrue(announced.claimPush("s-dm"))
        assertFalse(announced.claimLive(dm("s-dm"), OWN))
    }

    @Test
    fun `a duplicate push posts once and sign-out forgets everything, on disk too`() = runTest {
        assertTrue(announced.claimPush("s-1"))
        assertFalse(announced.claimPush("s-1"))

        announced.clear()
        assertEquals(emptyList<String>(), prefs.pushAnnouncedItems.first())
        assertTrue(announced.claimLive(dm("s-1"), OWN))
    }

    @Test
    fun `memory is bounded, oldest first, on disk too`() = runTest {
        val small = AnnouncedItems(SessionPrefsPushedItems(prefs), capacity = 2)
        small.claimPush("a")
        small.claimPush("b")
        small.claimPush("c")

        assertEquals(listOf("b", "c"), prefs.pushAnnouncedItems.first())
        assertTrue("the oldest push is forgotten", small.claimLive(dm("a"), OWN))
        assertFalse(small.claimLive(dm("c"), OWN))
    }

    private companion object {
        const val OWN = "icepuma@waddle.test"
        const val ROOM = "general@muc.waddle.test"
    }
}
