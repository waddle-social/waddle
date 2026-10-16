package social.waddle.android.service

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.testMessage
import social.waddle.client.ffi.WaddleStanzaId

/** One banner per message, whichever of push and the live stream comes first. */
class AnnouncedItemsTest {
    private val announced = AnnouncedItems()

    private fun dm(stanzaId: String, delayed: Boolean) = testMessage(
        from = "alice@waddle.test/phone",
        timestamp = if (delayed) "2026-10-16T09:00:00Z" else null,
    ).copy(stanzaIds = listOf(WaddleStanzaId(id = stanzaId, by = "icepuma@waddle.test")))

    @Test
    fun `the offline flush of a pushed message does not announce it again`() {
        assertTrue(announced.claimPush("s-pushed"))

        assertFalse(announced.claimLive(dm("s-pushed", delayed = true)))
    }

    @Test
    fun `a delayed redelivery push never announced is announced live`() {
        // XEP-0198 redelivery to a sibling resource: delayed, never pushed.
        assertTrue(announced.claimPush("s-other"))

        assertTrue(announced.claimLive(dm("s-redelivered", delayed = true)))
    }

    @Test
    fun `a push after the live message stays silent`() {
        assertTrue(announced.claimLive(dm("s-live", delayed = false)))

        assertTrue(announced.handledLive("s-live"))
        assertFalse(announced.claimPush("s-live"))
    }

    @Test
    fun `any of the message's stanza ids matches the pushed item`() {
        assertTrue(announced.claimPush("s-room"))
        val twoIds = dm("s-own", delayed = true).copy(
            stanzaIds = listOf(
                WaddleStanzaId(id = "s-own", by = "icepuma@waddle.test"),
                WaddleStanzaId(id = "s-room", by = "general@muc.waddle.test"),
            ),
        )

        assertFalse(announced.claimLive(twoIds))
    }

    @Test
    fun `a duplicate push posts once and sign-out forgets everything`() {
        assertTrue(announced.claimPush("s-1"))
        assertFalse(announced.claimPush("s-1"))

        announced.clear()
        assertTrue(announced.claimLive(dm("s-1", delayed = true)))
    }

    @Test
    fun `memory is bounded, oldest first`() {
        val small = AnnouncedItems(capacity = 2)
        small.claimPush("a")
        small.claimPush("b")
        small.claimPush("c")

        assertTrue("the oldest push is forgotten", small.claimLive(dm("a", delayed = true)))
        assertFalse(small.claimLive(dm("c", delayed = true)))
    }
}
