package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class IdleAgeTest {

    private val nowMs = 1_750_000_000_000L

    private fun sinceMs(deltaMs: Long): String =
        java.time.Instant.ofEpochMilli(nowMs - deltaMs).toString()

    @Test
    fun `buckets mirror the web formatIdle`() {
        assertEquals(IdleAge.UnderMinute, idleAgeOf(sinceMs(30_000), nowMs))
        assertEquals(IdleAge.Minutes(20), idleAgeOf(sinceMs(20 * 60_000), nowMs))
        assertEquals(IdleAge.Minutes(59), idleAgeOf(sinceMs(59 * 60_000 + 59_000), nowMs))
        assertEquals(IdleAge.Hours(1), idleAgeOf(sinceMs(60 * 60_000), nowMs))
        assertEquals(IdleAge.Hours(23), idleAgeOf(sinceMs(23 * 60 * 60_000L), nowMs))
        assertEquals(IdleAge.Days(1), idleAgeOf(sinceMs(24 * 60 * 60_000L), nowMs))
        assertEquals(IdleAge.Days(3), idleAgeOf(sinceMs(3 * 24 * 60 * 60_000L), nowMs))
    }

    @Test
    fun `future idle instants clamp to under a minute`() {
        assertEquals(IdleAge.UnderMinute, idleAgeOf(sinceMs(-60_000), nowMs))
    }

    @Test
    fun `unparsable timestamps yield null`() {
        assertNull(idleAgeOf("garbage", nowMs))
    }

    @Test
    fun `idle shows only for away and xa`() {
        assertTrue(presenceShowsIdle("away"))
        assertTrue(presenceShowsIdle("xa"))
        assertFalse(presenceShowsIdle("dnd"))
        assertFalse(presenceShowsIdle("chat"))
        assertFalse(presenceShowsIdle(null))
    }
}
