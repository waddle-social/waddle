package social.waddle.android.service

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ReplyDeliveryTrackerTest {
    private val tracker = ReplyDeliveryTracker()

    @Test
    fun `explicit rejection still identifies reply after server ack`() {
        assertFalse(tracker.track("s1", "alice@waddle.test", false))
        tracker.acknowledge("s1")
        assertEquals("alice@waddle.test" to false, tracker.fail("s1", rejected = true))
        tracker.acknowledge("s1")
        assertTrue(tracker.track("s1", "alice@waddle.test", false))
    }

    @Test
    fun `early rejection survives acknowledgement and reply registration`() {
        assertNull(tracker.fail("s1", rejected = true))
        tracker.acknowledge("s1")
        assertTrue(tracker.track("s1", "alice@waddle.test", false))
    }

    @Test
    fun `transport failure does not replace acknowledgement`() {
        tracker.acknowledge("s1")
        assertNull(tracker.fail("s1", rejected = false))
        assertFalse(tracker.track("s1", "alice@waddle.test", false))
    }

    @Test
    fun `sign out removes reply state`() {
        tracker.fail("s1", rejected = true)
        tracker.clear()
        assertFalse(tracker.track("s1", "alice@waddle.test", false))
    }
}
