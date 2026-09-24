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
    fun `known reply survives overflow of other replies and unmatched events`() {
        tracker.track("first", "alice@waddle.test", false)
        tracker.acknowledge("first")
        repeat(300) { index ->
            tracker.track("reply-$index", "bob@waddle.test", false)
            tracker.acknowledge("reply-$index")
        }
        repeat(300) { index ->
            tracker.acknowledge("unknown-ack-$index")
            tracker.fail("unknown-failure-$index", rejected = false)
        }
        assertEquals("alice@waddle.test" to false, tracker.fail("first", rejected = true))
        assertEquals("bob@waddle.test" to false, tracker.fail("reply-0", rejected = true))
        tracker.acknowledge("first")
        assertTrue(tracker.track("first", "alice@waddle.test", false))
    }

    @Test
    fun `unmatched events stay bounded until reply registration`() {
        repeat(300) { index -> tracker.fail("early-$index", rejected = true) }
        assertFalse(tracker.track("early-0", "alice@waddle.test", false))
        assertTrue(tracker.track("early-299", "alice@waddle.test", false))
        tracker.acknowledge("early-299")
        assertTrue(tracker.track("early-299", "alice@waddle.test", false))
    }

    @Test
    fun `sign out removes reply state`() {
        tracker.track("known", "bob@waddle.test", false)
        tracker.acknowledge("known")
        tracker.fail("s1", rejected = true)
        tracker.clear()
        assertNull(tracker.fail("known", rejected = true))
        assertFalse(tracker.track("s1", "alice@waddle.test", false))
    }
}
